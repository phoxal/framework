//! The NEW participant authoring model: `#[derive(phoxal::Api)]`,
//! `#[derive(phoxal::Config)]`, and the `#[phoxal::service]` /
//! `#[phoxal::driver]` / `#[phoxal::simulator]` / `#[phoxal::tool]` attribute
//! macros. These target `phoxal::participant::api`'s trait hierarchy
//! (`ParticipantApi` / `ParticipantConfig` / `Participant`), a DISTINCT set
//! of traits from the OLD `runtime_derive`/`runtime_impl` modules' targets
//! (`ParticipantSpec` / `ParticipantBehavior`) - see that module's docs for
//! why (`type Api` name collision). Old and new coexist unmodified during the
//! migration window; nothing in this file touches `runtime_derive.rs` /
//! `runtime_impl.rs`.
//!
//! `#[phoxal::behavior]` itself stays a single entry point
//! (`crate::runtime_impl::expand`) that dispatches to this model's codegen
//! (`crate::behavior_v2`) based on the `#[setup]` method's return shape - see
//! `crate::setup_shape`.

use heck::{ToKebabCase, ToShoutySnakeCase};
use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::Parser;
use syn::{
    Data, DeriveInput, Fields, FieldsNamed, GenericArgument, Ident, LitStr, PathArguments, Type,
    TypePath,
};

use crate::util::phoxal;

// ---------------------------------------------------------------------------
// shared: canonical-syntactic-form field classification (Publish/Subscribe/Serve)
// ---------------------------------------------------------------------------

/// A handle field's declared role(s), mirroring `runtime_derive::Decl` but
/// extended with `Serve` (the new `Server<Req, Resp>` field kind, which has
/// no old-model equivalent). Kept as an independent copy rather than a shared
/// type so old-model codegen (`runtime_derive.rs`) never needs to change
/// shape to accommodate the new `Serve` variant.
#[allow(clippy::large_enum_variant)] // transient macro-internal AST holder (mirrors runtime_derive::Decl)
enum ApiDecl {
    Publish(Type),
    Subscribe(Type),
    Serve { req: Type, resp: Type },
}

/// Recognize an `Api` struct field by canonical syntactic form (mirrors
/// `runtime_derive::classify_handle`, plus `Server<Req, Resp>` => `Serve`).
/// `Vec`/`BTreeMap`/`HashMap` of a handle carry the inner handle's
/// declaration. Returns `None` for an unrecognized (ignored) field.
fn classify_api_field(ty: &Type) -> Option<Vec<ApiDecl>> {
    let path = as_type_path(ty)?;
    let seg = path.path.segments.last()?;
    let name = seg.ident.to_string();

    match name.as_str() {
        "Publisher" => Some(vec![ApiDecl::Publish(generic_type(seg, 0)?)]),
        "Subscriber" | "Latest" => Some(vec![ApiDecl::Subscribe(generic_type(seg, 0)?)]),
        "Server" => Some(vec![ApiDecl::Serve {
            req: generic_type(seg, 0)?,
            resp: generic_type(seg, 1)?,
        }]),
        "Vec" => classify_api_field(&generic_type(seg, 0)?),
        "BTreeMap" | "HashMap" => classify_api_field(&generic_type(seg, 1)?),
        _ => None,
    }
}

fn as_type_path(ty: &Type) -> Option<&TypePath> {
    match ty {
        Type::Path(p) => Some(p),
        _ => None,
    }
}

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

fn named_fields<'a>(input: &'a DeriveInput, derive_path: &str) -> syn::Result<&'a FieldsNamed> {
    match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(named) => Ok(named),
            _ => Err(syn::Error::new_spanned(
                &input.ident,
                format!("{derive_path} requires a struct with named fields"),
            )),
        },
        _ => Err(syn::Error::new_spanned(
            &input.ident,
            format!("{derive_path} can only be applied to structs"),
        )),
    }
}

fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The cross-platform `#[link_section]` pair a metadata static is placed
/// under (cargo-auditable's embedding pattern, RECONCILIATION's confirmed
/// prior art at `xtask/src/release/package.rs:345-355`): `__DATA,__phoxal_meta`
/// on Mach-O (macOS; segment,section syntax, section name <=16 bytes), and
/// `.phoxal_api_meta` everywhere else (ELF and other platforms this
/// framework targets - Linux robots, primarily). `#[used]` keeps the linker
/// from discarding a static nothing else in the binary references, which is
/// the whole point: the section's *bytes*, not the symbol, are the payload a
/// later xtask reads straight out of the built artifact file (never by
/// executing it).
fn link_section_attrs() -> TokenStream {
    quote! {
        #[used]
        #[cfg_attr(target_os = "macos", unsafe(link_section = "__DATA,__phoxal_meta"))]
        #[cfg_attr(not(target_os = "macos"), unsafe(link_section = ".phoxal_api_meta"))]
    }
}

// ---------------------------------------------------------------------------
// #[derive(phoxal::Api)]
// ---------------------------------------------------------------------------

/// Derive [`ParticipantApi`](phoxal::participant::api::ParticipantApi) from an
/// `Api` handle struct: one `ApiContractUse` per recognized field (role +
/// `<Body as ContractBody>::TOPIC`, resolved by `rustc` from the emitted
/// `quote!`d expression - the proc-macro itself never evaluates it as a
/// string, matching how the old model's `FIELD_CONTRACTS` works today), plus
/// a `#[used]` linker-section static recording each field's role and its
/// version-qualified body type **as written** (`y2026_1::drive::Target`) -
/// the doc's own definition of the compatibility surface, and the only piece
/// that is a pure macro-time string literal (the resolved wire `TOPIC` is the
/// same information, mechanically derived by `phoxal_api_tree!`; resolving it
/// from the artifact is xtask/host-side work for a later slice, per the
/// "Metadata materialization is two-part" correction).
pub fn expand_api(input: TokenStream) -> syn::Result<TokenStream> {
    let input: DeriveInput = syn::parse2(input)?;
    let struct_name = &input.ident;

    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "#[derive(phoxal::Api)] does not support generic structs",
        ));
    }

    let fields = named_fields(&input, "#[derive(phoxal::Api)]")?;

    // Deduplicate the emitted contracts by (contract type as written, role),
    // in first-seen field order, so `CONTRACTS` is the participant's true
    // compatibility surface (two fields naming the same contract in the same
    // role - e.g. a `Publisher<X>` and a `Vec<Publisher<X>>` - collapse to one
    // entry) rather than a per-field multiset. The linker-section JSON is
    // deduplicated on the same key (its `field` is the first field that used
    // that contract in that role; the field name is incidental provenance, not
    // part of the identity). This matches the old model's
    // `runtime_derive.rs` FIELD_CONTRACTS/`Declares*` dedup on a normalized
    // type key.
    let mut seen = std::collections::BTreeSet::<String>::new();
    let mut contract_entries = Vec::new();
    let mut manifest_entries: Vec<String> = Vec::new();
    let mut push = |field: &str, role_snake: &str, role_pascal: &str, body: &Type| {
        let body_str = normalized_body_key(body);
        if !seen.insert(format!("{role_snake}:{body_str}")) {
            return;
        }
        manifest_entries.push(manifest_entry(field, role_snake, &body_str));
        contract_entries.push(contract_use_entry(body, role_pascal));
    };
    for field in &fields.named {
        let Some(field_name) = &field.ident else {
            continue;
        };
        let Some(decls) = classify_api_field(&field.ty) else {
            continue;
        };
        let field_str = field_name.to_string();
        for decl in decls {
            match decl {
                ApiDecl::Publish(body) => push(&field_str, "publish", "Publish", &body),
                ApiDecl::Subscribe(body) => push(&field_str, "subscribe", "Subscribe", &body),
                ApiDecl::Serve { req, resp } => {
                    push(&field_str, "serve", "Serve", &req);
                    push(&field_str, "serve", "Serve", &resp);
                }
            }
        }
    }

    let phoxal = phoxal();
    let manifest_json = format!(
        "{{\"participant_api\":\"{}\",\"contracts\":[{}]}}",
        json_escape(&struct_name.to_string()),
        manifest_entries.join(",")
    );
    let manifest_bytes = syn::LitByteStr::new(manifest_json.as_bytes(), struct_name.span());
    let manifest_len = manifest_json.len();
    let static_ident = Ident::new(
        &format!(
            "__PHOXAL_API_META_{}",
            struct_name.to_string().to_shouty_snake_case()
        ),
        struct_name.span(),
    );
    let link_section = link_section_attrs();

    Ok(quote! {
        impl #phoxal::participant::ParticipantApi for #struct_name {
            const CONTRACTS: &'static [#phoxal::participant::ApiContractUse] = &[
                #(#contract_entries),*
            ];
        }

        #link_section
        #[doc(hidden)]
        static #static_ident: [u8; #manifest_len] = *#manifest_bytes;
    })
}

