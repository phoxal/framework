//! Static metadata derives from the participant struct.
//!
//! Reads the mandatory `#[phoxal(api = y2026_N)]` selector (plus optional
//! `id` / `config`) and the struct's typed handle fields, then emits:
//! - an `impl ParticipantSpec` carrying `KIND`, `PARTICIPANT_CLASS`, `ID`,
//!   `Api`/`API_VERSION`, `Config`, and `FIELD_CONTRACTS` (one
//!   `{family, topic, direction}` per handle, all sharing the one API version);
//! - compile assertions that every handle body satisfies
//!   `ContractBody<Api = Self::Api>` (a body from another API version is a
//!   compile error - D60);
//! - `Declares<Body>` marker impls so `SetupContext` builders reject undeclared
//!   contract families at compile time (D44).
//!
//! Handles are recognized by **canonical syntactic form** only (`Publisher<T>`,
//! `Subscriber<T>`, `Latest<T>`, `Querier<A, B>`, and `Vec`/`BTreeMap` of them);
//! every other field is ignored as participant-private state.

use heck::ToKebabCase;
use proc_macro2::TokenStream;
use quote::{ToTokens, quote};
use syn::{
    Data, DeriveInput, Fields, GenericArgument, Ident, LitStr, PathArguments, Token, Type,
    TypePath, parenthesized,
};

use crate::util::{phoxal, phoxal_api};

#[derive(Clone, Copy)]
pub enum AuthoringKind {
    Service,
    Driver,
    Tool,
    Simulator,
}

