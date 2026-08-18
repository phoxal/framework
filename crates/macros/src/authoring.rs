//! The participant authoring model: `#[derive(phoxal::Config)]`, the
//! `#[phoxal::service]` / `#[phoxal::driver]` / `#[phoxal::brain]` role
//! attributes, and the method-level
//! `#[phoxal::step(hz = N)]`. Role attributes emit the static
//! [`ParticipantSpec`] contract; authors implement `Participant` directly for
//! lifecycle behavior.
//!
//! Everything emitted here reaches the framework through the `::phoxal` path,
//! which the engine crate makes resolve to itself with
//! `extern crate self as phoxal;`.

use heck::ToShoutySnakeCase;
use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::parse::Parser;
use syn::{Data, DeriveInput, Expr, ExprLit, Fields, Ident, ImplItemFn, Lit, LitStr, Type, UnOp};

// ---------------------------------------------------------------------------
// #[derive(phoxal::Config)]
// ---------------------------------------------------------------------------

/// Derive `phoxal::ParticipantConfig`
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

    let phoxal = quote!(::phoxal);
    // A JSON literal fragment spliced between the resolved per-field schema
    // consts, as a `concat!`-style macro argument.
    let json_lit = |fragment: &str| {
        let lit = LitStr::new(fragment, Span::call_site());
        quote!(#lit)
    };
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
        schema_args.push(quote!(<#ty as #phoxal::__private::ParticipantConfig>::SCHEMA_JSON));

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
        impl #phoxal::__private::ParticipantConfig for #struct_name {
            const __SCHEMA: #phoxal::__private::meta::ConstSchema =
                #phoxal::__private::meta::ConstSchema::new()
                    #(.push_str(#schema_args))*;
        }
    })
}

fn is_option_type(ty: &Type) -> bool {
    matches!(ty, Type::Path(path)
        if path
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "Option"))
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
            ch if ch <= '\u{1f}' => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn validate_config_serde_attributes(input: &DeriveInput) -> syn::Result<()> {
    SerdeAttrLocation::Container.validate(&input.attrs)?;
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            input,
            "#[derive(phoxal::Config)] supports only structs with named fields",
        ));
    };
    for field in &data.fields {
        SerdeAttrLocation::Field.validate(&field.attrs)?;
    }
    Ok(())
}

/// Where a `#[serde(...)]` attribute sits, which is what decides the keys the
/// schema derive can honor: a container reads the whole-type knobs, a field
/// only the two that describe one property.
#[derive(Clone, Copy)]
enum SerdeAttrLocation {
    Container,
    Field,
}

