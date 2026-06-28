//! `#[derive(phoxal::Runtime)]` — static metadata from the runtime struct.
//!
//! Reads the mandatory `#[phoxal(api = y2026_N)]` selector (plus optional
//! `id` / `config`) and the struct's typed handle fields, then emits:
//! - an `impl RuntimeFields` carrying `ID`, `Api`/`API_VERSION`, `Config`, and
//!   `FIELD_CONTRACTS` (one `{family, topic, direction}` per handle, all sharing
//!   the one API version);
//! - compile assertions that every handle body satisfies
//!   `ContractBody<Api = Self::Api>` (a body from another API version is a
//!   compile error — D60);
//! - `Declares<Body>` marker impls so `SetupContext` builders reject undeclared
//!   contract families at compile time (D44).
//!
//! Handles are recognized by **canonical syntactic form** only (`Publisher<T>`,
//! `Subscriber<T>`, `Latest<T>`, `Querier<A, B>`, and `Vec`/`BTreeMap` of them);
//! every other field is ignored as runtime-private state.

use heck::ToKebabCase;
use proc_macro2::TokenStream;
use quote::{ToTokens, quote};
use syn::{
    Data, DeriveInput, Fields, GenericArgument, Ident, LitStr, PathArguments, Type, TypePath,
};

use crate::util::phoxal;

pub fn expand(input: TokenStream) -> syn::Result<TokenStream> {
    let input: DeriveInput = syn::parse2(input)?;
    let struct_name = &input.ident;

    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "#[derive(phoxal::Runtime)] does not support generic runtime structs",
        ));
    }

    let args = PhoxalArgs::parse(&input)?;
    let api = args.api;
    let id = args
        .id
        .unwrap_or_else(|| struct_name.to_string().to_kebab_case());
    let config_ty: Type = match args.config {
        Some(ty) => ty,
        None => syn::parse_quote!(()),
    };

    let fields = match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(named) => &named.named,
            _ => {
                return Err(syn::Error::new_spanned(
                    struct_name,
                    "#[derive(phoxal::Runtime)] requires a struct with named fields",
                ));
            }
        },
        _ => {
            return Err(syn::Error::new_spanned(
                struct_name,
                "#[derive(phoxal::Runtime)] can only be applied to structs",
            ));
        }
    };

    // Collect (body type, direction) for every recognized handle field.
    let mut uses: Vec<HandleUse> = Vec::new();
    for field in fields {
        if let Some(found) = classify_handle(&field.ty) {
            uses.extend(found);
        }
    }

    let phoxal = phoxal();

    // FIELD_CONTRACTS entries reference the body type's ContractBody consts so the
    // family/topic/api_version are single-sourced from the api tree (D61).
    let contract_entries = uses.iter().map(|u| {
        let body = &u.body;
        let dir = u.direction.tokens(&phoxal);
        quote! {
            #phoxal::runtime::ContractUse {
                api_version: <<#body as #phoxal::api::ContractBody>::Api as #phoxal::api::ApiVersion>::ID,
                family: <#body as #phoxal::api::ContractBody>::FAMILY,
                topic: <#body as #phoxal::api::ContractBody>::TOPIC,
                direction: #dir,
            }
        }
    });

    // One ContractBody<Api = Self::Api> assertion per handle body (D60).
    let assertions = uses.iter().enumerate().map(|(i, u)| {
        let body = &u.body;
        let assert_fn = syn::Ident::new(&format!("__assert_body_{i}"), struct_name.span());
        quote! {
            fn #assert_fn() {
                fn assert<R, B>()
                where
                    R: #phoxal::runtime::RuntimeFields,
                    B: #phoxal::api::ContractBody<Api = <R as #phoxal::runtime::RuntimeFields>::Api>,
                {}
                assert::<#struct_name, #body>();
            }
        }
    });

    // Declares<Body> markers (deduplicated by token string) so builders accept
    // only declared contract families.
    let mut seen = std::collections::BTreeSet::new();
    let mut declares = TokenStream::new();
    for u in &uses {
        let key = u.body.to_token_stream().to_string();
        if seen.insert(key) {
            let body = &u.body;
            declares.extend(quote! {
                impl #phoxal::runtime::Declares<#body> for #struct_name {}
            });
        }
    }

    Ok(quote! {
        impl #phoxal::runtime::RuntimeFields for #struct_name {
            const ID: &'static str = #id;
            type Api = #phoxal::api::#api::Api;
            const API_VERSION: &'static str =
                <#phoxal::api::#api::Api as #phoxal::api::ApiVersion>::ID;
            type Config = #config_ty;
            const FIELD_CONTRACTS: &'static [#phoxal::runtime::ContractUse] = &[
                #(#contract_entries),*
            ];
        }

        #declares

        const _: () = {
            #(#assertions)*
        };
    })
}

