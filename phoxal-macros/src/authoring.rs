//! The participant authoring model: `#[derive(phoxal::Config)]` and the
//! `#[phoxal::service]` /
//! `#[phoxal::driver]` / `#[phoxal::simulator]` / `#[phoxal::tool]` attribute
//! macros. Role attributes emit the static [`ParticipantSpec`] contract;
//! authors implement `Participant` directly for lifecycle behavior.

use heck::ToShoutySnakeCase;
use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::Parser;
use syn::{Data, DeriveInput, Fields, Ident, LitStr, Type, TypePath};

use crate::util::phoxal;

fn as_type_path(ty: &Type) -> Option<&TypePath> {
    match ty {
        Type::Path(p) => Some(p),
        _ => None,
    }
}

/// The cross-platform `#[link_section]` pair a metadata static is placed
/// under (cargo-auditable's embedding pattern): `__DATA,__phoxal_meta`
/// on Mach-O (macOS; segment,section syntax, section name <=16 bytes), and
/// `.phoxal_meta` everywhere else (ELF and other platforms this
/// framework targets - Linux robots, primarily). The section carries the
/// strict `{schema, id, kind, class, config_schema}` process contract.
/// `#[used]` keeps the
/// linker from discarding the static during *this compilation unit's* own
/// dead-code elimination, but not from ELF `--gc-sections` at final link
/// time, which drops any section unreachable from `main` regardless of
/// `#[used]` - see `Participant::__retain_embedded_metadata`'s docs for the
/// mechanism that closes that gap. The section's *bytes*, not the symbol, are
/// the payload a reader (today `phoxal-cli`'s config-schema validation) lifts
/// straight out of the built artifact file, never by executing it.
fn link_section_attrs() -> TokenStream {
    quote! {
        #[used]
        #[cfg_attr(target_os = "macos", unsafe(link_section = "__DATA,__phoxal_meta"))]
        #[cfg_attr(not(target_os = "macos"), unsafe(link_section = ".phoxal_meta"))]
    }
}

/// A `syn::LitStr` token for a JSON literal fragment used as a
/// `__concatcp!`/`concat!`-style macro argument. Used by the config-schema
/// derive to splice literal JSON around resolved schema consts.
fn json_lit(s: &str) -> TokenStream {
    let lit = syn::LitStr::new(s, proc_macro2::Span::call_site());
    quote!(#lit)
}

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
            const __SCHEMA: #phoxal::__private::api::__meta::ConstSchema =
                #phoxal::__private::api::__meta::ConstSchema::new()
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
                quote!(#phoxal::__private::launch::ToolParticipantLaunch)
            }
            ParticipantKind::Simulator => {
                quote!(#phoxal::__private::launch::SimulatorParticipantLaunch)
            }
            ParticipantKind::Service | ParticipantKind::Driver => {
                quote!(#phoxal::__private::launch::ClockedParticipantLaunch)
            }
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
            ParticipantKind::Service => {
                quote! {
                    impl #phoxal::__private::surface::sealing::Sealed for #struct_name {}
                    impl #phoxal::__private::surface::TypedIoSurface for #struct_name {}
                    impl #phoxal::__private::SchedulableSurface for #struct_name {}
                }
            }
            ParticipantKind::Driver => quote! {
                impl #phoxal::__private::surface::sealing::Sealed for #struct_name {}
                impl #phoxal::__private::surface::TypedIoSurface for #struct_name {}
                impl #phoxal::__private::surface::ComponentBoundSurface for #struct_name {}
                impl #phoxal::__private::SchedulableSurface for #struct_name {}
            },
            ParticipantKind::Simulator => quote! {
                impl #phoxal::__private::surface::sealing::Sealed for #struct_name {}
                impl #phoxal::__private::surface::TypedIoSurface for #struct_name {}
                impl #phoxal::__private::surface::ComponentBoundSurface for #struct_name {}
                impl #phoxal::__private::surface::WorldAuthoritySurface for #struct_name {}
            },
            ParticipantKind::Tool => quote! {
                impl #phoxal::__private::surface::sealing::Sealed for #struct_name {}
                impl #phoxal::__private::surface::ToolSurface for #struct_name {}
            },
        }
    }
}