impl SerdeAttrLocation {
    /// Reject any serde attribute the emitted schema would silently ignore.
    /// The derive promises the schema matches what `Deserialize` accepts, so an
    /// unsupported key is a compile error rather than an approximate schema.
    fn validate(self, attrs: &[syn::Attribute]) -> syn::Result<()> {
        for attr in attrs.iter().filter(|attr| attr.path().is_ident("serde")) {
            attr.parse_nested_meta(|meta| {
                let path = &meta.path;
                let name = meta
                    .path
                    .get_ident()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| quote!(#path).to_string());
                let supported = match self {
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
}

// ---------------------------------------------------------------------------
// #[phoxal::service] / #[phoxal::driver] / #[phoxal::brain]
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub enum ParticipantKind {
    Service,
    Driver,
    Brain,
}

/// The one fixed participant identity: the mandatory root brain. Shared with
/// `phoxal-manifest`'s authored-identity reservation by value, not by
/// dependency - `phoxal-macros` is below it in the crate graph.
const BRAIN_ID: &str = "brain";

impl ParticipantKind {
    fn attr_name(self) -> &'static str {
        match self {
            ParticipantKind::Service => "#[phoxal::service]",
            ParticipantKind::Driver => "#[phoxal::driver]",
            ParticipantKind::Brain => "#[phoxal::brain]",
        }
    }

    /// The framework-owned kind value this role declares. Emitted as a path,
    /// never as a token: the wire spelling belongs to
    /// `phoxal::participant::metadata::ParticipantKind`'s serde rename.
    fn artifact_kind(self, phoxal: &TokenStream) -> TokenStream {
        let variant = match self {
            ParticipantKind::Service => quote!(Service),
            ParticipantKind::Driver => quote!(Driver),
            ParticipantKind::Brain => quote!(Brain),
        };
        quote!(#phoxal::__private::ParticipantKind::#variant)
    }

    /// The identity this role fixes, if the role admits no `id = "…"` at all.
    ///
    /// Only the root brain does: exactly one brain exists per robot project
    /// and the CLI stages it under the canonical `bin/brain`, so a
    /// project-chosen identity would have nothing to name.
    fn fixed_id(self) -> Option<&'static str> {
        match self {
            ParticipantKind::Brain => Some(BRAIN_ID),
            ParticipantKind::Service | ParticipantKind::Driver => None,
        }
    }

    /// Whether this role admits a `config = Type` argument. The root brain
    /// does not: there is no config side channel for it, so its `Config` is
    /// always `()`.
    fn accepts_config(self) -> bool {
        !matches!(self, ParticipantKind::Brain)
    }

    /// The `(...)` keys this role accepts, for the unknown-key diagnostic.
    fn accepted_keys(self) -> &'static str {
        if self.accepts_config() {
            "id, config, state, or api"
        } else {
            "state or api"
        }
    }

    /// Emits the participant's sealed authoring surfaces. Writing one of these
    /// impls by hand - without going through this macro - does not compile
    /// because the sealing bound is left
    /// unsatisfied, and this expansion is the only thing that names the hidden
    /// sealing path as a matter of course. See
    /// `phoxal::__private::surface::sealing`'s docs for the exact strength of
    /// that seal (it closes the accidental route, not a capability
    /// boundary - the sealing path is `#[doc(hidden)]`, not private, because
    /// this expansion runs in the downstream participant crate).
    fn marker_impl(self, phoxal: &TokenStream, struct_name: &Ident) -> TokenStream {
        match self {
            // The brain is deliberately identical to a service here: the
            // ordinary checked typed-I/O surface and a schedulable step, with
            // no component binding.
            ParticipantKind::Service | ParticipantKind::Brain => {
                quote! {
                    impl #phoxal::__private::surface::sealing::Sealed for #struct_name {}
                    impl #phoxal::__private::surface::TypedIoSurface for #struct_name {}
                }
            }
            ParticipantKind::Driver => quote! {
                impl #phoxal::__private::surface::sealing::Sealed for #struct_name {}
                impl #phoxal::__private::surface::TypedIoSurface for #struct_name {}
                impl #phoxal::__private::surface::ComponentBoundSurface for #struct_name {}
            },
        }
    }
}

/// Reject a participant id outside the grammar the rest of the framework
/// already assumes for identity tokens.
///
/// An `id` ends up in two places that both require this: it is spliced
/// directly between JSON quotes in the embedded linker-section metadata
/// (`expand_participant`'s `#metadata_const_ident`, via `concatcp!`, which
/// concatenates raw `&str` values with no escaping), and it is used as a
/// literal Zenoh key segment (a Liveliness token's participant segment,
/// `crates/bus/src/liveliness.rs`'s `validate_participant`; a dynamic
/// `{participant_id}` topic var, e.g. `phoxal/src/participant/bus_log.rs`'s
/// `logs(&participant_id)`). A `"`, `\`, control character, or `/` in the id
/// would corrupt the JSON or split the key, so this rejects at compile time
/// rather than leaving either to a runtime surprise.
///
/// The character set mirrors the framework's other identity-token grammar
/// (`phoxal::model::component::is_valid_token`, used for component and
/// capability ids): non-empty, lowercase ASCII letters, digits, `_`, or `-`
/// only. This is also the grammar `default_participant_id` (the default when
/// `id = "…"` is omitted, in `expand_participant`) is *expected* to
/// produce - but a `CARGO_PKG_NAME` a human is free to type however they like
/// (uppercase, a lone digit, empty after stripping a kind prefix) can still
/// come out the other side violating this grammar.
/// `validate_participant_id` checks an explicit `id = "…"` at parse time;
/// `expand_participant` applies `is_valid_participant_id` to the *computed*
/// id too, whichever way it was produced, so the invariant actually holds for
/// every participant rather than just the ones that spelled it out.
fn is_valid_participant_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '_' | '-'))
}

fn validate_participant_id(value: &LitStr) -> syn::Result<()> {
    let id = value.value();
    if is_valid_participant_id(&id) {
        Ok(())
    } else {
        Err(syn::Error::new_spanned(
            value,
            format!(
                "invalid participant id '{id}': must be non-empty and contain only lowercase \
                 ASCII letters, digits, '_' or '-'"
            ),
        ))
    }
}

/// The package-name prefixes an official participant crate's directory
/// convention produces: `<kind>/<id>/Cargo.toml` names the crate
/// `phoxal-<kind>-<id>`. Stripping one of these turns the package name back
/// into the bare id it was built from.
///
/// `cargo xtask policy` enforces that convention for the kinds it knows -
/// service and component. `driver` is in this list but not in that one: it is a
/// kind of the canonical participant taxonomy with no directory in this
/// workspace yet, so stripping it here is forward-looking rather than enforced.
const PARTICIPANT_PACKAGE_PREFIXES: &[&str] =
    &["phoxal-service-", "phoxal-driver-", "phoxal-component-"];

/// The default participant id when `id = "…"` is omitted: `CARGO_PKG_NAME`
/// with a leading `phoxal-<kind>-` stripped when present, otherwise the
/// package name as-is.
///
/// `CARGO_PKG_NAME` is read here (proc-macro execution, not `env!` spliced
/// into generated tokens) because computing the *default* - including the
/// prefix strip and the `is_valid_participant_id` check - has to happen at
/// macro-expansion time, before any tokens are emitted; the value is
/// available because a proc-macro runs inside the same `rustc` process
/// invocation Cargo set the variable for when compiling the downstream
/// participant crate, not the `phoxal-macros` crate's own build (verified
/// empirically: a probe inside `expand_participant` reading
/// `std::env::var("CARGO_PKG_NAME")` while building `phoxal-component-bno085`
/// reported `"phoxal-component-bno085"`, never `"phoxal-macros"`).
///
/// The package name is the default rather than the marker struct's name
/// because it is the one that actually matches the intended id: measured
/// across the official participant set, the package name minus its kind prefix
/// matches every one of them, while kebab-casing the struct name fails the
/// components with an underscore in their id (`oak_d_lite` kebabs to
/// `oak-d-lite`). A crate that
/// defines more than one participant still needs an explicit `id = "…"` per
/// struct - they cannot all default to the one package name - which is why the
/// override stays fully supported.
fn default_participant_id(pkg_name: &str) -> String {
    for prefix in PARTICIPANT_PACKAGE_PREFIXES {
        if let Some(stripped) = pkg_name.strip_prefix(prefix)
            && !stripped.is_empty()
        {
            return stripped.to_string();
        }
    }
    pkg_name.to_string()
}

#[derive(Default)]
struct ParticipantArgs {
    id: Option<String>,
    config: Option<Type>,
    state: Option<Type>,
    api: Option<Type>,
}

impl ParticipantArgs {
    fn parse(attr: TokenStream, kind: ParticipantKind) -> syn::Result<Self> {
        let attr_name = kind.attr_name();
        let accepted_keys = kind.accepted_keys();
        let mut args = ParticipantArgs::default();
        let parser = syn::meta::parser(|meta: syn::meta::ParseNestedMeta| {
            if meta.path.is_ident("id") {
                if let Some(fixed) = kind.fixed_id() {
                    return Err(meta.error(format!(
                        "{attr_name} does not accept an `id` argument: there is exactly one root \
                         brain per robot project and its participant identity is fixed to \
                         \"{fixed}\""
                    )));
                }
                let value: LitStr = meta.value()?.parse()?;
                validate_participant_id(&value)?;
                args.id = Some(value.value());
                Ok(())
            } else if meta.path.is_ident("config") {
                if !kind.accepts_config() {
                    return Err(meta.error(format!(
                        "{attr_name} does not accept a `config` argument: the root brain has no \
                         configuration side channel, so its Config is always () - put robot \
                         policy in ordinary Rust code and read robot facts through ctx.robot()"
                    )));
                }
                let value: Type = meta.value()?.parse()?;
                args.config = Some(value);
                Ok(())
            } else if meta.path.is_ident("state") {
                let value: Type = meta.value()?.parse()?;
                args.state = Some(value);
                Ok(())
            } else if meta.path.is_ident("api") {
                let value: Type = meta.value()?.parse()?;
                args.api = Some(value);
                Ok(())
            } else {
                Err(meta.error(format!(
                    "unknown {attr_name}(...) key (expected {accepted_keys})"
                )))
            }
        });
        parser.parse2(attr)?;
        Ok(args)
    }
}

/// Declare a unit marker's static participant contract and identity.
///
/// Mutable runtime state and bus handles are separate `state = …` / `api = …`
/// types. All three associated types default to `()`; there is no local-name
/// inference and the marker itself is never mutable participant state.
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
    if !matches!(item_struct.fields, Fields::Unit) {
        return Err(syn::Error::new_spanned(
            &item_struct.fields,
            format!(
                "{attr_name} requires a unit marker struct; declare mutable data with `state = Type`"
            ),
        ));
    }

