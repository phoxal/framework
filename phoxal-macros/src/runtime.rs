//! Expansion for the root `runtime` registration attribute.

use heck::ToSnakeCase;
use proc_macro2::{Span, TokenStream};
use quote::{ToTokens, format_ident, quote};
use syn::parse::Parser;
use syn::{Expr, ExprLit, ImplItemFn, ItemImpl, Lit, Path, Type};

use crate::message::{anchored, anchored_type};

/// Expands `#[phoxal::runtime(contract = ..., period_ms = ..., timeout_ms = ..., init_timeout_ms = ...)]`.
pub fn expand_runtime(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let (options, contract) = parse_options(attr)?;
    let implementation: ItemImpl = syn::parse2(item)?;
    if implementation.trait_.is_some() {
        return Err(syn::Error::new_spanned(
            &implementation,
            "#[phoxal::runtime] requires an inherent impl with contract = ...; manual Runtime implementations are not an authoring API",
        ));
    }
    expand_inherent(implementation, options, contract)
}

/// One annotated method of an authored runtime impl, with its helper
/// attribute stripped and its signature facts extracted.
struct AuthoredInit {
    method: ImplItemFn,
    with_init_context: bool,
    config: Type,
}

struct AuthoredHandle {
    endpoint: syn::Ident,
    method: ImplItemFn,
    mutable_context: bool,
    payload: Type,
    output: syn::ReturnType,
}

/// One `#[complete(endpoint)]` direct completion handler.
struct AuthoredCompletion {
    endpoint: syn::Ident,
    method: ImplItemFn,
    response: Type,
}

