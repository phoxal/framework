//! Expansion for the Rust-authored endpoint declaration.
//!
//! `#[phoxal::endpoints]` turns one declaration struct into the complete local
//! contract surface: endpoint method constants, message aliases, the
//! runtime `Inputs` and `Outputs` transactions, composition-bound call
//! handles, projection hooks, and the runtime attachment macro. Nothing
//! here constructs a value or transmits anything; accepted invocations
//! remain the only effect boundary.

use heck::{ToShoutySnakeCase, ToUpperCamelCase};
use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::parse::Parser;
use syn::{Fields, Ident, ItemStruct, LitInt, LitStr, Path, Type};

use crate::message::anchored;

/// Default admitted item count for queued endpoint forms.
const DEFAULT_MAX_ITEMS: u64 = 16;

/// Default whole-batch encoded byte bound for endpoint forms.
const DEFAULT_MAX_BYTES: u64 = 16_384;

fn default_max_items() -> LitInt {
    LitInt::new(&DEFAULT_MAX_ITEMS.to_string(), Span::call_site())
}

fn default_max_bytes() -> LitInt {
    LitInt::new(&DEFAULT_MAX_BYTES.to_string(), Span::call_site())
}

/// One declared endpoint.
enum Endpoint {
    Input {
        name: Ident,
        message: Path,
        queued: bool,
        lease_ms: Option<LitInt>,
        max_age_ms: Option<LitInt>,
        max_items: Option<LitInt>,
        max_bytes: LitInt,
    },
    Output {
        name: Ident,
        message: Path,
        queued: bool,
        projection: bool,
        forwarded: bool,
        family: Option<Family>,
        lease_ms: Option<LitInt>,
        max_items: Option<LitInt>,
        max_bytes: LitInt,
        bootstrap: bool,
        on_change: bool,
    },
    Request {
        name: Ident,
        source: RequestSource,
        request: PayloadRef,
        response: PayloadRef,
        max_items: LitInt,
        max_bytes: LitInt,
        call: bool,
    },
}

/// Where one operation endpoint's payloads and identity come from.
enum RequestSource {
    /// Authored `RequestReply<Request, Response>` fields with a declared or
    /// package-derived contract identity.
    Authored {
        contract: LitStr,
        request_identity: Option<LitStr>,
        response_identity: Option<LitStr>,
    },
    /// A typed operation descriptor: the field names a type implementing
    /// [`phoxal::contracts::Operation`], which carries the identity and both
    /// payload types.
    Descriptor { descriptor: Path },
}

/// One payload type reference: a message path, or an associated payload of
/// an operation descriptor.
enum PayloadRef {
    Message(Path),
    Descriptor { descriptor: Path, response: bool },
}