    let args = ParticipantArgs::parse(attr, kind)?;
    let id = match kind.fixed_id() {
        // A fixed-identity role never reaches the `CARGO_PKG_NAME` default:
        // `ParticipantArgs::parse` already rejected an explicit `id`, so this
        // is the identity, unconditionally.
        Some(fixed) => fixed.to_string(),
        None => match args.id {
            // Already validated against the identity-token grammar at parse time
            // (`ParticipantArgs::parse`), against the literal's own span.
            Some(id) => id,
            None => {
                // Every real build goes through Cargo, which always sets
                // `CARGO_PKG_NAME` for the crate it invokes `rustc` on - the
                // process this proc-macro runs inside (see
                // `default_participant_id`'s docs). Only a `rustc` invocation
                // bypassing Cargo entirely would leave it unset; that is not a
                // supported way to build a participant, so this is a compile
                // error pointing at the fix rather than a silent fallback.
                let pkg_name = std::env::var("CARGO_PKG_NAME").map_err(|_| {
                    syn::Error::new_spanned(
                        struct_name,
                        format!(
                            "{attr_name} could not read CARGO_PKG_NAME to compute a default \
                         participant id (this build did not go through Cargo) - pass an \
                         explicit id = \"...\" instead"
                        ),
                    )
                })?;
                let computed = default_participant_id(&pkg_name);
                if !is_valid_participant_id(&computed) {
                    return Err(syn::Error::new_spanned(
                        struct_name,
                        format!(
                            "computed participant id '{computed}' (from crate `{pkg_name}`, with any \
                         leading phoxal-<kind>- prefix stripped) is invalid: must be non-empty \
                         and contain only lowercase ASCII letters, digits, '_' or '-' - pass an \
                         explicit id = \"...\" instead"
                        ),
                    ));
                }
                computed
            }
        },
    };
    let config_ty: Type = args.config.unwrap_or_else(|| syn::parse_quote!(()));
    let state_ty: Type = args.state.unwrap_or_else(|| syn::parse_quote!(()));
    let api_ty: Type = args.api.unwrap_or_else(|| syn::parse_quote!(()));