/// Expands `#[phoxal::runtime(contract = ...)]` on one inherent impl block.
///
/// The authored struct owns its state in ordinary fields; the expansion
/// lowers the block into a private runtime adapter whose `Runtime::State` is
/// the authored struct itself, preserving the owner's move-in/move-out
/// failure model. One `#[init]` constructor, an optional `#[step]`, one
/// `#[handle(endpoint)]` per admitted operation or queued event, and one
/// `#[publish(endpoint)]` per projected output are wired through the
/// contract's generated endpoint view and dispatch driver.
fn expand_inherent(
    mut implementation: ItemImpl,
    options: Options,
    contract: Option<Path>,
) -> syn::Result<TokenStream> {
    if !implementation.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &implementation.generics,
            "#[phoxal::runtime] does not support generic runtime implementations",
        ));
    }
    let contract = contract.ok_or_else(|| {
        syn::Error::new_spanned(
            &implementation,
            "an authored runtime attaches one endpoint contract; declare contract = ...",
        )
    })?;
    let self_type = (*implementation.self_ty).clone();
    let Type::Path(path) = &self_type else {
        return Err(syn::Error::new_spanned(
            &self_type,
            "#[phoxal::runtime] requires a named runtime type",
        ));
    };
    if path
        .path
        .segments
        .last()
        .is_none_or(|segment| !segment.arguments.is_none())
    {
        return Err(syn::Error::new_spanned(
            &self_type,
            "#[phoxal::runtime] requires a plain named runtime type",
        ));
    }
    let runtime_name = path
        .path
        .segments
        .last()
        .map(|segment| segment.ident.clone())
        .ok_or_else(|| syn::Error::new_spanned(&self_type, "runtime type has no name"))?;
    let options = options.with_inherent_defaults()?;

    let mut init: Option<AuthoredInit> = None;
    let mut step: Option<ImplItemFn> = None;
    let mut handles: Vec<AuthoredHandle> = Vec::new();
    let mut publishes: Vec<(syn::Ident, ImplItemFn)> = Vec::new();
    let mut completions: Vec<AuthoredCompletion> = Vec::new();

    for item in std::mem::take(&mut implementation.items) {
        let syn::ImplItem::Fn(mut method) = item else {
            implementation.items.push(item);
            continue;
        };
        let mut kind: Option<syn::Ident> = None;
        let mut endpoint: Option<syn::Ident> = None;
        let mut remaining_attrs = Vec::new();
        for attribute in std::mem::take(&mut method.attrs) {
            let Some(name) = attribute.path().get_ident() else {
                remaining_attrs.push(attribute);
                continue;
            };
            match name.to_string().as_str() {
                "init" | "step" => {
                    if !matches!(attribute.meta, syn::Meta::Path(_)) {
                        return Err(syn::Error::new_spanned(
                            &attribute,
                            format!("#[{name}] takes no options"),
                        ));
                    }
                    if kind.replace(name.clone()).is_some() {
                        return Err(syn::Error::new_spanned(
                            &method,
                            "a method declares one runtime helper attribute",
                        ));
                    }
                }
                "handle" | "publish" => {
                    let mut names = Vec::new();
                    attribute.parse_nested_meta(|meta| {
                        let Some(ident) = meta.path.get_ident() else {
                            return Err(meta.error(format!("{name} names one endpoint")));
                        };
                        names.push(ident.clone());
                        Ok(())
                    })?;
                    let [selected] = &names[..] else {
                        return Err(syn::Error::new_spanned(
                            &attribute,
                            format!("{name} names exactly one endpoint"),
                        ));
                    };
                    let selected = selected.clone();
                    if kind.is_some() {
                        return Err(syn::Error::new_spanned(
                            &method,
                            "a method declares one runtime helper attribute",
                        ));
                    }
                    kind = Some(name.clone());
                    endpoint = Some(selected.clone());
                }
                "complete" => {
                    let mut names = Vec::new();
                    attribute.parse_nested_meta(|meta| {
                        let Some(ident) = meta.path.get_ident() else {
                            return Err(meta.error("complete names one call endpoint"));
                        };
                        names.push(ident.clone());
                        Ok(())
                    })?;
                    let [selected] = &names[..] else {
                        return Err(syn::Error::new_spanned(
                            &attribute,
                            "complete names exactly one call endpoint",
                        ));
                    };
                    let selected = selected.clone();
                    if kind.is_some() {
                        return Err(syn::Error::new_spanned(
                            &method,
                            "a method declares one runtime helper attribute",
                        ));
                    }
                    kind = Some(name.clone());
                    endpoint = Some(selected.clone());
                }
                _ => remaining_attrs.push(attribute),
            }
        }
        method.attrs = remaining_attrs;
        let Some(kind) = kind else {
            implementation.items.push(syn::ImplItem::Fn(method));
            continue;
        };
        implementation.items.push(syn::ImplItem::Fn(method.clone()));
        match (kind.to_string().as_str(), endpoint) {
            ("init", _) => {
                if init.is_some() {
                    return Err(syn::Error::new_spanned(
                        &method,
                        "an authored runtime declares one #[init] constructor",
                    ));
                }
                let (with_init_context, config) = init_signature(&method)?;
                init = Some(AuthoredInit {
                    method,
                    with_init_context,
                    config,
                });
            }
            ("step", _) => {
                if step.is_some() {
                    return Err(syn::Error::new_spanned(
                        &method,
                        "an authored runtime declares at most one #[step]",
                    ));
                }
                step_signature(&method)?;
                step = Some(method);
            }
            ("complete", Some(endpoint)) => {
                if completions
                    .iter()
                    .any(|completion| completion.endpoint == endpoint)
                {
                    return Err(syn::Error::new_spanned(
                        &method,
                        format!("endpoint `{endpoint}` has more than one #[complete]"),
                    ));
                }
                let response = completion_signature(&method)?;
                completions.push(AuthoredCompletion {
                    endpoint,
                    method,
                    response,
                });
            }
            ("handle", Some(endpoint)) => {
                if handles.iter().any(|handle| handle.endpoint == endpoint) {
                    return Err(syn::Error::new_spanned(
                        &method,
                        format!("endpoint `{endpoint}` has more than one #[handle]"),
                    ));
                }
                let (mutable_context, payload, output) = handle_signature(&method)?;
                handles.push(AuthoredHandle {
                    endpoint,
                    method,
                    mutable_context,
                    payload,
                    output,
                });
            }
            ("publish", Some(endpoint)) => {
                if publishes.iter().any(|(known, _)| *known == endpoint) {
                    return Err(syn::Error::new_spanned(
                        &method,
                        format!("endpoint `{endpoint}` has more than one #[publish]"),
                    ));
                }
                publish_signature(&method)?;
                publishes.push((endpoint, method));
            }
            _ => unreachable!("kind and endpoint pairing is generated above"),
        }
    }

    let Some(AuthoredInit {
        method: init_method,
        with_init_context,
        config: config_type,
    }) = init
    else {
        return Err(syn::Error::new_spanned(
            &implementation,
            "an authored runtime declares one #[init] constructor",
        ));
    };
    let init_name = init_method.sig.ident.clone();
    // The adapter module sits one scope below the authored impl: both the
    // state type and the initializer's configuration type must be re-anchored
    // to that scope, so an authored spelling like `Settings`,
    // `settings::Settings`, `crate::Config`, or `super::Peer::Config`
    // resolves exactly as it does at the declaration site.
    let state_path = match &self_type {
        Type::Path(type_path) => {
            let anchored_path = anchored(&type_path.path, 1);
            quote! { #anchored_path }
        }
        other => quote! { #other },
    };
    let anchored_config = anchored_type(&config_type, 1);
    let init_call = if with_init_context {
        quote! { #state_path::#init_name(ctx, config) }
    } else {
        quote! { #state_path::#init_name(config) }
    };

    let step_call = match step {
        Some(method) => {
            let step_name = method.sig.ident.clone();
            quote! {
                {
                    let mut context = resources.context::<#state_path>(ctx, inputs, &mut outputs);
                    #state_path::#step_name(&mut state, &mut context)?;
                }
            }
        }
        None => quote! {},
    };

    // Direct completion handlers run before the periodic step, routed by
    // recorded owner: the central retained store is enumerated once, each
    // claimed ticket is delivered to exactly the field whose accepted call
    // staged it (declaration order never decides delivery), tree-owned
    // tickets stay for their leaf, and unowned results — foreign, retired,
    // or from another era — are dropped from the store so their item and
    // byte charges are released instead of lingering forever.
    let completion_dispatch = if completions.is_empty() {
        quote! {}
    } else {
        let arms = completions.iter().map(|completion| {
            let AuthoredCompletion {
                endpoint,
                method,
                response,
            } = completion;
            let method_name = method.sig.ident.clone();
            let endpoint_str = endpoint.to_string();
            quote! {
                #endpoint_str => {
                    let completion = ::phoxal::runtime::input::CallCompletion::<#response>::
                        from_transport_result(ticket, result)?;
                    let mut context = resources.context::<#state_path>(ctx, inputs, &mut outputs);
                    #state_path::#method_name(&mut state, &mut context, completion)?;
                }
            }
        });
        quote! {
            for ticket in inputs.store_ticket_ids() {
                if let ::std::option::Option::Some(field) = resources.pending.claim_direct(ticket)
                    && let ::std::option::Option::Some(result) = inputs.take_completion_raw(ticket)
                {
                    match field {
                        #(#arms)*
                        _ => ::core::unreachable!(
                            "a claimed direct owner names a field of this contract"
                        ),
                    }
                } else if !resources.pending.is_any_tree_call(ticket) {
                    // No owner may ever claim this result: release its
                    // retained charges instead of refusing delivery
                    // forever.
                    inputs.discard_completion(ticket);
                }
            }
        }
    };

    // The Handlers impl is emitted even for a runtime with no handled
    // endpoints: `phoxal_dispatch` requires the bound for every attached
    // runtime, and an empty impl is the complete implementation when the
    // contract declares no operations or queued events.
    let handler_entries = handles.iter().map(|handle| {
        let AuthoredHandle {
            endpoint,
            method,
            mutable_context,
            payload,
            output,
        } = handle;
        let method_name = method.sig.ident.clone();
        // The registration closure's parameter and return types are fully
        // inferred from the dispatch table's setter bound, so every
        // authored spelling resolves at the impl's own scope and no
        // generated path can shadow or duplicate it.
        let _ = (payload, output);
        // An authored handler that immutably borrows its context is
        // adapted, keeping its read-only signature honest at the call
        // boundary.
        let context_argument = if *mutable_context {
            quote! { context }
        } else {
            quote! { &*context }
        };
        quote! {
            table.#endpoint(|state, context, request| {
                #state_path::#method_name(state, #context_argument, request)
            });
        }
    });
    let handler_registrations = quote! {
        #(#handler_entries)*
    };

    let projection_registrations = if publishes.is_empty() {
        quote! {}
    } else {
        let entries = publishes.iter().map(|(endpoint, method)| {
            let method_name = method.sig.ident.clone();
            quote! {
                table.#endpoint(
                    |service: &#state_path, _state: &#state_path| {
                        #state_path::#method_name(service)
                    },
                );
            }
        });
        quote! {
            #(#entries)*
        }
    };

    let module_name = format_ident!(
        "phoxal_runtime_{}",
        runtime_name.to_string().to_snake_case()
    );
    let conversion_step = crate::attachment::fragment("PHOXAL_CONVERSION_STEP")?
        .map(|fragment| syn::parse_str::<syn::Block>(&fragment))
        .transpose()?;
    let conversion_routes =
        crate::attachment::fragment("PHOXAL_CONVERSION_ROUTES")?.unwrap_or_else(|| "[]".to_owned());
    let adapter_name = format_ident!("Adapter");
    let contract_inside = anchored(&contract, 1);
    let check_name = format_ident!("inputs_check");
    let artifact_static = format_ident!("ARTIFACT");
    let period = options.period_ms;
    let timeout = options.timeout_ms;
    let init_timeout = options.init_timeout_ms;
    let spec_expression =
        quote! { ::phoxal::runtime::RuntimeSpec::from_millis(#period, #timeout, #init_timeout) };

    Ok(quote! {
        #implementation

        impl ::phoxal::runtime::EndpointView for #self_type {
            type Inputs = <#contract as ::phoxal::runtime::RuntimeContract>::Inputs;
            type Outputs = <#contract as ::phoxal::runtime::RuntimeContract>::Outputs;
            type View<'a> = <#contract as ::phoxal::runtime::RuntimeContract>::View<'a>;

            fn endpoint_view<'a>(
                step: &'a ::phoxal::runtime::StepContext,
                inputs: &'a Self::Inputs,
                outputs: &'a mut Self::Outputs,
            ) -> Self::View<'a> {
                <#contract as ::phoxal::runtime::RuntimeContract>::endpoints_view(
                    step,
                    inputs,
                    outputs,
                )
            }

            fn take_completion_bytes(
                view: &mut Self::View<'_>,
                ticket: u128,
            ) -> ::std::option::Option<
                ::std::result::Result<::std::vec::Vec<u8>, ::phoxal::runtime::input::RequestError>,
            > {
                #contract::phoxal_take_completion(view, ticket)
            }

            fn retire_completion_bytes(view: &mut Self::View<'_>, ticket: u128) {
                #contract::phoxal_retire_completion(view, ticket);
            }

            fn stage_tree_operation<O>(
                view: &mut Self::View<'_>,
                generation: u64,
                operation: O,
            ) -> ::phoxal::Result<::phoxal::runtime::CallTicket<O::Response>>
            where
                O: ::phoxal::runtime::outputs::GeneratedSend,
            {
                #contract::phoxal_stage_operation(view, generation, operation)
            }
        }

        impl ::phoxal::runtime::LaunchedRuntime for #self_type {
            const SPEC: ::phoxal::runtime::RuntimeSpec =
                <#module_name::#adapter_name as ::phoxal::runtime::RegisteredRuntime>::SPEC;
            fn artifact_metadata() -> &'static ::phoxal::runtime::artifact::ArtifactRecord {
                &#module_name::#artifact_static
            }

            fn launch() -> ::phoxal::Result<()> {
                ::phoxal::runtime::run_registered(#module_name::#adapter_name::new())
            }
        }

        impl ::phoxal::runtime::HarnessAttachment for #self_type {
            type HarnessView = <#contract as ::phoxal::runtime::RuntimeContract>::HarnessView;

            fn harness_driver(
                config: ::phoxal::runtime::ConfigDocument,
                now: ::phoxal::runtime::ExecutionTime,
            ) -> ::phoxal::Result<
                ::phoxal::runtime::HarnessDriver<Self::Inputs, Self::Outputs>,
            > {
                ::phoxal::runtime::new_harness_driver::<#module_name::#adapter_name>(config, now)
            }

            fn build_inputs(
                view: &mut Self::HarnessView,
                now: ::phoxal::runtime::ExecutionTime,
            ) -> Self::Inputs {
                #contract::harness_build_inputs(view, now)
            }

            fn capture_outputs(
                view: &mut Self::HarnessView,
                outputs: &Self::Outputs,
            ) -> ::phoxal::Result<()> {
                #contract::harness_capture_transient(view, outputs)
            }

            fn store_state_record(
                view: &mut Self::HarnessView,
                field: &str,
                bytes: ::std::option::Option<::std::vec::Vec<u8>>,
            ) {
                match bytes {
                    Some(bytes) => { view.state_records.insert(field.to_owned(), bytes); }
                    None => { view.state_records.remove(field); }
                }
            }

            fn take_reply_bytes(
                view: &mut Self::HarnessView,
                id: ::phoxal::runtime::input::CommandId,
            ) -> ::std::option::Option<::std::vec::Vec<u8>> {
                #contract::harness_take_reply_bytes(view, id)
            }

            fn call_upper_bound(view: &Self::HarnessView) -> u64 {
                #contract::harness_call_upper_bound(view)
            }

            fn call_is_staged(
                view: &Self::HarnessView,
                id: ::phoxal::runtime::input::CommandId,
            ) -> bool {
                #contract::harness_call_is_staged(view, id)
            }

            fn validate_retention(
                view: &Self::HarnessView,
                outputs: &Self::Outputs,
            ) -> ::phoxal::Result<()> {
                #contract::harness_validate_retention(view, outputs)
            }

            fn bind_harness(view: &mut Self::HarnessView, owner: u64) {
                #contract::harness_bind(view, owner);
            }

            fn clear(view: &mut Self::HarnessView) {
                #contract::harness_clear(view);
            }
        }

        mod #module_name {
            //! Generated runtime adapter for the authored runtime: the
            //! authored struct is its owned state, moved in and out of each
            //! invocation through the runtime owner's failure model.

            /// The generated runtime adapter for one authored runtime.
            ///
            /// Launch glue, tests, and the harness attachment drive the
            /// authored runtime through this adapter; the module is private
            /// to the authored scope, so application code never names it.
            /// The adapter owns the runtime's bounded pending-call table.
            #[derive(Default)]
            pub(super) struct #adapter_name {
                pub(super) authoring: ::phoxal::runtime::authoring::AuthoringState,
            }

            impl #adapter_name {
                pub(super) fn new() -> Self {
                    Self::default()
                }

                #[must_use]
                pub fn execution_epoch(&self) -> u64 {
                    self.authoring.execution_epoch()
                }

                #[must_use]
                pub fn behavior_diary(&self) -> &::phoxal::runtime::behavior::BehaviorDiary {
                    self.authoring.resources().diary
                }
            }

            impl ::phoxal::runtime::Runtime for #adapter_name {
                type Config = #anchored_config;
                type State = #state_path;
                type Inputs =
                    <#contract_inside as ::phoxal::runtime::RuntimeContract>::Inputs;
                type Outputs =
                    <#contract_inside as ::phoxal::runtime::RuntimeContract>::Outputs;

                fn init(
                    &self,
                    ctx: &::phoxal::runtime::InitContext,
                    config: Self::Config,
                ) -> ::phoxal::Result<Self::State> {
                    self.authoring.reset();
                    #init_call
                }

                fn step(
                    &self,
                    ctx: &::phoxal::runtime::StepContext,
                    state: Self::State,
                    inputs: &Self::Inputs,
                ) -> ::phoxal::Result<(Self::State, Self::Outputs)> {
                let mut state = state;
                let mut outputs = Self::Outputs::default();
                let ctx = &ctx.with_execution_epoch(self.execution_epoch());
                let resources = self.authoring.resources();
                #conversion_step
                #contract_inside::phoxal_dispatch(
                    ctx,
                    inputs,
                    &mut outputs,
                    &mut state,
                    &resources,
                    |table: &mut _| {
                        #handler_registrations
                    },
                )?;
                // Release every unowned result in the retained store —
                // foreign, retired, or from another era — whether or not
                // this contract declares direct completion handlers.
                inputs.discard_unowned(resources.pending);
                // Direct completion handlers run before the periodic step:
                // behavior leaves consume their own committed tickets
                // during the step, and each direct handler receives
                // exactly the completions its field's accepted calls
                // staged, exactly once, from the single retained store.
                #completion_dispatch
                #step_call
                self.authoring.stage_calls(
                    outputs.take_direct_owners(),
                    outputs.take_tree_owners(),
                )?;
                ::std::result::Result::Ok((state, outputs))
                }

                fn accepted(&self) {
                    self.authoring.accepted();
                }

                fn discarded(&self) {
                    self.authoring.discarded();
                }
            }

            impl ::phoxal::runtime::outputs::OutputBindings for #adapter_name {
                const FIELDS: &'static [::phoxal::runtime::outputs::OutputField] =
                    <#contract_inside as ::phoxal::runtime::RuntimeContract>::BINDINGS;

                fn encode_transport(
                    &self,
                    state: &Self::State,
                    context: ::phoxal::runtime::StepContext,
                    resolve_input_port: &dyn Fn(&str) -> Option<::phoxal::contracts::MethodSignature>,
                    source: &str,
                ) -> ::phoxal::Result<::std::vec::Vec<::phoxal::runtime::transport::PreparedOutput>> {
                    #contract_inside::phoxal_encode_bindings(
                        state,
                        state,
                        context,
                        resolve_input_port,
                        source,
                        |table: &mut _| {
                            #projection_registrations
                        },
                    )
                }
            }

            impl ::phoxal::runtime::RegisteredRuntime for #adapter_name {
                const SPEC: ::phoxal::runtime::RuntimeSpec = #spec_expression;

                fn retain_artifact_metadata() {
                    ::std::hint::black_box(
                        <#contract_inside as ::phoxal::runtime::RuntimeContract>::retain_schemas(),
                    );
                    ::std::hint::black_box(&#artifact_static);
                    for field in <Self::Inputs as ::phoxal::runtime::input::InputSet>::FIELDS {
                        if let Some(signature) = field.port_signature {
                            ::std::hint::black_box(signature.descriptor_set());
                        }
                    }
                    for field in <Self::Outputs as ::phoxal::runtime::outputs::OutputSet>::FIELDS {
                        if let Some(signature) = field.port_signature {
                            ::std::hint::black_box(signature.descriptor_set());
                        }
                    }
                }
            }

            #[used]
            #[cfg_attr(target_os = "macos", unsafe(link_section = "__DATA,__phoxal_art"))]
            #[cfg_attr(target_os = "linux", unsafe(link_section = ".phoxal_art"))]
            pub(super) static #artifact_static: ::phoxal::runtime::artifact::ArtifactRecord =
                ::phoxal::runtime::artifact::runtime_record(
                    <#adapter_name as ::phoxal::runtime::RegisteredRuntime>::SPEC,
                    <<#adapter_name as ::phoxal::runtime::Runtime>::Config as ::phoxal::runtime::Config>::SCHEMA_JSON,
                    #conversion_routes,
                    <<#adapter_name as ::phoxal::runtime::Runtime>::Inputs as ::phoxal::runtime::input::InputSet>::FIELDS,
                    <<#adapter_name as ::phoxal::runtime::Runtime>::Outputs as ::phoxal::runtime::outputs::OutputSet>::FIELDS,
                    <#adapter_name as ::phoxal::runtime::outputs::OutputBindings>::FIELDS,
                    );

            const fn #check_name<T: ::phoxal::runtime::input::InputSet>() {}
            const _: () = {
                #check_name::<<#adapter_name as ::phoxal::runtime::Runtime>::Inputs>();
                assert!(
                    <#adapter_name as ::phoxal::runtime::RegisteredRuntime>::SPEC
                        .validate()
                        .is_ok(),
                    "runtime timing values must be positive"
                );
            };
        }
    })
}