fn contract_use_entry(body: &Type, role: &str) -> TokenStream {
    let phoxal = phoxal();
    let role_ident = Ident::new(role, proc_macro2::Span::call_site());
    quote! {
        #phoxal::participant::ApiContractUse {
            topic: <#body as #phoxal::bus::ContractBody>::TOPIC,
            role: #phoxal::participant::ContractRole::#role_ident,
        }
    }
}

/// The contract type key used both for dedup and for the linker-section JSON:
/// the body type exactly as written, whitespace-stripped (e.g.
/// `y2026_1::drive::Target`). The macro cannot resolve the wire `TOPIC` string
/// at expansion time, so the written type is the compatibility identity here
/// (`CONTRACTS`'s topic const does resolve it, via `rustc`).
fn normalized_body_key(body: &Type) -> String {
    quote!(#body).to_string().replace(' ', "")
}

fn manifest_entry(field: &str, role: &str, body_str: &str) -> String {
    format!(
        "{{\"field\":\"{}\",\"role\":\"{}\",\"contract\":\"{}\"}}",
        json_escape(field),
        role,
        json_escape(body_str)
    )
}

// ---------------------------------------------------------------------------
// #[derive(phoxal::Config)]
// ---------------------------------------------------------------------------

/// Derive [`ParticipantConfig`](phoxal::participant::api::ParticipantConfig)
/// from a `Config` struct. `SCHEMA_JSON` is a placeholder (`"{}"`) in this
/// slice - see `phoxal::participant::api`'s module docs for why (the real
/// schema needs a host-side `build.rs` walk, not macro-time syntax).
pub fn expand_config(input: TokenStream) -> syn::Result<TokenStream> {
    let input: DeriveInput = syn::parse2(input)?;
    let struct_name = &input.ident;

    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "#[derive(phoxal::Config)] does not support generic structs",
        ));
    }
    // Identity-only derive: any struct/enum shape is a valid config (the old
    // model's `Config` type is likewise unconstrained in shape - it is
    // whatever `serde`/`schemars` can walk). No field scan is needed here.

    let phoxal = phoxal();
    Ok(quote! {
        impl #phoxal::participant::ParticipantConfig for #struct_name {
            // TODO(phoxal-api-refactor, config-schema slice): materialize the
            // real `schemars` JSON schema via a host-side `build.rs` step and
            // `include_str!` it here; a proc-macro cannot reproduce
            // schemars's recursive trait walk across crate boundaries
            // (RECONCILIATION correction #12).
            const SCHEMA_JSON: &'static str = "{}";
        }
    })
}

// ---------------------------------------------------------------------------
// #[phoxal::service] / #[phoxal::driver] / #[phoxal::simulator] / #[phoxal::tool]
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub enum ParticipantKind {
    Service,
    Driver,
    Simulator,
    Tool,
}