impl PayloadRef {
    /// The payload type at one `super::` depth inside the generated module.
    fn at(&self, depth: usize) -> Type {
        match self {
            PayloadRef::Message(path) => {
                let anchored = anchored(path, depth);
                syn::parse_quote!(#anchored)
            }
            PayloadRef::Descriptor {
                descriptor,
                response,
            } => {
                let anchored = anchored(descriptor, depth);
                if *response {
                    syn::parse_quote!(
                        <#anchored as ::phoxal::contracts::Operation>::Response
                    )
                } else {
                    syn::parse_quote!(
                        <#anchored as ::phoxal::contracts::Operation>::Request
                    )
                }
            }
        }
    }
}

struct Family {
    pointer: LitStr,
    suffix: LitStr,
    max_ports: LitInt,
    lease_ms: LitInt,
}

impl Family {
    fn metadata(&self) -> TokenStream {
        let Self {
            pointer,
            suffix,
            max_ports,
            ..
        } = self;
        quote!(::phoxal::runtime::outputs::OutputFamily {
            config_pointer: #pointer, suffix: #suffix, max_ports: #max_ports,
        })
    }
}

/// Attribute values collected from one endpoint field.
#[derive(Default)]
struct FieldOptions {
    family: Option<LitStr>,
    suffix: Option<LitStr>,
    max_ports: Option<LitInt>,
    lease_ms: Option<LitInt>,
    max_age_ms: Option<LitInt>,
    max_items: Option<LitInt>,
    max_bytes: Option<LitInt>,
    contract: Option<LitStr>,
    request_identity: Option<LitStr>,
    response_identity: Option<LitStr>,
    projection: bool,
    stamped: bool,
    bootstrap: bool,
    on_change: bool,
}

/// The endpoint role an attribute path spells.
fn endpoint_role(attr: &syn::Attribute) -> Option<String> {
    let segments = &attr.path().segments;
    if segments.len() != 2 || segments.first()?.ident != "phoxal" {
        return None;
    }
    let role = segments.last()?.ident.to_string();
    ["input", "output", "operation", "call"]
        .contains(&role.as_str())
        .then_some(role)
}

fn parse_options(attrs: &[syn::Attribute], role: &str) -> syn::Result<FieldOptions> {
    let mut options = FieldOptions::default();
    for attr in attrs.iter().filter(|attr| endpoint_role(attr).is_some()) {
        let Some(last) = attr.path().segments.last() else {
            continue;
        };
        if last.ident != role {
            return Err(syn::Error::new_spanned(
                attr,
                "an endpoint field declares exactly one role",
            ));
        }
        // A bare `#[phoxal::output]` selects the role with every default.
        if matches!(attr.meta, syn::Meta::Path(_)) {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            let integer = |meta: &syn::meta::ParseNestedMeta<'_>| -> syn::Result<LitInt> {
                let literal: LitInt = meta.value()?.parse()?;
                if literal.base10_parse::<u64>()? == 0 {
                    return Err(meta.error("bounds must be positive"));
                }
                Ok(literal)
            };
            if meta.path.is_ident("family") {
                options.family = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("suffix") {
                options.suffix = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("max_ports") {
                options.max_ports = Some(integer(&meta)?);
            } else if meta.path.is_ident("lease_ms") {
                options.lease_ms = Some(integer(&meta)?);
            } else if meta.path.is_ident("max_age_ms") {
                options.max_age_ms = Some(integer(&meta)?);
            } else if meta.path.is_ident("max_items") {
                options.max_items = Some(integer(&meta)?);
            } else if meta.path.is_ident("max_bytes") {
                options.max_bytes = Some(integer(&meta)?);
            } else if meta.path.is_ident("contract") {
                options.contract = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("request") {
                options.request_identity = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("response") {
                options.response_identity = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("projection") {
                let value = if meta.input.peek(syn::Token![=]) {
                    let value = meta.value()?;
                    if value.peek(syn::LitStr) {
                        value.parse::<LitStr>()?.value()
                    } else {
                        value.parse::<syn::Ident>()?.to_string()
                    }
                } else if meta.input.peek(syn::Ident) {
                    meta.input.parse::<syn::Ident>()?.to_string()
                } else {
                    return Err(meta.error("projection must name state"));
                };
                if value != "state" {
                    return Err(meta.error("state is the only supported projection"));
                }
                options.projection = true;
            } else if meta.path.is_ident("stamped") {
                options.stamped = true;
            } else if meta.path.is_ident("bootstrap") {
                options.bootstrap = true;
            } else if meta.path.is_ident("on_change") {
                options.on_change = true;
            } else {
                return Err(meta.error(format!(
                    "unsupported {role} option; expected lease_ms, max_age_ms, max_items, max_bytes, contract, projection, stamped, bootstrap, or on_change"
                )));
            }
            Ok(())
        })?;
    }
    Ok(options)
}

/// Expands `#[phoxal::endpoints]` on one declaration struct.
/// Reads this package's derived standard endpoint fields, when its build
/// helper generated a capability fragment.
fn standard_endpoint_fields() -> syn::Result<Option<syn::FieldsNamed>> {
    let Some(fragment) = crate::attachment::fragment("PHOXAL_STANDARD_ENDPOINTS")? else {
        return Ok(None);
    };
    syn::parse_str::<syn::FieldsNamed>(&fragment)
        .map(Some)
        .map_err(|error| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                format!("the generated standard endpoint fragment is invalid: {error}"),
            )
        })
}

pub(crate) fn expand_api(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let mut package: Option<LitStr> = None;
    syn::meta::parser(|meta| {
        if meta.path.is_ident("package") {
            package = Some(meta.value()?.parse()?);
            Ok(())
        } else {
            Err(meta.error("expected package"))
        }
    })
    .parse2(attr)?;
    let mut item: ItemStruct = syn::parse2(item)?;
    if !item.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &item.generics,
            "#[phoxal::endpoints] does not support generic contracts",
        ));
    }
    // A component's derived standard endpoints travel as a field fragment
    // from its build helper; an explicitly authored contract splices them
    // in so standard and component-specific endpoints assemble into one
    // Runtime contract without repetition.
    if let Some(standard) = standard_endpoint_fields()? {
        let Fields::Named(fields) = &mut item.fields else {
            return Err(syn::Error::new_spanned(
                &item.fields,
                "#[phoxal::endpoints] structs declare named endpoint fields",
            ));
        };
        for field in standard.named {
            let Some(name) = field.ident.as_ref().map(ToString::to_string) else {
                continue;
            };
            if fields
                .named
                .iter()
                .any(|existing| existing.ident.as_ref().is_some_and(|ident| *ident == name))
            {
                return Err(syn::Error::new_spanned(
                    &field,
                    format!(
                        "endpoint `{name}` is declared by both this contract and the \
                         component's derived standard surface; capabilities determine \
                         their standard endpoints — declare only the component-specific \
                         surface here"
                    ),
                ));
            }
            fields.named.push(field);
        }
    }
    if let Some(fragment) = crate::attachment::fragment("PHOXAL_CONVERSION_ENDPOINTS")? {
        let generated = syn::parse_str::<syn::FieldsNamed>(&fragment)?;
        let Fields::Named(fields) = &mut item.fields else {
            return Err(syn::Error::new_spanned(
                &item,
                "endpoint contracts require named fields",
            ));
        };
        for field in generated.named {
            if fields
                .named
                .iter()
                .any(|existing| existing.ident == field.ident)
            {
                return Err(syn::Error::new_spanned(
                    &field,
                    "endpoint name is reserved for generated conversion routing",
                ));
            }
            fields.named.push(field);
        }
    }
    let Fields::Named(fields) = &item.fields else {
        return Err(syn::Error::new_spanned(
            &item.fields,
            "#[phoxal::endpoints] structs declare named endpoint fields; a unit \
             struct declares an empty contract as `Contract;` is rejected — \
             use `Contract {}`",
        ));
    };
    let contract_name = item.ident.clone();
    let module = format_ident!("{}", to_snake(&contract_name));
    let vis = &item.vis;

    let mut endpoints = Vec::new();
    let mut struct_fields = Vec::new();
    for field in &fields.named {
        endpoints.push(analyze_endpoint(field, package.as_ref())?);
        let attrs: Vec<_> = field
            .attrs
            .iter()
            .filter(|attr| endpoint_role(attr).is_none())
            .cloned()
            .collect();
        let field_vis = &field.vis;
        let ident = field
            .ident
            .as_ref()
            .ok_or_else(|| syn::Error::new_spanned(field, "endpoint fields must be named"))?;
        let ty = &field.ty;
        struct_fields.push(quote! {
            #(#attrs)*
            #field_vis #ident: #ty,
        });
    }

    let mut input_fields = Vec::new();
    let mut freshness = Vec::new();
    let mut output_fields = Vec::new();
    let mut output_methods = Vec::new();
    let mut constants = Vec::new();
    let mut aliases = Vec::new();
    let mut projection_table_fields = Vec::new();
    let mut projection_table_defaults = Vec::new();
    let mut projection_table_setters = Vec::new();
    let mut encode_statements = Vec::new();
    let mut binding_fields = Vec::new();
    let mut port_checks = Vec::new();
    let mut call_handles = Vec::new();
    let mut retention = Vec::new();
    let mut view_methods = Vec::new();
    let mut call_field_names: Vec<syn::Ident> = Vec::new();
    let mut dispatch_table_fields = Vec::new();
    let mut dispatch_table_defaults = Vec::new();
    let mut dispatch_table_setters = Vec::new();
    let mut dispatch_admissions = Vec::new();
    let mut dispatch_arms = Vec::new();
    let mut event_dispatch = Vec::new();
    let mut input_descriptors: Vec<(proc_macro2::Ident, syn::Path)> = Vec::new();
    let mut view_idents: Vec<Ident> = Vec::new();
    let mut operation_position = 0_usize;
    let mut harness_fields = Vec::new();
    let mut harness_methods = Vec::new();
    let mut harness_build = Vec::new();
    let mut harness_capture = Vec::new();
    let mut harness_validation = Vec::new();
    let mut harness_take_reply = Vec::new();
    let mut harness_staged_check = Vec::new();
    let mut first_call_field: Option<Ident> = None;
    let mut harness_clear = Vec::new();
    let mut harness_method_names: Vec<Ident> = Vec::new();

    for endpoint in &endpoints {
        match endpoint {
            Endpoint::Input {
                name,
                message,
                queued,
                lease_ms,
                max_age_ms,
                max_items,
                max_bytes,
            } => {
                let anchored_message = anchored(message, 1);
                let constant = format_ident!("{}", name.to_string().to_shouty_snake_case());
                let alias = format_ident!("{}Message", to_camel(name));
                aliases.push(quote! {
                    /// The payload type behind this endpoint.
                    pub type #alias = #anchored_message;
                });
                retention.push(
                    quote!(<#anchored_message as ::phoxal::schema::MessageSchema>::retain_schema()),
                );
                match (queued, lease_ms) {
                    (false, None) => {
                        let max_age = max_age_ms
                            .as_ref()
                            .map(|value| quote!(max_age_ms = #value,));
                        input_fields.push(quote! {
                            #[::phoxal::runtime::input(#max_age max_bytes = #max_bytes)]
                            pub #name: ::phoxal::runtime::input::Latest<#anchored_message>,
                        });
                        let max_age_argument = max_age_ms.as_ref().map_or_else(
                            || quote!(::std::option::Option::None),
                            |value| quote!(::std::option::Option::Some(#value)),
                        );
                        view_methods.push(quote! {
                            /// Reads this latest observation against its declared age bound.
                            ///
                            /// The returned read borrows the frozen input cut,
                            /// not this view: `ctx.#name().fresh()` may be bound
                            /// across other uses of the context in one handler.
                            pub fn #name(&self) -> ::phoxal::runtime::Observation<'a, #anchored_message> {
                                ::phoxal::runtime::Observation::new(
                                    &self.inputs.#name,
                                    self.step.now(),
                                    #max_age_argument,
                                )
                            }
                        });
                        let staged = format_ident!("{}_staged_latest", name);
                        let endpoint_str = name.to_string();
                        let inject = format_ident!("inject_{}", name);
                        harness_method_names.push(inject.clone());
                        harness_fields.push(quote! {
                            pub(crate) #staged: ::std::option::Option<
                                ::phoxal::runtime::Sample<#anchored_message>,
                            >,
                        });
                        harness_methods.push(quote! {
                            /// Injects one stamped latest observation for this
                            /// input. The injected value (with its original
                            /// stamp and source) enters the NEXT frozen cut
                            /// and every later cut until replaced: Latest
                            /// retention, never a capacity-one queue. A
                            /// second injection before the next cut replaces
                            /// the pending value whole.
                            pub fn #inject(
                                &mut self,
                                sample: ::phoxal::runtime::Sample<#anchored_message>,
                            ) -> ::std::result::Result<(), ::phoxal::runtime::HarnessError> {
                                let bytes = <#anchored_message as ::phoxal::contracts::ProstPayload>::
                                    encode_payload(sample.payload()).map_err(|_| {
                                        ::phoxal::runtime::HarnessError::PendingUnencodable {
                                            endpoint: #endpoint_str,
                                        }
                                    })?;
                                if bytes.len() as u64 > #max_bytes {
                                    return ::std::result::Result::Err(
                                        ::phoxal::runtime::HarnessError::PendingFull {
                                            endpoint: #endpoint_str,
                                        },
                                    );
                                }
                                self.#staged = ::std::option::Option::Some(sample);
                                ::std::result::Result::Ok(())
                            }
                        });
                        harness_build.push(quote! {
                            // Latest retention: the accepted observation is
                            // re-applied to every frozen cut until replaced,
                            // preserving its original stamp and source.
                            if let ::std::option::Option::Some(sample) = &view.#staged {
                                inputs.#name = ::phoxal::runtime::input::Latest::new(
                                    ::std::clone::Clone::clone(sample.payload()),
                                    ::std::clone::Clone::clone(sample.stamp()),
                                );
                            }
                        });
                        harness_clear.push(quote! {
                            view.#staged = ::std::option::Option::None;
                        });
                    }
                    (false, Some(lease)) => {
                        let max_age = max_age_ms
                            .as_ref()
                            .map(|value| quote!(max_age_ms = #value,));
                        input_fields.push(quote! {
                            #[::phoxal::runtime::input(port = #constant, #max_age max_bytes = #max_bytes)]
                            pub #name: ::phoxal::runtime::input::Setpoint<#anchored_message>,
                        });
                        view_methods.push(quote! {
                            /// Reads this leased intent against its lease validity.
                            ///
                            /// The returned read borrows the frozen input cut,
                            /// not this view: `ctx.#name().valid()` may be bound
                            /// across other uses of the context in one handler.
                            pub fn #name(&self) -> ::phoxal::runtime::Leased<'a, #anchored_message> {
                                ::phoxal::runtime::Leased::new(&self.inputs.#name, self.step.now())
                            }
                        });
                        let staged = format_ident!("{}_staged_lease", name);
                        let inject = format_ident!("inject_{}", name);
                        let endpoint_str = name.to_string();
                        harness_fields.push(quote! {
                            pub(crate) #staged: ::phoxal::runtime::input::Setpoint<#anchored_message>,
                        });
                        harness_method_names.push(inject.clone());
                        harness_methods.push(quote! {
                            /// Stages a leased input with its original authenticated source and
                            /// validity interval. Forwarding never renews the lease; withdrawal
                            /// replaces the retained intent explicitly.
                            pub fn #inject(&mut self, value: ::phoxal::runtime::input::Setpoint<#anchored_message>)
                                -> ::std::result::Result<(), ::phoxal::runtime::HarnessError>
                            {
                                if let ::std::option::Option::Some(payload) = value.value() {
                                    let duration = value.issued_at().zip(value.valid_until())
                                        .and_then(|(issued, until)| until.checked_duration_since(issued));
                                    if !duration.is_some_and(|duration| duration.as_nanos() > 0
                                        && duration.as_nanos() <= #lease * 1_000_000)
                                    {
                                        return ::std::result::Result::Err(::phoxal::runtime::HarnessError::InvalidLease { endpoint: #endpoint_str });
                                    }
                                    let bytes = <#anchored_message as ::phoxal::contracts::ProstPayload>::encode_payload(payload)
                                        .map_err(|_| ::phoxal::runtime::HarnessError::PendingUnencodable { endpoint: #endpoint_str })?;
                                    if bytes.len() as u64 > #max_bytes {
                                        return ::std::result::Result::Err(::phoxal::runtime::HarnessError::PendingFull { endpoint: #endpoint_str });
                                    }
                                }
                                self.#staged = value;
                                ::std::result::Result::Ok(())
                            }
                        });
                        harness_build.push(quote! { inputs.#name = view.#staged.clone(); });
                        harness_clear.push(quote! { view.#staged = ::phoxal::runtime::input::Setpoint::withdrawn(); });
                        let wire = wire_name(message);
                        constants.push(quote! {
                            pub const #constant: ::phoxal::contracts::CallMethod<#anchored_message, ::phoxal::contracts::Empty> =
                                ::phoxal::contracts::CallMethod::new(
                                    #wire,
                                    stringify!(#name),
                                    stringify!(#name),
                                    #wire,
                                    "google.protobuf.Empty",
                                    ::std::option::Option::Some(#lease),
                                    &[],
                                );
                        });
                    }
                    (true, _) => {
                        let max_items = max_items
                            .as_ref()
                            .map_or_else(|| quote!(1), |value| quote!(#value));
                        input_fields.push(quote! {
                            #[::phoxal::runtime::input(max_items = #max_items, max_bytes = #max_bytes)]
                            pub #name: ::phoxal::runtime::input::Samples<#anchored_message>,
                        });
                        view_methods.push(quote! {
                            /// Reads this queued batch of stamped samples as one
                            /// frozen cut; batch-level collection keeps its own
                            /// cross-sample capture-time semantics.
                            ///
                            /// The returned read borrows the frozen input cut,
                            /// not this view.
                            pub fn #name(
                                &self,
                            ) -> &'a ::phoxal::runtime::input::Samples<#anchored_message> {
                                &self.inputs.#name
                            }
                        });
                        let staged = format_ident!("{}_staged", name);
                        let endpoint_str = name.to_string();
                        input_descriptors.push((name.clone(), message.clone()));
                        let enqueue = format_ident!("enqueue_{}", name);
                        let inject = format_ident!("inject_{}", name);
                        let source_name = format!("harness.{endpoint_str}");
                        harness_fields.push(quote! {
                            pub(crate) #staged: ::phoxal::runtime::HarnessQueue<#anchored_message>,
                        });
                        harness_method_names.push(enqueue.clone());
                        harness_method_names.push(inject.clone());
                        harness_methods.push(quote! {
                            /// Stages a bounded item stamped at its next input cut.
                            pub fn #enqueue(&mut self, item: #anchored_message)
                                -> ::std::result::Result<(), ::phoxal::runtime::HarnessError>
                            {
                                self.#staged.push(item, None, #endpoint_str, #max_items, #max_bytes)
                            }
                            /// Stages a bounded captured sample with its original
                            /// source, capture time, and revision intact.
                            pub fn #inject(&mut self, sample: ::phoxal::runtime::Sample<#anchored_message>)
                                -> ::std::result::Result<(), ::phoxal::runtime::HarnessError>
                            {
                                let (item, stamp) = sample.into_parts();
                                self.#staged.push(item, Some(stamp), #endpoint_str, #max_items, #max_bytes)
                            }
                        });
                        harness_clear.push(quote! { view.#staged.clear(); });
                        harness_build.push(quote! {
                            inputs.#name = ::phoxal::runtime::input::Samples::new(
                                view.#staged.freeze(now, #source_name),
                            );
                        });
                        dispatch_table_defaults.push(quote! { #name: ::std::option::Option::None });
                        dispatch_table_fields.push(quote! {
                            /// The attached runtime's per-item handler for
                            /// queued input `#name`; unregistered means the
                            /// runtime reads the whole frozen batch from
                            /// its step instead.
                            pub #name: ::std::option::Option<
                                ::std::boxed::Box<
                                    dyn ::std::ops::Fn(
                                            &mut R,
                                            &mut ::phoxal::runtime::Context<'_, R>,
                                            #anchored_message,
                                        ) -> ::phoxal::Result<()>
                                        + ::std::marker::Send,
                                >,
                            >,
                        });
                        dispatch_table_setters.push(quote! {
                            /// Registers the attached runtime's per-item
                            /// handler for queued input `#name`.
                            pub fn #name<F>(&mut self, handler: F)
                            where
                                F: ::std::ops::Fn(
                                        &mut R,
                                        &mut ::phoxal::runtime::Context<'_, R>,
                                        #anchored_message,
                                    ) -> ::phoxal::Result<()>
                                    + ::std::marker::Send
                                    + 'static,
                            {
                                self.#name =
                                    ::std::option::Option::Some(::std::boxed::Box::new(handler));
                            }
                        });
                        event_dispatch.push(quote! {
                            // Copy this admitted batch into every active
                            // typed capture of the field BEFORE ordinary
                            // handlers drain it: a handler and an explicit
                            // capture may both observe the same event.
                            if resources.captures.has_active(#endpoint_str) {
                                for sample in inputs.#name.items() {
                                    if let ::std::result::Result::Ok(bytes) =
                                        <#anchored_message as ::phoxal::contracts::ProstPayload>::
                                            encode_payload(sample.payload())
                                    {
                                        resources.captures.copy_admitted(
                                            #endpoint_str,
                                            step.invocation_index(),
                                            bytes.as_slice(),
                                        );
                                    }
                                }
                            }
                            if let ::std::option::Option::Some(handler) = table.#name.as_ref() {
                                for sample in inputs.#name.items() {
                                    let mut context =
                                        resources.context::<R>(step, inputs, outputs)
                                            .with_dispatch_source(::std::option::Option::Some(
                                                sample.stamp().source(),
                                            ));
                                    handler(
                                        state,
                                        &mut context,
                                        ::std::clone::Clone::clone(sample.payload()),
                                    )?;
                                }
                            }
                        });
                    }
                }
                if let Some(max_age) = max_age_ms {
                    let fresh = format_ident!("{}_fresh", name);
                    freshness.push(quote! {
                        /// Reports whether this input satisfies its declared age bound at `now`.
                        pub fn #fresh(&self, now: ::phoxal::runtime::ExecutionTime) -> bool {
                            self.#name.is_fresh_at(now, ::std::option::Option::Some(#max_age))
                        }
                    });
                }
            }
            Endpoint::Output {
                name,
                message,
                queued,
                projection,
                forwarded,
                family,
                lease_ms,
                max_items,
                max_bytes,
                bootstrap,
                on_change,
            } => {
                let anchored_message = anchored(message, 1);
                let constant = format_ident!("{}", name.to_string().to_shouty_snake_case());
                let alias = format_ident!("{}Message", to_camel(name));
                aliases.push(quote! {
                    /// The payload type behind this endpoint.
                    pub type #alias = #anchored_message;
                });
                retention.push(
                    quote!(<#anchored_message as ::phoxal::schema::MessageSchema>::retain_schema()),
                );
                let lease = lease_ms.as_ref().map_or_else(
                    || quote!(::std::option::Option::None),
                    |value| quote!(::std::option::Option::Some(#value)),
                );
                let wire = wire_name(message);
                if *queued {
                    let max_items = max_items
                        .as_ref()
                        .map_or_else(|| quote!(1), |value| quote!(#value));
                    constants.push(quote! {
                        pub const #constant: ::phoxal::contracts::ObservationMethod<#anchored_message> =
                            ::phoxal::contracts::ObservationMethod::new(
                                #wire,
                                stringify!(#name),
                                stringify!(#name),
                                "google.protobuf.Empty",
                                #wire,
                                false,
                                #lease,
                                &[],
                            );
                    });
                    output_fields.push(quote! {
                        #[::phoxal::runtime::outputs::event(port = #constant, max_items = #max_items, max_bytes = #max_bytes)]
                        pub #name: ::std::vec::Vec<#anchored_message>,
                    });
                    output_methods.push(quote! {
                        /// Publishes one batch for this step.
                        pub fn #name(&mut self, values: ::std::vec::Vec<#anchored_message>) -> ::phoxal::Result<()> {
                            self.#name = values;
                            ::std::result::Result::Ok(())
                        }
                    });
                    let emit = format_ident!("emit_{}", name);
                    view_methods.push(quote! {
                        /// Stages one event for this output's accepted batch.
                        pub fn #emit(&mut self, value: #anchored_message) -> ::phoxal::Result<()> {
                            self.outputs.#name.push(value);
                            ::std::result::Result::Ok(())
                        }
                    });
                    view_idents.push(emit);
                    let retained = format_ident!("{}_retained", name);
                    let bytes_field = format_ident!("{}_bytes", name);
                    harness_fields.push(quote! {
                        pub(crate) #retained: ::std::vec::Vec<#anchored_message>,
                        pub(crate) #bytes_field: usize,
                    });
                    harness_method_names.push(name.clone());
                    harness_methods.push(quote! {
                        /// Drains the accepted events retained for this output.
                        /// Undrained retention is bounded by the endpoint's
                        /// declared item and byte bounds.
                        pub fn #name(&mut self) -> ::std::vec::Vec<#anchored_message> {
                            self.#bytes_field = 0;
                            ::std::mem::take(&mut self.#retained)
                        }
                    });
                    harness_clear.push(quote! {
                        view.#retained.clear();
                        view.#bytes_field = 0;
                    });
                    harness_validation.push(quote! {
                        let mut added_bytes = 0_usize;
                        for item in &outputs.#name {
                            added_bytes += <#anchored_message as ::phoxal::contracts::ProstPayload>::
                                encode_payload(item)?.len();
                        }
                        if view.#retained.len() as u64 + outputs.#name.len() as u64
                            > #max_items
                            || view.#bytes_field as u64 + added_bytes as u64 > #max_bytes
                        {
                            return ::std::result::Result::Err(::phoxal::anyhow!(
                                ::phoxal::runtime::HarnessError::RetainedFull
                            ));
                        }
                    });
                    harness_capture.push(quote! {
                        // Retention reserves the whole accepted batch before
                        // committing any of it: a batch that would exceed the
                        // endpoint's declared bounds contributes nothing, so a
                        // retention failure can never partially capture an
                        // accepted batch.
                        let mut added_bytes = 0_usize;
                        for item in &outputs.#name {
                            added_bytes += <#anchored_message as ::phoxal::contracts::ProstPayload>::
                                encode_payload(item)?.len();
                        }
                        if view.#retained.len() as u64 + outputs.#name.len() as u64
                            > #max_items
                            || view.#bytes_field as u64 + added_bytes as u64 > #max_bytes
                        {
                            return ::std::result::Result::Err(::phoxal::anyhow!(
                                ::phoxal::runtime::HarnessError::RetainedFull
                            ));
                        }
                        for item in &outputs.#name {
                            view.#retained.push(::std::clone::Clone::clone(item));
                        }
                        view.#bytes_field += added_bytes;
                    });
                } else if *projection {
                    constants.push(quote! {
                        pub const #constant: ::phoxal::contracts::ObservationMethod<#anchored_message> =
                            ::phoxal::contracts::ObservationMethod::new(
                                #wire,
                                stringify!(#name),
                                stringify!(#name),
                                "google.protobuf.Empty",
                                #wire,
                                true,
                                #lease,
                                &[],
                            );
                    });
                    let endpoint_str = name.to_string();
                    harness_method_names.push(name.clone());
                    if let Some(family) = family {
                        let suffix = &family.suffix;
                        harness_methods.push(quote! {
                            /// The latest accepted scalar publication for one logical member.
                            pub fn #name(&self, member: &str) -> ::std::option::Option<#anchored_message> {
                                self.state_records.get(&::std::format!("{}{}", member, #suffix))
                                    .and_then(|bytes| ::phoxal::runtime::transport::decode_prost(bytes).ok())
                            }
                        });
                    } else {
                        harness_methods.push(quote! {
                            /// The latest accepted publication for this retained state
                            /// output, including its bootstrap publication. The value
                            /// is decoded lazily from the accepted encoded record.
                            pub fn #name(&self) -> ::std::option::Option<#anchored_message> {
                                self.state_records
                                    .get(#endpoint_str)
                                    .and_then(|bytes| {
                                        ::phoxal::runtime::transport::decode_prost(bytes).ok()
                                    })
                            }
                        });
                    }
                    let (table_field, table_setter) = projection_table_entries(
                        name,
                        &anchored(message, 1),
                        lease_ms.is_some(),
                        family.is_some(),
                    );
                    projection_table_fields.push(table_field);
                    projection_table_setters.push(table_setter);
                    projection_table_defaults.push(quote! { #name: ::std::option::Option::None });
                    encode_statements.push(encode_statement(
                        name,
                        &constant,
                        lease_ms,
                        max_bytes,
                        *on_change,
                        family.as_ref(),
                    ));
                    binding_fields.push(binding_field_entry(
                        name,
                        &constant,
                        lease_ms,
                        max_bytes,
                        *bootstrap,
                        *on_change,
                        family.as_ref(),
                    ));
                    port_checks.push(port_check_entry(
                        name,
                        &constant,
                        &anchored(message, 1),
                        lease_ms,
                        family.is_some(),
                    ));
                } else {
                    constants.push(quote! {
                        pub const #constant: ::phoxal::contracts::ObservationMethod<#anchored_message> =
                            ::phoxal::contracts::ObservationMethod::new(
                                #wire,
                                stringify!(#name),
                                stringify!(#name),
                                "google.protobuf.Empty",
                                #wire,
                                true,
                                #lease,
                                &[],
                            );
                    });
                    let output_type = if *forwarded {
                        quote!(::phoxal::runtime::Sample<#anchored_message>)
                    } else {
                        quote!(#anchored_message)
                    };
                    let encoded_value = if *forwarded {
                        quote!(value.payload())
                    } else {
                        quote!(value)
                    };
                    if *forwarded {
                        let retained = format_ident!("{}_sample", name);
                        harness_fields.push(quote! { pub(crate) #retained: ::std::option::Option<::phoxal::runtime::Sample<#anchored_message>>, });
                        harness_method_names.push(retained.clone());
                        harness_methods.push(quote! {
                            /// Returns the accepted forwarded publication with its original provenance.
                            pub fn #retained(&self) -> ::std::option::Option<::phoxal::runtime::Sample<#anchored_message>> {
                                self.#retained.as_ref().map(|sample| ::phoxal::runtime::Sample::new(sample.payload().clone(), sample.stamp().clone()))
                            }
                        });
                        harness_capture.push(quote! {
                            if let Some(sample) = &outputs.#name {
                                view.#retained = Some(::phoxal::runtime::Sample::new(sample.payload().clone(), sample.stamp().clone()));
                            }
                        });
                        harness_clear.push(quote! { view.#retained = None; });
                    }

                    output_fields.push(quote! {
                        #[::phoxal::runtime::outputs::state(port = #constant, max_bytes = #max_bytes)]
                        pub #name: ::std::option::Option<#output_type>,
                    });
                    output_methods.push(quote! {
                        /// Publishes the retained observation for this step.
                        pub fn #name(&mut self, value: #output_type) -> ::phoxal::Result<()> {
                            self.#name = ::std::option::Option::Some(value);
                            ::std::result::Result::Ok(())
                        }
                    });
                    let publish = format_ident!("publish_{}", name);
                    view_methods.push(quote! {
                        /// Stages the retained observation for this step.
                        pub fn #publish(&mut self, value: #output_type) -> ::phoxal::Result<()> {
                            self.outputs.#name(value)
                        }
                    });
                    view_idents.push(publish);
                    let endpoint_str = name.to_string();
                    harness_method_names.push(name.clone());
                    harness_methods.push(quote! {
                        /// The latest accepted publication for this retained
                        /// latest output, decoded from its accepted encoded
                        /// record. An invocation that publishes no replacement
                        /// leaves the previously accepted value; a `Latest`
                        /// marker alone authorizes no initial publication.
                        pub fn #name(&self) -> ::std::option::Option<#anchored_message> {
                            self.state_records
                                .get(#endpoint_str)
                                .and_then(|bytes| {
                                    ::phoxal::runtime::transport::decode_prost(bytes).ok()
                                })
                        }
                    });
                    // Validate the staged replacement's encoding before the
                    // owner accepts the candidate: a rejected candidate
                    // exposes no new latest output.
                    harness_validation.push(quote! {
                        if let ::std::option::Option::Some(value) = &outputs.#name {
                            let bytes = <#anchored_message as ::phoxal::contracts::ProstPayload>::
                                encode_payload(#encoded_value)?;
                            if bytes.len() as u64 > #max_bytes {
                                return ::std::result::Result::Err(::phoxal::anyhow!(
                                    ::phoxal::runtime::HarnessError::RetainedFull
                                ));
                            }
                        }
                    });
                    // Capture the accepted replacement; absence retains the
                    // previous accepted record.
                    harness_capture.push(quote! {
                        if let ::std::option::Option::Some(value) = &outputs.#name {
                            let bytes = <#anchored_message as ::phoxal::contracts::ProstPayload>::
                                encode_payload(#encoded_value)?;
                            view.state_records.insert(#endpoint_str.to_owned(), bytes);
                        }
                    });
                }
            }
            Endpoint::Request {
                name,
                source,
                request,
                response,
                max_items,
                max_bytes,
                call,
            } => {
                let anchored_request = request.at(1);
                let anchored_response = response.at(1);
                let constant = format_ident!("{}", name.to_string().to_shouty_snake_case());
                let constant_definition = match source {
                    RequestSource::Descriptor { descriptor } => {
                        let anchored_descriptor = anchored(descriptor, 1);
                        retention.push(quote!(
                            <#anchored_request as ::phoxal::schema::MessageSchema>::retain_schema()
                        ));
                        retention.push(quote!(
                            <#anchored_response as ::phoxal::schema::MessageSchema>::retain_schema()
                        ));
                        quote! {
                            pub const #constant: ::phoxal::contracts::CallMethod<#anchored_request, #anchored_response> =
                                <#anchored_descriptor as ::phoxal::contracts::Operation>::METHOD;

                            const _: () = <#anchored_descriptor as ::phoxal::contracts::Operation>::CALL_SHAPE;
                        }
                    }
                    RequestSource::Authored {
                        contract,
                        request_identity,
                        response_identity,
                    } => {
                        // An explicit identity marks a foreign payload whose
                        // definition ships with the prepared provider closure.
                        let (PayloadRef::Message(request_path), PayloadRef::Message(response_path)) =
                            (request, response)
                        else {
                            unreachable!("authored requests carry message paths")
                        };
                        if request_identity.is_none() && !is_empty(request_path) {
                            retention.push(quote!(
                                <#anchored_request as ::phoxal::schema::MessageSchema>::retain_schema()
                            ));
                        }
                        if response_identity.is_none() && !is_empty(response_path) {
                            retention.push(quote!(
                                <#anchored_response as ::phoxal::schema::MessageSchema>::retain_schema()
                            ));
                        }
                        let request_wire = identity_or_wire(request_identity, request_path);
                        let response_wire = identity_or_wire(response_identity, response_path);
                        quote! {
                            pub const #constant: ::phoxal::contracts::CallMethod<#anchored_request, #anchored_response> =
                                ::phoxal::contracts::CallMethod::new(
                                    #contract,
                                    stringify!(#name),
                                    stringify!(#name),
                                    #request_wire,
                                    #response_wire,
                                    ::std::option::Option::None,
                                    &[],
                                );
                        }
                    }
                };
                constants.push(constant_definition);
                if *call {
                    input_fields.push(quote! {
                        #[::phoxal::runtime::input(port = #constant, max_items = #max_items, max_bytes = #max_bytes)]
                        pub #name: ::phoxal::runtime::input::Completions,
                    });
                    if first_call_field.is_none() {
                        first_call_field = Some(name.clone());
                    }
                    call_field_names.push(name.clone());
                    // The handles live in the nested `calls` module, one
                    // level deeper than the rest of the generated surface.
                    let handle_request = request.at(2);
                    let handle_response = response.at(2);
                    let field_str = name.to_string();
                    call_handles.push(quote! {
                        /// Stages one call on this composition-bound requirement,
                        /// carrying the field's own identity through routing; its
                        /// typed completion arrives in a later input cut.
                        #[must_use]
                        pub fn #name(request: #handle_request) -> ::phoxal::contracts::Call<#handle_request, #handle_response> {
                            super::#constant.bind(#field_str, request)
                        }
                    });
                    let field_str = name.to_string();
                    view_methods.push(quote! {
                        /// Stages one call on this composition-bound requirement; its
                        /// typed completion arrives in a later input cut, owned by
                        /// this field's direct completion handler.
                        pub fn #name(
                            &mut self,
                            request: #anchored_request,
                        ) -> ::phoxal::Result<
                            ::phoxal::runtime::CallTicket<#anchored_response>,
                        > {
                            self.outputs.send_direct(
                                self.step,
                                calls::#name(request),
                                #field_str,
                            )
                        }
                    });
                } else {
                    input_fields.push(quote! {
                        #[::phoxal::runtime::input(port = #constant, max_items = #max_items, max_bytes = #max_bytes)]
                        pub #name: ::phoxal::runtime::input::Commands<#anchored_request, #anchored_response>,
                    });
                    let replies = format_ident!("{}_replies", name);
                    let reply_method = format_ident!("{}_reply", name);
                    output_fields.push(quote! {
                        #[::phoxal::runtime::outputs::reply(#name, max_items = #max_items, max_bytes = #max_bytes)]
                        pub #replies: ::std::vec::Vec<::phoxal::runtime::Reply<#anchored_response>>,
                    });
                    let staged = format_ident!("{}_harness_commands", name);
                    let endpoint_str = name.to_string();
                    let enqueue = format_ident!("enqueue_{}", name);
                    let enqueue_from = format_ident!("enqueue_{}_from", name);
                    let inject = format_ident!("inject_{}", name);
                    harness_fields.push(quote! {
                        pub(crate) #staged: ::phoxal::runtime::HarnessCommands<#anchored_request, #anchored_response>,
                    });
                    harness_method_names.extend([
                        enqueue.clone(),
                        enqueue_from.clone(),
                        inject.clone(),
                    ]);
                    harness_methods.push(quote! {
                        /// Stages one request with the harness's direct caller identity.
                        pub fn #enqueue(&mut self, request: #anchored_request)
                            -> ::std::result::Result<::phoxal::runtime::HarnessCall<#anchored_response>, ::phoxal::runtime::HarnessError>
                        { self.#enqueue_from(request, "direct") }
                        /// Stages one bounded request with its caller identity. The returned
                        /// token consumes its accepted reply once and remains unique across resets.
                        pub fn #enqueue_from(&mut self, request: #anchored_request, source: impl Into<::std::string::String>)
                            -> ::std::result::Result<::phoxal::runtime::HarnessCall<#anchored_response>, ::phoxal::runtime::HarnessError>
                        {
                            let id = ::phoxal::runtime::CommandId::new(self.next_call);
                            self.#inject(::phoxal::runtime::Command::with_source_order(
                                ::phoxal::runtime::CommandOrder::new(0, 0, id), source, request,
                            ))
                        }
                        /// Stages a received command with its original merge order and caller
                        /// identity. Its harness reply token stays distinct from the wire order.
                        pub fn #inject(&mut self, command: ::phoxal::runtime::Command<#anchored_request, #anchored_response>)
                            -> ::std::result::Result<::phoxal::runtime::HarnessCall<#anchored_response>, ::phoxal::runtime::HarnessError>
                        {
                            self.#staged.push(command, &mut self.next_call, self.harness_id,
                                #endpoint_str, #max_items, #max_bytes)
                        }
                    });
                    harness_clear.push(quote! { view.#staged.clear(); });
                    harness_build.push(quote! { inputs.#name = view.#staged.freeze(); });
                    harness_capture.push(quote! { view.#staged.capture(&outputs.#replies)?; });
                    harness_validation.push(quote! { view.#staged.validate(&outputs.#replies, #max_items, #max_bytes)?; });
                    harness_take_reply.push(quote! {
                        if let Some(bytes) = view.#staged.take_reply(id) { return Some(bytes); }
                    });
                    harness_staged_check.push(quote! {
                        if view.#staged.is_staged(id) { return true; }
                    });
                    output_methods.push(quote! {
                        /// Replies to one accepted request.
                        pub fn #reply_method(&mut self, reply: ::phoxal::runtime::Reply<#anchored_response>) -> ::phoxal::Result<()> {
                            self.#replies.push(reply);
                            ::std::result::Result::Ok(())
                        }
                    });
                    let position = operation_position;
                    operation_position += 1;
                    dispatch_table_defaults.push(quote! { #name: ::std::option::Option::None });
                    dispatch_table_fields.push(quote! {
                        /// The attached runtime's handler for operation
                        /// `#name`, registered through the dispatch table.
                        pub #name: ::std::option::Option<
                            ::std::boxed::Box<
                                dyn ::std::ops::Fn(
                                        &mut R,
                                        &mut ::phoxal::runtime::Context<'_, R>,
                                        #anchored_request,
                                    ) -> ::phoxal::Result<#anchored_response>
                                    + ::std::marker::Send,
                            >,
                        >,
                    });
                    dispatch_table_setters.push(quote! {
                        /// Registers the attached runtime's handler for
                        /// operation `#name`.
                        pub fn #name<F>(&mut self, handler: F)
                        where
                            F: ::std::ops::Fn(
                                    &mut R,
                                    &mut ::phoxal::runtime::Context<'_, R>,
                                    #anchored_request,
                                ) -> ::phoxal::Result<#anchored_response>
                                + ::std::marker::Send
                                + 'static,
                        {
                            self.#name = ::std::option::Option::Some(::std::boxed::Box::new(handler));
                        }
                    });
                    let name_str = name.to_string();
                    dispatch_admissions.push(quote! {
                        admitted.extend(
                            inputs.#name.items().iter().enumerate().map(|(index, command)| {
                                (command.order(), #position, index)
                            }),
                        );
                        inputs
                            .#name
                            .validate_order()
                            .map_err(|error| ::phoxal::anyhow!(error))?;
                    });
                    dispatch_arms.push(quote! {
                        #position => {
                            let command = &inputs.#name.items()[index];
                            let handler = table
                                .#name
                                .as_ref()
                                .unwrap_or_else(|| {
                                    ::std::panic!(
                                        "operation `{}` has no registered handler; declare                                          #[handle({})] on the attached runtime",
                                        #name_str,
                                        #name_str,
                                    )
                                });
                            let mut context =
                                resources.context::<R>(step, inputs, outputs)
                                    .with_dispatch_source(::std::option::Option::Some(
                                        command.source(),
                                    ));
                            let response = handler(
                                state,
                                &mut context,
                                ::std::clone::Clone::clone(command.request()),
                            )?;
                            ::std::mem::drop(context);
                            outputs.#replies.push(command.reply(response));
                        }
                    });
                }
            }
        }
    }

    let has_calls = endpoints
        .iter()
        .any(|endpoint| matches!(endpoint, Endpoint::Request { call: true, .. }));
    let operations_member = if has_calls {
        quote! {
            /// Stages one generated call; its typed completion arrives in a later input cut.
            pub fn send<O>(&mut self, context: &::phoxal::runtime::StepContext, operation: O) -> ::phoxal::Result<::phoxal::runtime::outputs::CallTicket<O::Response>>
            where
                O: ::phoxal::runtime::outputs::GeneratedSend,
            {
                self.operations.send(context, operation)
            }

            /// Stages one generated call for the named field's direct
            /// completion handler, recording that field's exclusive
            /// ownership of the eventual completion.
            pub(crate) fn send_direct<O>(
                &mut self,
                context: &::phoxal::runtime::StepContext,
                operation: O,
                field: &'static str,
            ) -> ::phoxal::Result<::phoxal::runtime::outputs::CallTicket<O::Response>>
            where
                O: ::phoxal::runtime::outputs::GeneratedSend,
            {
                self.operations.send_direct(context, operation, field)
            }

            /// Stages one generated call owned by the submitting tree
            /// generation, qualified by this execution's epoch and
            /// recorded for promotion with the candidate.
            pub(crate) fn send_tree<O>(
                &mut self,
                context: &::phoxal::runtime::StepContext,
                generation: u64,
                operation: O,
            ) -> ::phoxal::Result<::phoxal::runtime::outputs::CallTicket<O::Response>>
            where
                O: ::phoxal::runtime::outputs::GeneratedSend,
            {
                self.operations.send_tree(context, generation, operation)
            }
        }
    } else {
        quote! {
            #[allow(dead_code, reason = "kept symmetric with calling services")]
            pub(crate) fn operations(&mut self) -> &mut ::phoxal::runtime::outputs::Outputs {
                &mut self.operations
            }
        }
    };

    // The ownership handoffs consumed by the runtime adapter after each
    // accepted candidate: one take clears the candidate's records.
    let outputs_owner_member = quote! {
        pub(crate) fn take_direct_owners(
            &mut self,
        ) -> ::std::vec::Vec<(u128, &'static str)> {
            self.operations.take_direct_owners()
        }

        pub(crate) fn take_tree_owners(&mut self) -> ::std::vec::Vec<(u128, u64)> {
            self.operations.take_tree_owners()
        }

        /// Withdraws one not-yet-accepted call submission from this
        /// candidate output transaction.
        pub(crate) fn withdraw(&mut self, ticket: u128) -> bool {
            self.operations.withdraw(ticket)
        }
    };

    // The view-level tree-owned staging method, present exactly when the
    // contract declares generated calls.
    let view_stage_member = if has_calls {
        quote! {
            /// Stages one generated call on this invocation's output
            /// transaction, owned by the submitting tree generation: the
            /// caller keeps the returned ticket and consumes the
            /// completion itself.
            pub fn stage<O>(
                &mut self,
                generation: u64,
                operation: O,
            ) -> ::phoxal::Result<::phoxal::runtime::CallTicket<O::Response>>
            where
                O: ::phoxal::runtime::outputs::GeneratedSend,
            {
                self.outputs.send_tree(self.step, generation, operation)
            }
        }
    } else {
        quote! {}
    };

    let inputs_impl = if freshness.is_empty() {
        quote!()
    } else {
        quote! {
            impl Inputs {
                #(#freshness)*
            }
        }
    };

    // Endpoint view methods share one namespace with the SDK context's own
    // methods; the context resolves its inherent methods first, so a
    // colliding endpoint name would be silently unreachable.
    for endpoint in &endpoints {
        match endpoint {
            Endpoint::Input { name, .. } => view_idents.push(name.clone()),
            Endpoint::Request {
                name, call: true, ..
            } => view_idents.push(name.clone()),
            _ => {}
        }
    }
    for ident in &view_idents {
        let collision = [
            "new",
            "now",
            "period",
            "elapsed",
            "missed_releases",
            "invocation_index",
        ]
        .iter()
        .any(|reserved| ident == reserved);
        if collision {
            return Err(syn::Error::new_spanned(
                ident,
                format!("endpoint `{ident}` collides with a context method; rename the endpoint"),
            ));
        }
    }
    for (index, ident) in view_idents.iter().enumerate() {
        if view_idents[..index].contains(ident) {
            return Err(syn::Error::new_spanned(
                ident,
                format!(
                    "endpoint `{ident}` and an earlier endpoint generate the same context \
                     method; rename one of them"
                ),
            ));
        }
    }

    let dispatch_loop = if dispatch_arms.is_empty() {
        quote!()
    } else {
        quote! {
            let mut admitted: ::std::vec::Vec<(
                ::phoxal::runtime::CommandOrder,
                usize,
                usize,
            )> = ::std::vec::Vec::new();
            #(#dispatch_admissions)*
            // Requests merge by their admitted command order across every
            // operation endpoint, with declaration order breaking exact ties;
            // the attached runtime sees one cross-endpoint accepted trace.
            admitted.sort_by(|left, right| (left.0, left.1).cmp(&(right.0, right.1)));
            for (_, position, index) in admitted {
                match position {
                    #(#dispatch_arms)*
                    _ => ::std::unreachable!("dispatch positions are generated"),
                }
            }
        }
    };

    // Generated harness methods must be usable through the harness value:
    // reject collisions with the SDK's own harness methods and duplicates
    // among the generated ones before emitting the view.
    let mut emitted_harness_methods = std::collections::BTreeSet::new();
    for ident in &harness_method_names {
        let method = ident.to_string();
        if matches!(method.as_str(), "reply" | "reset" | "advance_to" | "new") {
            return Err(syn::Error::new_spanned(
                ident,
                format!(
                    "harness method `{method}` collides with an SDK harness \
                     method; rename the endpoint"
                ),
            ));
        }
        if !emitted_harness_methods.insert(method.clone()) {
            return Err(syn::Error::new_spanned(
                ident,
                format!(
                    "duplicate generated harness method `{method}`; endpoint \
                     names collide (for example an output named \
                     `enqueue_{{op}}` beside an operation `{{op}}`); rename \
                     the endpoint"
                ),
            ));
        }
    }

    // The harness endpoint view is emitted for every contract: staged
    // pending inputs, retained accepted effects, and the endpoint-specific
    // enqueue/drain methods. Free functions move data between the view and
    // the contract's input/output transactions; the runtime attachment
    // supplies the state projections through the authored publish methods.
    let harness_view = quote! {
        /// The contract's harness endpoint view: staged pending inputs and
        /// retained accepted effects with the endpoint-specific methods.
        ///
        /// Generated plumbing reached through `Harness<R>`; it is never
        /// constructed by hand.
        #[derive(Default)]
        pub struct HarnessView {
            /// Monotonic call correlation source.
            pub(crate) next_call: u64,
            /// The owning harness's identity, stamped into every issued
            /// call correlation.
            pub(crate) harness_id: u64,
            /// Encoded state publications keyed by output field name.
            pub(crate) state_records: ::std::collections::BTreeMap<
                ::std::string::String,
                ::std::vec::Vec<u8>,
            >,
            #(#harness_fields)*
        }

        impl HarnessView {
            #(#harness_methods)*
        }

        // The harness entry points are inherent on the contract type, so
        // the runtime attachment resolves them through the contract rather
        // than a generated module path.
        impl #contract_name {
        /// Freezes the staged inputs into one invocation's input cut,
        /// stamping queued items at the release instant with harness
        /// provenance.
        pub(crate) fn harness_build_inputs(
            view: &mut HarnessView,
            now: ::phoxal::runtime::ExecutionTime,
        ) -> Inputs {
            let mut inputs = <Inputs as ::phoxal::runtime::input::InputSnapshot>::empty();
            #(#harness_build)*
            inputs
        }

        /// Retains one accepted invocation's transient outputs (replies and
        /// queued events) within the contract's declared bounds.
        pub(crate) fn harness_capture_transient(
            view: &mut HarnessView,
            outputs: &Outputs,
        ) -> ::phoxal::Result<()> {
            #(#harness_capture)*
            ::std::result::Result::Ok(())
        }


        /// Validates that one candidate's complete retained effect — every
        /// endpoint's events and correlated replies together with what is
        /// already retained — fits the declared bounds, without mutating
        /// anything. Called before the owner accepts the candidate.
        pub(crate) fn harness_validate_retention(
            view: &HarnessView,
            outputs: &Outputs,
        ) -> ::phoxal::Result<()> {
            #(#harness_validation)*
            ::std::result::Result::Ok(())
        }

        /// Reports whether one call correlation is still staged for a
        /// future release, so its reply is pending rather than consumed.
        pub(crate) fn harness_call_is_staged(
            view: &HarnessView,
            id: ::phoxal::runtime::input::CommandId,
        ) -> bool {
            #(#harness_staged_check)*
            false
        }

        /// Takes the encoded reply for one call correlation, if accepted.
        pub(crate) fn harness_take_reply_bytes(
            view: &mut HarnessView,
            id: ::phoxal::runtime::input::CommandId,
        ) -> ::std::option::Option<::std::vec::Vec<u8>> {
            #(#harness_take_reply)*
            ::std::option::Option::None
        }

        /// The exclusive upper bound of issued call correlations.
        pub(crate) fn harness_call_upper_bound(view: &HarnessView) -> u64 {
            view.next_call
        }

        /// Binds the view to its owning harness identity.
        pub(crate) fn harness_bind(view: &mut HarnessView, owner: u64) {
            view.harness_id = owner;
        }

        /// Clears every staged input and retained effect while keeping the
        /// call-correlation counter monotonic, so correlations enqueued
        /// before a reset can never alias correlations enqueued after it.
        pub(crate) fn harness_clear(view: &mut HarnessView) {
            #(#harness_clear)*
            view.state_records.clear();
        }
        }
    };

    // Generated completion-ownership helpers: one per calling contract,
    // routing raw tickets through the contract's completion inputs. The
    // runtime attachment's EndpointView override delegates here.
    let completion_owner = match &first_call_field {
        None => quote! {
            /// This contract declares no generated calls: nothing completes.
            #[allow(dead_code, reason = "the runtime attachment routes here")]
            pub fn phoxal_take_completion(
                _view: &mut Endpoints<'_>,
                _ticket: u128,
            ) -> ::std::option::Option<
                ::std::result::Result<::std::vec::Vec<u8>, ::phoxal::runtime::input::RequestError>,
            > {
                ::std::option::Option::None
            }

            /// This contract declares no generated calls.
            #[allow(dead_code, reason = "the runtime attachment routes here")]
            pub fn phoxal_retire_completion(_view: &mut Endpoints<'_>, _ticket: u128) {}
        },
        Some(_field) => {
            quote! {
                /// Takes one completion by ticket from the contract's
                /// single retained completion store, destructively.
                #[allow(dead_code, reason = "the runtime attachment routes here")]
                pub fn phoxal_take_completion(
                    view: &mut Endpoints<'_>,
                    ticket: u128,
                ) -> ::std::option::Option<
                    ::std::result::Result<::std::vec::Vec<u8>, ::phoxal::runtime::input::RequestError>,
                > {
                    view.inputs.take_completion_raw(ticket)
                }

                /// Retires one call's local completion eligibility: the
                /// retained copy is released and the not-yet-accepted
                /// submission is withdrawn from the candidate output
                /// transaction, so cancellation before acceptance produces
                /// no remote effect.
                #[allow(dead_code, reason = "the runtime attachment routes here")]
                pub fn phoxal_retire_completion(view: &mut Endpoints<'_>, ticket: u128) {
                    let _ = Self::phoxal_take_completion(view, ticket);
                    let _ = view.outputs.withdraw(ticket);
                }
            }
        }
    };

    // Generated tree-owned staging: one hook per calling contract, routing
    // the inert call through this invocation's output transaction without
    // recording direct-field ownership. The runtime attachment's
    // EndpointView override delegates here.
    let staging_owner = if has_calls {
        quote! {
            /// Stages one generated call owned by the given tree
            /// generation.
            #[allow(dead_code, reason = "the runtime attachment routes here")]
            pub fn phoxal_stage_operation<O>(
                view: &mut Endpoints<'_>,
                generation: u64,
                operation: O,
            ) -> ::phoxal::Result<::phoxal::runtime::CallTicket<O::Response>>
            where
                O: ::phoxal::runtime::outputs::GeneratedSend,
            {
                view.stage(generation, operation)
            }
        }
    } else {
        quote! {
            /// This contract stages no generated operations.
            #[allow(dead_code, reason = "the runtime attachment routes here")]
            pub fn phoxal_stage_operation<O>(
                _view: &mut Endpoints<'_>,
                _generation: u64,
                _operation: O,
            ) -> ::phoxal::Result<::phoxal::runtime::CallTicket<O::Response>>
            where
                O: ::phoxal::runtime::outputs::GeneratedSend,
            {
                Err(::phoxal::anyhow!(
                    "this runtime's contract stages no generated operations"
                ))
            }
        }
    };

    // An endpoint view and dispatch driver are emitted for every contract,
    // even one with no handled endpoints, so a runtime attachment can always
    // bind and dispatch through the same surface.
    let endpoints_view = quote! {
        /// The generated endpoint view over one runtime invocation: typed
        /// reads of this contract's inputs, inert staging of its calls, and
        /// staging of its non-projected outputs.
        ///
        /// Authored code reaches these methods on the SDK context through
        /// its `Deref`; the view itself is generated plumbing and is never
        /// constructed by hand.
        pub struct Endpoints<'a> {
            step: &'a ::phoxal::runtime::StepContext,
            inputs: &'a Inputs,
            outputs: &'a mut Outputs,
        }

        impl<'a> Endpoints<'a> {
            fn new(
                step: &'a ::phoxal::runtime::StepContext,
                inputs: &'a Inputs,
                outputs: &'a mut Outputs,
            ) -> Self {
                Self {
                    step,
                    inputs,
                    outputs,
                }
            }

            #(#view_methods)*

            #view_stage_member
        }

        /// The dispatch table one attached runtime registers its
        /// `#[handle]` entry points into. Generated plumbing reached
        /// through the contract type's dispatch function; it is never
        /// constructed by hand.
        pub struct DispatchTable<R: ::phoxal::runtime::EndpointView + ?Sized + 'static> {
            #(#dispatch_table_fields)*
            marker: ::std::marker::PhantomData<fn(&R)>,
        }

        impl<R: ::phoxal::runtime::EndpointView + ?Sized + 'static> ::std::default::Default
            for DispatchTable<R>
        {
            fn default() -> Self {
                Self {
                    #(#dispatch_table_defaults,)*
                    marker: ::std::marker::PhantomData,
                }
            }
        }

        impl<R: ::phoxal::runtime::EndpointView + ?Sized + 'static> DispatchTable<R> {
            #(#dispatch_table_setters)*
        }

        impl #contract_name {
            /// Dispatches one invocation's admitted requests and queued
            /// events to the runtime's registered handlers.
            ///
            /// Requests merge across operation endpoints by their admitted
            /// command order, with declaration order breaking exact ties, so
            /// the attached runtime preserves the accepted cross-endpoint
            /// trace. Queued events follow, endpoint by endpoint in
            /// declaration order; no ordering relation exists across those
            /// unrelated input classes.
            ///
            /// Resolution is compiler-directed through the contract type:
            /// any import spelling of the contract attaches identically,
            /// because the runtime registers its handlers into the table
            /// through the endpoint names it authored.
            pub fn phoxal_dispatch<R>(
                step: &::phoxal::runtime::StepContext,
                inputs: &Inputs,
                outputs: &mut Outputs,
                state: &mut R,
                resources: &::phoxal::runtime::ContextResources<'_>,
                attach: impl ::std::ops::FnOnce(&mut DispatchTable<R>),
            ) -> ::phoxal::Result<()>
            where
                R: ::phoxal::runtime::EndpointView<Inputs = Inputs, Outputs = Outputs>
                    + 'static,
            {
                let mut table = <DispatchTable<R> as Default>::default();
                attach(&mut table);
                #dispatch_loop
                #(#event_dispatch)*
                ::std::result::Result::Ok(())
            }
        }
    };

    // One typed descriptor per queued input endpoint, so behavior trees
    // can capture already-admitted events by their authored field name.
    let input_descriptors: Vec<_> = input_descriptors.iter().map(|(name, message)| {
        let constant = format_ident!("{}", name.to_string().to_shouty_snake_case());
        let anchored_message = anchored(message, 2);
        quote! {
            /// One typed queued-input descriptor for behavior captures.
            pub const #constant: ::phoxal::runtime::capture::InputDescriptor<#anchored_message> =
                ::phoxal::runtime::capture::InputDescriptor::new(stringify!(#name));
        }
    }).collect();
    let inputs_module = if input_descriptors.is_empty() {
        quote!()
    } else {
        quote! {
            /// Typed queued-input descriptors for behavior captures.
            pub mod inputs {
                #(#input_descriptors)*
            }
        }
    };

    let projection_table = if projection_table_fields.is_empty() {
        quote! {
            /// This contract declares no projected outputs; the encoding
            /// function's registration parameter still resolves through
            /// this empty table.
            #[derive(Default)]
            pub struct ProjectionTable<S: ?Sized, St: ?Sized> {
                marker: ::std::marker::PhantomData<fn(&S, &St)>,
            }
        }
    } else {
        quote! {
            /// The projection table one attached runtime registers its
            /// `#[publish]` entry points into. Generated plumbing reached
            /// through the contract type's encoding function; it is never
            /// constructed by hand.
            pub struct ProjectionTable<S: ?Sized, St: ?Sized> {
                #(#projection_table_fields)*
                marker: ::std::marker::PhantomData<fn(&S, &St)>,
            }

            impl<S: ?Sized, St: ?Sized> ::std::default::Default for ProjectionTable<S, St> {
                fn default() -> Self {
                    Self {
                        #(#projection_table_defaults,)*
                        marker: ::std::marker::PhantomData,
                    }
                }
            }

            impl<S: ?Sized, St: ?Sized> ProjectionTable<S, St> {
                #(#projection_table_setters)*
            }
        }
    };

    // Runtime attachment resolves entirely through the contract type: the
    // inherent `phoxal_encode_bindings` function carries the per-endpoint
    // projection encoding, so the runtime attribute needs no derived module
    // or macro path and any import spelling of the contract works.
    let attachment = if encode_statements.is_empty() {
        quote! {
            impl #contract_name {
                /// Encodes this contract's state projections for one attached
                /// runtime.  A contract without projected outputs binds an
                /// empty projection surface.
                #[allow(dead_code, reason = "runtime attachment invokes this through OutputBindings")]
                pub fn phoxal_encode_bindings<S, St>(
                    _service: &S,
                    _state: &St,
                    _context: ::phoxal::runtime::StepContext,
                    _resolve_input_port: &dyn ::std::ops::Fn(
                        &str,
                    ) -> ::std::option::Option<::phoxal::contracts::MethodSignature>,
                    _source: &str,
                    _attach: impl ::std::ops::FnOnce(&mut ProjectionTable<S, St>),
                ) -> ::phoxal::Result<
                    ::std::vec::Vec<::phoxal::runtime::transport::PreparedOutput>,
                > {
                    ::std::result::Result::Ok(::std::vec::Vec::new())
                }
            }
        }
    } else {
        quote! {
            impl #contract_name {
                /// Encodes this contract's state projections for one attached
                /// runtime.  `#[phoxal::runtime]` routes its generated
                /// `OutputBindings::encode_transport` here; the projection
                /// values come from the runtime's own `Projections` impl.
                #[allow(dead_code, reason = "runtime attachment invokes this through OutputBindings")]
                pub fn phoxal_encode_bindings<S, St>(
                    service: &S,
                    state: &St,
                    context: ::phoxal::runtime::StepContext,
                    _resolve_input_port: &dyn ::std::ops::Fn(
                        &str,
                    ) -> ::std::option::Option<::phoxal::contracts::MethodSignature>,
                    source: &str,
                    attach: impl ::std::ops::FnOnce(&mut ProjectionTable<S, St>),
                ) -> ::phoxal::Result<
                    ::std::vec::Vec<::phoxal::runtime::transport::PreparedOutput>,
                > {
                    let mut table = <ProjectionTable<S, St> as Default>::default();
                    attach(&mut table);
                    let mut records = ::std::vec::Vec::new();
                    let sequence = context.invocation_index();
                    #(#encode_statements)*
                    ::std::result::Result::Ok(records)
                }
            }

            const _: () = {
                #(#port_checks)*
            };
        }
    };

    let calls_module = if call_handles.is_empty() {
        quote!()
    } else {
        quote! {
            /// Composition-bound requirement handles declared by this contract.
            /// Each call stays inert until the output transaction accepts it;
            /// `robot.yaml` selects the provider instance and the typed
            /// completion arrives in a later input cut.
            pub mod calls {
                #(#call_handles)*
            }
        }
    };

    let public_methods = endpoints.iter().filter_map(|endpoint| {
        match endpoint {
            Endpoint::Output { name, message, .. } => {
                let constant = format_ident!("{}", name.to_string().to_shouty_snake_case());
                Some(quote! {
                    /// The typed method descriptor of this authored endpoint.
                    pub const #constant: ::phoxal::contracts::ObservationMethod<#message> = #module::#constant;
                })
            }
            Endpoint::Request { name, request, response, call, .. } => {
                let constant = format_ident!("{}", name.to_string().to_shouty_snake_case());
                let request = request.at(0);
                let response = response.at(0);
                let constructor = call.then(|| {
                    let field = name.to_string();
                    quote! {
                        /// Constructs a call bound to this declared local requirement.
                        pub fn #name(request: #request) -> ::phoxal::contracts::Call<#request, #response> {
                            Self::#constant.bind(#field, request)
                        }
                    }
                });
                Some(quote! {
                    /// The typed method descriptor of this authored endpoint.
                    pub const #constant: ::phoxal::contracts::CallMethod<#request, #response> = #module::#constant;
                    #constructor
                })
            }
            Endpoint::Input { .. } => None,
        }
    });
    let struct_attrs: Vec<_> = item
        .attrs
        .iter()
        .filter(|attr| !attr.path().is_ident("phoxal"))
        .cloned()
        .collect();

    let generated_inputs = crate::inputs::expand_inputs(
        TokenStream::new(),
        quote! {
            pub struct Inputs {
                #(#input_fields)*
            }
        },
    )?;
    let generated_outputs = crate::outputs::expand_outputs(
        TokenStream::new(),
        quote! {
            #[derive(Default)]
            pub struct Outputs {
                #(#output_fields)*
                operations: ::phoxal::runtime::outputs::Outputs,
            }
        },
    )?;

    // The declaration struct is a compile-time marker: its fields exist only
    // to declare the contract, so reading them is not an authored operation.
    let output = quote! {
        #(#struct_attrs)*
        #[allow(dead_code, reason = "declaration fields are compile-time contract input only")]
        #vis struct #contract_name {
            #(#struct_fields)*
        }

        #[allow(dead_code, reason = "authored contracts expose each typed method without implementation-module imports")]
        impl #contract_name {
            #(#public_methods)*
        }

        /// The generated local contract surface for
        /// [`#contract_name`]: endpoint method constants, message aliases,
        /// the runtime input and output transactions, composition-bound
        /// call handles, projection hooks, and the runtime attachment.
        #vis mod #module {
            use super::#contract_name;

            #(#constants)*

            #(#aliases)*

            #inputs_module

            #generated_inputs

            #inputs_impl

            #generated_outputs

            impl Outputs {
                #(#output_methods)*
                #operations_member

                #outputs_owner_member
            }

            #calls_module

            #projection_table

            #endpoints_view

            impl #contract_name {
                #completion_owner

                #staging_owner
            }

            #harness_view

            #attachment

            impl ::phoxal::runtime::RuntimeContract for #contract_name {
                type Inputs = Inputs;
                type Outputs = Outputs;
                type View<'a> = Endpoints<'a>;
                type HarnessView = HarnessView;

                const BINDINGS: &'static [::phoxal::runtime::outputs::OutputField] =
                    &[#(#binding_fields),*];

                fn endpoints_view<'a>(
                    step: &'a ::phoxal::runtime::StepContext,
                    inputs: &'a Self::Inputs,
                    outputs: &'a mut Self::Outputs,
                ) -> Self::View<'a> {
                    Endpoints::new(step, inputs, outputs)
                }

                fn retain_schemas() -> usize {
                    let mut size = 0_usize;
                    #(
                        size += #retention;
                    )*
                    size
                }
            }
        }
    };
    Ok(output)
}

/// The wire identity of a message path, through its schema trait.
///
/// A type whose final segment is `Empty` keeps the canonical
/// `google.protobuf.Empty` identity. An explicit identity names a type the
/// consuming crate does not own, such as a prepared participant's payload.
fn wire_name(message: &Path) -> TokenStream {
    if is_empty(message) {
        quote!("google.protobuf.Empty")
    } else {
        let anchored_message = anchored(message, 1);
        quote!(<#anchored_message as ::phoxal::schema::MessageSchema>::WIRE_NAME)
    }
}

fn identity_or_wire(identity: &Option<LitStr>, message: &Path) -> TokenStream {
    match identity {
        Some(literal) => quote!(#literal),
        None => wire_name(message),
    }
}

/// Snake-cases one identifier.
pub(crate) fn to_snake(ident: &Ident) -> String {
    let name = ident.to_string();
    let mut snake = String::with_capacity(name.len() + 4);
    let mut previous_lower = false;
    for character in name.chars() {
        if character.is_uppercase() {
            if previous_lower {
                snake.push('_');
            }
            snake.push(character.to_ascii_lowercase());
            previous_lower = false;
        } else {
            snake.push(character);
            previous_lower = character.is_lowercase() || character.is_ascii_digit();
        }
    }
    snake
}

fn is_empty(path: &Path) -> bool {
    path.segments
        .last()
        .is_some_and(|segment| segment.ident == "Empty")
}

/// The dispatch-table field and setter for one projected endpoint.
fn projection_table_entries(
    name: &Ident,
    message: &Path,
    leased: bool,
    family: bool,
) -> (TokenStream, TokenStream) {
    let returns = if family {
        quote!(::std::vec::Vec<(::std::string::String, ::std::option::Option<#message>)>)
    } else if leased {
        quote!(::std::option::Option<#message>)
    } else {
        quote!(#message)
    };
    (
        quote! {
            /// The attached runtime's projection for this endpoint.
            pub #name: ::std::option::Option<
                ::std::boxed::Box<dyn ::std::ops::Fn(&S, &St) -> #returns + ::std::marker::Send>,
            >,
        },
        quote! {
            /// Registers the attached runtime's projection for this
            /// endpoint.
            pub fn #name<F>(&mut self, project: F)
            where
                F: ::std::ops::Fn(&S, &St) -> #returns + ::std::marker::Send + 'static,
            {
                self.#name = ::std::option::Option::Some(::std::boxed::Box::new(project));
            }
        },
    )
}

/// One projection encoding statement inside `phoxal_encode_bindings`,
/// mirroring the transport encoding the outputs collector emits for a
/// state or setpoint projection method.
fn encode_statement(
    name: &Ident,
    constant: &Ident,
    lease_ms: &Option<LitInt>,
    max_bytes: &LitInt,
    on_change: bool,
    family: Option<&Family>,
) -> TokenStream {
    let field = name.to_string();
    if let Some(family) = family {
        let metadata = family.metadata();
        let lease = &family.lease_ms;
        return quote! {
            let values = table.#name.as_ref().ok_or_else(|| ::phoxal::anyhow!("output family projection is missing"))?(service, state);
            records.extend(::phoxal::runtime::transport::PreparedOutput::leased_family(
                #constant.signature(), #metadata, values, #max_bytes,
                ::phoxal::runtime::transport::setpoint_metadata(source, context, sequence, #lease), #field,
            )?);
        };
    }
    if let Some(lease) = lease_ms {
        quote! {
            {
                let signature = #constant.signature();
                let prepared = match table
                    .#name
                    .as_ref()
                    .map(|project| project(service, state))
                    .flatten()
                {
                    ::std::option::Option::Some(value) =>
                        ::phoxal::runtime::transport::PreparedOutput::response(
                            signature,
                            &value,
                            #max_bytes,
                            ::phoxal::runtime::transport::setpoint_metadata(
                                source,
                                context,
                                sequence,
                                #lease,
                            ),
                        )?,
                    ::std::option::Option::None =>
                        ::phoxal::runtime::transport::PreparedOutput::withdrawal(
                            signature,
                            ::phoxal::runtime::transport::setpoint_metadata(
                                source,
                                context,
                                sequence,
                                #lease,
                            ),
                        ),
                }.for_field(#field);
                records.push(prepared);
            }
        }
    } else {
        quote! {
            if let ::std::option::Option::Some(project) = table.#name.as_ref() {
                let signature = #constant.signature();
                let value = project(service, state);
                let prepared = ::phoxal::runtime::transport::PreparedOutput::response(
                    signature,
                    &value,
                    #max_bytes,
                    ::phoxal::runtime::transport::publication_metadata(
                        source,
                        context,
                        sequence,
                    ),
                )?.for_field(#field);
                let prepared = if #on_change {
                    prepared.with_change_token(&value, ::std::option::Option::None)
                } else {
                    prepared
                };
                records.push(prepared);
            }
        }
    }
}

/// One served projection's `OutputField` record for `RuntimeContract::BINDINGS`.
fn binding_field_entry(
    name: &Ident,
    constant: &Ident,
    lease_ms: &Option<LitInt>,
    max_bytes: &LitInt,
    bootstrap: bool,
    on_change: bool,
    family: Option<&Family>,
) -> TokenStream {
    let family = family.map_or_else(
        || quote!(::std::option::Option::None),
        |family| {
            let value = family.metadata();
            quote!(::std::option::Option::Some(#value))
        },
    );
    let (kind, port, valid_for_ms) = match lease_ms {
        Some(lease) => (
            quote!(Setpoint),
            quote!(#constant),
            quote!(::std::option::Option::Some(#lease)),
        ),
        None => (
            quote!(State),
            quote!(#constant),
            quote!(::std::option::Option::None),
        ),
    };
    quote! {
        ::phoxal::runtime::outputs::OutputField {
            family: #family,
            name: stringify!(#name),
            kind: ::phoxal::runtime::outputs::OutputKind::#kind,
            port: ::std::option::Option::Some(#port.signature().endpoint),
            port_signature: ::std::option::Option::Some(#port.signature()),
            input: ::std::option::Option::None,
            project: ::std::option::Option::None,
            max_items: ::std::option::Option::None,
            max_bytes: ::std::option::Option::Some(#max_bytes),
            max_request_bytes: ::std::option::Option::None,
            every_steps: ::std::option::Option::None,
            on_change: #on_change,
            bootstrap: #bootstrap,
            valid_for_ms: #valid_for_ms,
            timeout_ms: ::std::option::Option::None,
            cancel_grace_ms: ::std::option::Option::None,
        }
    }
}

/// One compile-time port/payload agreement check for a served projection.
fn port_check_entry(
    name: &Ident,
    constant: &Ident,
    message: &Path,
    lease_ms: &Option<LitInt>,
    family: bool,
) -> TokenStream {
    if family {
        return quote!();
    }
    if lease_ms.is_some() {
        let check_name = format_ident!("__phoxal_setpoint_port_{}", name);
        quote! {
            fn #check_name() {
                ::phoxal::runtime::macro_support::assert_leased_method::<_, ::std::option::Option<#message>>(
                    #constant,
                );
            }
        }
    } else {
        let check_name = format_ident!("__phoxal_state_port_{}", name);
        quote! {
            fn #check_name() {
                ::phoxal::runtime::macro_support::assert_retained_observation::<_, #message>(
                    #constant,
                );
            }
        }
    }
}

/// CamelCases one endpoint name for its message alias.
fn to_camel(name: &Ident) -> String {
    name.to_string()
        .split('_')
        .map(|part| {
            let mut characters = part.chars();
            match characters.next() {
                Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// Analyzes one endpoint field into its declared shape.
fn analyze_endpoint(field: &syn::Field, package: Option<&LitStr>) -> syn::Result<Endpoint> {
    let name = field
        .ident
        .clone()
        .ok_or_else(|| syn::Error::new_spanned(field, "endpoint fields must be named"))?;
    let roles: Vec<String> = field.attrs.iter().filter_map(endpoint_role).collect();
    if roles.len() > 1 {
        return Err(syn::Error::new_spanned(
            field,
            "an endpoint field declares exactly one role",
        ));
    }
    let Some(role) = roles.first() else {
        return Err(syn::Error::new_spanned(
            field,
            "every endpoint field declares its role, e.g. #[phoxal::input(max_bytes = 1024)]",
        ));
    };
    let options = parse_options(&field.attrs, role)?;
    if (options.family.is_some() || options.suffix.is_some() || options.max_ports.is_some())
        && (role != "output" || split_wrapper(&field.ty)?.0 != "Latest")
    {
        return Err(syn::Error::new_spanned(
            field,
            "output families apply only to leased Latest<T> projections",
        ));
    }
    // Small-message capacities default to one bounded batch; an endpoint
    // that needs more declares its explicit override.
    let max_bytes = options.max_bytes.unwrap_or_else(default_max_bytes);
    if options.stamped
        && (role != "output"
            || options.projection
            || options.lease_ms.is_some()
            || options.bootstrap
            || options.on_change)
    {
        return Err(syn::Error::new_spanned(
            field,
            "stamped applies only to a transiently replaced, unleased latest output",
        ));
    }
    match role.as_str() {
        "input" => {
            let (wrapper, inner) = split_wrapper(&field.ty)?;
            match wrapper.as_str() {
                "Latest" => {
                    if options.max_items.is_some() {
                        return Err(syn::Error::new_spanned(
                            field,
                            "latest inputs do not declare max_items",
                        ));
                    }
                    if options.projection || options.bootstrap || options.on_change {
                        return Err(syn::Error::new_spanned(
                            field,
                            "inputs do not declare projection, stamped, bootstrap, or on_change",
                        ));
                    }
                    if options.lease_ms.is_none() && options.max_age_ms.is_none() {
                        return Err(syn::Error::new_spanned(
                            field,
                            "an unleased latest input declares max_age_ms; a deliberately \
                             timeless observation needs its own named policy",
                        ));
                    }
                    Ok(Endpoint::Input {
                        name,
                        message: inner,
                        queued: false,
                        lease_ms: options.lease_ms,
                        max_age_ms: options.max_age_ms,
                        max_items: None,
                        max_bytes,
                    })
                }
                "Queue" => {
                    if options.lease_ms.is_some() || options.max_age_ms.is_some() {
                        return Err(syn::Error::new_spanned(
                            field,
                            "queued inputs do not declare lease_ms or max_age_ms",
                        ));
                    }
                    Ok(Endpoint::Input {
                        name,
                        message: inner,
                        queued: true,
                        lease_ms: None,
                        max_age_ms: None,
                        max_items: Some(options.max_items.unwrap_or_else(default_max_items)),
                        max_bytes,
                    })
                }
                other => Err(syn::Error::new_spanned(
                    &field.ty,
                    format!("inputs use Latest<T> or Queue<T>, not {other}<T>"),
                )),
            }
        }
        "output" => {
            let (wrapper, inner) = split_wrapper(&field.ty)?;
            let state = wrapper == "State";
            let queued = match wrapper.as_str() {
                "Latest" | "State" => false,
                "Queue" => true,
                other => {
                    return Err(syn::Error::new_spanned(
                        &field.ty,
                        format!("outputs use Latest<T>, Queue<T>, or State<T>, not {other}<T>"),
                    ));
                }
            };
            if state
                && (options.stamped
                    || options.projection
                    || options.bootstrap
                    || options.lease_ms.is_some()
                    || options.max_items.is_some())
            {
                return Err(syn::Error::new_spanned(
                    field,
                    "a State<T> output declares only max_bytes and on_change; initial \
                     publication and projection are part of the endpoint semantic",
                ));
            }
            if queued {
                if options.projection
                    || options.stamped
                    || options.bootstrap
                    || options.on_change
                    || options.lease_ms.is_some()
                {
                    return Err(syn::Error::new_spanned(
                        field,
                        "queued outputs declare only max_items and max_bytes",
                    ));
                }
                return Ok(Endpoint::Output {
                    name,
                    message: inner,
                    queued: true,
                    projection: false,
                    forwarded: false,
                    family: None,
                    lease_ms: None,
                    max_items: Some(options.max_items.unwrap_or_else(default_max_items)),
                    max_bytes,
                    bootstrap: false,
                    on_change: false,
                });
            }
            if options.max_items.is_some() {
                return Err(syn::Error::new_spanned(
                    field,
                    "latest outputs do not declare max_items",
                ));
            }
            if !options.projection && !state && options.lease_ms.is_some() {
                return Err(syn::Error::new_spanned(
                    field,
                    "only projected outputs carry lease_ms",
                ));
            }
            let family = if let Some(pointer) = options.family {
                if !options.projection
                    || options.lease_ms.is_none()
                    || options.stamped
                    || options.on_change
                    || options.bootstrap
                {
                    return Err(syn::Error::new_spanned(
                        field,
                        "an output family requires a leased state projection",
                    ));
                }
                let suffix = options.suffix.ok_or_else(|| {
                    syn::Error::new_spanned(field, "an output family requires suffix")
                })?;
                let max_ports = options.max_ports.ok_or_else(|| {
                    syn::Error::new_spanned(field, "an output family requires max_ports")
                })?;
                if !(1..=64).contains(&max_ports.base10_parse::<u64>()?)
                    || !pointer.value().starts_with('/')
                {
                    return Err(syn::Error::new_spanned(
                        field,
                        "output family requires a JSON pointer and at most 64 ports",
                    ));
                }
                Some(Family {
                    pointer,
                    suffix,
                    max_ports,
                    lease_ms: options.lease_ms.clone().ok_or_else(|| {
                        syn::Error::new_spanned(field, "an output family requires lease_ms")
                    })?,
                })
            } else {
                if options.suffix.is_some() || options.max_ports.is_some() {
                    return Err(syn::Error::new_spanned(
                        field,
                        "suffix and max_ports require family",
                    ));
                }
                None
            };
            Ok(Endpoint::Output {
                name,
                message: inner,
                queued: false,
                // A State<T> output is a projection whose initial
                // publication is part of the endpoint semantic.
                projection: options.projection || state,
                forwarded: options.stamped,
                family,
                lease_ms: options.lease_ms,
                max_items: None,
                max_bytes,
                bootstrap: options.bootstrap || state,
                on_change: options.on_change,
            })
        }
        "operation" | "call" => {
            if options.lease_ms.is_some()
                || options.max_age_ms.is_some()
                || options.projection
                || options.bootstrap
                || options.on_change
            {
                return Err(syn::Error::new_spanned(
                    field,
                    "operations and calls declare contract, request, response, max_items, and max_bytes",
                ));
            }
            let max_items = options.max_items.unwrap_or_else(default_max_items);
            // A call field may name a typed operation descriptor instead of
            // repeating the request/response pair and provider identity.
            if role == "call"
                && matches!(&field.ty, Type::Path(path)
                    if path.path.segments.last().is_some_and(|segment| segment.arguments.is_none()))
            {
                let descriptor = plain_path(&field.ty)?;
                return Ok(Endpoint::Request {
                    name,
                    source: RequestSource::Descriptor {
                        descriptor: descriptor.clone(),
                    },
                    request: PayloadRef::Descriptor {
                        descriptor,
                        response: false,
                    },
                    response: PayloadRef::Descriptor {
                        descriptor: plain_path(&field.ty)?,
                        response: true,
                    },
                    max_items,
                    max_bytes,
                    call: true,
                });
            }
            let (wrapper, request, response) = split_request_reply(&field.ty)?;
            if wrapper != "RequestReply" {
                return Err(syn::Error::new_spanned(
                    &field.ty,
                    "operations and calls use RequestReply<Request, Response>",
                ));
            }
            let contract = options.contract.or_else(|| {
                // A provided operation belongs to this package. A call requires
                // an explicit external identity, independent of its consumer.
                (role == "operation").then_some(package).flatten().map(|package| {
                    LitStr::new(
                        &format!("{}.{}", package.value(), name.to_string().to_upper_camel_case()),
                        name.span(),
                    )
                })
            }).ok_or_else(|| {
                syn::Error::new_spanned(
                    field,
                    "declare contract = \"pkg.v1.Name\", or give provided operations a package on #[phoxal::endpoints]",
                )
            })?;
            Ok(Endpoint::Request {
                name,
                source: RequestSource::Authored {
                    contract,
                    request_identity: options.request_identity,
                    response_identity: options.response_identity,
                },
                request: PayloadRef::Message(request),
                response: PayloadRef::Message(response),
                max_items,
                max_bytes,
                call: role == "call",
            })
        }
        other => Err(syn::Error::new_spanned(
            field,
            format!("unknown endpoint role `{other}`"),
        )),
    }
}

/// Returns the plain type path of a descriptor-typed call field.
fn plain_path(ty: &Type) -> syn::Result<Path> {
    let Type::Path(type_path) = ty else {
        return Err(syn::Error::new_spanned(
            ty,
            "a descriptor call field names one operation descriptor type",
        ));
    };
    Ok(type_path.path.clone())
}

/// Splits `Wrapper<T>` into its wrapper name and message path.
fn split_wrapper(ty: &Type) -> syn::Result<(String, Path)> {
    let Type::Path(type_path) = ty else {
        return Err(syn::Error::new_spanned(
            ty,
            "endpoint fields use the Latest, Queue, or RequestReply wrappers",
        ));
    };
    let segment =
        type_path.path.segments.last().ok_or_else(|| {
            syn::Error::new_spanned(ty, "endpoint field type has no final segment")
        })?;
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return Err(syn::Error::new_spanned(
            ty,
            "endpoint wrappers carry exactly one message type",
        ));
    };
    if arguments.args.len() != 1 {
        return Err(syn::Error::new_spanned(
            ty,
            "endpoint wrappers carry exactly one message type",
        ));
    }
    let Some(syn::GenericArgument::Type(Type::Path(inner))) = arguments.args.first() else {
        return Err(syn::Error::new_spanned(
            ty,
            "endpoint wrappers carry exactly one message type",
        ));
    };
    if inner
        .path
        .segments
        .iter()
        .any(|segment| !segment.arguments.is_none())
    {
        return Err(syn::Error::new_spanned(
            ty,
            "endpoint payload types are plain message paths",
        ));
    }
    Ok((segment.ident.to_string(), inner.path.clone()))
}

/// Splits `RequestReply<A, B>` into its wrapper name and both paths.
fn split_request_reply(ty: &Type) -> syn::Result<(String, Path, Path)> {
    let Type::Path(type_path) = ty else {
        return Err(syn::Error::new_spanned(
            ty,
            "endpoint fields use the Latest, Queue, or RequestReply wrappers",
        ));
    };
    let segment =
        type_path.path.segments.last().ok_or_else(|| {
            syn::Error::new_spanned(ty, "endpoint field type has no final segment")
        })?;
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return Err(syn::Error::new_spanned(
            ty,
            "RequestReply carries a request and a response type",
        ));
    };
    if arguments.args.len() != 2 {
        return Err(syn::Error::new_spanned(
            ty,
            "RequestReply carries a request and a response type",
        ));
    }
    let paths = arguments
        .args
        .iter()
        .map(|argument| match argument {
            syn::GenericArgument::Type(Type::Path(inner)) => Ok(inner.path.clone()),
            _ => Err(syn::Error::new_spanned(
                argument,
                "RequestReply type arguments are message paths",
            )),
        })
        .collect::<syn::Result<Vec<_>>>()?;
    Ok((
        segment.ident.to_string(),
        paths[0].clone(),
        paths[1].clone(),
    ))
}