/// Validates one `#[init]` signature and returns whether it receives the
/// init context plus the configuration type it accepts.
fn init_signature(method: &ImplItemFn) -> syn::Result<(bool, Type)> {
    if method.sig.receiver().is_some() {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "#[init] is an associated constructor; it takes no self receiver",
        ));
    }
    let arguments: Vec<_> = method.sig.inputs.iter().collect();
    match arguments.as_slice() {
        [config] => Ok((false, argument_type(config)?)),
        [first, config] => {
            let syn::FnArg::Typed(init_context) = first else {
                return Err(syn::Error::new_spanned(
                    &method.sig,
                    "#[init] takes (config) or (&InitContext, config)",
                ));
            };
            let is_init_context = matches!(&*init_context.ty, Type::Reference(reference)
                if reference
                    .elem
                    .to_token_stream()
                    .to_string()
                    .contains("InitContext"));
            if !is_init_context {
                return Err(syn::Error::new_spanned(
                    &method.sig,
                    "#[init] takes (config) or (&InitContext, config)",
                ));
            }
            Ok((true, argument_type(config)?))
        }
        _ => Err(syn::Error::new_spanned(
            &method.sig,
            "#[init] takes (config) or (&InitContext, config)",
        )),
    }
}

/// Returns the payload type of one typed method argument.
fn argument_type(argument: &syn::FnArg) -> syn::Result<Type> {
    let syn::FnArg::Typed(typed) = argument else {
        return Err(syn::Error::new_spanned(
            argument,
            "the argument carries a typed payload",
        ));
    };
    Ok((*typed.ty).clone())
}