impl ParticipantKind {
    fn attr_name(self) -> &'static str {
        match self {
            ParticipantKind::Service => "#[phoxal::service]",
            ParticipantKind::Driver => "#[phoxal::driver]",
            ParticipantKind::Simulator => "#[phoxal::simulator]",
            ParticipantKind::Tool => "#[phoxal::tool]",
        }
    }

    fn artifact_kind(self) -> &'static str {
        match self {
            ParticipantKind::Service => "service",
            ParticipantKind::Driver => "driver",
            ParticipantKind::Simulator => "simulator",
            ParticipantKind::Tool => "tool",
        }
    }

    fn participant_class(self) -> &'static str {
        match self {
            ParticipantKind::Tool => "privileged",
            ParticipantKind::Service | ParticipantKind::Driver | ParticipantKind::Simulator => {
                "checked"
            }
        }
    }

    /// Default `Api` type when `api = …` is not given: tools stay raw-bus
    /// only (decided 2026-07-09 - no typed tool `Api` until a real need
    /// appears), every other kind defaults to the local `Api` struct.
    fn default_api(self) -> Type {
        match self {
            ParticipantKind::Tool => syn::parse_quote!(()),
            _ => syn::parse_quote!(Api),
        }
    }

    fn marker_impl(self, phoxal: &TokenStream, struct_name: &Ident) -> TokenStream {
        match self {
            ParticipantKind::Service => {
                quote!(impl #phoxal::participant::TypedGraphSurface for #struct_name {})
            }
            ParticipantKind::Driver => quote! {
                impl #phoxal::participant::IsDriver for #struct_name {}
                impl #phoxal::participant::TypedGraphSurface for #struct_name {}
            },
            ParticipantKind::Simulator => quote! {
                impl #phoxal::participant::IsSimulator for #struct_name {}
                impl #phoxal::participant::TypedGraphSurface for #struct_name {}
            },
            ParticipantKind::Tool => quote!(impl #phoxal::participant::IsTool for #struct_name {}),
        }
    }
}

#[derive(Default)]
struct ParticipantArgs {
    id: Option<String>,
    config: Option<Type>,
    api: Option<Type>,
}

impl ParticipantArgs {
    fn parse(attr: TokenStream, attr_name: &str) -> syn::Result<Self> {
        let mut args = ParticipantArgs::default();
        let parser = syn::meta::parser(|meta: syn::meta::ParseNestedMeta| {
            if meta.path.is_ident("id") {
                let value: LitStr = meta.value()?.parse()?;
                args.id = Some(value.value());
                Ok(())
            } else if meta.path.is_ident("config") {
                let value: Type = meta.value()?.parse()?;
                args.config = Some(value);
                Ok(())
            } else if meta.path.is_ident("api") {
                let value: Type = meta.value()?.parse()?;
                args.api = Some(value);
                Ok(())
            } else {
                Err(meta.error(format!(
                    "unknown {attr_name}(...) key (expected id, config, or api)"
                )))
            }
        });
        parser.parse2(attr)?;
        Ok(args)
    }
}

/// Link a participant state struct to its `Config`/`Api` types and record its
/// identity (the NEW model's counterpart to `#[derive(phoxal::Service)]` /
/// `Driver` / `Tool` / `Simulator`, which scanned the participant struct
/// itself for handle fields - this attribute never does that: the struct is
/// private runtime state only, per the target model).
pub fn expand_participant(
    attr: TokenStream,
    item: TokenStream,
    kind: ParticipantKind,
) -> syn::Result<TokenStream> {
    let item_struct: syn::ItemStruct = syn::parse2(item)?;
    let struct_name = &item_struct.ident;
    let attr_name = kind.attr_name();

    if !item_struct.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &item_struct.generics,
            format!("{attr_name} does not support generic participant structs"),
        ));
    }

    let args = ParticipantArgs::parse(attr, attr_name)?;
    let id = args
        .id
        .unwrap_or_else(|| struct_name.to_string().to_kebab_case());
    let config_ty: Type = args.config.unwrap_or_else(|| syn::parse_quote!(Config));
    let api_ty: Type = args.api.unwrap_or_else(|| kind.default_api());

    let phoxal = phoxal();
    let artifact_kind = kind.artifact_kind();
    let participant_class = kind.participant_class();
    let marker = kind.marker_impl(&phoxal, struct_name);

    Ok(quote! {
        #item_struct

        impl #phoxal::participant::Participant for #struct_name {
            const KIND: &'static str = #artifact_kind;
            const PARTICIPANT_CLASS: &'static str = #participant_class;
            const ID: &'static str = #id;
            type Config = #config_ty;
            type Api = #api_ty;
        }

        #marker
    })
}