    let phoxal = quote!(::phoxal);
    let artifact_kind = kind.artifact_kind(&phoxal);
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

    Ok(quote! {
        #item_struct

        impl #phoxal::__private::ParticipantSpec for #struct_name {
            const KIND: #phoxal::__private::ParticipantKind = #artifact_kind;
            const ID: &'static str = #id;
            // The participant-authoring facade family, spliced from the
            // framework path rather than named by the author: a participant
            // does not get to pick a contract family, and every handle it
            // builds is bounded on this one.
            type ContractApi = #phoxal::api::Api;
            type Config = #config_ty;
            type State = #state_ty;
            type Api = #api_ty;

            #[doc(hidden)]
            fn __new() -> Self {
                Self
            }

            // Defeats ELF `--gc-sections` dropping `#metadata_static_ident`
            // as unreachable from `main` (see this method's own docs on
            // `ParticipantSpec`, and the metadata static's own `#[used]`
            // comment below on why `#[used]` alone is not enough). `black_box`
            // is an unmistakable "reads this on purpose" marker, not an
            // accident a future cleanup pass would delete as a no-op.
            #[doc(hidden)]
            fn __retain_embedded_metadata() {
                ::std::hint::black_box(&#metadata_static_ident);
            }
        }

        #marker

        // Process-boundary metadata is self-identifying and strict. The
        // framework train version - the single compatibility identity - comes
        // in from `__private::compatibility` and the document is composed by
        // the framework-owned writer, so this expansion spells no version of
        // its own. The CLI reads this record out of the built artifact before
        // launch; it is the binary's whole compatibility declaration.
        #[doc(hidden)]
        const #metadata_const_ident: &'static str =
            #phoxal::__private::compatibility::participant_metadata_json!(
                framework = #phoxal::__private::compatibility::FRAMEWORK,
                id = #id,
                kind = #artifact_kind,
                config_schema =
                    <#config_ty as #phoxal::__private::ParticipantConfig>::SCHEMA_JSON,
            );
        #[doc(hidden)]
        const #metadata_len_ident: usize = #metadata_const_ident.len();

        // The cross-platform `#[link_section]` pair the metadata static is
        // placed under (cargo-auditable's embedding pattern):
        // `__DATA,__phoxal_meta` on Mach-O (macOS; segment,section syntax,
        // section name <=16 bytes), and `.phoxal_meta` everywhere else (ELF and
        // the other platforms this framework targets - Linux robots,
        // primarily). `#[used]` keeps the linker from discarding the static
        // during *this compilation unit's* own dead-code elimination, but not
        // from ELF `--gc-sections` at final link time, which drops any section
        // unreachable from `main` regardless of `#[used]` - the
        // `__retain_embedded_metadata` black-box read above closes that gap.
        // The section's *bytes*, not the symbol, are the payload a reader
        // (today `phoxal-cli`'s config-schema validation) lifts straight out of
        // the built artifact file, never by executing it.
        #[used]
        #[cfg_attr(target_os = "macos", unsafe(link_section = "__DATA,__phoxal_meta"))]
        #[cfg_attr(not(target_os = "macos"), unsafe(link_section = ".phoxal_meta"))]
        #[doc(hidden)]
        static #metadata_static_ident: [u8; #metadata_len_ident] =
            #phoxal::__private::meta::bytes_of(#metadata_const_ident);
    })
}