/// Validates one `#[step]` signature.
fn step_signature(method: &ImplItemFn) -> syn::Result<()> {
    if !method
        .sig
        .receiver()
        .is_some_and(|receiver| receiver.reference.is_some() && receiver.mutability.is_some())
    {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "#[step] takes &mut self and the invocation context",
        ));
    }
    let arguments: Vec<_> = method
        .sig
        .inputs
        .iter()
        .filter(|argument| !matches!(argument, syn::FnArg::Receiver(_)))
        .collect();
    if arguments.len() != 1 {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "#[step] takes &mut self and the invocation context",
        ));
    }
    if !context_argument(arguments[0])? {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "#[step] mutably borrows the invocation context",
        ));
    }
    Ok(())
}

/// Validates one `#[handle]` signature and returns whether it mutably
/// borrows the context, plus its payload and output types.
fn handle_signature(method: &ImplItemFn) -> syn::Result<(bool, Type, syn::ReturnType)> {
    if !method
        .sig
        .receiver()
        .is_some_and(|receiver| receiver.reference.is_some())
    {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "#[handle] takes &mut self or &self",
        ));
    }
    let arguments: Vec<_> = method
        .sig
        .inputs
        .iter()
        .filter(|argument| !matches!(argument, syn::FnArg::Receiver(_)))
        .collect();
    if arguments.len() != 2 {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "#[handle] takes the invocation context and one payload",
        ));
    }
    let mutable = context_argument(arguments[0])?;
    let payload = argument_type(arguments[1])?;
    Ok((mutable, payload, method.sig.output.clone()))
}

