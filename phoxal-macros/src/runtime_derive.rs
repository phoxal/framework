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

    // Collect a declaration for every recognized handle field.
    let mut decls: Vec<Decl> = Vec::new();
    for field in fields {
        if let Some(found) = classify_handle(&field.ty) {
            decls.extend(found);
        }
    }

    let phoxal = phoxal();

    // FIELD_CONTRACTS entries reference the body type's ContractBody consts so the
    // family/topic/api_version are single-sourced from the api tree (D61). A query
    // field contributes both legs.
    let contract_entries = decls.iter().flat_map(|d| {
        d.contract_bodies()
            .into_iter()
            .map(|(body, dir)| {
                let dir = dir.tokens(&phoxal);
                quote! {
                    #phoxal::runtime::ContractUse {
                        api_version: <<#body as #phoxal::api::ContractBody>::Api as #phoxal::api::ApiVersion>::ID,
                        family: <#body as #phoxal::api::ContractBody>::FAMILY,
                        topic: <#body as #phoxal::api::ContractBody>::TOPIC,
                        direction: #dir,
                    }
                }
            })
            .collect::<Vec<_>>()
    });

    // One ContractBody<Api = Self::Api> assertion per body (D60), so a body from
    // another API version is a compile error.
    let bodies: Vec<&Type> = decls.iter().flat_map(|d| d.bodies()).collect();
    let assertions = bodies.iter().enumerate().map(|(i, body)| {
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

    // Direction-specific Declares markers (D44): publishing a family declared only
    // as subscribe (or vice versa) is a compile error — and would otherwise be IO
    // `emit-apis` never reports. Deduplicated by (direction, body) token string.
    let mut seen = std::collections::BTreeSet::new();
    let mut declares = TokenStream::new();
    for decl in &decls {
        let (marker, key) = match decl {
            Decl::Publish(b) => (
                quote!(impl #phoxal::runtime::DeclaresPublish<#b> for #struct_name {}),
                format!("pub:{}", b.to_token_stream()),
            ),
            Decl::Subscribe(b) => (
                quote!(impl #phoxal::runtime::DeclaresSubscribe<#b> for #struct_name {}),
                format!("sub:{}", b.to_token_stream()),
            ),
            Decl::Query { req, resp } => (
                quote!(impl #phoxal::runtime::DeclaresQuery<#req, #resp> for #struct_name {}),
                format!("qry:{}=>{}", req.to_token_stream(), resp.to_token_stream()),
            ),
        };
        if seen.insert(key) {
            declares.extend(marker);
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

/// A handle declaration recognized from a struct field.
#[allow(clippy::large_enum_variant)] // transient macro-internal AST holder
enum Decl {
    Publish(Type),
    Subscribe(Type),
    Query { req: Type, resp: Type },
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

impl Decl {
    /// The (body, direction) contract entries this declaration contributes.
    fn contract_bodies(&self) -> Vec<(Type, Direction)> {
        match self {
            Decl::Publish(b) => vec![(b.clone(), Direction::Publish)],
            Decl::Subscribe(b) => vec![(b.clone(), Direction::Subscribe)],
            Decl::Query { req, resp } => vec![
                (req.clone(), Direction::QueryRequest),
                (resp.clone(), Direction::QueryResponse),
            ],
        }
    }

    /// The body types this declaration references (for the API-version assertions).
    fn bodies(&self) -> Vec<&Type> {
        match self {
            Decl::Publish(b) | Decl::Subscribe(b) => vec![b],
            Decl::Query { req, resp } => vec![req, resp],
        }
    }
}

/// Recognize a handle field by canonical syntactic form, returning the
/// declaration(s). `Vec`/`BTreeMap` of a handle carry the inner handle's
/// declaration. Returns `None` for non-handle (runtime-private) fields.
fn classify_handle(ty: &Type) -> Option<Vec<Decl>> {
    let path = as_type_path(ty)?;
    let seg = path.path.segments.last()?;
    let name = seg.ident.to_string();

    match name.as_str() {
        "Publisher" => Some(vec![Decl::Publish(generic_type(seg, 0)?)]),
        "Subscriber" | "Latest" => Some(vec![Decl::Subscribe(generic_type(seg, 0)?)]),
        "Querier" => Some(vec![Decl::Query {
            req: generic_type(seg, 0)?,
            resp: generic_type(seg, 1)?,
        }]),
        // Vec<Handle> — the element is the handle.
        "Vec" => classify_handle(&generic_type(seg, 0)?),
        // BTreeMap<K, Handle> / HashMap<K, Handle> — the value is the handle.
        "BTreeMap" | "HashMap" => classify_handle(&generic_type(seg, 1)?),
        _ => None,
    }
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
