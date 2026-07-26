//! The participant authoring model: `#[derive(phoxal::Api)]`,
//! `#[derive(phoxal::Config)]`, and the `#[phoxal::service]` /
//! `#[phoxal::driver]` / `#[phoxal::simulator]` / `#[phoxal::tool]` attribute
//! macros. These target `phoxal::participant::api`'s trait hierarchy
//! (`ParticipantApi` / `ParticipantConfig` / `Participant`).
//!
//! `#[phoxal::behavior]` (`crate::behavior::expand`) is the paired impl-level
//! macro that reads the lifecycle/server helper attributes and emits the
//! `ParticipantLifecycle` impl the runner drives.

use heck::{ToKebabCase, ToShoutySnakeCase};
use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::Parser;
use syn::spanned::Spanned;
use syn::{
    Data, DeriveInput, Fields, FieldsNamed, GenericArgument, Ident, LitStr, PathArguments, Type,
    TypePath,
};

use crate::util::phoxal;

// ---------------------------------------------------------------------------
// shared: canonical-syntactic-form field classification (Publish/Subscribe/Serve)
// ---------------------------------------------------------------------------

/// A handle field's declared role(s): one of the four role-gated publishers,
/// `Subscriber<T>`/`Latest<T>`, `Server<Req, Resp>`, or `Querier<Req, Resp>`.
#[allow(clippy::large_enum_variant)] // transient macro-internal AST holder
enum ApiDecl {
    Publish(Type),
    Subscribe(Type),
    Serve { req: Type, resp: Type },
    Ask { req: Type, resp: Type },
}