/// Validates one typed argument as the invocation context and returns
/// whether it is mutably borrowed.
fn context_argument(argument: &syn::FnArg) -> syn::Result<bool> {
    let syn::FnArg::Typed(context) = argument else {
        return Err(syn::Error::new_spanned(
            argument,
            "the invocation context is a shared or mutable context reference",
        ));
    };
    let Type::Reference(reference) = &*context.ty else {
        return Err(syn::Error::new_spanned(
            argument,
            "the invocation context is a shared or mutable context reference",
        ));
    };
    if !reference
        .elem
        .to_token_stream()
        .to_string()
        .contains("Context")
    {
        return Err(syn::Error::new_spanned(
            argument,
            "the invocation context is a shared or mutable context reference",
        ));
    }
    Ok(reference.mutability.is_some())
}

/// Validates one `#[complete]` signature and returns the completion's
/// response payload type.
fn completion_signature(method: &ImplItemFn) -> syn::Result<Type> {
    if !method
        .sig
        .receiver()
        .is_some_and(|receiver| receiver.reference.is_some() && receiver.mutability.is_some())
    {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "#[complete] takes &mut self, the invocation context, and the completion",
        ));
    }
    let arguments: Vec<_> = method
        .sig
        .inputs
        .iter()
        .filter(|argument| !matches!(argument, syn::FnArg::Receiver(_)))
        .collect();
    if arguments.len() != 2 {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "#[complete] takes the invocation context and one completion",
        ));
    }
    context_argument(arguments[0])?;
    let syn::FnArg::Typed(completion) = arguments[1] else {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "the completion argument is typed",
        ));
    };
    // Accept any path spelling whose last segment is `CallCompletion`
    // carrying exactly one angle-bracketed payload argument.
    let Type::Path(path) = &*completion.ty else {
        return Err(syn::Error::new_spanned(
            &completion.ty,
            "the completion argument is one `CallCompletion<Response>`",
        ));
    };
    let is_completion = path
        .path
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "CallCompletion");
    if !is_completion {
        return Err(syn::Error::new_spanned(
            &completion.ty,
            "the completion argument is one `CallCompletion<Response>`",
        ));
    }
    let arguments = path
        .path
        .segments
        .last()
        .and_then(|segment| match &segment.arguments {
            syn::PathArguments::AngleBracketed(arguments) => Some(arguments),
            _ => None,
        })
        .ok_or_else(|| {
            syn::Error::new_spanned(
                &completion.ty,
                "the completion argument is one `CallCompletion<Response>`",
            )
        })?;
    let mut payload = None;
    for argument in &arguments.args {
        if let syn::GenericArgument::Type(inner) = argument {
            payload = Some(inner.clone());
        }
    }
    let payload = payload.ok_or_else(|| {
        syn::Error::new_spanned(
            &completion.ty,
            "the completion argument is one `CallCompletion<Response>`",
        )
    })?;
    // The dispatch is emitted one module below the authored impl; rebase
    // the payload path onto the author's scope.
    Ok(crate::message::anchored_type(&payload, 1))
}