// ---------------------------------------------------------------------------
// #[phoxal::step(hz = N)]
// ---------------------------------------------------------------------------

/// Record the step cadence on the ordinary synchronous `Participant::step`
/// override. The schedule rides on a hidden associated fn rather than
/// replacing the method, so the authored body stays exactly as written.
pub fn expand_step(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let hz = parse_step_hz(attr)?;
    let method: ImplItemFn = syn::parse2(item)?;

    if method.sig.ident != "step" {
        return Err(syn::Error::new_spanned(
            &method.sig.ident,
            "#[phoxal::step] must annotate the `step` Participant method",
        ));
    }
    if method.sig.asyncness.is_some() {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "#[phoxal::step] requires a synchronous method",
        ));
    }
    let phoxal = quote!(::phoxal);
    Ok(quote! {
        #[doc(hidden)]
        fn __step_schedule() -> ::core::option::Option<#phoxal::__private::StepSchedule> {
            ::core::option::Option::Some(#phoxal::__private::StepSchedule::hz(#hz))
        }

        #method
    })
}

fn parse_step_hz(attr: TokenStream) -> syn::Result<f64> {
    let mut hz = None;
    let parser = syn::meta::parser(|meta| {
        if !meta.path.is_ident("hz") {
            return Err(meta.error("unknown #[phoxal::step(...)] key (expected `hz`)"));
        }
        if hz.is_some() {
            return Err(meta.error("duplicate `hz`"));
        }
        let value: Expr = meta.value()?.parse()?;
        hz = Some(step_hz_literal(&value)?);
        Ok(())
    });
    parser.parse2(attr)?;

    let hz = hz.ok_or_else(|| {
        syn::Error::new(
            Span::call_site(),
            "#[phoxal::step(hz = N)] requires a frequency",
        )
    })?;
    if !hz.is_finite() || hz <= 0.0 {
        return Err(syn::Error::new(
            Span::call_site(),
            "#[phoxal::step(hz = N)] frequency must be positive and finite",
        ));
    }
    Ok(hz)
}

