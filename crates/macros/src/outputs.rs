//! Expansion for the `runtime::outputs` collector family.

use proc_macro2::{Span, TokenStream};
use quote::{ToTokens, format_ident, quote};
use syn::{
    Expr, ExprLit, Field, Fields, FnArg, GenericArgument, Ident, ImplItem, ImplItemFn, ItemImpl,
    ItemStruct, Lit, Path, ReturnType, Type,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    State,
    Sample,
    Event,
    Stream,
    Setpoint,
    Read,
    Reply,
    Activate,
    Operation,
}

impl Role {
    fn kind(self) -> &'static str {
        match self {
            Self::State => "State",
            Self::Sample => "Sample",
            Self::Event => "Event",
            Self::Stream => "Stream",
            Self::Setpoint => "Setpoint",
            Self::Read => "Read",
            Self::Reply => "Reply",
            Self::Activate => "Activate",
            Self::Operation => "Operation",
        }
    }

    fn served(self) -> bool {
        matches!(
            self,
            Self::State | Self::Sample | Self::Event | Self::Stream | Self::Setpoint | Self::Read
        )
    }
}

#[derive(Default)]
struct Options {
    selector: Option<Ident>,
    saw_named_argument: bool,
    port: Option<Expr>,
    project: Option<Path>,
    max_items: Option<u64>,
    max_bytes: Option<u64>,
    max_request_bytes: Option<u64>,
    max_response_bytes: Option<u64>,
    every_steps: Option<u64>,
    valid_for_ms: Option<u64>,
    timeout_ms: Option<u64>,
    cancel_grace_ms: Option<u64>,
    refresh_every_steps: Option<u64>,
    on_change: bool,
    bootstrap: bool,
}

/// Expands an output struct or an inherent service output implementation.
pub fn expand_outputs(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    if !attr.is_empty() {
        return Err(syn::Error::new(
            Span::call_site(),
            "#[phoxal::runtime::outputs] takes no arguments",
        ));
    }
    if let Ok(mut output) = syn::parse2::<ItemStruct>(item.clone()) {
        return expand_output_struct(&mut output);
    }
    let mut implementation: ItemImpl = syn::parse2(item)?;
    expand_output_impl(&mut implementation)
}

/// Nested output markers are consumed by `#[outputs]`.
pub fn expand_marker(
    role: Role,
    _attr: TokenStream,
    _item: TokenStream,
) -> syn::Result<TokenStream> {
    Err(syn::Error::new(
        Span::call_site(),
        format!(
            "#[phoxal::runtime::outputs::{}] is valid only inside #[phoxal::runtime::outputs]",
            role.kind().to_lowercase()
        ),
    ))
}

fn expand_output_struct(output: &mut ItemStruct) -> syn::Result<TokenStream> {
    if !output.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &output.generics,
            "#[phoxal::runtime::outputs] does not support generic output structs",
        ));
    }
    let name = output.ident.clone();
    let fields = match &mut output.fields {
        Fields::Named(fields) => &mut fields.named,
        _ => {
            return Err(syn::Error::new_spanned(
                output,
                "#[phoxal::runtime::outputs] requires a struct with named fields",
            ));
        }
    };
    let mut metadata = Vec::new();
    let mut checks = Vec::new();
    for field in fields.iter_mut() {
        let Some(field_name) = field.ident.clone() else {
            return Err(syn::Error::new_spanned(
                field,
                "runtime output fields need names",
            ));
        };
        let mut markers = Vec::new();
        let mut retained = Vec::new();
        for attribute in field.attrs.drain(..) {
            if let Some(role) = output_role(&attribute) {
                markers.push((role, attribute));
            } else {
                retained.push(attribute);
            }
        }
        field.attrs = retained;
        if markers.len() != 1 {
            return Err(syn::Error::new_spanned(
                field,
                "each runtime output field needs exactly one role marker",
            ));
        }
        let Some((role, attribute)) = markers.pop() else {
            return Err(syn::Error::new_spanned(
                field,
                "each runtime output field needs exactly one role marker",
            ));
        };
        if !matches!(
            role,
            Role::Reply | Role::Sample | Role::Event | Role::Stream
        ) {
            return Err(syn::Error::new_spanned(
                field,
                "state, setpoint, read, activate, and operation markers belong on methods",
            ));
        }
        let mut options = Options::default();
        parse_options(&attribute, role, &mut options)?;
        validate_options(role, &options, field)?;
        let field_type = field.ty.clone();
        let payload = vector_payload(&field_type, role, field)?;
        if let Some(port) = options.port.as_ref() {
            let check_name = format_ident!(
                "__phoxal_{}_port_{}",
                role.kind().to_lowercase(),
                field_name
            );
            let check = match role {
                Role::Sample => quote! {
                    fn #check_name() {
                        ::phoxal::runtime::__private::assert_sample_port::<_, #payload>(#port);
                    }
                },
                Role::Event => quote! {
                    fn #check_name() {
                        ::phoxal::runtime::__private::assert_event_port::<_, #payload>(#port);
                    }
                },
                Role::Stream => quote! {
                    fn #check_name() {
                        ::phoxal::runtime::__private::assert_stream_port::<_, #payload>(#port);
                    }
                },
                Role::Reply => quote! {},
                _ => unreachable!(),
            };
            checks.push(check);
        }
        metadata.push(output_metadata(&field_name, role, &options));
    }

    Ok(quote! {
        #output

        impl ::phoxal::runtime::outputs::OutputSet for #name {
            const FIELDS: &'static [::phoxal::runtime::outputs::OutputField] = &[#(#metadata),*];
        }

        const _: () = {
            #(#checks)*
        };
    })
}