impl AuthoringKind {
    fn derive_path(self) -> &'static str {
        match self {
            AuthoringKind::Service => "#[derive(phoxal::Service)]",
            AuthoringKind::Driver => "#[derive(phoxal::Driver)]",
            AuthoringKind::Tool => "#[derive(phoxal::Tool)]",
            AuthoringKind::Simulator => "#[derive(phoxal::Simulator)]",
        }
    }

    fn artifact_kind(self) -> &'static str {
        match self {
            AuthoringKind::Service => "service",
            AuthoringKind::Driver => "driver",
            AuthoringKind::Tool => "tool",
            AuthoringKind::Simulator => "simulator",
        }
    }

    fn participant_class(self) -> &'static str {
        match self {
            AuthoringKind::Tool => "privileged",
            AuthoringKind::Service | AuthoringKind::Driver | AuthoringKind::Simulator => "checked",
        }
    }

    fn marker_impl(self, phoxal: &TokenStream, struct_name: &Ident) -> TokenStream {
        match self {
            AuthoringKind::Service => TokenStream::new(),
            AuthoringKind::Driver => {
                quote!(impl #phoxal::participant::IsDriver for #struct_name {})
            }
            AuthoringKind::Tool => quote!(impl #phoxal::participant::IsTool for #struct_name {}),
            AuthoringKind::Simulator => {
                quote!(impl #phoxal::participant::IsSimulator for #struct_name {})
            }
        }
    }
}

pub fn expand(input: TokenStream, kind: AuthoringKind) -> syn::Result<TokenStream> {
    let input: DeriveInput = syn::parse2(input)?;
    let struct_name = &input.ident;
    let derive_path = kind.derive_path();

    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            format!("{derive_path} does not support generic participant structs"),
        ));
    }

    let args = PhoxalArgs::parse(&input, derive_path)?;
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
                    format!("{derive_path} requires a struct with named fields"),
                ));
            }
        },
        _ => {
            return Err(syn::Error::new_spanned(
                struct_name,
                format!("{derive_path} can only be applied to structs"),
            ));
        }
    };

    // Collect a declaration for every recognized handle field, plus any
    // explicit IO the derive cannot see in fields (`contracts(...)`).
    let mut decls: Vec<Decl> = Vec::new();
    for field in fields {
        if let Some(found) = classify_handle(&field.ty) {
            decls.extend(found);
        }
    }
    decls.extend(args.contracts);

    let phoxal = phoxal();
    let phoxal_api = phoxal_api();
    let artifact_kind = kind.artifact_kind();
    let participant_class = kind.participant_class();

    // FIELD_CONTRACTS entries reference the body type's ContractBody consts so the
    // family/topic/api_version are single-sourced from the api tree (D61). A query
    // field contributes both legs.
    let mut seen_contracts = std::collections::BTreeSet::new();
    let mut contract_entries = Vec::new();
    for decl in &decls {
        for (body, dir) in decl.contract_bodies() {
            let key = format!("{}:{}", dir.key(), normalized_type_key(&body, &api));
            if seen_contracts.insert(key) {
                let dir = dir.tokens(&phoxal);
                contract_entries.push(quote! {
                    #phoxal::participant::ContractUse {
                        api_version: <<#body as #phoxal::bus::ContractBody>::Api as #phoxal::bus::ApiVersion>::ID,
                        schema_id: <#body as #phoxal::bus::ContractBody>::SCHEMA_ID,
                        family: <#body as #phoxal::bus::ContractBody>::FAMILY,
                        topic: <#body as #phoxal::bus::ContractBody>::TOPIC,
                        direction: #dir,
                    }
                });
            }
        }
    }

    // One ContractBody<Api = Self::Api> assertion per body (D60), so a body from
    // another API version is a compile error.
    let bodies: Vec<&Type> = decls.iter().flat_map(|d| d.bodies()).collect();
    let assertions = bodies.iter().enumerate().map(|(i, body)| {
        let assert_fn = syn::Ident::new(&format!("__assert_body_{i}"), struct_name.span());
        quote! {
            fn #assert_fn() {
                fn assert<R, B>()
                where
                    R: #phoxal::participant::ParticipantSpec,
                    B: #phoxal::bus::ContractBody<Api = <R as #phoxal::participant::ParticipantSpec>::Api>,
                {}
                assert::<#struct_name, #body>();
            }
        }
    });

    // Direction-specific Declares markers (D44): publishing a family declared only
    // as subscribe (or vice versa) is a compile error - and would otherwise be IO
    // `emit-apis` never reports. Deduplicate common API-module aliases
    // (`api::...` vs `phoxal_api::y2026_N::...`) so the same semantic body does
    // not emit conflicting impls.
    let mut seen = std::collections::BTreeSet::new();
    let mut declares = TokenStream::new();
    for decl in &decls {
        let (marker, key) = match decl {
            Decl::Publish(b) => (
                quote!(impl #phoxal::participant::DeclaresPublish<#b> for #struct_name {}),
                format!("pub:{}", normalized_type_key(b, &api)),
            ),
            Decl::Subscribe(b) => (
                quote!(impl #phoxal::participant::DeclaresSubscribe<#b> for #struct_name {}),
                format!("sub:{}", normalized_type_key(b, &api)),
            ),
            Decl::Query { req, resp } => (
                quote!(impl #phoxal::participant::DeclaresQuery<#req, #resp> for #struct_name {}),
                format!(
                    "qry:{}=>{}",
                    normalized_type_key(req, &api),
                    normalized_type_key(resp, &api)
                ),
            ),
        };
        if seen.insert(key) {
            declares.extend(marker);
        }
    }

    let kind_marker = kind.marker_impl(&phoxal, struct_name);

    Ok(quote! {
        impl #phoxal::participant::ParticipantSpec for #struct_name {
            const KIND: &'static str = #artifact_kind;
            const PARTICIPANT_CLASS: &'static str = #participant_class;
            const ID: &'static str = #id;
            type Api = #phoxal_api::#api::Api;
            const API_VERSION: &'static str =
                <#phoxal_api::#api::Api as #phoxal::bus::ApiVersion>::ID;
            type Config = #config_ty;
            const FIELD_CONTRACTS: &'static [#phoxal::participant::ContractUse] = &[
                #(#contract_entries),*
            ];
        }

        #declares

        #kind_marker

        const _: () = {
            #(#assertions)*
        };
    })
}

struct PhoxalArgs {
    id: Option<String>,
    api: Ident,
    config: Option<Type>,
    contracts: Vec<Decl>,
}

