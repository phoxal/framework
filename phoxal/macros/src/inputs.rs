//! Expansion for the `runtime::inputs` collector.

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::{Expr, ExprLit, Field, Fields, GenericArgument, Ident, ItemStruct, Lit, Type};

#[derive(Default)]
struct Options {
    max_age_ms: Option<u64>,
    max_items: Option<u64>,
    max_bytes: Option<u64>,
    port: Option<Expr>,
}

/// Expands `#[phoxal::runtime::inputs]` and consumes nested input markers.
pub fn expand_inputs(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    if !attr.is_empty() {
        return Err(syn::Error::new(
            Span::call_site(),
            "#[phoxal::runtime::inputs] takes no arguments",
        ));
    }
    let mut input: ItemStruct = syn::parse2(item)?;
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "#[phoxal::runtime::inputs] does not support generic input structs",
        ));
    }
    let name = input.ident.clone();
    let fields = match &mut input.fields {
        Fields::Named(fields) => &mut fields.named,
        _ => {
            return Err(syn::Error::new_spanned(
                &input,
                "#[phoxal::runtime::inputs] requires a struct with named fields",
            ));
        }
    };

    let mut metadata = Vec::new();
    let mut checks = Vec::new();
    let mut bindings = Vec::new();
    let mut generated_transport_fields = Vec::new();
    let mut generated_transport_decoders = Vec::new();
    let mut generated_transport_encoders = Vec::new();
    let mut generated_transport_bounds = Vec::new();
    let marker_name = format_ident!("__phoxal_transport_{}", name);
    let binding_fn = format_ident!("__phoxal_require_binding_{}", name);
    let mut transport_params = Vec::new();
    let mut transport_types = Vec::new();
    let mut sink_latest = Vec::new();
    let mut sink_clear_latest = Vec::new();
    let mut expire_latest = Vec::new();
    let mut sink_samples = Vec::new();
    let mut sink_events = Vec::new();
    let mut sink_setpoints = Vec::new();
    let mut sink_streams = Vec::new();
    let mut sink_commands = Vec::new();
    let mut sink_reads = Vec::new();
    let mut sink_requests = Vec::new();
    let mut sink_operations = Vec::new();
    let mut completion_field = None;
    let mut restore_managed = Vec::new();
    let mut select_managed = Vec::new();
    let mut retire_managed = Vec::new();
    let mut take_managed = Vec::new();
    let mut field_names = Vec::new();
    for field in fields.iter_mut() {
        let Some(field_name) = field.ident.clone() else {
            return Err(syn::Error::new_spanned(
                field,
                "runtime input fields need names",
            ));
        };
        let ty = field.ty.clone();
        field_names.push(field_name.clone());
        let kind = input_kind(&ty)?;
        let mut options = Options::default();
        let mut retained = Vec::new();
        for attribute in field.attrs.drain(..) {
            if attribute
                .path()
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "input")
            {
                parse_options(&attribute, kind, &mut options)?;
            } else {
                retained.push(attribute);
            }
        }
        field.attrs = retained;
        validate_options(kind, &options, field)?;
        let kind_variant = Ident::new(kind.variant(), field_name.span());
        let max_age = option_tokens(options.max_age_ms);
        let max_items = option_tokens(options.max_items);
        let max_bytes = option_tokens(options.max_bytes);
        let port = options
            .port
            .as_ref()
            .map_or_else(|| quote!(None), |port| quote!(Some((#port).name())));
        let port_signature = options
            .port
            .as_ref()
            .map_or_else(|| quote!(None), |port| quote!(Some((#port).signature())));
        let message_type =
            |ty: &Type| quote!(Some(::phoxal::runtime::input::MessageType::of::<#ty>()));
        let (request_type, response_type) = match kind {
            InputKind::Operation | InputKind::Completions => (quote!(None), quote!(None)),
            InputKind::Read | InputKind::Request => {
                let (_, request, response) = generic_types3(&ty, field)?;
                (message_type(&request), message_type(&response))
            }
            InputKind::Commands => {
                let (request, response) = generic_types2(&ty, field)?;
                (message_type(&request), message_type(&response))
            }
            InputKind::Setpoint if options.port.is_some() => {
                let request = generic_type(&ty, 1, field)?;
                (
                    message_type(&request),
                    message_type(&syn::parse_quote!(::phoxal::contract::Empty)),
                )
            }
            _ => (quote!(None), message_type(&generic_type(&ty, 1, field)?)),
        };
        metadata.push(quote! {
            ::phoxal::runtime::input::InputField {
                name: stringify!(#field_name),
                kind: ::phoxal::runtime::input::InputKind::#kind_variant,
                max_age_ms: #max_age,
                max_items: #max_items,
                max_bytes: #max_bytes,
                port: #port,
                port_signature: #port_signature,
                request_type: #request_type,
                response_type: #response_type,
            }
        });
        checks.push(input_type_check(&ty, &field_name));
        let field_id = field_id(&field_name);
        bindings.push(quote! {
            impl ::phoxal::runtime::input::InputFieldBinding<{#field_id}> for #name {
                type Form = #ty;
            }
        });
        if kind == InputKind::Commands {
            let (request, response) = generic_types(&ty, 2)?;
            let check_name = format_ident!("__phoxal_commands_port_{}", field_name);
            let port = options
                .port
                .as_ref()
                .ok_or_else(|| syn::Error::new_spanned(&*field, "Commands requires port = ..."))?;
            checks.push(quote! {
                fn #check_name() {
                    ::phoxal::runtime::__private::assert_commands_port::<_, #request, #response>(#port);
                }
            });
        }
        if kind == InputKind::Setpoint
            && let Some(port) = options.port.as_ref()
        {
            let payload = generic_type(&ty, 1, field)?;
            let check_name = format_ident!("__phoxal_setpoint_port_{}", field_name);
            checks.push(quote! {
                fn #check_name() {
                    ::phoxal::runtime::__private::assert_setpoint_port::<_, #payload>(#port);
                }
            });
        }

        let transport_max_items = match kind {
            InputKind::Samples | InputKind::Events | InputKind::Stream | InputKind::Commands => {
                max_items.clone()
            }
            InputKind::Read | InputKind::Request => quote!(Some(1_u64)),
            InputKind::Latest
            | InputKind::Setpoint
            | InputKind::Operation
            | InputKind::Completions => quote!(None),
        };
        let transport_max_bytes = match kind {
            InputKind::Samples
            | InputKind::Events
            | InputKind::Stream
            | InputKind::Commands
            | InputKind::Read
            | InputKind::Request
            | InputKind::Setpoint => max_bytes.clone(),
            InputKind::Latest | InputKind::Operation | InputKind::Completions => quote!(None),
        };
        generated_transport_fields.push(quote! {
            ::phoxal::runtime::transport::InputTransportField {
                name: stringify!(#field_name),
                kind: ::phoxal::runtime::input::InputKind::#kind_variant,
                signature: #port_signature,
                max_age_ms: #max_age,
                max_items: #transport_max_items,
                max_bytes: #transport_max_bytes,
            }
        });

        let generated = expand_transport_decoder(
            kind,
            &field_name,
            &ty,
            &options,
            &mut transport_params,
            &mut transport_types,
            &binding_fn,
            field,
        )?;
        generated_transport_decoders.push(generated.decoder);
        if !generated.encoder.is_empty() {
            generated_transport_encoders.push(generated.encoder);
        }
        generated_transport_bounds.extend(generated.bounds);

        let sink = expand_transport_sink(kind, &field_name, &ty, &options, field)?;
        if kind == InputKind::Latest {
            sink_clear_latest.push(quote! {
                stringify!(#field_name) => {
                    self.#field_name = ::phoxal::runtime::Latest::unavailable();
                    Ok(())
                }
            });
            if let Some(max_age_ms) = options.max_age_ms {
                expire_latest.push(quote! {
                    if inputs.#field_name.value().is_some()
                        && !inputs.#field_name.is_fresh_at(
                            now,
                            Some(#max_age_ms),
                        )
                    {
                        ::phoxal::runtime::input::TransportInputSink::clear_latest(
                            inputs,
                            stringify!(#field_name),
                        )?;
                    }
                });
            }
        }
        match kind {
            InputKind::Latest => sink_latest.push(sink.latest),
            InputKind::Samples => sink_samples.push(sink.samples),
            InputKind::Events => sink_events.push(sink.events),
            InputKind::Setpoint => sink_setpoints.push(sink.setpoint),
            InputKind::Stream => sink_streams.push(sink.stream),
            InputKind::Commands => sink_commands.push(sink.commands),
            InputKind::Read => sink_reads.push(sink.read),
            InputKind::Request => sink_requests.push(sink.request),
            InputKind::Operation => sink_operations.push(sink.operation),
            InputKind::Completions => {
                if let Some(previous) = &completion_field {
                    return Err(syn::Error::new_spanned(
                        field,
                        format!(
                            "runtime inputs may declare only one Completions field; `{previous}` is already declared"
                        ),
                    ));
                }
                completion_field = Some(field_name.clone());
            }
        }
        if matches!(
            kind,
            InputKind::Read | InputKind::Request | InputKind::Operation
        ) {
            let key = match kind {
                InputKind::Read | InputKind::Request => generic_types3(&ty, field)?.0,
                InputKind::Operation => generic_types2(&ty, field)?.0,
                _ => unreachable!(),
            };
            let type_error = sink_type_error(&quote!(stringify!(#field_name)));
            restore_managed.push(quote! {
                stringify!(#field_name) => {
                    let value = value.downcast::<#ty>().map_err(|_| #type_error)?;
                    self.#field_name = *value;
                    Ok(())
                }
            });
            select_managed.push(quote! {
                stringify!(#field_name) => {
                    let key = key.downcast::<#key>().map_err(|_| #type_error)?;
                    self.#field_name.select(*key, attempt_started);
                    Ok(())
                }
            });
            retire_managed.push(quote! {
                stringify!(#field_name) => {
                    self.#field_name = ::core::default::Default::default();
                    Ok(())
                }
            });
            take_managed.push(quote! {
                stringify!(#field_name) => {
                    self.#field_name.finish_invocation();
                    Ok(::std::boxed::Box::new(::core::mem::take(&mut self.#field_name))
                        as ::phoxal::runtime::input::TransportValue)
                }
            });
        }
    }

    let set_call_completions = completion_field.map_or_else(
        || {
            quote! {
                if values.is_empty() {
                    Ok(())
                } else {
                    Err(::phoxal::__private::anyhow::anyhow!(
                        ::phoxal::runtime::transport::TransportError::InvalidMetadata {
                            detail: "runtime inputs have no generated call completion field".to_owned(),
                        }
                    ))
                }
            }
        },
        |field| {
            quote! {
                self.#field = ::phoxal::runtime::input::Completions::from_transport(values);
                Ok(())
            }
        },
    );

    Ok(quote! {
        #input

        impl ::phoxal::runtime::input::InputSet for #name {
            const FIELDS: &'static [::phoxal::runtime::input::InputField] = &[#(#metadata),*];

        }

        #[allow(non_camel_case_types)]
        #[doc(hidden)]
        pub struct #marker_name<#(#transport_params),*>(
            ::core::marker::PhantomData<fn() -> (#(#transport_params,)*)>,
        );

        #[doc(hidden)]
        #[allow(non_snake_case)]
        fn #binding_fn<'a>(
            binding: ::core::option::Option<&'a ::phoxal::runtime::transport::PortBinding>,
            field: &str,
        ) -> ::phoxal::Result<&'a ::phoxal::runtime::transport::PortBinding> {
            binding.ok_or_else(|| ::phoxal::__private::anyhow::anyhow!(
                ::phoxal::runtime::transport::TransportError::InvalidMetadata {
                    detail: format!("input field `{field}` has no resolved source port"),
                }
            ))
        }

        impl<#(#transport_params),*> ::phoxal::runtime::input::GeneratedTransportDecoder<#name>
            for #marker_name<#(#transport_params),*>
        where
            #(#generated_transport_bounds)*
        {
            const FIELDS: &'static [::phoxal::runtime::transport::InputTransportField] =
                &[#(#generated_transport_fields),*];

            fn decode(
                inputs: &mut #name,
                field: &str,
                binding: Option<&::phoxal::runtime::transport::PortBinding>,
                samples: ::std::vec::Vec<::phoxal::runtime::transport::WireSample>,
            ) -> ::phoxal::Result<()> {
                Self::decode_at(
                    inputs,
                    field,
                    binding,
                    samples,
                    ::phoxal::runtime::ExecutionTime::default(),
                )
            }

            fn decode_at(
                inputs: &mut #name,
                field: &str,
                binding: Option<&::phoxal::runtime::transport::PortBinding>,
                mut samples: ::std::vec::Vec<::phoxal::runtime::transport::WireSample>,
                now: ::phoxal::runtime::ExecutionTime,
            ) -> ::phoxal::Result<()> {
                let mut keys: ::core::option::Option<&mut dyn ::phoxal::runtime::input::TransportKeyLookup> = None;
                match field {
                    #(#generated_transport_decoders,)*
                    _ => Err(::phoxal::__private::anyhow::anyhow!(
                        ::phoxal::runtime::transport::TransportError::InvalidMetadata {
                            detail: format!("input field `{field}` has no generated transport binding"),
                        }
                    )),
                }
            }

            fn encode_request(
                field: &str,
                request: &dyn ::core::any::Any,
            ) -> ::phoxal::Result<::std::vec::Vec<u8>> {
                match field {
                    #(#generated_transport_encoders,)*
                    _ => Err(::phoxal::__private::anyhow::anyhow!(
                        ::phoxal::runtime::transport::TransportError::InvalidMetadata {
                            detail: format!("input field `{field}` has no generated request encoder"),
                        }
                    )),
                }
            }

            fn decode_with_keys(
                inputs: &mut #name,
                field: &str,
                binding: Option<&::phoxal::runtime::transport::PortBinding>,
                samples: ::std::vec::Vec<::phoxal::runtime::transport::WireSample>,
                keys: &mut dyn ::phoxal::runtime::input::TransportKeyLookup,
            ) -> ::phoxal::Result<()> {
                Self::decode_with_keys_at(
                    inputs,
                    field,
                    binding,
                    samples,
                    keys,
                    ::phoxal::runtime::ExecutionTime::default(),
                )
            }

            fn decode_with_keys_at(
                inputs: &mut #name,
                field: &str,
                binding: Option<&::phoxal::runtime::transport::PortBinding>,
                mut samples: ::std::vec::Vec<::phoxal::runtime::transport::WireSample>,
                keys: &mut dyn ::phoxal::runtime::input::TransportKeyLookup,
                now: ::phoxal::runtime::ExecutionTime,
            ) -> ::phoxal::Result<()> {
                let mut keys = Some(keys);
                match field {
                    #(#generated_transport_decoders,)*
                    _ => Err(::phoxal::__private::anyhow::anyhow!(
                        ::phoxal::runtime::transport::TransportError::InvalidMetadata {
                            detail: format!("input field `{field}` has no generated transport binding"),
                        }
                    )),
                }
            }

            fn expire_at(
                inputs: &mut #name,
                now: ::phoxal::runtime::ExecutionTime,
            ) -> ::phoxal::Result<()> {
                #(#expire_latest)*
                Ok(())
            }
        }

        impl ::phoxal::runtime::input::TransportInputSink for #name {
            fn set_latest(
                &mut self,
                field: &str,
                value: ::phoxal::runtime::input::TransportValue,
                stamp: ::phoxal::runtime::ObservationStamp,
            ) -> ::phoxal::Result<()> {
                match field {
                    #(#sink_latest,)*
                    _ => Err(::phoxal::__private::anyhow::anyhow!(::phoxal::runtime::transport::TransportError::InvalidMetadata {
                        detail: format!("input field `{field}` is not a latest value"),
                    })),
                }
            }

            fn clear_latest(&mut self, field: &str) -> ::phoxal::Result<()> {
                match field {
                    #(#sink_clear_latest,)*
                    _ => Err(::phoxal::__private::anyhow::anyhow!(::phoxal::runtime::transport::TransportError::InvalidMetadata {
                        detail: format!("input field `{field}` is not a latest value"),
                    })),
                }
            }

            fn set_samples(
                &mut self,
                field: &str,
                values: ::std::vec::Vec<::phoxal::runtime::input::TransportSample>,
                gap: bool,
            ) -> ::phoxal::Result<()> {
                match field {
                    #(#sink_samples,)*
                    _ => Err(::phoxal::__private::anyhow::anyhow!(::phoxal::runtime::transport::TransportError::InvalidMetadata {
                        detail: format!("input field `{field}` is not a samples batch"),
                    })),
                }
            }

            fn set_events(
                &mut self,
                field: &str,
                values: ::std::vec::Vec<::phoxal::runtime::input::TransportValue>,
                gap: bool,
            ) -> ::phoxal::Result<()> {
                match field {
                    #(#sink_events,)*
                    _ => Err(::phoxal::__private::anyhow::anyhow!(::phoxal::runtime::transport::TransportError::InvalidMetadata {
                        detail: format!("input field `{field}` is not an events batch"),
                    })),
                }
            }

            fn set_setpoint(
                &mut self,
                field: &str,
                value: ::core::option::Option<::phoxal::runtime::input::SetpointUpdate>,
            ) -> ::phoxal::Result<()> {
                match field {
                    #(#sink_setpoints,)*
                    _ => Err(::phoxal::__private::anyhow::anyhow!(::phoxal::runtime::transport::TransportError::InvalidMetadata {
                        detail: format!("input field `{field}` is not a setpoint"),
                    })),
                }
            }

            fn set_stream(
                &mut self,
                field: &str,
                values: ::std::vec::Vec<::phoxal::runtime::input::TransportStreamItem>,
            ) -> ::phoxal::Result<()> {
                match field {
                    #(#sink_streams,)*
                    _ => Err(::phoxal::__private::anyhow::anyhow!(::phoxal::runtime::transport::TransportError::InvalidMetadata {
                        detail: format!("input field `{field}` is not a stream"),
                    })),
                }
            }

            fn set_commands(
                &mut self,
                field: &str,
                values: ::std::vec::Vec<::phoxal::runtime::input::TransportCommand>,
                encoded_bytes: u64,
            ) -> ::phoxal::Result<()> {
                match field {
                    #(#sink_commands,)*
                    _ => Err(::phoxal::__private::anyhow::anyhow!(::phoxal::runtime::transport::TransportError::InvalidMetadata {
                        detail: format!("input field `{field}` is not commands"),
                    })),
                }
            }

            fn set_read(
                &mut self,
                field: &str,
                key: ::phoxal::runtime::input::TransportValue,
                result: ::core::result::Result<
                    ::phoxal::runtime::input::TransportValue,
                    ::phoxal::runtime::input::ReadError,
                >,
                provenance: ::core::option::Option<::phoxal::runtime::ObservationStamp>,
            ) -> ::phoxal::Result<()> {
                match field {
                    #(#sink_reads,)*
                    _ => Err(::phoxal::__private::anyhow::anyhow!(::phoxal::runtime::transport::TransportError::InvalidMetadata {
                        detail: format!("input field `{field}` is not a read"),
                    })),
                }
            }

            fn restore_managed(
                &mut self,
                field: &str,
                value: ::phoxal::runtime::input::TransportValue,
            ) -> ::phoxal::Result<()> {
                match field {
                    #(#restore_managed,)*
                    _ => Err(::phoxal::__private::anyhow::anyhow!(::phoxal::runtime::transport::TransportError::InvalidMetadata {
                        detail: format!("input field `{field}` has no managed state"),
                    })),
                }
            }

            fn select_managed(
                &mut self,
                field: &str,
                key: ::phoxal::runtime::input::TransportValue,
                attempt_started: bool,
            ) -> ::phoxal::Result<()> {
                match field {
                    #(#select_managed,)*
                    _ => Err(::phoxal::__private::anyhow::anyhow!(::phoxal::runtime::transport::TransportError::InvalidMetadata {
                        detail: format!("input field `{field}` has no managed state"),
                    })),
                }
            }

            fn retire_managed(&mut self, field: &str) -> ::phoxal::Result<()> {
                match field {
                    #(#retire_managed,)*
                    _ => Err(::phoxal::__private::anyhow::anyhow!(::phoxal::runtime::transport::TransportError::InvalidMetadata {
                        detail: format!("input field `{field}` has no managed state"),
                    })),
                }
            }

            fn take_managed(
                &mut self,
                field: &str,
            ) -> ::phoxal::Result<::phoxal::runtime::input::TransportValue> {
                match field {
                    #(#take_managed,)*
                    _ => Err(::phoxal::__private::anyhow::anyhow!(::phoxal::runtime::transport::TransportError::InvalidMetadata {
                        detail: format!("input field `{field}` has no managed state"),
                    })),
                }
            }

            fn set_request(
                &mut self,
                field: &str,
                key: ::phoxal::runtime::input::TransportValue,
                result: ::core::result::Result<
                    ::phoxal::runtime::input::TransportValue,
                    ::phoxal::runtime::input::RequestError,
                >,
            ) -> ::phoxal::Result<()> {
                match field {
                    #(#sink_requests,)*
                    _ => Err(::phoxal::__private::anyhow::anyhow!(::phoxal::runtime::transport::TransportError::InvalidMetadata {
                        detail: format!("input field `{field}` is not a request"),
                    })),
                }
            }

            fn set_operation(
                &mut self,
                field: &str,
                key: ::phoxal::runtime::input::TransportValue,
                result: ::core::result::Result<
                    ::phoxal::runtime::input::TransportValue,
                    ::phoxal::runtime::input::OperationInputError,
                >,
            ) -> ::phoxal::Result<()> {
                match field {
                    #(#sink_operations,)*
                    _ => Err(::phoxal::__private::anyhow::anyhow!(::phoxal::runtime::transport::TransportError::InvalidMetadata {
                        detail: format!("input field `{field}` is not an operation"),
                    })),
                }
            }

            fn set_call_completions(
                &mut self,
                values: ::std::vec::Vec<::phoxal::runtime::input::TransportCallCompletion>,
            ) -> ::phoxal::Result<()> {
                #set_call_completions
            }
        }

        impl ::phoxal::runtime::input::InputSnapshot for #name {
            type Transport = #marker_name<#(#transport_types),*>;

            fn empty() -> Self {
                Self {
                    #(#field_names: ::core::default::Default::default(),)*
                }
            }
        }

        #(#bindings)*

        const _: () = {
            #(#checks)*
        };
    })
}

/// A direct nested marker is consumed by its enclosing collector.  Keeping a
/// real proc macro for it makes malformed placement produce a focused error.
pub fn expand_input_marker(_attr: TokenStream, _item: TokenStream) -> syn::Result<TokenStream> {
    Err(syn::Error::new(
        Span::call_site(),
        "#[phoxal::runtime::input] is valid only on a field inside #[phoxal::runtime::inputs]",
    ))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InputKind {
    Latest,
    Samples,
    Events,
    Setpoint,
    Stream,
    Commands,
    Read,
    Request,
    Operation,
    Completions,
}

impl InputKind {
    fn variant(self) -> &'static str {
        match self {
            Self::Latest => "Latest",
            Self::Samples => "Samples",
            Self::Events => "Events",
            Self::Setpoint => "Setpoint",
            Self::Stream => "Stream",
            Self::Commands => "Commands",
            Self::Read => "Read",
            Self::Request => "Request",
            Self::Operation => "Operation",
            Self::Completions => "Completions",
        }
    }
}

fn input_kind(ty: &Type) -> syn::Result<InputKind> {
    let Type::Path(path) = ty else {
        return Err(syn::Error::new_spanned(
            ty,
            "runtime inputs must use one of input::Latest, Samples, Events, Setpoint, Stream, Commands, Read, Request, Operation, or Completions",
        ));
    };
    let name = path
        .path
        .segments
        .last()
        .map(|segment| segment.ident.to_string())
        .ok_or_else(|| syn::Error::new_spanned(ty, "runtime input type has no name"))?;
    match name.as_str() {
        "Latest" => Ok(InputKind::Latest),
        "Samples" => Ok(InputKind::Samples),
        "Events" => Ok(InputKind::Events),
        "Setpoint" => Ok(InputKind::Setpoint),
        "Stream" => Ok(InputKind::Stream),
        "Commands" => Ok(InputKind::Commands),
        "Read" => Ok(InputKind::Read),
        "Request" => Ok(InputKind::Request),
        "Operation" => Ok(InputKind::Operation),
        "Completions" => Ok(InputKind::Completions),
        _ => Err(syn::Error::new_spanned(
            ty,
            "unsupported runtime input type; use a type from phoxal::runtime::input",
        )),
    }
}

struct TransportDecoder {
    decoder: TokenStream,
    encoder: TokenStream,
    bounds: Vec<TokenStream>,
}

fn fresh_param(params: &mut Vec<Ident>) -> Ident {
    let ident = format_ident!("__PhoxalTransportT{}", params.len());
    params.push(ident.clone());
    ident
}

#[allow(clippy::too_many_arguments)]
fn expand_transport_decoder(
    kind: InputKind,
    field_name: &Ident,
    ty: &Type,
    options: &Options,
    params: &mut Vec<Ident>,
    type_args: &mut Vec<Type>,
    binding_fn: &Ident,
    item: &Field,
) -> syn::Result<TransportDecoder> {
    let field_text = quote!(stringify!(#field_name));
    let max_age = option_tokens(options.max_age_ms);
    let mut bounds = Vec::new();
    let decoder = match kind {
        InputKind::Latest => {
            let payload_type = generic_type(ty, 1, item)?;
            let payload = fresh_param(params);
            type_args.push(payload_type);
            bounds.push(prost_bound(&payload));
            quote! {
                #field_text => {
                    let binding = #binding_fn(binding, #field_text)?;
                    ::phoxal::runtime::transport::validate_publication_binding::<#payload>(
                        binding,
                        ::phoxal::__private::PortKind::State,
                    )?;
                    if samples.len() > 1 {
                        return Err(::phoxal::__private::anyhow::anyhow!(
                            ::phoxal::runtime::transport::TransportError::BatchTooLarge {
                                port: binding.name.clone(),
                                what: "item count",
                                actual: samples.len() as u64,
                                maximum: 1,
                            }
                        ));
                    }
                    let sample = samples.pop().ok_or_else(|| ::phoxal::__private::anyhow::anyhow!(
                        ::phoxal::runtime::transport::TransportError::InvalidMetadata {
                            detail: format!("input field `{}` received no sample", #field_text),
                        }
                    ))?;
                    let stamp = ::phoxal::runtime::transport::observation_stamp(sample.metadata())?;
                    if let Some(max_age_ms) = #max_age {
                        let Some(age) = now.checked_duration_since(stamp.capture_time()) else {
                            return ::phoxal::runtime::input::TransportInputSink::clear_latest(
                                inputs,
                                #field_text,
                            );
                        };
                        if age.as_millis() > max_age_ms {
                            return ::phoxal::runtime::input::TransportInputSink::clear_latest(
                                inputs,
                                #field_text,
                            );
                        }
                    }
                    let value = ::phoxal::runtime::transport::decode_message::<#payload>(
                        binding,
                        &sample,
                        u64::MAX,
                    )?;
                    ::phoxal::runtime::input::TransportInputSink::set_latest(
                        inputs,
                        #field_text,
                        ::std::boxed::Box::new(value),
                        stamp,
                    )
                }
            }
        }
        InputKind::Samples => {
            let payload_type = generic_type(ty, 1, item)?;
            let payload = fresh_param(params);
            type_args.push(payload_type);
            let max_items = options
                .max_items
                .ok_or_else(|| syn::Error::new_spanned(item, "Samples requires max_items = ..."))?;
            let max_bytes = options
                .max_bytes
                .ok_or_else(|| syn::Error::new_spanned(item, "Samples requires max_bytes = ..."))?;
            bounds.push(prost_bound(&payload));
            quote! {
                #field_text => {
                    let binding = #binding_fn(binding, #field_text)?;
                    ::phoxal::runtime::transport::validate_publication_binding::<#payload>(
                        binding,
                        ::phoxal::__private::PortKind::Sample,
                    )?;
                    let count = samples.len() as u64;
                    let encoded_bytes = samples.iter().try_fold(0_u64, |total, sample| {
                        total.checked_add(sample.payload().len() as u64).ok_or_else(|| {
                            ::phoxal::__private::anyhow::anyhow!(
                                ::phoxal::runtime::transport::TransportError::BatchTooLarge {
                                    port: binding.name.clone(),
                                    what: "encoded bytes",
                                    actual: u64::MAX,
                                    maximum: #max_bytes,
                                }
                            )
                        })
                    })?;
                    let capacity = ::phoxal::runtime::Capacity::new(#max_items, #max_bytes)
                        .map_err(|error| ::phoxal::__private::anyhow::anyhow!(error))?;
                    capacity.check(count, encoded_bytes)
                        .map_err(|error| ::phoxal::__private::anyhow::anyhow!(error))?;
                    let mut items = ::std::vec::Vec::with_capacity(samples.len());
                    let mut gap = false;
                    for sample in samples {
                        match sample.metadata().wire_control()? {
                            ::phoxal::runtime::transport::WireControl::Data => {
                                let stamp = ::phoxal::runtime::transport::observation_stamp(sample.metadata())?;
                                let value = ::phoxal::runtime::transport::decode_message::<#payload>(
                                    binding,
                                    &sample,
                                    #max_bytes,
                                )?;
                                items.push(::phoxal::runtime::input::TransportSample {
                                    value: ::std::boxed::Box::new(value),
                                    stamp,
                                });
                            }
                            ::phoxal::runtime::transport::WireControl::Gap => gap = true,
                            control => return Err(::phoxal::__private::anyhow::anyhow!(
                                ::phoxal::runtime::transport::TransportError::InvalidMetadata {
                                    detail: format!("samples field `{}` received {:?} control", #field_text, control),
                                }
                            )),
                        }
                    }
                    ::phoxal::runtime::input::TransportInputSink::set_samples(
                        inputs,
                        #field_text,
                        items,
                        gap,
                    )
                }
            }
        }
        InputKind::Events => {
            let payload_type = generic_type(ty, 1, item)?;
            let payload = fresh_param(params);
            type_args.push(payload_type);
            let max_items = options
                .max_items
                .ok_or_else(|| syn::Error::new_spanned(item, "Events requires max_items = ..."))?;
            let max_bytes = options
                .max_bytes
                .ok_or_else(|| syn::Error::new_spanned(item, "Events requires max_bytes = ..."))?;
            bounds.push(prost_bound(&payload));
            quote! {
                #field_text => {
                    let binding = #binding_fn(binding, #field_text)?;
                    ::phoxal::runtime::transport::validate_publication_binding::<#payload>(
                        binding,
                        ::phoxal::__private::PortKind::Event,
                    )?;
                    let count = samples.len() as u64;
                    let encoded_bytes = samples.iter().try_fold(0_u64, |total, sample| {
                        total.checked_add(sample.payload().len() as u64).ok_or_else(|| {
                            ::phoxal::__private::anyhow::anyhow!(
                                ::phoxal::runtime::transport::TransportError::BatchTooLarge {
                                    port: binding.name.clone(),
                                    what: "encoded bytes",
                                    actual: u64::MAX,
                                    maximum: #max_bytes,
                                }
                            )
                        })
                    })?;
                    let capacity = ::phoxal::runtime::Capacity::new(#max_items, #max_bytes)
                        .map_err(|error| ::phoxal::__private::anyhow::anyhow!(error))?;
                    capacity.check(count, encoded_bytes)
                        .map_err(|error| ::phoxal::__private::anyhow::anyhow!(error))?;
                    let mut items = ::std::vec::Vec::with_capacity(samples.len());
                    let mut gap = false;
                    for sample in samples {
                        match sample.metadata().wire_control()? {
                            ::phoxal::runtime::transport::WireControl::Data => {
                                let _stamp = ::phoxal::runtime::transport::observation_stamp(sample.metadata())?;
                                let value = ::phoxal::runtime::transport::decode_message::<#payload>(
                                    binding,
                                    &sample,
                                    #max_bytes,
                                )?;
                                items.push(::std::boxed::Box::new(value) as ::phoxal::runtime::input::TransportValue);
                            }
                            ::phoxal::runtime::transport::WireControl::Gap => gap = true,
                            control => return Err(::phoxal::__private::anyhow::anyhow!(
                                ::phoxal::runtime::transport::TransportError::InvalidMetadata {
                                    detail: format!("events field `{}` received {:?} control", #field_text, control),
                                }
                            )),
                        }
                    }
                    ::phoxal::runtime::input::TransportInputSink::set_events(
                        inputs,
                        #field_text,
                        items,
                        gap,
                    )
                }
            }
        }
        InputKind::Setpoint => {
            let payload_type = generic_type(ty, 1, item)?;
            let payload = fresh_param(params);
            type_args.push(payload_type);
            bounds.push(prost_bound(&payload));
            let validate_binding = if options.port.is_some() {
                quote! {
                    ::phoxal::runtime::transport::validate_exchange_binding::<
                        #payload,
                        ::phoxal::contract::Empty,
                    >(binding, ::phoxal::__private::PortKind::Setpoint)?;
                }
            } else {
                quote! {
                    ::phoxal::runtime::transport::validate_publication_binding::<#payload>(
                        binding,
                        ::phoxal::__private::PortKind::Setpoint,
                    )?;
                }
            };
            quote! {
                #field_text => {
                    let binding = #binding_fn(binding, #field_text)?;
                    #validate_binding
                    if samples.len() > 1 {
                        return Err(::phoxal::__private::anyhow::anyhow!(
                            ::phoxal::runtime::transport::TransportError::BatchTooLarge {
                                port: binding.name.clone(),
                                what: "item count",
                                actual: samples.len() as u64,
                                maximum: 1,
                            }
                        ));
                    }
                    let sample = samples.pop().ok_or_else(|| ::phoxal::__private::anyhow::anyhow!(
                        ::phoxal::runtime::transport::TransportError::InvalidMetadata {
                            detail: format!("input field `{}` received no sample", #field_text),
                        }
                    ))?;
                    let control = sample.metadata().wire_control()?;
                    if control == ::phoxal::runtime::transport::WireControl::Withdraw {
                        if !sample.payload().is_empty() {
                            return Err(::phoxal::__private::anyhow::anyhow!("setpoint withdrawal carries a payload"));
                        }
                        return ::phoxal::runtime::input::TransportInputSink::set_setpoint(inputs, #field_text, None);
                    }
                    if control != ::phoxal::runtime::transport::WireControl::Data {
                        return Err(::phoxal::__private::anyhow::anyhow!("setpoint field received an incompatible control"));
                    }
                    let issued_at = sample.metadata().logical_time()?;
                    let valid_until = sample.metadata().expires_at_nanos.ok_or_else(||
                        ::phoxal::__private::anyhow::anyhow!("setpoint renewal is missing expiry"))?;
                    if valid_until < issued_at.as_nanos() {
                        return Err(::phoxal::__private::anyhow::anyhow!(
                            ::phoxal::runtime::transport::TransportError::InvalidMetadata {
                                detail: format!("setpoint field `{}` expires before issue time", #field_text),
                            }
                        ));
                    }
                    let value = ::phoxal::runtime::transport::decode_message::<#payload>(
                        binding,
                        &sample,
                        u64::MAX,
                    )?;
                    let source = sample.metadata().publisher().ok_or_else(||
                        ::phoxal::__private::anyhow::anyhow!(
                            ::phoxal::runtime::transport::TransportError::InvalidMetadata {
                                detail: format!("setpoint field `{}` is missing source identity", #field_text),
                            }
                        ))?.to_owned();
                    ::phoxal::runtime::input::TransportInputSink::set_setpoint(
                        inputs,
                        #field_text,
                        Some(::phoxal::runtime::input::SetpointUpdate {
                            value: ::std::boxed::Box::new(value),
                            source,
                            issued_at,
                            valid_until: ::phoxal::runtime::ExecutionTime::from_nanos(valid_until),
                        }),
                    )
                }
            }
        }
        InputKind::Stream => {
            let payload_type = generic_type(ty, 1, item)?;
            let payload = fresh_param(params);
            type_args.push(payload_type);
            let max_items = options
                .max_items
                .ok_or_else(|| syn::Error::new_spanned(item, "Stream requires max_items = ..."))?;
            let max_bytes = options
                .max_bytes
                .ok_or_else(|| syn::Error::new_spanned(item, "Stream requires max_bytes = ..."))?;
            bounds.push(prost_bound(&payload));
            quote! {
                #field_text => {
                    let binding = #binding_fn(binding, #field_text)?;
                    ::phoxal::runtime::transport::validate_publication_binding::<#payload>(
                        binding,
                        ::phoxal::__private::PortKind::Stream,
                    )?;
                    let count = samples.len() as u64;
                    let encoded_bytes = samples.iter().try_fold(0_u64, |total, sample| {
                        total.checked_add(sample.payload().len() as u64).ok_or_else(|| {
                            ::phoxal::__private::anyhow::anyhow!(
                                ::phoxal::runtime::transport::TransportError::BatchTooLarge {
                                    port: binding.name.clone(),
                                    what: "encoded bytes",
                                    actual: u64::MAX,
                                    maximum: #max_bytes,
                                }
                            )
                        })
                    })?;
                    let capacity = ::phoxal::runtime::Capacity::new(#max_items, #max_bytes)
                        .map_err(|error| ::phoxal::__private::anyhow::anyhow!(error))?;
                    capacity.check(count, encoded_bytes)
                        .map_err(|error| ::phoxal::__private::anyhow::anyhow!(error))?;
                    let mut items = ::std::vec::Vec::with_capacity(samples.len());
                    for sample in samples {
                        match sample.metadata().wire_control()? {
                            ::phoxal::runtime::transport::WireControl::Data => {
                                let stamp = ::phoxal::runtime::transport::observation_stamp(sample.metadata())?;
                                let value = ::phoxal::runtime::transport::decode_message::<#payload>(
                                    binding,
                                    &sample,
                                    #max_bytes,
                                )?;
                                items.push(::phoxal::runtime::input::TransportStreamItem::Data(
                                    ::phoxal::runtime::input::TransportSample {
                                        value: ::std::boxed::Box::new(value),
                                        stamp,
                                    },
                                ));
                            }
                            ::phoxal::runtime::transport::WireControl::Gap =>
                                items.push(::phoxal::runtime::input::TransportStreamItem::Gap),
                            ::phoxal::runtime::transport::WireControl::End =>
                                items.push(::phoxal::runtime::input::TransportStreamItem::End),
                            ::phoxal::runtime::transport::WireControl::Failed =>
                                items.push(::phoxal::runtime::input::TransportStreamItem::Failed(
                                    sample
                                        .metadata()
                                        .reason
                                        .clone()
                                        .unwrap_or_else(|| "source stream failed".to_owned()),
                                )),
                            ::phoxal::runtime::transport::WireControl::Rejected
                            | ::phoxal::runtime::transport::WireControl::Withdraw
                            | ::phoxal::runtime::transport::WireControl::Busy
                            | ::phoxal::runtime::transport::WireControl::Oversized =>
                                return Err(::phoxal::__private::anyhow::anyhow!(
                                    ::phoxal::runtime::transport::TransportError::InvalidMetadata {
                                        detail: format!("stream field `{}` received a request rejection", #field_text),
                                    }
                                )),
                        }
                    }
                    ::phoxal::runtime::input::TransportInputSink::set_stream(
                        inputs,
                        #field_text,
                        items,
                    )
                }
            }
        }
        InputKind::Commands => {
            let (request_type, _response_type) = generic_types(ty, 2)?;
            let request = fresh_param(params);
            type_args.push(request_type);
            let port = options
                .port
                .as_ref()
                .ok_or_else(|| syn::Error::new_spanned(item, "Commands requires port = ..."))?;
            let max_bytes = options.max_bytes.ok_or_else(|| {
                syn::Error::new_spanned(item, "Commands requires max_bytes = ...")
            })?;
            bounds.push(prost_bound(&request));
            quote! {
                #field_text => {
                    let signature = #binding_fn(binding, #field_text)?;
                    ::phoxal::runtime::transport::validate_binding_identity(
                        signature,
                        (#port).signature(),
                    )?;
                    if signature.kind != ::phoxal::__private::PortKind::Commands {
                        return Err(::phoxal::__private::anyhow::anyhow!(
                            ::phoxal::runtime::transport::TransportError::InvalidMetadata {
                                detail: format!("Commands input `{}` received a non-Commands binding", #field_text),
                            }
                        ));
                    }
                    ::phoxal::runtime::transport::sort_command_samples(&mut samples)?;
                    let encoded_bytes = samples.iter().try_fold(0_u64, |total, sample| {
                        total.checked_add(sample.payload().len() as u64).ok_or_else(|| {
                            ::phoxal::__private::anyhow::anyhow!(::phoxal::runtime::transport::TransportError::BatchTooLarge {
                                port: signature.name.clone(),
                                what: "encoded bytes",
                                actual: u64::MAX,
                                maximum: #max_bytes,
                            })
                        })
                    })?;
                    let mut items = ::std::vec::Vec::with_capacity(samples.len());
                    for sample in samples {
                        let request: #request = ::phoxal::runtime::transport::decode_request(
                            (#port).signature(),
                            &sample,
                            #max_bytes,
                        ).map_err(|error| ::phoxal::__private::anyhow::anyhow!(error))?;
                        let order = ::phoxal::runtime::transport::command_order(sample.metadata())
                            .map_err(|error| ::phoxal::__private::anyhow::anyhow!(error))?;
                        items.push(::phoxal::runtime::input::TransportCommand {
                            order,
                            source: sample.metadata().caller.clone().or_else(|| sample.metadata().source.clone()).ok_or_else(||
                                ::phoxal::__private::anyhow::anyhow!(
                                    ::phoxal::runtime::transport::TransportError::InvalidMetadata {
                                        detail: format!("Commands input `{}` is missing caller identity", #field_text),
                                    }
                                ))?,
                            request: ::std::boxed::Box::new(request) as ::phoxal::runtime::input::TransportValue,
                        });
                    }
                    ::phoxal::runtime::input::TransportInputSink::set_commands(
                        inputs,
                        #field_text,
                        items,
                        encoded_bytes,
                    )
                }
            }
        }
        InputKind::Read => {
            let types = generic_types3(ty, item)?;
            let key = fresh_param(params);
            let request = fresh_param(params);
            let response = fresh_param(params);
            type_args.extend([types.0, types.1, types.2]);
            let max_bytes = options.max_bytes.ok_or_else(|| {
                syn::Error::new_spanned(item, "Read requires max_response_bytes = ...")
            })?;
            bounds.push(prost_bound(&request));
            bounds.push(prost_bound(&response));
            bounds.push(quote! {
                #key: ::core::convert::From<u64> + ::core::cmp::Eq + ::core::marker::Send + 'static,
            });
            quote! {
                #field_text => {
                    let binding = #binding_fn(binding, #field_text)?;
                    ::phoxal::runtime::transport::validate_exchange_binding::<#request, #response>(
                        binding,
                        ::phoxal::__private::PortKind::Read,
                    )?;
                    if samples.len() != 1 {
                        return Err(::phoxal::__private::anyhow::anyhow!(
                            ::phoxal::runtime::transport::TransportError::BatchTooLarge {
                                port: binding.name.clone(),
                                what: "item count",
                                actual: samples.len() as u64,
                                maximum: 1,
                            }
                        ));
                    }
                    let sample = samples.pop().ok_or_else(|| ::phoxal::__private::anyhow::anyhow!(
                        ::phoxal::runtime::transport::TransportError::InvalidMetadata {
                            detail: format!("read input `{}` received no completion sample", #field_text),
                        }
                    ))?;
                    let command_id = sample.metadata().command_id.ok_or_else(|| ::phoxal::__private::anyhow::anyhow!(
                        ::phoxal::runtime::transport::TransportError::CommandCorrelation(
                            "read completion is missing command id".to_owned(),
                        )
                    ))?;
                    if let Some(lookup) = keys.as_deref_mut()
                        && lookup.is_expired(#field_text, command_id)
                    {
                        return Ok(());
                    }
                    if let Some(lookup) = keys.as_deref_mut() {
                        lookup.validate_reply(#field_text, command_id, &sample)?;
                    }
                    let _completion_stamp = ::phoxal::runtime::transport::observation_stamp(sample.metadata())?;
                    let result = match sample.metadata().wire_control()? {
                        ::phoxal::runtime::transport::WireControl::Busy => Err(::phoxal::runtime::ReadError::Busy),
                        ::phoxal::runtime::transport::WireControl::Oversized => Err(::phoxal::runtime::ReadError::Oversized),
                        ::phoxal::runtime::transport::WireControl::Data =>
                            Ok(::std::boxed::Box::new(::phoxal::runtime::transport::decode_message::<#response>(
                                binding,
                                &sample,
                                #max_bytes,
                            )?) as ::phoxal::runtime::input::TransportValue),
                        ::phoxal::runtime::transport::WireControl::Failed =>
                            Err(::phoxal::runtime::ReadError::Transport("read provider failed".to_owned())),
                        ::phoxal::runtime::transport::WireControl::Rejected =>
                            Err(::phoxal::runtime::ReadError::Unavailable(
                                sample
                                    .metadata()
                                    .reason
                                    .clone()
                                    .unwrap_or_else(|| "read request rejected before admission".to_owned()),
                            )),
                        control => return Err(::phoxal::__private::anyhow::anyhow!(
                            ::phoxal::runtime::transport::TransportError::InvalidMetadata {
                                detail: format!("read completion used {:?} control", control),
                            }
                        )),
                    };
                    let key = match keys.as_mut() {
                        Some(lookup) => lookup.take_key(#field_text, command_id).ok_or_else(|| {
                            ::phoxal::__private::anyhow::anyhow!(
                                ::phoxal::runtime::transport::TransportError::CommandCorrelation(
                                    format!("stale or unknown read correlation id {command_id}"),
                                )
                            )
                        })?,
                        None => ::std::boxed::Box::new(
                            <#key as ::core::convert::From<u64>>::from(command_id),
                        ) as ::phoxal::runtime::input::TransportValue,
                    };
                    ::phoxal::runtime::input::TransportInputSink::set_read(
                        inputs,
                        #field_text,
                        key,
                        result,
                        Some(_completion_stamp),
                    )
                }
            }
        }
        InputKind::Request => {
            let types = generic_types3(ty, item)?;
            let key = fresh_param(params);
            let request = fresh_param(params);
            let response = fresh_param(params);
            type_args.extend([types.0, types.1, types.2]);
            let max_bytes = options.max_bytes.ok_or_else(|| {
                syn::Error::new_spanned(item, "Request requires max_response_bytes = ...")
            })?;
            bounds.push(prost_bound(&request));
            bounds.push(prost_bound(&response));
            bounds.push(quote! {
                #key: ::core::convert::From<u64> + ::core::cmp::Eq + ::core::marker::Send + 'static,
            });
            quote! {
                #field_text => {
                    let binding = #binding_fn(binding, #field_text)?;
                    ::phoxal::runtime::transport::validate_exchange_binding::<#request, #response>(
                        binding,
                        ::phoxal::__private::PortKind::Commands,
                    )?;
                    if samples.len() != 1 {
                        return Err(::phoxal::__private::anyhow::anyhow!(
                            ::phoxal::runtime::transport::TransportError::BatchTooLarge {
                                port: binding.name.clone(),
                                what: "item count",
                                actual: samples.len() as u64,
                                maximum: 1,
                            }
                        ));
                    }
                    let sample = samples.pop().ok_or_else(|| ::phoxal::__private::anyhow::anyhow!(
                        ::phoxal::runtime::transport::TransportError::InvalidMetadata {
                            detail: format!("request input `{}` received no completion sample", #field_text),
                        }
                    ))?;
                    let command_id = sample.metadata().command_id.ok_or_else(|| ::phoxal::__private::anyhow::anyhow!(
                        ::phoxal::runtime::transport::TransportError::CommandCorrelation(
                            "request completion is missing command id".to_owned(),
                        )
                    ))?;
                    if let Some(lookup) = keys.as_deref_mut()
                        && lookup.is_expired(#field_text, command_id)
                    {
                        return Ok(());
                    }
                    if let Some(lookup) = keys.as_deref_mut() {
                        lookup.validate_reply(#field_text, command_id, &sample)?;
                    }
                    let _completion_stamp = ::phoxal::runtime::transport::observation_stamp(sample.metadata())?;
                    let result = match sample.metadata().wire_control()? {
                        ::phoxal::runtime::transport::WireControl::Data =>
                            Ok(::std::boxed::Box::new(::phoxal::runtime::transport::decode_message::<#response>(
                                binding,
                                &sample,
                                #max_bytes,
                            )?) as ::phoxal::runtime::input::TransportValue),
                        ::phoxal::runtime::transport::WireControl::Failed =>
                            Err(::phoxal::runtime::RequestError::OutcomeUnknown(
                                sample
                                    .metadata()
                                    .reason
                                    .clone()
                                    .unwrap_or_else(|| "request provider failed".to_owned()),
                            )),
                        ::phoxal::runtime::transport::WireControl::Rejected =>
                            Err(::phoxal::runtime::RequestError::RejectedBeforeAdmission(
                                sample
                                    .metadata()
                                    .reason
                                    .clone()
                                    .unwrap_or_else(|| "request rejected before admission".to_owned()),
                            )),
                        control => return Err(::phoxal::__private::anyhow::anyhow!(
                            ::phoxal::runtime::transport::TransportError::InvalidMetadata {
                                detail: format!("request completion used {:?} control", control),
                            }
                        )),
                    };
                    let key = match keys.as_mut() {
                        Some(lookup) => lookup.take_key(#field_text, command_id).ok_or_else(|| {
                            ::phoxal::__private::anyhow::anyhow!(
                                ::phoxal::runtime::transport::TransportError::CommandCorrelation(
                                    format!("stale or unknown request correlation id {command_id}"),
                                )
                            )
                        })?,
                        None => ::std::boxed::Box::new(
                            <#key as ::core::convert::From<u64>>::from(command_id),
                        ) as ::phoxal::runtime::input::TransportValue,
                    };
                    ::phoxal::runtime::input::TransportInputSink::set_request(
                        inputs,
                        #field_text,
                        key,
                        result,
                    )
                }
            }
        }
        InputKind::Operation => {
            let _types = generic_types2(ty, item)?;
            quote! {
                #field_text => Err(::phoxal::__private::anyhow::anyhow!(
                    ::phoxal::runtime::transport::TransportError::InvalidMetadata {
                        detail: format!("Operation field `{}` is runner-local and has no public wire binding", #field_text),
                    }
                ))
            }
        }
        InputKind::Completions => quote! {
            #field_text => Err(::phoxal::__private::anyhow::anyhow!(
                ::phoxal::runtime::transport::TransportError::InvalidMetadata {
                    detail: format!("Completions field `{}` is runner-local and has no public wire binding", #field_text),
                }
            ))
        },
    };

    let encoder = match kind {
        InputKind::Read | InputKind::Request => {
            let (_, request, _) = generic_types3(ty, item)?;
            quote! {
                #field_text => {
                    let request = request.downcast_ref::<#request>().ok_or_else(|| {
                        ::phoxal::__private::anyhow::anyhow!(
                            ::phoxal::runtime::transport::TransportError::InvalidMetadata {
                                detail: format!(
                                    "input field `{}` received a value of the wrong generated Rust type",
                                    #field_text,
                                ),
                            }
                        )
                    })?;
                    ::phoxal::runtime::transport::encode_prost(request).map_err(|error| {
                        ::phoxal::__private::anyhow::anyhow!(
                            ::phoxal::runtime::transport::TransportError::PayloadEncode {
                                port: #field_text.to_owned(),
                                detail: error.to_string(),
                            }
                        )
                    })
                }
            }
        }
        _ => TokenStream::new(),
    };

    Ok(TransportDecoder {
        decoder,
        encoder,
        bounds,
    })
}

struct TransportSink {
    latest: TokenStream,
    samples: TokenStream,
    events: TokenStream,
    setpoint: TokenStream,
    stream: TokenStream,
    commands: TokenStream,
    read: TokenStream,
    request: TokenStream,
    operation: TokenStream,
}

fn sink_type_error(field_text: &TokenStream) -> TokenStream {
    quote! {
        ::phoxal::__private::anyhow::anyhow!(::phoxal::runtime::transport::TransportError::InvalidMetadata {
            detail: format!("input field `{}` received a value of the wrong generated Rust type", #field_text),
        })
    }
}

fn expand_transport_sink(
    kind: InputKind,
    field_name: &Ident,
    ty: &Type,
    options: &Options,
    item: &Field,
) -> syn::Result<TransportSink> {
    let field_text = quote!(stringify!(#field_name));
    let empty = TokenStream::new();
    let type_error = sink_type_error(&field_text);
    let mut sink = TransportSink {
        latest: empty.clone(),
        samples: empty.clone(),
        events: empty.clone(),
        setpoint: empty.clone(),
        stream: empty.clone(),
        commands: empty.clone(),
        read: empty.clone(),
        request: empty.clone(),
        operation: empty,
    };
    match kind {
        InputKind::Latest => {
            let payload = generic_type(ty, 1, item)?;
            sink.latest = quote! {
                #field_text => {
                    let value = value.downcast::<#payload>().map_err(|_| #type_error)?;
                    self.#field_name = ::phoxal::runtime::Latest::new(*value, stamp);
                    Ok(())
                }
            };
        }
        InputKind::Samples => {
            let payload = generic_type(ty, 1, item)?;
            sink.samples = quote! {
                #field_text => {
                    let mut items = ::std::vec::Vec::with_capacity(values.len());
                    for item in values {
                        let value = item.value.downcast::<#payload>().map_err(|_| #type_error)?;
                        items.push(::phoxal::runtime::Sample::new(*value, item.stamp));
                    }
                    self.#field_name = if gap {
                        ::phoxal::runtime::Samples::with_gap(items)
                    } else {
                        ::phoxal::runtime::Samples::new(items)
                    };
                    Ok(())
                }
            };
        }
        InputKind::Events => {
            let payload = generic_type(ty, 1, item)?;
            sink.events = quote! {
                #field_text => {
                    let mut items = ::std::vec::Vec::with_capacity(values.len());
                    for value in values {
                        let value = value.downcast::<#payload>().map_err(|_| #type_error)?;
                        items.push(*value);
                    }
                    self.#field_name = if gap {
                        ::phoxal::runtime::Events::with_gap(items)
                    } else {
                        ::phoxal::runtime::Events::new(items)
                    };
                    Ok(())
                }
            };
        }
        InputKind::Setpoint => {
            let payload = generic_type(ty, 1, item)?;
            sink.setpoint = quote! {
                #field_text => {
                    self.#field_name = match value {
                        Some(value) => {
                            let payload = value.value.downcast::<#payload>().map_err(|_| #type_error)?;
                            ::phoxal::runtime::Setpoint::from_wire_parts(
                                *payload,
                                value.source,
                                value.issued_at,
                                value.valid_until,
                            )
                        }
                        None => ::phoxal::runtime::Setpoint::withdrawn(),
                    };
                    Ok(())
                }
            };
        }
        InputKind::Stream => {
            let payload = generic_type(ty, 1, item)?;
            sink.stream = quote! {
                #field_text => {
                    let mut items = ::std::vec::Vec::with_capacity(values.len());
                    for value in values {
                        items.push(match value {
                            ::phoxal::runtime::input::TransportStreamItem::Data(item) => {
                                let value = item.value.downcast::<#payload>().map_err(|_| #type_error)?;
                                ::phoxal::runtime::StreamItem::Data(
                                    ::phoxal::runtime::Sample::new(*value, item.stamp),
                                )
                            }
                            ::phoxal::runtime::input::TransportStreamItem::Gap =>
                                ::phoxal::runtime::StreamItem::Gap,
                            ::phoxal::runtime::input::TransportStreamItem::End =>
                                ::phoxal::runtime::StreamItem::End,
                            ::phoxal::runtime::input::TransportStreamItem::Failed(reason) =>
                                ::phoxal::runtime::StreamItem::Failed(
                                    ::phoxal::runtime::StreamFailure::new(reason),
                                ),
                        });
                    }
                    self.#field_name = ::phoxal::runtime::Stream::new(items);
                    Ok(())
                }
            };
        }
        InputKind::Commands => {
            let (request, _response) = generic_types(ty, 2)?;
            let max_items = options.max_items.ok_or_else(|| {
                syn::Error::new_spanned(item, "Commands requires max_items = ...")
            })?;
            let max_bytes = options.max_bytes.ok_or_else(|| {
                syn::Error::new_spanned(item, "Commands requires max_bytes = ...")
            })?;
            sink.commands = quote! {
                #field_text => {
                    let mut items = ::std::vec::Vec::with_capacity(values.len());
                    for value in values {
                        let request = value.request.downcast::<#request>().map_err(|_| #type_error)?;
                        items.push(::phoxal::runtime::Command::with_source_order(
                            value.order,
                            value.source,
                            *request,
                        ));
                    }
                    let capacity = ::phoxal::runtime::Capacity::new(#max_items, #max_bytes)
                        .map_err(|error| ::phoxal::__private::anyhow::anyhow!(error))?;
                    self.#field_name = ::phoxal::runtime::Commands::bounded(
                        items,
                        encoded_bytes,
                        capacity,
                    )
                    .map_err(|error| ::phoxal::__private::anyhow::anyhow!(error))?;
                    Ok(())
                }
            };
        }
        InputKind::Read => {
            let (key, _request, response) = generic_types3(ty, item)?;
            sink.read = quote! {
                #field_text => {
                    let key = key.downcast::<#key>().map_err(|_| #type_error)?;
                    let result = match result {
                        Ok(value) => {
                            let value = value.downcast::<#response>().map_err(|_| #type_error)?;
                            Ok(*value)
                        }
                        Err(error) => Err(error),
                    };
                    self.#field_name.admit(*key, result, provenance);
                    Ok(())
                }
            };
        }
        InputKind::Request => {
            let (key, _request, response) = generic_types3(ty, item)?;
            sink.request = quote! {
                #field_text => {
                    let key = key.downcast::<#key>().map_err(|_| #type_error)?;
                    let result = match result {
                        Ok(value) => {
                            let value = value.downcast::<#response>().map_err(|_| #type_error)?;
                            Ok(*value)
                        }
                        Err(error) => Err(error),
                    };
                    self.#field_name.admit(*key, result);
                    Ok(())
                }
            };
        }
        InputKind::Operation => {
            let (key, response) = generic_types2(ty, item)?;
            sink.operation = quote! {
                #field_text => {
                    let key = key.downcast::<#key>().map_err(|_| #type_error)?;
                    let result = match result {
                        Ok(value) => {
                            let value = value.downcast::<#response>().map_err(|_| #type_error)?;
                            Ok(*value)
                        }
                        Err(error) => Err(error),
                    };
                    self.#field_name.admit(*key, result);
                    Ok(())
                }
            };
        }
        InputKind::Completions => {}
    }
    Ok(sink)
}