/// Validates one `#[publish]` signature: a pure projection from state.
fn publish_signature(method: &ImplItemFn) -> syn::Result<()> {
    if !method
        .sig
        .receiver()
        .is_some_and(|receiver| receiver.reference.is_some() && receiver.mutability.is_none())
    {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "#[publish] is a pure projection; it takes &self and no context",
        ));
    }
    if method
        .sig
        .inputs
        .iter()
        .any(|argument| !matches!(argument, syn::FnArg::Receiver(_)))
    {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "#[publish] is a pure projection; it takes &self and no context",
        ));
    }
    Ok(())
}

#[derive(Clone)]
struct Options {
    period_ms: Option<u64>,
    timeout_ms: Option<u64>,
    init_timeout_ms: Option<u64>,
}

impl Options {
    /// An authored runtime declares its cadence explicitly; host invocation
    /// and initialization deadlines default to the small-control values
    /// every current participant declares when omitted.
    fn with_inherent_defaults(self) -> syn::Result<ResolvedOptions> {
        Ok(ResolvedOptions {
            period_ms: self
                .period_ms
                .ok_or_else(|| syn::Error::new(Span::call_site(), "runtime requires period_ms"))?,
            timeout_ms: self.timeout_ms.unwrap_or(100),
            init_timeout_ms: self.init_timeout_ms.unwrap_or(1_000),
        })
    }
}