/// Reject a participant id outside the grammar the rest of the framework
/// already assumes for identity tokens.
///
/// An `id` ends up in two places that both require this: it is spliced
/// directly between JSON quotes in the embedded linker-section metadata
/// (`expand_participant`'s `#metadata_const_ident`, via `__concatcp!`, which
/// concatenates raw `&str` values with no escaping), and it is used as a
/// literal Zenoh key segment (a Liveliness token's participant segment,
/// `phoxal-bus/src/liveliness.rs`'s `validate_participant`; a dynamic
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
/// `workspace-policy`'s `expected_package_name` enforces that convention for
/// the kinds it knows - service, component, tool, simulator. `driver` is in
/// this list but not in that one: it is a kind of the canonical participant
/// taxonomy with no directory in this workspace yet, so stripping it here is
/// forward-looking rather than enforced.
const PARTICIPANT_PACKAGE_PREFIXES: &[&str] = &[
    "phoxal-service-",
    "phoxal-driver-",
    "phoxal-tool-",
    "phoxal-simulator-",
    "phoxal-component-",
];

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
/// Chosen over the struct name (the previous default) because measured
/// against all 25 official participants, the package name minus its kind
/// prefix matches the intended id for 25/25, while kebab-casing the struct
/// name only matched 16/25 - it fails every tool (`struct ToolLog` kebabs to
/// `tool-log`, not `log`), `BehaviorService`, `WebotsControllerSimulator`,
/// and the two components with an underscore in their id (`oak_d_lite`
/// kebabs to `oak-d-lite`). A crate that defines more than one participant
/// still needs an explicit `id = "…"` per struct - they cannot all default to
/// the one package name - which is why the override stays fully supported.
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
    fn parse(attr: TokenStream, attr_name: &str) -> syn::Result<Self> {
        let mut args = ParticipantArgs::default();
        let parser = syn::meta::parser(|meta: syn::meta::ParseNestedMeta| {
            if meta.path.is_ident("id") {
                let value: LitStr = meta.value()?.parse()?;
                validate_participant_id(&value)?;
                args.id = Some(value.value());
                Ok(())
            } else if meta.path.is_ident("config") {
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
                    "unknown {attr_name}(...) key (expected id, config, state, or api)"
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

    let args = ParticipantArgs::parse(attr, attr_name)?;
    let id = match args.id {
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
    };
    let config_ty: Type = args.config.unwrap_or_else(|| syn::parse_quote!(()));
    let state_ty: Type = args.state.unwrap_or_else(|| syn::parse_quote!(()));
    let api_ty: Type = args.api.unwrap_or_else(|| syn::parse_quote!(()));

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

        impl #phoxal::__private::ParticipantSpec for #struct_name {
            const KIND: &'static str = #artifact_kind;
            const PARTICIPANT_CLASS: &'static str = #participant_class;
            const ID: &'static str = #id;
            type LaunchPolicy = #launch_policy;
            type Config = #config_ty;
            type State = #state_ty;
            type Api = #api_ty;

            #[doc(hidden)]
            fn __new() -> Self {
                Self
            }

            // Defeats ELF `--gc-sections` dropping `#metadata_static_ident`
            // as unreachable from `main` (see this method's own docs on
            // `ParticipantSpec`, and `link_section_attrs`'s docs on why `#[used]`
            // alone is not enough). `black_box` is an unmistakable "reads
            // this on purpose" marker, not an accident a future cleanup pass
            // would delete as a no-op.
            #[doc(hidden)]
            fn __retain_embedded_metadata() {
                ::std::hint::black_box(&#metadata_static_ident);
            }
        }

        #marker

        // Process-boundary metadata is self-identifying and strict. Its schema,
        // kind, and capability class are interpreted by the CLI before launch.
        #[doc(hidden)]
        const #metadata_const_ident: &'static str =
            #phoxal::__private::api::__meta::__concatcp!(
                "{\"schema\":\"phoxal/participant-metadata/v0\",\"id\":\"",
                #id,
                "\",\"kind\":\"",
                #artifact_kind,
                "\",\"class\":\"",
                #participant_class,
                "\",\"config_schema\":",
                <#config_ty as #phoxal::__private::ParticipantConfig>::SCHEMA_JSON,
                "}"
            );
        #[doc(hidden)]
        const #metadata_len_ident: usize = #metadata_const_ident.len();
        #link_section
        #[doc(hidden)]
        static #metadata_static_ident: [u8; #metadata_len_ident] =
            #phoxal::__private::api::__meta::__bytes_of(#metadata_const_ident);
    })
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
        assert_eq!(default_participant_id("phoxal-tool-log"), "log");
        assert_eq!(
            default_participant_id("phoxal-simulator-webots-controller"),
            "webots-controller"
        );
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
                ParticipantKind::Tool,
            )
            .expect("expands with a defaulted id"),
        );
        assert!(
            expanded.contains("const ID : & 'static str = \"phoxal-macros\""),
            "the default id must come from CARGO_PKG_NAME, not the struct name \
             (`omitted-id`, the old default, must NOT appear): {expanded}"
        );
    }

    #[test]
    fn an_explicit_id_overrides_the_package_name_default() {
        let expanded = compact_tokens(
            expand_participant(
                quote! { id = "custom-id" },
                quote! { struct ExplicitId; },
                ParticipantKind::Tool,
            )
            .expect("expands with the explicit id"),
        );
        assert!(
            expanded.contains("const ID : & 'static str = \"custom-id\""),
            "an explicit id = \"...\" must win over CARGO_PKG_NAME: {expanded}"
        );
    }

    #[test]
    fn expand_participant_emits_a_black_box_read_of_its_own_metadata_static() {
        // The ELF `--gc-sections` defeat (this file's `link_section_attrs`
        // docs, and `Participant::__retain_embedded_metadata`'s docs): every
        // participant's generated `impl Participant` must read its metadata
        // static through `std::hint::black_box`, or a future edit to this
        // function could silently drop the one line that keeps the section
        // out of the linker's reachability GC.
        let expanded = compact_tokens(
            expand_participant(quote! {}, quote! { struct Probe; }, ParticipantKind::Tool)
                .expect("expands"),
        );
        assert!(
            expanded.contains("fn __retain_embedded_metadata () { :: std :: hint :: black_box (& __PHOXAL_PARTICIPANT_META_PROBE) ; }"),
            "expected a black_box read of the participant's own metadata static: {expanded}"
        );
    }
}