fn prost_bound<T: quote::ToTokens>(ty: &T) -> TokenStream {
    quote! {
        #ty: ::phoxal::runtime::transport::ProstPayload,
    }
}

fn generic_type(ty: &Type, expected: usize, item: &Field) -> syn::Result<Type> {
    let Type::Path(path) = ty else {
        return Err(syn::Error::new_spanned(
            item,
            "runtime input type must be a generic path",
        ));
    };
    let segment = path.path.segments.last().ok_or_else(|| {
        syn::Error::new_spanned(item, "runtime input type has no generic payload")
    })?;
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return Err(syn::Error::new_spanned(
            item,
            "runtime input type is missing its payload",
        ));
    };
    let types = arguments
        .args
        .iter()
        .filter_map(|argument| match argument {
            GenericArgument::Type(ty) => Some(ty.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if types.len() != expected {
        return Err(syn::Error::new_spanned(
            item,
            format!("runtime input expects {expected} type parameters"),
        ));
    }
    Ok(types[0].clone())
}

fn generic_types2(ty: &Type, item: &Field) -> syn::Result<(Type, Type)> {
    let Type::Path(path) = ty else {
        return Err(syn::Error::new_spanned(
            item,
            "runtime input type must be a generic path",
        ));
    };
    let segment = path.path.segments.last().ok_or_else(|| {
        syn::Error::new_spanned(item, "runtime input type has no generic payload")
    })?;
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return Err(syn::Error::new_spanned(
            item,
            "runtime input type is missing its payloads",
        ));
    };
    let types = arguments
        .args
        .iter()
        .filter_map(|argument| match argument {
            GenericArgument::Type(ty) => Some(ty.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if types.len() != 2 {
        return Err(syn::Error::new_spanned(
            item,
            "runtime input expects 2 type parameters",
        ));
    }
    Ok((types[0].clone(), types[1].clone()))
}

fn generic_types3(ty: &Type, item: &Field) -> syn::Result<(Type, Type, Type)> {
    let Type::Path(path) = ty else {
        return Err(syn::Error::new_spanned(
            item,
            "runtime input type must be a generic path",
        ));
    };
    let segment = path.path.segments.last().ok_or_else(|| {
        syn::Error::new_spanned(item, "runtime input type has no generic payload")
    })?;
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return Err(syn::Error::new_spanned(
            item,
            "runtime input type is missing its payloads",
        ));
    };
    let types = arguments
        .args
        .iter()
        .filter_map(|argument| match argument {
            GenericArgument::Type(ty) => Some(ty.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if types.len() != 3 {
        return Err(syn::Error::new_spanned(
            item,
            "runtime input expects 3 type parameters",
        ));
    }
    Ok((types[0].clone(), types[1].clone(), types[2].clone()))
}

fn parse_options(
    attribute: &syn::Attribute,
    kind: InputKind,
    options: &mut Options,
) -> syn::Result<()> {
    attribute.parse_nested_meta(|meta| {
        let name = meta
            .path
            .get_ident()
            .map(Ident::to_string)
            .ok_or_else(|| meta.error("runtime input options must use identifier names"))?;
        match name.as_str() {
            "max_age_ms" if kind == InputKind::Latest => {
                set_u64(&mut options.max_age_ms, &meta, "max_age_ms")?;
            }
            "max_items"
                if matches!(
                    kind,
                    InputKind::Samples
                        | InputKind::Events
                        | InputKind::Stream
                        | InputKind::Commands
                ) =>
            {
                set_u64(&mut options.max_items, &meta, "max_items")?;
            }
            "max_bytes"
                if matches!(
                    kind,
                    InputKind::Samples
                        | InputKind::Events
                        | InputKind::Stream
                        | InputKind::Commands
                        | InputKind::Setpoint
                ) =>
            {
                set_u64(&mut options.max_bytes, &meta, "max_bytes")?;
            }
            "max_response_bytes" if matches!(kind, InputKind::Read | InputKind::Request) => {
                set_u64(&mut options.max_bytes, &meta, "max_response_bytes")?;
            }
            "port" if matches!(kind, InputKind::Commands | InputKind::Setpoint) => {
                if options.port.is_some() {
                    return Err(meta.error("duplicate runtime input option `port`"));
                }
                options.port = Some(meta.value()?.parse::<Expr>()?);
            }
            _ => {
                return Err(meta.error(format!(
                    "option `{name}` is not allowed on this runtime input type"
                )));
            }
        }
        Ok(())
    })
}

fn set_u64(
    slot: &mut Option<u64>,
    meta: &syn::meta::ParseNestedMeta<'_>,
    name: &str,
) -> syn::Result<()> {
    if slot.is_some() {
        return Err(meta.error(format!("duplicate runtime input option `{name}`")));
    }
    let expression: Expr = meta.value()?.parse()?;
    let Expr::Lit(ExprLit {
        lit: Lit::Int(value),
        ..
    }) = expression
    else {
        return Err(meta.error(format!("`{name}` must be a positive integer literal")));
    };
    let parsed = value.base10_parse::<u64>()?;
    if parsed == 0 {
        return Err(meta.error(format!("`{name}` must be positive")));
    }
    *slot = Some(parsed);
    Ok(())
}

fn validate_options(kind: InputKind, options: &Options, field: &Field) -> syn::Result<()> {
    let needs_batch_bound = matches!(
        kind,
        InputKind::Samples | InputKind::Events | InputKind::Stream | InputKind::Commands
    );
    if needs_batch_bound && (options.max_items.is_none() || options.max_bytes.is_none()) {
        return Err(syn::Error::new_spanned(
            field,
            "this runtime input requires both max_items and max_bytes",
        ));
    }
    if matches!(kind, InputKind::Read | InputKind::Request) && options.max_bytes.is_none() {
        return Err(syn::Error::new_spanned(
            field,
            "this runtime input requires max_response_bytes",
        ));
    }
    if kind == InputKind::Commands && options.port.is_none() {
        return Err(syn::Error::new_spanned(
            field,
            "Commands requires port = public::CONSTANT",
        ));
    }
    if kind == InputKind::Setpoint && options.port.is_some() && options.max_bytes.is_none() {
        return Err(syn::Error::new_spanned(
            field,
            "a bound Setpoint input requires max_bytes",
        ));
    }
    if !matches!(kind, InputKind::Commands | InputKind::Setpoint) && options.port.is_some() {
        return Err(syn::Error::new_spanned(
            field,
            "port is allowed only on a Commands or Setpoint input",
        ));
    }
    Ok(())
}

fn input_type_check(ty: &Type, field: &Ident) -> TokenStream {
    let name = format_ident!("__phoxal_input_type_{}", field);
    quote! {
        fn #name() {
            fn check<T: ::phoxal::runtime::input::InputSpec>() {}
            check::<#ty>();
        }
    }
}

fn generic_types(ty: &Type, expected: usize) -> syn::Result<(Type, Type)> {
    let Type::Path(path) = ty else {
        return Err(syn::Error::new_spanned(
            ty,
            "runtime input type must be a generic path",
        ));
    };
    let segment = path
        .path
        .segments
        .last()
        .ok_or_else(|| syn::Error::new_spanned(ty, "missing type segment"))?;
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return Err(syn::Error::new_spanned(
            ty,
            "runtime input type is missing its generic payloads",
        ));
    };
    let types = arguments
        .args
        .iter()
        .filter_map(|argument| match argument {
            GenericArgument::Type(ty) => Some(ty.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if types.len() != expected {
        return Err(syn::Error::new_spanned(
            ty,
            format!("runtime input expects {expected} type parameters"),
        ));
    }
    Ok((types[0].clone(), types[1].clone()))
}

fn option_tokens(value: Option<u64>) -> TokenStream {
    value.map_or_else(|| quote!(None), |value| quote!(Some(#value)))
}

/// Produces the shared type-level identity used to connect an input field to
/// an activation or reply collector in another declaration.
pub(crate) fn field_id(name: &Ident) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in name.to_string().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3_u64);
    }
    hash
}