impl PhoxalArgs {
    fn parse(input: &DeriveInput, derive_path: &str) -> syn::Result<Self> {
        let mut id = None;
        let mut api = None;
        let mut config = None;
        let mut contracts = Vec::new();

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
                } else if meta.path.is_ident("contracts") {
                    parse_contracts(meta.input, &mut contracts)
                } else {
                    Err(meta.error(
                        "unknown #[phoxal(...)] key (expected id, api, config, or contracts)",
                    ))
                }
            })?;
        }

        let api = api.ok_or_else(|| {
            syn::Error::new_spanned(
                &input.ident,
                format!(
                    "{derive_path} requires #[phoxal(api = y2026_N)] (D59/D60): \
                     it selects the one API version this participant - and the whole graph - runs against"
                ),
            )
        })?;

        Ok(PhoxalArgs {
            id,
            api,
            config,
            contracts,
        })
    }
}

fn parse_contracts(input: syn::parse::ParseStream<'_>, decls: &mut Vec<Decl>) -> syn::Result<()> {
    let content;
    parenthesized!(content in input);
    while !content.is_empty() {
        let directive: Ident = content.parse()?;
        let body;
        parenthesized!(body in content);
        if directive == "publishes" {
            let body_ty: Type = body.parse()?;
            if !body.is_empty() {
                return Err(body.error("unexpected tokens after publish body type"));
            }
            decls.push(Decl::Publish(body_ty));
        } else if directive == "subscribes" {
            let body_ty: Type = body.parse()?;
            if !body.is_empty() {
                return Err(body.error("unexpected tokens after subscribe body type"));
            }
            decls.push(Decl::Subscribe(body_ty));
        } else if directive == "queries" {
            let req: Type = body.parse()?;
            if body.peek(Token![=>]) {
                let _arrow: Token![=>] = body.parse()?;
            } else if body.peek(Token![->]) {
                let _arrow: Token![->] = body.parse()?;
            } else {
                return Err(body.error("queries(...) expects `Req => Resp`"));
            }
            let resp: Type = body.parse()?;
            if !body.is_empty() {
                return Err(body.error("unexpected tokens after query response type"));
            }
            decls.push(Decl::Query { req, resp });
        } else {
            return Err(syn::Error::new(
                directive.span(),
                "unknown contracts(...) directive (expected publishes, subscribes, or queries)",
            ));
        }

        if content.is_empty() {
            break;
        }
        content.parse::<Token![,]>()?;
    }
    Ok(())
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
    fn key(self) -> &'static str {
        match self {
            Direction::Publish => "pub",
            Direction::Subscribe => "sub",
            Direction::QueryRequest => "qreq",
            Direction::QueryResponse => "qresp",
        }
    }

    fn tokens(self, phoxal: &TokenStream) -> TokenStream {
        let variant = match self {
            Direction::Publish => quote!(Publish),
            Direction::Subscribe => quote!(Subscribe),
            Direction::QueryRequest => quote!(QueryRequest),
            Direction::QueryResponse => quote!(QueryResponse),
        };
        quote!(#phoxal::participant::Direction::#variant)
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
/// declaration. Returns `None` for non-handle (participant-private) fields.
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
        // Vec<Handle> - the element is the handle.
        "Vec" => classify_handle(&generic_type(seg, 0)?),
        // BTreeMap<K, Handle> / HashMap<K, Handle> - the value is the handle.
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

fn normalized_type_key(ty: &Type, api: &Ident) -> String {
    let Some(path) = as_type_path(ty) else {
        return ty.to_token_stream().to_string();
    };
    let mut segments = path
        .path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>();
    let selected_api = api.to_string();
    if segments.len() >= 2 && segments[0] == "phoxal_api" && segments[1] == selected_api {
        segments.drain(0..2);
    } else if segments
        .first()
        .is_some_and(|segment| segment == "api" || segment == &selected_api)
    {
        segments.drain(0..1);
    }
    segments.join("::")
}