struct ResolvedOptions {
    period_ms: u64,
    timeout_ms: u64,
    init_timeout_ms: u64,
}

fn parse_options(attr: TokenStream) -> syn::Result<(Options, Option<syn::Path>)> {
    let mut period_ms = None;
    let mut timeout_ms = None;
    let mut init_timeout_ms = None;
    let mut contract = None;
    syn::meta::parser(|meta| {
        let name = meta
            .path
            .get_ident()
            .map(ToString::to_string)
            .ok_or_else(|| meta.error("runtime options must use identifier names"))?;
        if name == "contract" {
            if contract.is_some() {
                return Err(meta.error("duplicate runtime option `contract`"));
            }
            contract = Some(meta.value()?.parse::<syn::Path>()?);
            return Ok(());
        }
        let slot = match name.as_str() {
            "period_ms" => &mut period_ms,
            "timeout_ms" => &mut timeout_ms,
            "init_timeout_ms" => &mut init_timeout_ms,
            _ => return Err(meta.error(format!("unknown runtime option `{name}`"))),
        };
        if slot.is_some() {
            return Err(meta.error(format!("duplicate runtime option `{name}`")));
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
    })
    .parse2(attr)?;
    Ok((
        Options {
            period_ms,
            timeout_ms,
            init_timeout_ms,
        },
        contract,
    ))
}