fn expand_output_impl(implementation: &mut ItemImpl) -> syn::Result<TokenStream> {
    if implementation.trait_.is_some() {
        return Err(syn::Error::new_spanned(
            implementation,
            "#[phoxal::runtime::outputs] applies only to an inherent impl",
        ));
    }
    if !implementation.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &implementation.generics,
            "#[phoxal::runtime::outputs] does not support generic service impls",
        ));
    }
    let self_type = (*implementation.self_ty).clone();
    let mut metadata = Vec::new();
    let mut checks = Vec::new();
    for item in &mut implementation.items {
        let ImplItem::Fn(method) = item else {
            continue;
        };
        let mut markers = Vec::new();
        let mut retained = Vec::new();
        for attribute in method.attrs.drain(..) {
            if let Some(role) = output_role(&attribute) {
                markers.push((role, attribute));
            } else {
                retained.push(attribute);
            }
        }
        method.attrs = retained;
        if markers.is_empty() {
            continue;
        }
        if markers.len() != 1 {
            return Err(syn::Error::new_spanned(
                method,
                "each runtime output method needs exactly one role marker",
            ));
        }
        let Some((role, attribute)) = markers.pop() else {
            return Err(syn::Error::new_spanned(
                method,
                "each runtime output method needs exactly one role marker",
            ));
        };
        if !role.served() && role == Role::Reply {
            return Err(syn::Error::new_spanned(
                method,
                "reply is a field output, not a method output",
            ));
        }
        let mut options = Options::default();
        parse_options(&attribute, role, &mut options)?;
        validate_options(role, &options, method)?;
        let method_name = method.sig.ident.clone();
        if role == Role::State {
            validate_projection_signature(method, "state")?;
            let return_type = normalize_borrowed(&return_type(&method.sig.output, method)?);
            let check_name = format_ident!("__phoxal_state_port_{}", method_name);
            let Some(port) = options.port.as_ref() else {
                return Err(syn::Error::new_spanned(
                    method,
                    "state output requires port = public::CONSTANT",
                ));
            };
            checks.push(quote! {
                fn #check_name() {
                    ::phoxal::runtime::__private::assert_state_port::<_, #return_type>(#port);
                }
            });
        } else if role == Role::Setpoint {
            validate_projection_signature(method, "setpoint")?;
            let return_type = normalize_borrowed(&return_type(&method.sig.output, method)?);
            let check_name = format_ident!("__phoxal_setpoint_port_{}", method_name);
            let Some(port) = options.port.as_ref() else {
                return Err(syn::Error::new_spanned(
                    method,
                    "setpoint output requires port = public::CONSTANT",
                ));
            };
            checks.push(quote! {
                fn #check_name() {
                    ::phoxal::runtime::__private::assert_setpoint_port::<_, #return_type>(#port);
                }
            });
        } else if role == Role::Read {
            validate_read_signature(method)?;
            let request = method_argument_type(method, 1)?;
            let response = return_type(&method.sig.output, method)?;
            let check_name = format_ident!("__phoxal_read_port_{}", method_name);
            let Some(port) = options.port.as_ref() else {
                return Err(syn::Error::new_spanned(
                    method,
                    "read output requires port = public::CONSTANT",
                ));
            };
            checks.push(quote! {
                fn #check_name() {
                    ::phoxal::runtime::__private::assert_read_port::<_, #request, #response>(#port);
                }
            });
        } else if role == Role::Activate {
            validate_activation_signature(method)?;
            let selector = options.selector.as_ref().ok_or_else(|| {
                syn::Error::new_spanned(
                    &*method,
                    "activation markers require an input field identifier first",
                )
            })?;
            let field_id = crate::inputs::field_id(selector);
            let actual_return = return_type(&method.sig.output, method)?;
            let check_name = format_ident!("__phoxal_activate_{}", method_name);
            let has_timeout = options.timeout_ms.is_some();
            let has_refresh = options.refresh_every_steps.is_some();
            checks.push(quote! {
                const fn #check_name<I, Actual, Service, State>(
                    _method: fn(&Service, &State) -> Actual,
                ) where
                    I: ::phoxal::runtime::input::InputFieldBinding<{#field_id}>,
                    <I as ::phoxal::runtime::input::InputFieldBinding<{#field_id}>>::Form:
                        ::phoxal::runtime::input::ActivationPolicy,
                    Actual: ::phoxal::runtime::input::ActivationFor<
                        <I as ::phoxal::runtime::input::InputFieldBinding<{#field_id}>>::Form,
                    >,
                {
                    assert!(
                        <<I as ::phoxal::runtime::input::InputFieldBinding<{#field_id}>>::Form
                            as ::phoxal::runtime::input::ActivationPolicy>::REQUIRES_TIMEOUT
                            == #has_timeout,
                        "activation timeout policy does not match its input form"
                    );
                    assert!(
                        !#has_refresh
                            || <<I as ::phoxal::runtime::input::InputFieldBinding<{#field_id}>>::Form
                                as ::phoxal::runtime::input::ActivationPolicy>::ALLOWS_REFRESH,
                        "refresh_every_steps is allowed only for Read activation"
                    );
                }

                const _: () = {
                    #check_name::<
                        <#self_type as ::phoxal::runtime::Runtime>::Inputs,
                        #actual_return,
                        #self_type,
                        <#self_type as ::phoxal::runtime::Runtime>::State,
                    >(#self_type::#method_name);
                };
            });
        } else if role == Role::Operation {
            validate_worker_signature(method)?;
            let selector = options.selector.as_ref().ok_or_else(|| {
                syn::Error::new_spanned(
                    &*method,
                    "operation markers require an input field identifier first",
                )
            })?;
            let field_id = crate::inputs::field_id(selector);
            let input = method_argument_type(method, 0)?;
            let actual_return = return_type(&method.sig.output, method)?;
            let check_name = format_ident!("__phoxal_operation_{}", method_name);
            checks.push(quote! {
                const fn #check_name<I, Input, Actual>(
                    _worker: fn(Input) -> Actual,
                ) where
                    I: ::phoxal::runtime::input::InputFieldBinding<{#field_id}>,
                    Actual: ::phoxal::runtime::input::OperationWorkerFor<
                        <I as ::phoxal::runtime::input::InputFieldBinding<{#field_id}>>::Form,
                    >,
                {}

                const _: () = {
                    #check_name::<
                        <#self_type as ::phoxal::runtime::Runtime>::Inputs,
                        #input,
                        #actual_return,
                    >(#self_type::#method_name);
                };
            });
        }
        metadata.push(output_metadata(&method_name, role, &options));
    }

    Ok(quote! {
        #implementation

        impl ::phoxal::runtime::outputs::OutputBindings for #self_type {
            const FIELDS: &'static [::phoxal::runtime::outputs::OutputField] = &[#(#metadata),*];
        }

        const _: () = {
            #(#checks)*
        };
    })
}

fn output_role(attribute: &syn::Attribute) -> Option<Role> {
    let name = attribute.path().segments.last()?.ident.to_string();
    match name.as_str() {
        "state" => Some(Role::State),
        "sample" => Some(Role::Sample),
        "event" => Some(Role::Event),
        "stream" => Some(Role::Stream),
        "setpoint" => Some(Role::Setpoint),
        "read" => Some(Role::Read),
        "reply" => Some(Role::Reply),
        "activate" => Some(Role::Activate),
        "operation" => Some(Role::Operation),
        _ => None,
    }
}

fn parse_options(attribute: &syn::Attribute, role: Role, options: &mut Options) -> syn::Result<()> {
    attribute.parse_nested_meta(|meta| {
        if matches!(role, Role::Reply | Role::Activate | Role::Operation)
            && !meta.input.peek(syn::Token![=])
            && options.selector.is_none()
        {
            if options.saw_named_argument {
                return Err(meta
                    .error("the first runtime output argument must be an input field identifier"));
            }
            let selector = meta.path.get_ident().ok_or_else(|| {
                meta.error("the first runtime output argument must be an input field identifier")
            })?;
            options.selector = Some(selector.clone());
            return Ok(());
        }
        options.saw_named_argument = true;
        let name = meta
            .path
            .get_ident()
            .map(Ident::to_string)
            .ok_or_else(|| meta.error("runtime output options must use identifier names"))?;
        match name.as_str() {
            "port"
                if matches!(
                    role,
                    Role::State
                        | Role::Sample
                        | Role::Event
                        | Role::Stream
                        | Role::Setpoint
                        | Role::Read
                ) =>
            {
                if options.port.is_some() {
                    return Err(meta.error("duplicate runtime output option `port`"));
                }
                options.port = Some(meta.value()?.parse::<Expr>()?);
            }
            "project" if role == Role::Read => {
                if options.project.is_some() {
                    return Err(meta.error("duplicate runtime output option `project`"));
                }
                options.project = Some(meta.value()?.parse::<Path>()?);
            }
            "max_items"
                if matches!(
                    role,
                    Role::Sample | Role::Event | Role::Stream | Role::Reply
                ) =>
            {
                set_u64(&mut options.max_items, &meta, "max_items")?;
            }
            "max_bytes"
                if matches!(
                    role,
                    Role::State
                        | Role::Sample
                        | Role::Event
                        | Role::Stream
                        | Role::Setpoint
                        | Role::Reply
                ) =>
            {
                set_u64(&mut options.max_bytes, &meta, "max_bytes")?;
            }
            "max_request_bytes" if role == Role::Read => {
                set_u64(&mut options.max_request_bytes, &meta, "max_request_bytes")?;
            }
            "max_response_bytes" if role == Role::Read => {
                set_u64(&mut options.max_response_bytes, &meta, "max_response_bytes")?;
            }
            "every_steps" if role == Role::State => {
                set_u64(&mut options.every_steps, &meta, "every_steps")?;
            }
            "valid_for_ms" if role == Role::Setpoint => {
                set_u64(&mut options.valid_for_ms, &meta, "valid_for_ms")?;
            }
            "timeout_ms" if matches!(role, Role::Read | Role::Activate | Role::Operation) => {
                set_u64(&mut options.timeout_ms, &meta, "timeout_ms")?;
            }
            "cancel_grace_ms" if role == Role::Operation => {
                set_u64(&mut options.cancel_grace_ms, &meta, "cancel_grace_ms")?;
            }
            "refresh_every_steps" if role == Role::Activate => {
                set_u64(
                    &mut options.refresh_every_steps,
                    &meta,
                    "refresh_every_steps",
                )?;
            }
            "on_change" if role == Role::State && !meta.input.peek(syn::Token![=]) => {
                if options.on_change {
                    return Err(meta.error("duplicate runtime output option `on_change`"));
                }
                options.on_change = true;
            }
            "bootstrap" if role == Role::State && !meta.input.peek(syn::Token![=]) => {
                if options.bootstrap {
                    return Err(meta.error("duplicate runtime output option `bootstrap`"));
                }
                options.bootstrap = true;
            }
            _ => {
                return Err(meta.error(format!(
                    "option `{name}` is not allowed on this runtime output role"
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
        return Err(meta.error(format!("duplicate runtime output option `{name}`")));
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

fn validate_options<R: ToTokens>(role: Role, options: &Options, item: &R) -> syn::Result<()> {
    if role.served() && options.port.is_none() {
        return Err(syn::Error::new_spanned(
            item,
            "served runtime outputs require port = public::CONSTANT",
        ));
    }
    if !role.served() && options.port.is_some() {
        return Err(syn::Error::new_spanned(
            item,
            "port is allowed only on served runtime outputs",
        ));
    }
    if matches!(
        role,
        Role::Sample | Role::Event | Role::Stream | Role::Reply
    ) && (options.max_items.is_none() || options.max_bytes.is_none())
    {
        return Err(syn::Error::new_spanned(
            item,
            "this runtime output requires max_items and max_bytes",
        ));
    }
    if matches!(role, Role::State | Role::Setpoint) && options.max_bytes.is_none() {
        return Err(syn::Error::new_spanned(
            item,
            "this runtime output requires max_bytes",
        ));
    }
    if role == Role::Setpoint && options.valid_for_ms.is_none() {
        return Err(syn::Error::new_spanned(
            item,
            "setpoint output requires valid_for_ms",
        ));
    }
    if role == Role::Read
        && (options.project.is_none()
            || options.max_request_bytes.is_none()
            || options.max_response_bytes.is_none())
    {
        return Err(syn::Error::new_spanned(
            item,
            "read output requires project, max_request_bytes, and max_response_bytes",
        ));
    }
    if matches!(role, Role::Activate | Role::Operation) && options.selector.is_none() {
        return Err(syn::Error::new_spanned(
            item,
            "activation and operation markers require an input field identifier first",
        ));
    }
    if role == Role::Operation && options.cancel_grace_ms.is_none() {
        return Err(syn::Error::new_spanned(
            item,
            "operation markers require cancel_grace_ms",
        ));
    }
    if role != Role::Activate && options.refresh_every_steps.is_some() {
        return Err(syn::Error::new_spanned(
            item,
            "refresh_every_steps is allowed only on activate(Read)",
        ));
    }
    Ok(())
}

fn vector_payload(ty: &Type, role: Role, field: &Field) -> syn::Result<Type> {
    let Type::Path(path) = ty else {
        return Err(syn::Error::new_spanned(
            field,
            "batch output must be Vec<T>",
        ));
    };
    let segment = path
        .path
        .segments
        .last()
        .ok_or_else(|| syn::Error::new_spanned(field, "batch output has no type name"))?;
    if segment.ident != "Vec" {
        return Err(syn::Error::new_spanned(
            field,
            "batch output must be Vec<T>",
        ));
    }
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return Err(syn::Error::new_spanned(
            field,
            "batch output is missing its item type",
        ));
    };
    let item = arguments
        .args
        .iter()
        .find_map(|argument| match argument {
            GenericArgument::Type(ty) => Some(ty.clone()),
            _ => None,
        })
        .ok_or_else(|| syn::Error::new_spanned(field, "batch output is missing its item type"))?;
    match role {
        Role::Sample => wrapped_payload(&item, "Sample", field),
        Role::Stream => wrapped_payload(&item, "StreamItem", field),
        Role::Reply => wrapped_payload(&item, "Reply", field),
        Role::Event => Ok(item),
        _ => Err(syn::Error::new_spanned(
            field,
            "this role is not a batch output",
        )),
    }
}

fn wrapped_payload(ty: &Type, wrapper: &str, item: &Field) -> syn::Result<Type> {
    let Type::Path(path) = ty else {
        return Err(syn::Error::new_spanned(
            item,
            format!("batch item must be {wrapper}<T>"),
        ));
    };
    let segment = path
        .path
        .segments
        .last()
        .ok_or_else(|| syn::Error::new_spanned(item, "batch item has no type name"))?;
    if segment.ident != wrapper {
        return Err(syn::Error::new_spanned(
            item,
            format!("batch item must be {wrapper}<T>"),
        ));
    }
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return Err(syn::Error::new_spanned(
            item,
            "batch item is missing its payload type",
        ));
    };
    arguments
        .args
        .iter()
        .find_map(|argument| match argument {
            GenericArgument::Type(ty) => Some(ty.clone()),
            _ => None,
        })
        .ok_or_else(|| syn::Error::new_spanned(item, "batch item is missing its payload type"))
}

fn return_type(output: &ReturnType, method: &ImplItemFn) -> syn::Result<Type> {
    match output {
        ReturnType::Type(_, ty) => Ok((**ty).clone()),
        ReturnType::Default => Err(syn::Error::new_spanned(
            method,
            "runtime output method needs an explicit return type",
        )),
    }
}

fn normalize_borrowed(ty: &Type) -> Type {
    if let Type::Reference(reference) = ty {
        return (*reference.elem).clone();
    }
    let Type::Path(path) = ty else {
        return ty.clone();
    };
    let Some(segment) = path.path.segments.last() else {
        return ty.clone();
    };
    if segment.ident != "Option" {
        return ty.clone();
    }
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return ty.clone();
    };
    let Some(GenericArgument::Type(inner)) = arguments.args.first() else {
        return ty.clone();
    };
    let normalized = normalize_borrowed(inner);
    if normalized == *inner {
        return ty.clone();
    }
    let mut normalized_path = path.clone();
    let Some(segment) = normalized_path.path.segments.last_mut() else {
        return ty.clone();
    };
    let syn::PathArguments::AngleBracketed(arguments) = &mut segment.arguments else {
        return ty.clone();
    };
    if let Some(GenericArgument::Type(inner)) = arguments.args.first_mut() {
        *inner = normalized;
    }
    Type::Path(syn::TypePath {
        qself: None,
        path: normalized_path.path,
    })
}

fn method_argument_type(method: &ImplItemFn, index: usize) -> syn::Result<Type> {
    let arguments = method
        .sig
        .inputs
        .iter()
        .filter(|argument| !matches!(argument, FnArg::Receiver(_)))
        .collect::<Vec<_>>();
    let argument = arguments.get(index).ok_or_else(|| {
        syn::Error::new_spanned(
            method,
            "runtime read handler needs view and request arguments",
        )
    })?;
    let FnArg::Typed(argument) = argument else {
        return Err(syn::Error::new_spanned(
            method,
            "runtime read handler arguments must be typed",
        ));
    };
    match argument.ty.as_ref() {
        Type::Reference(reference) => Ok((*reference.elem).clone()),
        ty => Ok(ty.clone()),
    }
}

fn validate_projection_signature(method: &ImplItemFn, role: &str) -> syn::Result<()> {
    validate_shared_receiver(method, role)?;
    validate_one_shared_state_argument(method, role)
}

fn validate_activation_signature(method: &ImplItemFn) -> syn::Result<()> {
    validate_shared_receiver(method, "activation")?;
    validate_one_shared_state_argument(method, "activation")
}

fn validate_read_signature(method: &ImplItemFn) -> syn::Result<()> {
    validate_shared_receiver(method, "read")?;
    if method
        .sig
        .inputs
        .iter()
        .filter(|arg| !is_receiver(arg))
        .count()
        != 2
    {
        return Err(syn::Error::new_spanned(
            method,
            "read output method needs exactly view and request arguments",
        ));
    }
    for argument in method.sig.inputs.iter().filter(|arg| !is_receiver(arg)) {
        let FnArg::Typed(argument) = argument else {
            return Err(syn::Error::new_spanned(
                argument,
                "read output arguments must be typed",
            ));
        };
        if !is_shared_reference(&argument.ty) {
            return Err(syn::Error::new_spanned(
                &argument.ty,
                "read output view and request arguments must be shared references",
            ));
        }
    }
    Ok(())
}

fn validate_worker_signature(method: &ImplItemFn) -> syn::Result<()> {
    if method.sig.asyncness.is_some() {
        return Err(syn::Error::new_spanned(
            method,
            "asynchronous operation workers are not supported by the direct runtime profile",
        ));
    }
    if let Some(receiver) = method.sig.receiver() {
        return Err(syn::Error::new_spanned(
            receiver.self_token,
            "operation workers must not borrow the service",
        ));
    }
    let arguments = method.sig.inputs.iter().collect::<Vec<_>>();
    if arguments.len() != 1 {
        return Err(syn::Error::new_spanned(
            method,
            "operation worker needs exactly one owned input argument",
        ));
    }
    let FnArg::Typed(argument) = arguments[0] else {
        return Err(syn::Error::new_spanned(
            method,
            "operation worker input must be typed",
        ));
    };
    if matches!(argument.ty.as_ref(), Type::Reference(_)) {
        return Err(syn::Error::new_spanned(
            &argument.ty,
            "operation worker input must be owned",
        ));
    }
    if matches!(method.sig.output, ReturnType::Default) {
        return Err(syn::Error::new_spanned(
            method,
            "operation worker needs an explicit Result return type",
        ));
    }
    Ok(())
}

fn validate_shared_receiver(method: &ImplItemFn, role: &str) -> syn::Result<()> {
    if method.sig.asyncness.is_some() {
        return Err(syn::Error::new_spanned(
            method,
            format!("asynchronous {role} methods are not supported by the direct runtime profile"),
        ));
    }
    let Some(receiver) = method.sig.receiver() else {
        return Err(syn::Error::new_spanned(
            method,
            format!("{role} output methods must borrow the service as `&self`"),
        ));
    };
    if receiver.reference.is_none() || receiver.mutability.is_some() {
        return Err(syn::Error::new_spanned(
            receiver,
            format!("{role} output methods must borrow the service as `&self`"),
        ));
    }
    Ok(())
}

fn validate_one_shared_state_argument(method: &ImplItemFn, role: &str) -> syn::Result<()> {
    if method
        .sig
        .inputs
        .iter()
        .filter(|arg| !is_receiver(arg))
        .count()
        != 1
    {
        return Err(syn::Error::new_spanned(
            method,
            format!("{role} output method needs exactly one state argument"),
        ));
    }
    let Some(argument) = method.sig.inputs.iter().find(|arg| !is_receiver(arg)) else {
        return Err(syn::Error::new_spanned(
            method,
            format!("{role} output method needs exactly one state argument"),
        ));
    };
    let FnArg::Typed(argument) = argument else {
        return Err(syn::Error::new_spanned(
            argument,
            format!("{role} output state argument must be typed"),
        ));
    };
    if !is_shared_reference(&argument.ty) {
        return Err(syn::Error::new_spanned(
            &argument.ty,
            format!("{role} output state argument must be a shared reference"),
        ));
    }
    Ok(())
}

fn is_shared_reference(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Reference(reference) if reference.mutability.is_none()
    )
}

fn is_receiver(argument: &FnArg) -> bool {
    matches!(argument, FnArg::Receiver(_))
}

fn output_metadata(name: &Ident, role: Role, options: &Options) -> TokenStream {
    let kind = Ident::new(role.kind(), name.span());
    let port = options
        .port
        .as_ref()
        .map_or_else(|| quote!(None), |port| quote!(Some((#port).name())));
    let input = options.selector.as_ref().map_or_else(
        || quote!(None),
        |selector| quote!(Some(stringify!(#selector))),
    );
    let project = options.project.as_ref().map_or_else(
        || quote!(None),
        |project| quote!(Some(stringify!(#project))),
    );
    let max_items = option_tokens(options.max_items);
    let max_bytes = option_tokens(options.max_bytes);
    let max_request_bytes = option_tokens(options.max_request_bytes);
    let every_steps = option_tokens(options.every_steps);
    let valid_for_ms = option_tokens(options.valid_for_ms);
    let timeout_ms = option_tokens(options.timeout_ms);
    let cancel_grace_ms = option_tokens(options.cancel_grace_ms);
    let on_change = options.on_change;
    let bootstrap = options.bootstrap;
    quote! {
        ::phoxal::runtime::outputs::OutputField {
            name: stringify!(#name),
            kind: ::phoxal::runtime::outputs::OutputKind::#kind,
            port: #port,
            input: #input,
            project: #project,
            max_items: #max_items,
            max_bytes: #max_bytes,
            max_request_bytes: #max_request_bytes,
            every_steps: #every_steps,
            on_change: #on_change,
            bootstrap: #bootstrap,
            valid_for_ms: #valid_for_ms,
            timeout_ms: #timeout_ms,
            cancel_grace_ms: #cancel_grace_ms,
        }
    }
}

fn option_tokens(value: Option<u64>) -> TokenStream {
    value.map_or_else(|| quote!(None), |value| quote!(Some(#value)))
}