/// A step frequency is a numeric literal (optionally negated, so the
/// positive-and-finite check above reports the real value rather than a parse
/// error). It is never an arbitrary expression: the schedule has to be readable
/// at expansion time.
fn step_hz_literal(expr: &Expr) -> syn::Result<f64> {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Int(value),
            ..
        }) => value.base10_parse(),
        Expr::Lit(ExprLit {
            lit: Lit::Float(value),
            ..
        }) => value.base10_parse(),
        Expr::Unary(unary) if matches!(unary.op, UnOp::Neg(_)) => {
            Ok(-step_hz_literal(&unary.expr)?)
        }
        _ => Err(syn::Error::new_spanned(
            expr,
            "step frequency must be a numeric literal",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;

    fn compact_tokens(tokens: TokenStream) -> String {
        tokens
            .to_string()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn default_participant_id_strips_the_kind_prefix_for_every_official_directory() {
        assert_eq!(default_participant_id("phoxal-service-drive"), "drive");
        assert_eq!(default_participant_id("phoxal-driver-bno085"), "bno085");
        assert_eq!(
            default_participant_id("phoxal-component-ddsm115"),
            "ddsm115"
        );
    }

    #[test]
    fn default_participant_id_passes_through_an_unprefixed_package_name() {
        // A user crate outside the official phoxal-<kind>-<id> directory
        // convention - e.g. `avoid`, matching a `services.avoid` key in
        // `robot.yaml`.
        assert_eq!(default_participant_id("avoid"), "avoid");
    }

    #[test]
    fn default_participant_id_keeps_the_full_name_when_stripping_would_empty_it() {
        // A package literally named `phoxal-service-` (empty id part) must
        // not collapse to an empty id: falling through to the full name lets
        // `is_valid_participant_id` reject it with a clear compile error
        // instead of this function silently producing "".
        assert_eq!(default_participant_id("phoxal-service-"), "phoxal-service-");
    }

    #[test]
    fn an_invalid_computed_id_fails_the_same_grammar_check_expand_participant_applies() {
        // Cargo allows uppercase in `package.name`, unlike this framework's
        // identity-token grammar, so a mixed-case package computes an
        // invalid id - exactly the condition `expand_participant` guards on
        // before erroring rather than embedding it (see the call site right
        // after `default_participant_id` in `expand_participant`).
        let computed = default_participant_id("MyRobot");
        assert_eq!(computed, "MyRobot");
        assert!(!is_valid_participant_id(&computed));
    }

    #[test]
    fn omitted_id_defaults_to_this_crate_s_own_package_name() {
        // `cargo test -p phoxal-macros` sets CARGO_PKG_NAME to
        // "phoxal-macros" for this test binary - the exact mechanism
        // `expand_participant` relies on for every downstream participant
        // crate (verified empirically against a real downstream build; see
        // `default_participant_id`'s doc comment). "phoxal-macros" matches
        // none of the official kind prefixes, so it passes through
        // unchanged.
        let expanded = compact_tokens(
            expand_participant(
                quote! {},
                quote! { struct OmittedId; },
                ParticipantKind::Service,
            )
            .expect("expands with a defaulted id"),
        );
        assert!(
            expanded.contains("const ID : & 'static str = \"phoxal-macros\""),
            "the default id must come from CARGO_PKG_NAME, never from the struct \
             name (which would read `omitted-id`): {expanded}"
        );
    }

    #[test]
    fn an_explicit_id_overrides_the_package_name_default() {
        let expanded = compact_tokens(
            expand_participant(
                quote! { id = "custom-id" },
                quote! { struct ExplicitId; },
                ParticipantKind::Service,
            )
            .expect("expands with the explicit id"),
        );
        assert!(
            expanded.contains("const ID : & 'static str = \"custom-id\""),
            "an explicit id = \"...\" must win over CARGO_PKG_NAME: {expanded}"
        );
    }

    #[test]
    fn the_brain_role_fixes_its_identity_kind_and_config() {
        let expanded = compact_tokens(
            expand_participant(quote! {}, quote! { struct Brain; }, ParticipantKind::Brain)
                .expect("expands"),
        );
        // The identity is fixed, never the `CARGO_PKG_NAME` default this test
        // binary would otherwise produce ("phoxal-macros").
        assert!(
            expanded.contains("const ID : & 'static str = \"brain\""),
            "{expanded}"
        );
        assert!(
            expanded.contains("const KIND : :: phoxal :: __private :: ParticipantKind = :: phoxal :: __private :: ParticipantKind :: Brain"),
            "{expanded}"
        );
        assert!(expanded.contains("type Config = () ;"), "{expanded}");
        // Launch parsing is one process-boundary contract in `phoxal`; role
        // macros carry no launch policy or environment fallback.
        // The embedded record carries the brain kind under the fixed identity,
        // and every version comes from the facade compatibility set as a typed
        // value rather than a literal in this expansion.
        assert!(
            expanded.contains("compatibility :: participant_metadata_json !"),
            "{expanded}"
        );
        assert!(
            expanded.contains(
                "id = \"brain\" , kind = :: phoxal :: __private :: ParticipantKind :: Brain"
            ),
            "{expanded}"
        );
    }

    #[test]
    fn the_brain_role_grants_no_capability_beyond_the_checked_service_surface() {
        let expanded = compact_tokens(
            expand_participant(quote! {}, quote! { struct Brain; }, ParticipantKind::Brain)
                .expect("expands"),
        );
        assert!(
            expanded.contains("surface :: TypedIoSurface for Brain"),
            "{expanded}"
        );
        assert!(
            !expanded.contains("ComponentBoundSurface"),
            "the brain must not be component-bound: {expanded}"
        );
    }

    #[test]
    fn the_brain_role_accepts_state_and_api_but_rejects_id_and_config() {
        expand_participant(
            quote! { state = MyState, api = MyApi },
            quote! { struct Brain; },
            ParticipantKind::Brain,
        )
        .expect("state and api are ordinary checked-role arguments");

        let id = expand_participant(
            quote! { id = "policy" },
            quote! { struct Brain; },
            ParticipantKind::Brain,
        )
        .expect_err("an explicit id must be rejected");
        assert!(
            id.to_string().contains("does not accept an `id` argument"),
            "{id}"
        );

        let config = expand_participant(
            quote! { config = MyConfig },
            quote! { struct Brain; },
            ParticipantKind::Brain,
        )
        .expect_err("an explicit config must be rejected");
        assert!(
            config
                .to_string()
                .contains("does not accept a `config` argument"),
            "{config}"
        );
    }

    #[test]
    fn the_embedded_record_names_the_single_framework_compatibility_constant() {
        let expanded = compact_tokens(
            expand_participant(
                quote! {},
                quote! { struct Probe; },
                ParticipantKind::Service,
            )
            .expect("expands"),
        );
        assert!(
            expanded.contains("compatibility :: FRAMEWORK"),
            "the embedded record must name compatibility::FRAMEWORK: {expanded}"
        );
        // The framework train version is the whole compatibility claim, so the
        // per-boundary constants it replaced must not reappear beside it.
        for retired in ["API", "API_TOKEN", "BUS", "LAUNCH", "RUNTIME"] {
            assert!(
                !expanded.contains(&format!("compatibility :: {retired}")),
                "the embedded record must not name compatibility::{retired}: {expanded}"
            );
        }
    }

    /// The role attributes accept exactly four keys. The retired topology
    /// declaration was one of them until it was deleted, so an authored crate
    /// that still spells it gets a diagnostic
    /// naming the keys that remain rather than a silently ignored argument.
    #[test]
    fn a_retired_requirement_argument_is_an_unknown_key() {
        let error = expand_participant(
            quote! { requirement = SomeRequirement },
            quote! { struct Probe; },
            ParticipantKind::Service,
        )
        .expect_err("`requirement` is not a role-attribute key");
        assert!(
            error
                .to_string()
                .contains("expected id, config, state, or api"),
            "{error}"
        );
    }

    /// The framework train version is the single compatibility identity and it
    /// reaches the record through the facade constant, so this expansion spells
    /// no version at all: not the train version, not a retired per-boundary
    /// identity, not an authored source grammar.
    #[test]
    fn the_expansion_spells_no_version() {
        for kind in [
            ParticipantKind::Service,
            ParticipantKind::Driver,
            ParticipantKind::Brain,
        ] {
            let expanded = compact_tokens(
                expand_participant(quote! {}, quote! { struct Probe; }, kind).expect("expands"),
            );
            for token in [
                // This crate is built from the train it must not hand-spell.
                env!("CARGO_PKG_VERSION"),
                "participant-metadata",
                "bus-abi",
                "robot-api",
                "participant-launch",
                "runtime-bundle",
                "phoxal/robot/v0",
                "phoxal/component/v0",
                "phoxal/simulation/v0",
            ] {
                assert!(
                    !expanded.contains(token),
                    "the expansion must not spell the version token '{token}': {expanded}"
                );
            }
        }
    }

    /// The contract family a participant's handles may come from is fixed by
    /// the role attribute to the authoring facade, never named by the author.
    /// Every `SetupContext` builder is bounded on this associated type, so a
    /// participant that reached for another semantic contract tree fails to
    /// compile.
    #[test]
    fn every_role_fixes_its_contract_api_to_the_authoring_facade() {
        for kind in [
            ParticipantKind::Service,
            ParticipantKind::Driver,
            ParticipantKind::Brain,
        ] {
            let expanded = compact_tokens(
                expand_participant(quote! {}, quote! { struct Probe; }, kind).expect("expands"),
            );
            assert!(
                expanded.contains("type ContractApi = :: phoxal :: api :: Api ;"),
                "{} must fix ContractApi to the facade family: {expanded}",
                kind.attr_name()
            );
        }
    }

    #[test]
    fn expand_participant_emits_a_black_box_read_of_its_own_metadata_static() {
        // The ELF `--gc-sections` defeat (the `#[used]` comment in
        // `expand_participant`'s emitted metadata static, and
        // `Participant::__retain_embedded_metadata`'s docs): every
        // participant's generated `impl Participant` must read its metadata
        // static through `std::hint::black_box`, or a future edit to this
        // function could silently drop the one line that keeps the section
        // out of the linker's reachability GC.
        let expanded = compact_tokens(
            expand_participant(
                quote! {},
                quote! { struct Probe; },
                ParticipantKind::Service,
            )
            .expect("expands"),
        );
        assert!(
            expanded.contains("fn __retain_embedded_metadata () { :: std :: hint :: black_box (& __PHOXAL_PARTICIPANT_META_PROBE) ; }"),
            "expected a black_box read of the participant's own metadata static: {expanded}"
        );
    }

    #[test]
    fn step_emits_the_hidden_schedule_hook_and_original_method() {
        let expanded = expand_step(
            quote!(hz = 50),
            quote! {
                fn step(
                    &self,
                    _api: &Self::Api,
                    _step: StepContext,
                    _state: &mut Self::State,
                ) -> Result<()> {
                    Ok(())
                }
            },
        )
        .expect("step expands")
        .to_string();

        assert!(expanded.contains("__step_schedule"));
        assert!(expanded.contains("StepSchedule :: hz (50f64)"));
        assert!(expanded.contains("fn step"));
    }

    #[test]
    fn step_rejects_async_transitions_at_macro_expansion() {
        let error = expand_step(
            quote!(hz = 50),
            quote! {
                async fn step(
                    &self,
                    _api: &Self::Api,
                    _step: StepContext,
                    _state: &mut Self::State,
                ) -> Result<()> {
                    Ok(())
                }
            },
        )
        .expect_err("scheduled transitions must not be async");
        assert!(error.to_string().contains("synchronous method"));
    }
}