/// Recognize an `Api` struct field by canonical syntactic form.
/// `Vec`/`BTreeMap`/`HashMap` of a handle carry the inner handle's
/// declaration. Returns `None` for an unrecognized (ignored) field.
fn classify_api_field(ty: &Type) -> Option<Vec<ApiDecl>> {
    let path = as_type_path(ty)?;
    let seg = path.path.segments.last()?;
    let name = seg.ident.to_string();

    match name.as_str() {
        // The four publisher handles differ in the robot time they may express
        // (#952 section D); all four declare the same publish contract role.
        "StatePublisher" | "MeasurementPublisher" | "CommandPublisher" | "DiagnosticPublisher" => {
            Some(vec![ApiDecl::Publish(generic_type(seg, 0)?)])
        }
        "Subscriber" | "Latest" => Some(vec![ApiDecl::Subscribe(generic_type(seg, 0)?)]),
        "Server" => Some(vec![ApiDecl::Serve {
            req: generic_type(seg, 0)?,
            resp: generic_type(seg, 1)?,
        }]),
        "Querier" => Some(vec![ApiDecl::Ask {
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
/// `Api` handle struct.
///
/// Emits `CONTRACTS`: one `ApiContractUse` per recognized field, carrying the
/// role and the resolved `<Body as ContractBody>::TOPIC` wire key. `rustc`
/// resolves that expression - the proc-macro never evaluates it as a string.
///
/// It also emits one `impl Declares*<..> for #struct_name {}` per distinct
/// declared family (D44 - `DeclaresPublish`/`DeclaresSubscribe` per body,
/// `DeclaresAsk`/`DeclaresServe` per `(Req, Resp)` pair). That is what lets the
/// `SetupContext` builders (`SetupContextApiExt`) reject, at compile time, a
/// handle for a contract this `Api` struct never declared as a field.
///
/// It no longer emits a JSON contract inventory or an API type name. Those fed
/// the API-coherence pass, which is gone (organization#957): the exact
/// framework train is the compatibility boundary, so a per-binary record of
/// which contracts a participant uses has no reader. `#[phoxal(external)]` went
/// with it - it existed only to excuse a coherence mismatch.
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
    // deduplicated on the same key. Dedup keys off the type as written (a
    // macro-time string) purely to collapse syntactic repeats of the same
    // field type. `CONTRACTS` is what survives: it feeds the `Declares*`
    // compile-time handle gating and the participants' own tests. The embedded
    // JSON contract inventory this used to build alongside it went with the
    // coherence system (organization#957).
    let mut seen = std::collections::BTreeSet::<String>::new();
    let mut contract_entries = Vec::new();
    let mut push = |_field_span: proc_macro2::Span,
                    role_snake: &str,
                    role_pascal: &str,
                    body: &Type|
     -> syn::Result<()> {
        let body_str = normalized_body_key(body);
        if !seen.insert(format!("{role_snake}:{body_str}")) {
            return Ok(());
        }
        contract_entries.push(contract_use_entry(body, role_pascal));
        Ok(())
    };

    // `Declares*<B>` marker impls (D44): one per distinct (family, body-or-pair)
    // this `Api` struct declares, deduplicated separately from `push` above
    // because a query/serve declaration is keyed on the (Req, Resp) PAIR here
    // (matching the two-type-parameter `DeclaresAsk`/`DeclaresServe` traits),
    // not on the two split CONTRACTS entries `push` records for `Req` and
    // `Resp` individually.
    let mut seen_declares = std::collections::BTreeSet::<String>::new();
    let mut declare_impls = Vec::new();
    let mut subscribe_field_names = Vec::new();
    let phoxal_for_declares = phoxal();
    let mut declare = |key: String, tokens: TokenStream| {
        if seen_declares.insert(key) {
            declare_impls.push(tokens);
        }
    };
    for field in &fields.named {
        if field.ident.is_none() {
            continue;
        }
        let Some(decls) = classify_api_field(&field.ty) else {
            return Err(syn::Error::new_spanned(
                &field.ty,
                "unsupported #[derive(phoxal::Api)] field type; expected StatePublisher, MeasurementPublisher, CommandPublisher, DiagnosticPublisher, Subscriber, Latest, Querier, Server, or a supported collection wrapper",
            ));
        };
        let field_span = field.span();
        if decls
            .iter()
            .any(|decl| matches!(decl, ApiDecl::Subscribe(_)))
        {
            subscribe_field_names.push(
                field
                    .ident
                    .as_ref()
                    .expect("derive Api requires named fields"),
            );
        }
        for decl in decls {
            match decl {
                ApiDecl::Publish(body) => {
                    push(field_span, "publish", "Publish", &body)?;
                    let key = format!("declpub:{}", normalized_body_key(&body));
                    declare(
                        key,
                        quote! {
                            impl #phoxal_for_declares::participant::DeclaresPublish<#body> for #struct_name {}
                        },
                    );
                }
                ApiDecl::Subscribe(body) => {
                    push(field_span, "subscribe", "Subscribe", &body)?;
                    let key = format!("declsub:{}", normalized_body_key(&body));
                    declare(
                        key,
                        quote! {
                            impl #phoxal_for_declares::participant::DeclaresSubscribe<#body> for #struct_name {}
                        },
                    );
                }
                ApiDecl::Serve { req, resp } => {
                    push(field_span, "serve", "Serve", &req)?;
                    push(field_span, "serve", "Serve", &resp)?;
                    let key = format!(
                        "declserve:{}=>{}",
                        normalized_body_key(&req),
                        normalized_body_key(&resp)
                    );
                    declare(
                        key,
                        quote! {
                            impl #phoxal_for_declares::participant::DeclaresServe<#req, #resp> for #struct_name {}
                        },
                    );
                }
                ApiDecl::Ask { req, resp } => {
                    push(field_span, "ask", "Ask", &req)?;
                    push(field_span, "ask", "Ask", &resp)?;
                    let key = format!(
                        "declask:{}=>{}",
                        normalized_body_key(&req),
                        normalized_body_key(&resp)
                    );
                    declare(
                        key,
                        quote! {
                            impl #phoxal_for_declares::participant::DeclaresAsk<#req, #resp> for #struct_name {}
                        },
                    );
                }
            }
        }
    }

    let phoxal = phoxal();

    // `ParticipantApi: Clone` (F-runtime slice - see that trait's docs): every
    // real handle field type (`Publisher`/`Latest`/`Subscriber`/`Querier`/
    // `Server`) is itself `Clone`, so a plain field-wise clone always
    // typechecks for a struct authored per the target model ("the Api struct
    // should not be scanned for anything but handle fields"). Emitted here
    // (not left to the user to `#[derive(Clone)]` themselves) because a
    // derive macro cannot retroactively add `#[derive(Clone)]` to the item it
    // is attached to - only append new tokens - so a hand-written `impl
    // Clone` covering every named field is the only way to satisfy the bound
    // unconditionally.
    let clone_field_names: Vec<&Ident> = fields
        .named
        .iter()
        .filter_map(|field| field.ident.as_ref())
        .collect();

    Ok(quote! {
        impl #phoxal::participant::ParticipantApi for #struct_name {
            const CONTRACTS: &'static [#phoxal::participant::ApiContractUse] = &[
                #(#contract_entries),*
            ];

            fn __retain_timeline(&self, timeline: #phoxal::bus::TimelineId) {
                let _ = timeline;
                #(
                    #phoxal::participant::api::TimelineScopedApiField::__retain_timeline(
                        &self.#subscribe_field_names,
                        timeline,
                    );
                )*
            }
        }

        #(#declare_impls)*

        impl ::core::clone::Clone for #struct_name {
            fn clone(&self) -> Self {
                Self {
                    #(#clone_field_names: ::core::clone::Clone::clone(&self.#clone_field_names),)*
                }
            }
        }
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

/// A `syn::LitStr` token for a JSON literal fragment used as a
/// `__concatcp!`/`concat!`-style macro argument. Used by the config-schema
/// derive to splice literal JSON around resolved schema consts.
fn json_lit(s: &str) -> TokenStream {
    let lit = syn::LitStr::new(s, proc_macro2::Span::call_site());
    quote!(#lit)
}

/// The contract type key used ONLY for macro-time dedup (collapsing two
/// fields that name the same contract in the same role - e.g. a
/// `Publisher<X>` and a `Vec<Publisher<X>>` - to one `CONTRACTS`
/// entry): the body type exactly as written, whitespace-stripped (e.g.
/// `api::drive::Target`). This is a syntactic key only; it is never the
/// *value* recorded for a contract's identity (that is the resolved `NAME`,
/// spliced in as tokens by `manifest_entry_tokens_for` - see
/// `phoxal::participant::api::__meta`'s docs for why the macro cannot resolve
/// it directly).
fn normalized_body_key(body: &Type) -> String {
    quote!(#body).to_string().replace(' ', "")
}

// ---------------------------------------------------------------------------
// #[derive(phoxal::Config)]
// ---------------------------------------------------------------------------

/// Derive [`ParticipantConfig`](phoxal::participant::api::ParticipantConfig)
/// from a named struct using Serde's own deserialize attribute model.
pub fn expand_config(input: TokenStream) -> syn::Result<TokenStream> {
    let input: DeriveInput = syn::parse2(input)?;
    let struct_name = &input.ident;

    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "#[derive(phoxal::Config)] does not support generic structs",
        ));
    }
    validate_config_serde_attributes(&input)?;

    let cx = serde_derive_internals::Ctxt::new();
    let container = serde_derive_internals::ast::Container::from_ast(
        &cx,
        &input,
        serde_derive_internals::Derive::Deserialize,
    );
    cx.check()?;
    let container = container.ok_or_else(|| {
        syn::Error::new_spanned(&input, "unable to parse Config with Serde's derive model")
    })?;

    let serde_derive_internals::ast::Data::Struct(
        serde_derive_internals::ast::Style::Struct,
        fields,
    ) = &container.data
    else {
        return Err(syn::Error::new_spanned(
            &input,
            "#[derive(phoxal::Config)] supports only structs with named fields",
        ));
    };

    let phoxal = phoxal();
    let title = container.attrs.name().deserialize_name();
    let mut schema_args = vec![json_lit(&format!(
        "{{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\"title\":{},\"type\":\"object\",\"properties\":{{",
        serde_json_string(title)
    ))];
    let mut required = Vec::new();
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            schema_args.push(json_lit(","));
        }
        let field_name = field.attrs.name().deserialize_name();
        schema_args.push(json_lit(&format!("{}:", serde_json_string(field_name))));
        let ty = field.ty;
        schema_args.push(quote!(<#ty as #phoxal::participant::ParticipantConfig>::SCHEMA_JSON));

        if field.attrs.default().is_none() && !is_option_type(ty) {
            required.push(field_name.to_string());
        }
    }
    schema_args.push(json_lit("}"));
    if !required.is_empty() {
        let required_json = required
            .iter()
            .map(|name| serde_json_string(name))
            .collect::<Vec<_>>()
            .join(",");
        schema_args.push(json_lit(&format!(",\"required\":[{required_json}]")));
    }
    if container.attrs.deny_unknown_fields() {
        schema_args.push(json_lit(",\"additionalProperties\":false"));
    }
    schema_args.push(json_lit("}"));

    Ok(quote! {
        impl #phoxal::participant::ParticipantConfig for #struct_name {
            const __SCHEMA: #phoxal::participant::api::__meta::ConstSchema =
                #phoxal::participant::api::__meta::ConstSchema::new()
                    #(.push_str(#schema_args))*;
        }
    })
}

