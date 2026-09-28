//! Expansion for the Rust-authored endpoint declaration.
//!
//! `#[phoxal::endpoints]` turns one declaration struct into the complete local
//! contract surface: endpoint method constants, message aliases, the
//! runtime `Inputs` and `Outputs` transactions, composition-bound call
//! handles, projection hooks, and the runtime attachment macro. Nothing
//! here constructs a value or transmits anything; accepted invocations
//! remain the only effect boundary.

use heck::{ToShoutySnakeCase, ToUpperCamelCase};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::parse::Parser;
use syn::{Fields, Ident, ItemStruct, LitInt, LitStr, Path, Type};

use crate::message::anchored;

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
        lease_ms: Option<LitInt>,
        max_items: Option<LitInt>,
        max_bytes: LitInt,
        bootstrap: bool,
        on_change: bool,
    },
    Request {
        name: Ident,
        contract: LitStr,
        request: Path,
        response: Path,
        request_identity: Option<LitStr>,
        response_identity: Option<LitStr>,
        max_items: LitInt,
        max_bytes: LitInt,
        call: bool,
    },
}

/// Attribute values collected from one endpoint field.
#[derive(Default)]
struct FieldOptions {
    lease_ms: Option<LitInt>,
    max_age_ms: Option<LitInt>,
    max_items: Option<LitInt>,
    max_bytes: Option<LitInt>,
    contract: Option<LitStr>,
    request_identity: Option<LitStr>,
    response_identity: Option<LitStr>,
    projection: bool,
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
        attr.parse_nested_meta(|meta| {
            let integer = |meta: &syn::meta::ParseNestedMeta<'_>| -> syn::Result<LitInt> {
                let literal: LitInt = meta.value()?.parse()?;
                if literal.base10_parse::<u64>()? == 0 {
                    return Err(meta.error("bounds must be positive"));
                }
                Ok(literal)
            };
            if meta.path.is_ident("lease_ms") {
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
            } else if meta.path.is_ident("bootstrap") {
                options.bootstrap = true;
            } else if meta.path.is_ident("on_change") {
                options.on_change = true;
            } else {
                return Err(meta.error(format!(
                    "unsupported {role} option; expected lease_ms, max_age_ms, max_items, max_bytes, contract, projection, bootstrap, or on_change"
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
    let Some(out_dir) = std::env::var_os("OUT_DIR") else {
        return Ok(None);
    };
    let path = std::path::Path::new(&out_dir).join("phoxal-standard-endpoints.rs");
    let Ok(fragment) = std::fs::read_to_string(path) else {
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
    let mut projection_traits = Vec::new();
    let mut encode_statements = Vec::new();
    let mut binding_fields = Vec::new();
    let mut port_checks = Vec::new();
    let mut call_handles = Vec::new();
    let mut retention = Vec::new();

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
                    }
                    (false, Some(lease)) => {
                        let max_age = max_age_ms
                            .as_ref()
                            .map(|value| quote!(max_age_ms = #value,));
                        input_fields.push(quote! {
                            #[::phoxal::runtime::input(port = #constant.setpoint_port(), #max_age max_bytes = #max_bytes)]
                            pub #name: ::phoxal::runtime::input::Setpoint<#anchored_message>,
                        });
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
                        #[::phoxal::runtime::outputs::event(port = #constant.event_port(), max_items = #max_items, max_bytes = #max_bytes)]
                        pub #name: ::std::vec::Vec<#anchored_message>,
                    });
                    output_methods.push(quote! {
                        /// Publishes one batch for this step.
                        pub fn #name(&mut self, values: ::std::vec::Vec<#anchored_message>) -> ::phoxal::Result<()> {
                            self.#name = values;
                            ::std::result::Result::Ok(())
                        }
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
                    projection_traits.push(projection_trait_entry(
                        name,
                        &anchored(message, 2),
                        lease_ms.is_some(),
                    ));
                    encode_statements.push(encode_statement(
                        name, &constant, lease_ms, max_bytes, *on_change,
                    ));
                    binding_fields.push(binding_field_entry(
                        name, &constant, lease_ms, max_bytes, *bootstrap, *on_change,
                    ));
                    port_checks.push(port_check_entry(
                        name,
                        &constant,
                        &anchored(message, 1),
                        lease_ms,
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
                    output_fields.push(quote! {
                        #[::phoxal::runtime::outputs::state(port = #constant.state_port(), max_bytes = #max_bytes)]
                        pub #name: ::std::option::Option<#anchored_message>,
                    });
                    output_methods.push(quote! {
                        /// Publishes the retained observation for this step.
                        pub fn #name(&mut self, value: #anchored_message) -> ::phoxal::Result<()> {
                            self.#name = ::std::option::Option::Some(value);
                            ::std::result::Result::Ok(())
                        }
                    });
                }
            }
            Endpoint::Request {
                name,
                contract,
                request,
                response,
                request_identity,
                response_identity,
                max_items,
                max_bytes,
                call,
            } => {
                let anchored_request = anchored(request, 1);
                let anchored_response = anchored(response, 1);
                let constant = format_ident!("{}", name.to_string().to_shouty_snake_case());
                // An explicit identity marks a foreign payload whose
                // definition ships with the prepared provider closure.
                if request_identity.is_none() && !is_empty(request) {
                    retention.push(quote!(
                        <#anchored_request as ::phoxal::schema::MessageSchema>::retain_schema()
                    ));
                }
                if response_identity.is_none() && !is_empty(response) {
                    retention.push(quote!(
                        <#anchored_response as ::phoxal::schema::MessageSchema>::retain_schema()
                    ));
                }
                let request_wire = identity_or_wire(request_identity, request);
                let response_wire = identity_or_wire(response_identity, response);
                constants.push(quote! {
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
                });
                if *call {
                    input_fields.push(quote! {
                        #[::phoxal::runtime::input(port = #constant.commands_port(), max_items = #max_items, max_bytes = #max_bytes)]
                        pub #name: ::phoxal::runtime::input::Completions,
                    });
                    // The handles live in the nested `calls` module, one
                    // level deeper than the rest of the generated surface.
                    let handle_request = anchored(request, 2);
                    let handle_response = anchored(response, 2);
                    call_handles.push(quote! {
                        /// Stages one call on this composition-bound requirement; its typed
                        /// completion arrives in a later input cut.
                        #[must_use]
                        pub fn #name(request: #handle_request) -> ::phoxal::contracts::Call<#handle_request, #handle_response> {
                            super::#constant.bind("", request)
                        }
                    });
                } else {
                    input_fields.push(quote! {
                        #[::phoxal::runtime::input(port = #constant.commands_port(), max_items = #max_items, max_bytes = #max_bytes)]
                        pub #name: ::phoxal::runtime::input::Commands<#anchored_request, #anchored_response>,
                    });
                    let replies = format_ident!("{}_replies", name);
                    let reply_method = format_ident!("{}_reply", name);
                    output_fields.push(quote! {
                        #[::phoxal::runtime::outputs::reply(#name, max_items = #max_items, max_bytes = #max_bytes)]
                        pub #replies: ::std::vec::Vec<::phoxal::runtime::Reply<#anchored_response>>,
                    });
                    output_methods.push(quote! {
                        /// Replies to one accepted request.
                        pub fn #reply_method(&mut self, reply: ::phoxal::runtime::Reply<#anchored_response>) -> ::phoxal::Result<()> {
                            self.#replies.push(reply);
                            ::std::result::Result::Ok(())
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
        }
    } else {
        quote! {
            #[allow(dead_code, reason = "kept symmetric with calling services")]
            pub(crate) fn operations(&mut self) -> &mut ::phoxal::runtime::outputs::Outputs {
                &mut self.operations
            }
        }
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

    let projections_module = if projection_traits.is_empty() {
        quote!()
    } else {
        quote! {
            /// State-projection hooks owned by the service implementation.
            /// The contract owns registration, bounds, and invocation timing;
            /// each hook owns payload construction from private state.
            pub mod projections {
                /// One projection hook per projected output.
                pub trait Projections {
                    /// The runtime state these hooks project from.
                    type State;
                    #(#projection_traits)*
                }
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
                    ) -> ::std::option::Option<::phoxal::macro_support::PortSignature>,
                    _source: &str,
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
                    ) -> ::std::option::Option<::phoxal::macro_support::PortSignature>,
                    source: &str,
                ) -> ::phoxal::Result<
                    ::std::vec::Vec<::phoxal::runtime::transport::PreparedOutput>,
                >
                where
                    S: projections::Projections<State = St>,
                {
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

    let struct_attrs: Vec<_> = item
        .attrs
        .iter()
        .filter(|attr| !attr.path().is_ident("phoxal"))
        .cloned()
        .collect();

    // The declaration struct is a compile-time marker: its fields exist only
    // to declare the contract, so reading them is not an authored operation.
    let output = quote! {
        #(#struct_attrs)*
        #[allow(dead_code, reason = "declaration fields are compile-time contract input only")]
        #vis struct #contract_name {
            #(#struct_fields)*
        }

        /// The generated local contract surface for
        /// [`#contract_name`]: endpoint method constants, message aliases,
        /// the runtime input and output transactions, composition-bound
        /// call handles, projection hooks, and the runtime attachment.
        #vis mod #module {
            use super::#contract_name;

            #(#constants)*

            #(#aliases)*

            #[::phoxal::runtime::inputs]
            pub struct Inputs {
                #(#input_fields)*
            }

            #inputs_impl

            #[derive(Default)]
            #[::phoxal::runtime::outputs]
            pub struct Outputs {
                #(#output_fields)*
                operations: ::phoxal::runtime::outputs::Outputs,
            }

            impl Outputs {
                #(#output_methods)*
                #operations_member
            }

            #calls_module

            #projections_module

            #attachment

            impl ::phoxal::runtime::RuntimeContract for #contract_name {
                type Inputs = Inputs;
                type Outputs = Outputs;

                const BINDINGS: &'static [::phoxal::runtime::outputs::OutputField] =
                    &[#(#binding_fields),*];

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
fn to_snake(ident: &Ident) -> String {
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

/// One projection hook declaration.
fn projection_trait_entry(name: &Ident, message: &Path, leased: bool) -> TokenStream {
    let returns = if leased {
        quote!(::std::option::Option<#message>)
    } else {
        quote!(#message)
    };
    quote! {
        /// Projects one output from runtime state.
        fn #name(&self, state: &Self::State) -> #returns;
    }
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
) -> TokenStream {
    let field = name.to_string();
    if let Some(lease) = lease_ms {
        quote! {
            {
                let signature = #constant.setpoint_port().signature();
                let prepared = match <S as projections::Projections>::#name(service, state) {
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
            {
                let signature = #constant.state_port().signature();
                let value = <S as projections::Projections>::#name(service, state);
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
) -> TokenStream {
    let (kind, port, valid_for_ms) = match lease_ms {
        Some(lease) => (
            quote!(Setpoint),
            quote!(#constant.setpoint_port()),
            quote!(::std::option::Option::Some(#lease)),
        ),
        None => (
            quote!(State),
            quote!(#constant.state_port()),
            quote!(::std::option::Option::None),
        ),
    };
    quote! {
        ::phoxal::runtime::outputs::OutputField {
            name: stringify!(#name),
            kind: ::phoxal::runtime::outputs::OutputKind::#kind,
            port: ::std::option::Option::Some(#port.name()),
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
) -> TokenStream {
    if lease_ms.is_some() {
        let check_name = format_ident!("__phoxal_setpoint_port_{}", name);
        quote! {
            fn #check_name() {
                ::phoxal::runtime::macro_support::assert_setpoint_port::<_, ::std::option::Option<#message>>(
                    #constant.setpoint_port(),
                );
            }
        }
    } else {
        let check_name = format_ident!("__phoxal_state_port_{}", name);
        quote! {
            fn #check_name() {
                ::phoxal::runtime::macro_support::assert_state_port::<_, #message>(
                    #constant.state_port(),
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
    let max_bytes = options
        .max_bytes
        .ok_or_else(|| syn::Error::new_spanned(field, "every endpoint declares max_bytes"))?;
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
                            "inputs do not declare projection, bootstrap, or on_change",
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
                    let max_items = options.max_items.ok_or_else(|| {
                        syn::Error::new_spanned(field, "queued inputs declare max_items")
                    })?;
                    Ok(Endpoint::Input {
                        name,
                        message: inner,
                        queued: true,
                        lease_ms: None,
                        max_age_ms: None,
                        max_items: Some(max_items),
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
            let queued = match wrapper.as_str() {
                "Latest" => false,
                "Queue" => true,
                other => {
                    return Err(syn::Error::new_spanned(
                        &field.ty,
                        format!("outputs use Latest<T> or Queue<T>, not {other}<T>"),
                    ));
                }
            };
            if queued {
                if options.projection
                    || options.bootstrap
                    || options.on_change
                    || options.lease_ms.is_some()
                {
                    return Err(syn::Error::new_spanned(
                        field,
                        "queued outputs declare only max_items and max_bytes",
                    ));
                }
                let max_items = options.max_items.ok_or_else(|| {
                    syn::Error::new_spanned(field, "queued outputs declare max_items")
                })?;
                return Ok(Endpoint::Output {
                    name,
                    message: inner,
                    queued: true,
                    projection: false,
                    lease_ms: None,
                    max_items: Some(max_items),
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
            if !options.projection && options.lease_ms.is_some() {
                return Err(syn::Error::new_spanned(
                    field,
                    "only projected outputs carry lease_ms",
                ));
            }
            Ok(Endpoint::Output {
                name,
                message: inner,
                queued: false,
                projection: options.projection,
                lease_ms: options.lease_ms,
                max_items: None,
                max_bytes,
                bootstrap: options.bootstrap,
                on_change: options.on_change,
            })
        }
        "operation" | "call" => {
            let (wrapper, request, response) = split_request_reply(&field.ty)?;
            if wrapper != "RequestReply" {
                return Err(syn::Error::new_spanned(
                    &field.ty,
                    "operations and calls use RequestReply<Request, Response>",
                ));
            }
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
            let max_items = options.max_items.ok_or_else(|| {
                syn::Error::new_spanned(field, "operations and calls declare max_items")
            })?;
            Ok(Endpoint::Request {
                name,
                contract,
                request,
                response,
                request_identity: options.request_identity,
                response_identity: options.response_identity,
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