struct PhoxalArgs {
    id: Option<String>,
    api: Ident,
    config: Option<Type>,
}

impl PhoxalArgs {
    fn parse(input: &DeriveInput) -> syn::Result<Self> {
        let mut id = None;
        let mut api = None;
        let mut config = None;

        for attr in &input.attrs {
            if !attr.path().is_ident("phoxal") {
                continue;
            }
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("id") {
                    let value: LitStr = meta.value()?.parse()?;
                    id = Some(value.value());
                    Ok(())
                } else if meta.path.is_ident("api") {
                    let value: Ident = meta.value()?.parse()?;
                    api = Some(value);
                    Ok(())
                } else if meta.path.is_ident("config") {
                    let value: Type = meta.value()?.parse()?;
                    config = Some(value);
                    Ok(())
                } else {
                    Err(meta.error("unknown #[phoxal(...)] key (expected id, api, or config)"))
                }
            })?;
        }

        let api = api.ok_or_else(|| {
            syn::Error::new_spanned(
                &input.ident,
                "#[derive(phoxal::Runtime)] requires #[phoxal(api = y2026_N)] (D59/D60): \
                 it selects the one API version this runtime — and the whole graph — runs against",
            )
        })?;

        Ok(PhoxalArgs { id, api, config })
    }
}

struct HandleUse {
    body: Type,
    direction: Direction,
}

#[derive(Clone, Copy)]
enum Direction {
    Publish,
    Subscribe,
    QueryRequest,
    QueryResponse,
}

impl Direction {
    fn tokens(self, phoxal: &TokenStream) -> TokenStream {
        let variant = match self {
            Direction::Publish => quote!(Publish),
            Direction::Subscribe => quote!(Subscribe),
            Direction::QueryRequest => quote!(QueryRequest),
            Direction::QueryResponse => quote!(QueryResponse),
        };
        quote!(#phoxal::runtime::Direction::#variant)
    }
}

/// Recognize a handle field by canonical syntactic form, returning the body
/// type(s) + direction(s). `Vec`/`BTreeMap` of a handle carry the inner handle's
/// contracts. Returns `None` for non-handle (runtime-private) fields.
fn classify_handle(ty: &Type) -> Option<Vec<HandleUse>> {
    let path = as_type_path(ty)?;
    let seg = path.path.segments.last()?;
    let name = seg.ident.to_string();

    match name.as_str() {
        "Publisher" => single(generic_type(seg, 0)?, Direction::Publish),
        "Subscriber" | "Latest" => single(generic_type(seg, 0)?, Direction::Subscribe),
        "Querier" => {
            let req = generic_type(seg, 0)?;
            let resp = generic_type(seg, 1)?;
            Some(vec![
                HandleUse {
                    body: req,
                    direction: Direction::QueryRequest,
                },
                HandleUse {
                    body: resp,
                    direction: Direction::QueryResponse,
                },
            ])
        }
        // Vec<Handle> — the element is the handle.
        "Vec" => classify_handle(&generic_type(seg, 0)?),
        // BTreeMap<K, Handle> / HashMap<K, Handle> — the value is the handle.
        "BTreeMap" | "HashMap" => classify_handle(&generic_type(seg, 1)?),
        _ => None,
    }
}

fn single(body: Type, direction: Direction) -> Option<Vec<HandleUse>> {
    Some(vec![HandleUse { body, direction }])
}

fn as_type_path(ty: &Type) -> Option<&TypePath> {
    match ty {
        Type::Path(p) => Some(p),
        _ => None,
    }
}

/// Extract the `n`-th generic *type* argument of a path segment.
fn generic_type(seg: &syn::PathSegment, n: usize) -> Option<Type> {
    let PathArguments::AngleBracketed(args) = &seg.arguments else {
        return None;
    };
    let types: Vec<&Type> = args
        .args
        .iter()
        .filter_map(|a| match a {
            GenericArgument::Type(t) => Some(t),
            _ => None,
        })
        .collect();
    types.get(n).cloned().cloned()
}