fn is_option_type(ty: &Type) -> bool {
    as_type_path(ty)
        .and_then(|path| path.path.segments.last())
        .is_some_and(|segment| segment.ident == "Option")
}

fn serde_json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch <= '\u{1f}' => {
                use std::fmt::Write;
                write!(out, "\\u{:04x}", ch as u32).expect("writing to String cannot fail");
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn validate_config_serde_attributes(input: &DeriveInput) -> syn::Result<()> {
    validate_serde_attrs(&input.attrs, SerdeAttrLocation::Container)?;
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            input,
            "#[derive(phoxal::Config)] supports only structs with named fields",
        ));
    };
    for field in &data.fields {
        validate_serde_attrs(&field.attrs, SerdeAttrLocation::Field)?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum SerdeAttrLocation {
    Container,
    Field,
}

fn validate_serde_attrs(attrs: &[syn::Attribute], location: SerdeAttrLocation) -> syn::Result<()> {
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("serde")) {
        attr.parse_nested_meta(|meta| {
            let path = &meta.path;
            let name = meta
                .path
                .get_ident()
                .map(ToString::to_string)
                .unwrap_or_else(|| quote!(#path).to_string());
            let supported = match location {
                SerdeAttrLocation::Container => {
                    matches!(name.as_str(), "rename" | "rename_all" | "default" | "deny_unknown_fields")
                }
                SerdeAttrLocation::Field => matches!(name.as_str(), "rename" | "default"),
            };
            if !supported {
                return Err(meta.error(format!(
                    "unsupported serde attribute `{name}` for #[derive(phoxal::Config)]; supported container attributes: rename, rename_all, default, deny_unknown_fields; supported field attributes: rename, default"
                )));
            }

            match name.as_str() {
                "rename" | "rename_all" => {
                    let _: LitStr = meta.value()?.parse()?;
                }
                "default" if meta.input.peek(syn::Token![=]) => {
                    let _: LitStr = meta.value()?.parse()?;
                }
                "default" | "deny_unknown_fields" if meta.input.is_empty() => {}
                _ => {
                    return Err(meta.error(format!(
                        "unsupported form of serde attribute `{name}` for #[derive(phoxal::Config)]"
                    )));
                }
            }
            Ok(())
        })?;
    }
    Ok(())
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

    fn launch_policy(self, phoxal: &TokenStream) -> TokenStream {
        match self {
            ParticipantKind::Tool => {
                quote!(#phoxal::participant::launch::ToolParticipantLaunch)
            }
            ParticipantKind::Simulator => {
                quote!(#phoxal::participant::launch::SimulatorParticipantLaunch)
            }
            ParticipantKind::Service | ParticipantKind::Driver => {
                quote!(#phoxal::participant::launch::ClockedParticipantLaunch)
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

    /// Default `Config` type when `config = …` is not given.
    ///
    /// Tools default to `()` (no declared config): a configless tool must
    /// start cleanly with `PHOXAL_CONFIG` ABSENT (the runner deserializes an
    /// absent config as `Value::Null`, which `()`'s `Deserialize` accepts but
    /// a zero-field struct's derived `Deserialize` rejects - it expects a
    /// map). Every other kind keeps the historical default of a local
    /// `Config` struct, since services/drivers routinely declare one with
    /// real fields and still require `PHOXAL_CONFIG` to be present.
    fn default_config(self) -> Type {
        match self {
            ParticipantKind::Tool => syn::parse_quote!(()),
            _ => syn::parse_quote!(Config),
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
/// identity. The participant struct itself is never scanned for handle
/// fields - it is private runtime state only; the bus-facing contract surface
/// lives entirely on the companion `Api` struct (`#[derive(phoxal::Api)]`).
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
    let config_ty: Type = args.config.unwrap_or_else(|| kind.default_config());
    let api_ty: Type = args.api.unwrap_or_else(|| kind.default_api());

    let phoxal = phoxal();
    let artifact_kind = kind.artifact_kind();
    let participant_class = kind.participant_class();
    let launch_policy = kind.launch_policy(&phoxal);
    let marker = kind.marker_impl(&phoxal, struct_name);
    let metadata_const_ident = Ident::new(
        &format!(
            "__PHOXAL_PARTICIPANT_META_JSON_{}",
            struct_name.to_string().to_shouty_snake_case()
        ),
        struct_name.span(),
    );
    let metadata_len_ident = Ident::new(
        &format!(
            "__PHOXAL_PARTICIPANT_META_LEN_{}",
            struct_name.to_string().to_shouty_snake_case()
        ),
        struct_name.span(),
    );
    let metadata_static_ident = Ident::new(
        &format!(
            "__PHOXAL_PARTICIPANT_META_{}",
            struct_name.to_string().to_shouty_snake_case()
        ),
        struct_name.span(),
    );
    let link_section = link_section_attrs();

    Ok(quote! {
        #item_struct

        impl #phoxal::participant::Participant for #struct_name {
            const KIND: &'static str = #artifact_kind;
            const PARTICIPANT_CLASS: &'static str = #participant_class;
            const ID: &'static str = #id;
            type LaunchPolicy = #launch_policy;
            type Config = #config_ty;
            type Api = #api_ty;
        }

        #marker

        // Identity plus config, and nothing else. The contract inventory and
        // API-type provenance that used to live here went with the coherence
        // system (organization#957); the train is the compatibility boundary,
        // so a per-binary contract list has no reader. `id` is what lets a
        // consumer answer "which participant is this binary" without trusting
        // a filename.
        #[doc(hidden)]
        const #metadata_const_ident: &'static str =
            #phoxal::participant::api::__meta::__concatcp!(
                "{\"id\":\"",
                #id,
                "\",\"config_schema\":",
                <#config_ty as #phoxal::participant::ParticipantConfig>::SCHEMA_JSON,
                "}"
            );
        #[doc(hidden)]
        const #metadata_len_ident: usize = #metadata_const_ident.len();
        #link_section
        #[doc(hidden)]
        static #metadata_static_ident: [u8; #metadata_len_ident] =
            #phoxal::participant::api::__meta::__bytes_of(#metadata_const_ident);
    })
}
