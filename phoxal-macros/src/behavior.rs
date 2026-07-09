//! `#[phoxal::behavior]` codegen: reads the lifecycle/server helper attributes
//! on a participant's inherent impl and emits
//! `impl phoxal::participant::api::ParticipantLifecycle for Self`, threading
//! `Self::Api` through `#[step]`/`#[shutdown]`/`#[server]` and giving
//! `#[server_snapshot]` a read-only `Arc<Self::Api>` (D3).
//!
//! `#[setup]` returns `Result<(Self, Self::Api)>` - participant state and its
//! bus-facing `Api` handles are two independent values threaded separately
//! through every lifecycle callback rather than living on one struct, so the
//! `Api` struct's fields alone are the full compatibility surface
//! (`#[derive(phoxal::Api)]`, `phoxal::participant::api`'s module docs).
//!
//! `#[server(api = field)]`'s `field` names the `Api` struct field being
//! implemented, recorded for documentation but not cross-checked against the
//! `Api` struct's actual field type in this slice (a proc-macro on the `impl`
//! block cannot see the separate `Api` struct definition it did not derive) -
//! "`Api` declares the bus contract, `behavior` implements runtime logic".

use proc_macro2::{Span, TokenStream};
use quote::{quote, quote_spanned};
use syn::spanned::Spanned;
use syn::visit_mut::VisitMut;
use syn::{
    Attribute, Expr, ExprPath, ExprStruct, FnArg, GenericArgument, Ident, ImplItem, ImplItemFn,
    ItemImpl, Lit, Meta, Path, PathArguments, QSelf, ReturnType, Type, TypePath,
};

use crate::util::phoxal;

pub fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    if !attr.is_empty() {
        return Err(syn::Error::new_spanned(
            attr,
            "#[phoxal::behavior] takes no arguments; configure the participant on the struct via \
             #[phoxal::service(...)], #[phoxal::driver(...)], #[phoxal::simulator(...)], or \
             #[phoxal::tool(...)]",
        ));
    }
    let mut item_impl: ItemImpl = syn::parse2(item)?;
    let self_ty = (*item_impl.self_ty).clone();
    let phoxal = phoxal();

    let mut setup: Option<SetupFn> = None;
    let mut step: Option<StepFn> = None;
    let mut shutdown: Option<ShutdownFn> = None;
    let mut servers: Vec<ServerFn> = Vec::new();
    let mut snapshot_servers: Vec<SnapshotServerFn> = Vec::new();
    let mut snapshot: Option<SnapshotFn> = None;
    let mut tool_forbidden: Vec<(Span, &'static str)> = Vec::new();

    for impl_item in &mut item_impl.items {
        let ImplItem::Fn(method) = impl_item else {
            continue;
        };
        let Some((kind, attr_span)) = take_lifecycle_attr(method)? else {
            continue;
        };
        match kind {
            Lifecycle::Setup => {
                if setup.is_some() {
                    return Err(syn::Error::new(method.sig.span(), "duplicate #[setup]"));
                }
                if method.sig.ident != "setup" {
                    return Err(syn::Error::new(
                        method.sig.ident.span(),
                        "the #[setup] method must be named `setup` (D22)",
                    ));
                }
                setup = Some(SetupFn::parse(method)?);
            }
            Lifecycle::Step(hz) => {
                if step.is_some() {
                    return Err(syn::Error::new(
                        method.sig.span(),
                        "duplicate #[step]: a participant has at most one scheduled loop",
                    ));
                }
                tool_forbidden.push((attr_span, "step"));
                step = Some(StepFn::parse(method, hz)?);
            }
            Lifecycle::Shutdown => {
                if shutdown.is_some() {
                    return Err(syn::Error::new(method.sig.span(), "duplicate #[shutdown]"));
                }
                if method.sig.ident != "shutdown" {
                    return Err(syn::Error::new(
                        method.sig.ident.span(),
                        "the #[shutdown] method must be named `shutdown`",
                    ));
                }
                shutdown = Some(ShutdownFn::parse(method)?);
            }
            Lifecycle::Server(api_field) => {
                tool_forbidden.push((attr_span, "server"));
                servers.push(ServerFn::parse(method, api_field)?);
            }
            Lifecycle::ServerSnapshot(api_field) => {
                tool_forbidden.push((attr_span, "server_snapshot"));
                snapshot_servers.push(SnapshotServerFn::parse(method, api_field)?);
            }
            Lifecycle::Snapshot => {
                if snapshot.is_some() {
                    return Err(syn::Error::new(method.sig.span(), "duplicate #[snapshot]"));
                }
                snapshot = Some(SnapshotFn::parse(method)?);
            }
        }
    }

    let setup = setup.ok_or_else(|| {
        syn::Error::new_spanned(
            &item_impl.self_ty,
            "a participant impl must declare exactly one #[setup] method (D22)",
        )
    })?;

    if !snapshot_servers.is_empty() && snapshot.is_none() {
        return Err(syn::Error::new_spanned(
            &item_impl.self_ty,
            "#[server_snapshot] requires a #[snapshot] provider on the same participant",
        ));
    }

    let setup_call = setup_call(&setup);
    let step_call = step_call(step.as_ref());
    let step_schedule = step_schedule(step.as_ref());
    let shutdown_call = shutdown_call(shutdown.as_ref());
    let (snapshot_ty, take_snapshot, has_snapshot) = snapshot_items(snapshot.as_ref());
    let exclusive_topics = topic_list(servers.iter().map(|s| &s.req_ty));
    let snapshot_topics = topic_list(snapshot_servers.iter().map(|s| &s.req_ty));
    let server_contracts = server_contracts(&servers, &snapshot_servers);
    let serve_exclusive = serve_exclusive(&servers);
    let serve_snapshot = serve_snapshot(&snapshot_servers);
    let validate_server_topics = validate_server_topics(&servers, &snapshot_servers);
    let tool_forbidden_guards = tool_forbidden_guards(&self_ty, &tool_forbidden);

    // The user writes plain `Self::Api`/`Self::Config` (matching the doc's
    // fixture) in this impl's signatures AND bodies - notably `Self::Api {
    // .. }` as a struct literal in `#[setup]`'s body. Two problems rule out
    // leaving that as-is or rewriting it to the obvious fully-qualified form:
    //
    // - `Self::Api` alone is `E0223 ambiguous associated type`: Rust does not
    //   resolve an associated type off a concrete `Self`/type path even with
    //   exactly one implementing trait in scope (verified empirically).
    // - `<Self as Participant>::Api { .. }` (a qualified path as a struct
    //   literal head) is itself `E0658`, gated behind the unstable
    //   `more_qualified_paths` feature on this toolchain (rust-lang/rust#86935)
    //   - qualified-path stabilization covers type/call position, not the
    //     struct-literal head.
    //
    // So instead of qualifying the path, this defines plain type ALIASES
    // (module scope, using the concrete `#self_ty` rather than `Self` so they
    // do not need to live inside an impl) and rewrites every bare `Self::Api`
    // / `Self::Config` - in type position and in expression position,
    // including the struct-literal head - to reference the alias by its
    // single-segment name. A plain alias name has none of the two problems
    // above: it is not an associated-type projection (no ambiguity) and not a
    // qualified path (usable as a struct-literal head). The OLD model's
    // narrower need (`runtime_impl::rewrite_setup_self_config`, `#[setup]`'s
    // second parameter only) got away with the qualified-path rewrite because
    // old user code never writes `Self::Config` in expression position.
    let api_alias = Ident::new(
        &format!(
            "__PhoxalApiOf_{self_ty_ident}",
            self_ty_ident = self_ty_ident_string(&self_ty)
        ),
        self_ty.span(),
    );
    let config_alias = Ident::new(
        &format!(
            "__PhoxalConfigOf_{self_ty_ident}",
            self_ty_ident = self_ty_ident_string(&self_ty)
        ),
        self_ty.span(),
    );
    SelfAssocRewriter {
        api_alias: api_alias.clone(),
        config_alias: config_alias.clone(),
    }
    .visit_item_impl_mut(&mut item_impl);

    Ok(quote! {
        #[doc(hidden)]
        #[allow(non_camel_case_types)]
        type #api_alias = <#self_ty as #phoxal::participant::Participant>::Api;
        #[doc(hidden)]
        #[allow(non_camel_case_types)]
        type #config_alias = <#self_ty as #phoxal::participant::Participant>::Config;

        #item_impl

        #tool_forbidden_guards

        impl #phoxal::participant::ParticipantLifecycle for #self_ty {
            const SERVER_CONTRACTS: &'static [#phoxal::participant::ApiContractUse] = #server_contracts;

            type Snapshot = #snapshot_ty;
            const HAS_SNAPSHOT: bool = #has_snapshot;

            fn __exclusive_server_topics() -> &'static [&'static str] {
                #exclusive_topics
            }

            fn __snapshot_server_topics() -> &'static [&'static str] {
                #snapshot_topics
            }

            fn __validate_server_topics() -> ::core::result::Result<(), ::std::string::String> {
                #validate_server_topics
            }

            fn __step_schedule() -> ::core::option::Option<#phoxal::participant::StepSchedule> {
                #step_schedule
            }

            async fn __setup(
                ctx: &mut #phoxal::participant::SetupContext<Self>,
                config: <Self as #phoxal::participant::Participant>::Config,
            ) -> #phoxal::Result<(Self, <Self as #phoxal::participant::Participant>::Api)> {
                #setup_call
            }

            async fn __step(
                &mut self,
                api: &mut <Self as #phoxal::participant::Participant>::Api,
                step: #phoxal::participant::StepContext,
            ) -> #phoxal::Result<()> {
                #step_call
            }

            async fn __shutdown(
                &mut self,
                api: &mut <Self as #phoxal::participant::Participant>::Api,
                ctx: #phoxal::participant::ShutdownContext,
            ) -> #phoxal::Result<()> {
                #shutdown_call
            }

            fn __take_snapshot(&self) -> Self::Snapshot {
                #take_snapshot
            }

            async fn __serve_exclusive(
                &mut self,
                api: &mut <Self as #phoxal::participant::Participant>::Api,
                topic: &str,
                request: &[u8],
            ) -> #phoxal::participant::ServerOutcome {
                #serve_exclusive
            }

            fn __serve_snapshot(
                snapshot: ::std::sync::Arc<Self::Snapshot>,
                api: ::std::sync::Arc<<Self as #phoxal::participant::Participant>::Api>,
                topic: ::std::string::String,
                request: ::std::vec::Vec<u8>,
            ) -> ::std::pin::Pin<
                ::std::boxed::Box<
                    dyn ::core::future::Future<Output = #phoxal::participant::ServerOutcome> + ::core::marker::Send,
                >,
            > {
                #serve_snapshot
            }
        }
    })
}

// ---- lifecycle plumbing (setup/step/shutdown) ------------------------------

fn setup_call(setup: &SetupFn) -> TokenStream {
    let name = &setup.name;
    if setup.takes_config {
        quote!(Self::#name(ctx, config).await)
    } else {
        quote!({
            let _ = config;
            Self::#name(ctx).await
        })
    }
}

fn step_call(step: Option<&StepFn>) -> TokenStream {
    match step {
        Some(s) if s.takes_api => {
            let name = &s.name;
            quote!(self.#name(api, step).await)
        }
        Some(s) => {
            let name = &s.name;
            quote!({
                let _ = &api;
                self.#name(step).await
            })
        }
        None => quote!({
            let _ = (&api, step);
            ::core::result::Result::Ok(())
        }),
    }
}

fn step_schedule(step: Option<&StepFn>) -> TokenStream {
    match step {
        Some(s) => {
            let hz = s.hz;
            quote!(::core::option::Option::Some(::phoxal::participant::StepSchedule::hz(#hz)))
        }
        None => quote!(::core::option::Option::None),
    }
}

fn shutdown_call(shutdown: Option<&ShutdownFn>) -> TokenStream {
    match shutdown {
        Some(s) if s.takes_api && s.takes_ctx => {
            let name = &s.name;
            quote!(self.#name(api, ctx).await)
        }
        Some(s) if s.takes_api => {
            let name = &s.name;
            quote!({
                let _ = ctx;
                self.#name(api).await
            })
        }
        Some(s) if s.takes_ctx => {
            let name = &s.name;
            quote!({
                let _ = &api;
                self.#name(ctx).await
            })
        }
        Some(s) => {
            let name = &s.name;
            quote!({
                let _ = (&api, ctx);
                self.#name().await
            })
        }
        None => quote!({
            let _ = (&api, ctx);
            ::core::result::Result::Ok(())
        }),
    }
}

// ---- snapshot + server codegen ---------------------------------------------

fn snapshot_items(snapshot: Option<&SnapshotFn>) -> (TokenStream, TokenStream, TokenStream) {
    match snapshot {
        Some(s) => {
            let ty = &s.state_ty;
            let name = &s.name;
            (quote!(#ty), quote!(self.#name()), quote!(true))
        }
        None => (quote!(()), quote!({}), quote!(false)),
    }
}

fn topic_list<'a>(req_tys: impl Iterator<Item = &'a Type>) -> TokenStream {
    let entries = req_tys.map(|ty| quote!(<#ty as ::phoxal::bus::ContractBody>::TOPIC));
    quote!({
        const TOPICS: &[&str] = &[ #(#entries),* ];
        TOPICS
    })
}

fn server_contracts(servers: &[ServerFn], snapshot_servers: &[SnapshotServerFn]) -> TokenStream {
    let mut entries = Vec::new();
    for s in servers {
        entries.push(contract_entry(&s.req_ty));
        entries.push(contract_entry(&s.resp_ty));
    }
    for s in snapshot_servers {
        entries.push(contract_entry(&s.req_ty));
        entries.push(contract_entry(&s.resp_ty));
    }
    quote!(&[ #(#entries),* ])
}

fn contract_entry(ty: &Type) -> TokenStream {
    quote! {
        ::phoxal::participant::ApiContractUse {
            topic: <#ty as ::phoxal::bus::ContractBody>::TOPIC,
            role: ::phoxal::participant::ContractRole::Serve,
        }
    }
}

fn validate_server_topics(
    servers: &[ServerFn],
    snapshot_servers: &[SnapshotServerFn],
) -> TokenStream {
    let exclusive = servers
        .iter()
        .enumerate()
        .map(|(idx, s)| validate_one(idx, "server", &s.req_ty));
    let snapshot = snapshot_servers
        .iter()
        .enumerate()
        .map(|(idx, s)| validate_one(idx, "server_snapshot", &s.req_ty));
    quote! {
        let mut seen = ::std::collections::BTreeSet::<::std::string::String>::new();
        #(#exclusive)*
        #(#snapshot)*
        ::core::result::Result::Ok(())
    }
}

fn validate_one(idx: usize, kind: &str, req_ty: &Type) -> TokenStream {
    let _ = idx;
    quote! {
        {
            let topic_key = <#req_ty as ::phoxal::bus::ContractBody>::TOPIC;
            if !seen.insert(topic_key.to_string()) {
                return ::core::result::Result::Err(::std::format!(
                    "duplicate {} topic '{}'",
                    #kind,
                    topic_key,
                ));
            }
        }
    }
}

fn serve_exclusive(servers: &[ServerFn]) -> TokenStream {
    let arms = servers.iter().map(|s| {
        let name = &s.name;
        let req_ty = &s.req_ty;
        let encode = encode_reply();
        let call = if s.takes_api {
            quote!(self.#name(api, request).await)
        } else {
            quote!(self.#name(request).await)
        };
        quote! {
            if topic == <#req_ty as ::phoxal::bus::ContractBody>::TOPIC {
                let request: #req_ty = match <::phoxal::bus::MessagePack as ::phoxal::bus::Codec>::decode::<#req_ty>(request) {
                    ::core::result::Result::Ok(r) => r,
                    ::core::result::Result::Err(e) => {
                        return ::core::result::Result::Err(
                            ::phoxal::bus::QueryFailure::invalid_argument(::std::format!("decode request: {e}")),
                        );
                    }
                };
                return match #call {
                    ::core::result::Result::Ok(response) => #encode,
                    ::core::result::Result::Err(failure) => ::core::result::Result::Err(failure),
                };
            }
        }
    });
    quote! {
        #(#arms)*
        ::core::result::Result::Err(
            ::phoxal::bus::QueryFailure::unimplemented(::std::format!("no exclusive server for '{topic}'")),
        )
    }
}

fn serve_snapshot(snapshot_servers: &[SnapshotServerFn]) -> TokenStream {
    if snapshot_servers.is_empty() {
        return quote! {
            let _ = (snapshot, api, request);
            ::std::boxed::Box::pin(async move {
                ::core::result::Result::Err(
                    ::phoxal::bus::QueryFailure::unimplemented(::std::format!("no snapshot server for '{topic}'")),
                )
            })
        };
    }

    let arms = snapshot_servers.iter().map(|s| {
        let name = &s.name;
        let req_ty = &s.req_ty;
        let resp_ty = &s.resp_ty;
        let encode = encode_reply();
        let call = if s.takes_api {
            quote!(Self::#name(state, &api, request).await)
        } else {
            quote!(Self::#name(state, request).await)
        };
        let _ = resp_ty;
        quote! {
            if topic == <#req_ty as ::phoxal::bus::ContractBody>::TOPIC {
                let request: #req_ty = match <::phoxal::bus::MessagePack as ::phoxal::bus::Codec>::decode::<#req_ty>(&request) {
                    ::core::result::Result::Ok(r) => r,
                    ::core::result::Result::Err(e) => {
                        return ::core::result::Result::Err(
                            ::phoxal::bus::QueryFailure::invalid_argument(::std::format!("decode request: {e}")),
                        );
                    }
                };
                let state = ::phoxal::participant::Snapshot::from_arc(snapshot);
                return match #call {
                    ::core::result::Result::Ok(response) => #encode,
                    ::core::result::Result::Err(failure) => ::core::result::Result::Err(failure),
                };
            }
        }
    });

    quote! {
        ::std::boxed::Box::pin(async move {
            #(#arms)*
            ::core::result::Result::Err(
                ::phoxal::bus::QueryFailure::unimplemented(::std::format!("no snapshot server for '{topic}'")),
            )
        })
    }
}

fn encode_reply() -> TokenStream {
    quote! {
        match <::phoxal::bus::MessagePack as ::phoxal::bus::Codec>::encode(&response) {
            ::core::result::Result::Ok(payload) => ::core::result::Result::Ok(::phoxal::participant::ServerReply {
                payload,
            }),
            ::core::result::Result::Err(e) => ::core::result::Result::Err(
                ::phoxal::bus::QueryFailure::internal(::std::format!("encode response: {e}")),
            ),
        }
    }
}

/// One `const _: () = { ... };` per `#[step]`/`#[server]`/`#[server_snapshot]`
/// found, asserting the impl type has the `TypedGraphSurface` marker (emitted
/// by `#[phoxal::service|driver|simulator]`, not `#[phoxal::tool]`), so a
/// tool gets a compile error span-targeted at the offending attribute.
fn tool_forbidden_guards(self_ty: &Type, forbidden: &[(Span, &'static str)]) -> TokenStream {
    let guards = forbidden.iter().map(|(span, _attribute)| {
        quote_spanned! {*span=>
            const _: () = {
                type __PhoxalBehaviorSelf = #self_ty;
                fn _assert_typed_graph_surface<T: ::phoxal::participant::TypedGraphSurface>() {}
                let _ = _assert_typed_graph_surface::<__PhoxalBehaviorSelf>;
            };
        }
    });
    quote!(#(#guards)*)
}

// ---- parsed method descriptors ---------------------------------------------

enum Lifecycle {
    Setup,
    Step(f64),
    Shutdown,
    Server(Ident),
    ServerSnapshot(Ident),
    Snapshot,
}

struct SetupFn {
    name: Ident,
    takes_config: bool,
}

impl SetupFn {
    fn parse(method: &ImplItemFn) -> syn::Result<Self> {
        if method.sig.asyncness.is_none() {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[setup] must be `async`",
            ));
        }
        if method.sig.receiver().is_some() {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[setup] is an associated function: it takes `ctx` (and optional `config`), not `self`",
            ));
        }
        let typed = typed_arg_types(method);
        if typed.is_empty() || typed.len() > 2 {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[setup] takes `ctx: &mut SetupContext<Self>` and optional participant config",
            ));
        }
        if !ref_type_ends_with(&typed[0], "SetupContext") {
            return Err(syn::Error::new_spanned(
                &typed[0],
                "#[setup] first argument must be `ctx: &mut SetupContext<Self>`",
            ));
        }
        require_tuple_result_return(&method.sig.output)?;
        Ok(SetupFn {
            name: method.sig.ident.clone(),
            takes_config: typed.len() == 2,
        })
    }
}

struct StepFn {
    name: Ident,
    hz: f64,
    takes_api: bool,
}

impl StepFn {
    fn parse(method: &ImplItemFn, hz: f64) -> syn::Result<Self> {
        if method.sig.asyncness.is_none() {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[step] must be `async`",
            ));
        }
        if !has_exclusive_receiver(method) {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[step] takes `&mut self` (the scheduled control loop - D34)",
            ));
        }
        let typed = typed_arg_types(method);
        let takes_api = match typed.len() {
            2 => {
                if !type_is_self_api_mut_ref(&typed[0]) {
                    return Err(syn::Error::new_spanned(
                        &typed[0],
                        "#[step] second argument must be `api: &mut Self::Api`",
                    ));
                }
                if !type_ends_with(&typed[1], "StepContext") {
                    return Err(syn::Error::new_spanned(
                        &typed[1],
                        "#[step] last argument must be `step: StepContext`",
                    ));
                }
                true
            }
            1 => {
                if !type_ends_with(&typed[0], "StepContext") {
                    return Err(syn::Error::new_spanned(
                        &typed[0],
                        "#[step] argument must be `step: StepContext`",
                    ));
                }
                false
            }
            _ => {
                return Err(syn::Error::new(
                    method.sig.span(),
                    "#[step] takes `&mut self`, optional `api: &mut Self::Api`, and `step: StepContext`",
                ));
            }
        };
        require_result_return(&method.sig.output, "#[step] must return `Result<()>`")?;
        Ok(StepFn {
            name: method.sig.ident.clone(),
            hz,
            takes_api,
        })
    }
}

struct ShutdownFn {
    name: Ident,
    takes_api: bool,
    takes_ctx: bool,
}

impl ShutdownFn {
    fn parse(method: &ImplItemFn) -> syn::Result<Self> {
        if method.sig.asyncness.is_none() {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[shutdown] must be `async`",
            ));
        }
        if method.sig.receiver().is_none() {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[shutdown] takes `&mut self`",
            ));
        }
        let typed = typed_arg_types(method);
        let (takes_api, takes_ctx) = match typed.len() {
            0 => (false, false),
            1 => {
                if type_is_self_api_mut_ref(&typed[0]) {
                    (true, false)
                } else if type_ends_with(&typed[0], "ShutdownContext") {
                    (false, true)
                } else {
                    return Err(syn::Error::new_spanned(
                        &typed[0],
                        "#[shutdown] argument must be `api: &mut Self::Api` or `ctx: ShutdownContext`",
                    ));
                }
            }
            2 => {
                if !type_is_self_api_mut_ref(&typed[0]) {
                    return Err(syn::Error::new_spanned(
                        &typed[0],
                        "#[shutdown] first argument must be `api: &mut Self::Api`",
                    ));
                }
                if !type_ends_with(&typed[1], "ShutdownContext") {
                    return Err(syn::Error::new_spanned(
                        &typed[1],
                        "#[shutdown] second argument must be `ctx: ShutdownContext`",
                    ));
                }
                (true, true)
            }
            _ => {
                return Err(syn::Error::new(
                    method.sig.span(),
                    "#[shutdown] takes `&mut self` and optional `api: &mut Self::Api` / `ctx: ShutdownContext`",
                ));
            }
        };
        require_result_return(&method.sig.output, "#[shutdown] must return `Result<()>`")?;
        Ok(ShutdownFn {
            name: method.sig.ident.clone(),
            takes_api,
            takes_ctx,
        })
    }
}

struct ServerFn {
    name: Ident,
    #[allow(dead_code)] // recorded for documentation/future cross-checking (see module docs)
    api_field: Ident,
    req_ty: Type,
    resp_ty: Type,
    takes_api: bool,
}

impl ServerFn {
    fn parse(method: &ImplItemFn, api_field: Ident) -> syn::Result<Self> {
        if method.sig.asyncness.is_none() {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[server] must be `async`",
            ));
        }
        if !has_exclusive_receiver(method) {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[server] takes `&mut self` (exclusive, serialized with #[step] - D16)",
            ));
        }
        let typed = typed_arg_types(method);
        let (req_ty, takes_api) = match typed.len() {
            2 => {
                if !type_is_self_api_mut_ref(&typed[0]) {
                    return Err(syn::Error::new_spanned(
                        &typed[0],
                        "#[server] second argument must be `api: &mut Self::Api`",
                    ));
                }
                (typed[1].clone(), true)
            }
            1 => (typed[0].clone(), false),
            _ => {
                return Err(syn::Error::new(
                    method.sig.span(),
                    "#[server] takes `&mut self`, optional `api: &mut Self::Api`, and one request argument",
                ));
            }
        };
        let resp_ty = server_result_ty(&method.sig.output)?;
        Ok(ServerFn {
            name: method.sig.ident.clone(),
            api_field,
            req_ty,
            resp_ty,
            takes_api,
        })
    }
}

struct SnapshotServerFn {
    name: Ident,
    #[allow(dead_code)] // recorded for documentation/future cross-checking (see module docs)
    api_field: Ident,
    req_ty: Type,
    resp_ty: Type,
    takes_api: bool,
}

impl SnapshotServerFn {
    fn parse(method: &ImplItemFn, api_field: Ident) -> syn::Result<Self> {
        if method.sig.asyncness.is_none() {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[server_snapshot] must be `async`",
            ));
        }
        if method.sig.receiver().is_some() {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[server_snapshot] is an associated function: it takes `state: Snapshot<…>`, \
                 optional `api: &Self::Api`, and `request`, not `self` (concurrent, read-only - D16/D3)",
            ));
        }
        let typed = typed_arg_types(method);
        let (req_ty, takes_api) = match typed.len() {
            3 => {
                if snapshot_state_ty(&typed[0]).is_none() {
                    return Err(syn::Error::new_spanned(
                        &typed[0],
                        "#[server_snapshot] first argument must be `state: Snapshot<State>`",
                    ));
                }
                if !type_is_self_api_ref(&typed[1]) {
                    return Err(syn::Error::new_spanned(
                        &typed[1],
                        "#[server_snapshot] second argument must be `api: &Self::Api` (read-only - D3)",
                    ));
                }
                (typed[2].clone(), true)
            }
            2 => {
                if snapshot_state_ty(&typed[0]).is_none() {
                    return Err(syn::Error::new_spanned(
                        &typed[0],
                        "#[server_snapshot] first argument must be `state: Snapshot<State>`",
                    ));
                }
                (typed[1].clone(), false)
            }
            _ => {
                return Err(syn::Error::new(
                    method.sig.span(),
                    "#[server_snapshot] takes `state: Snapshot<State>`, optional `api: &Self::Api`, and one request argument",
                ));
            }
        };
        let resp_ty = server_result_ty(&method.sig.output)?;
        Ok(SnapshotServerFn {
            name: method.sig.ident.clone(),
            api_field,
            req_ty,
            resp_ty,
            takes_api,
        })
    }
}

struct SnapshotFn {
    name: Ident,
    state_ty: Type,
}

impl SnapshotFn {
    fn parse(method: &ImplItemFn) -> syn::Result<Self> {
        if method.sig.asyncness.is_some() {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[snapshot] must be synchronous",
            ));
        }
        if !has_shared_receiver(method) {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[snapshot] takes `&self` and returns the committed state",
            ));
        }
        if !typed_arg_types(method).is_empty() {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[snapshot] takes only `&self` and returns the committed state",
            ));
        }
        let state_ty = match &method.sig.output {
            ReturnType::Type(_, ty) => (**ty).clone(),
            ReturnType::Default => {
                return Err(syn::Error::new(
                    method.sig.span(),
                    "#[snapshot] must return the committed state type",
                ));
            }
        };
        Ok(SnapshotFn {
            name: method.sig.ident.clone(),
            state_ty,
        })
    }
}

// ---- helpers ----------------------------------------------------------------

fn has_exclusive_receiver(method: &ImplItemFn) -> bool {
    method.sig.receiver().is_some_and(|receiver| {
        receiver.reference.is_some()
            && receiver.mutability.is_some()
            && receiver.colon_token.is_none()
    })
}

fn has_shared_receiver(method: &ImplItemFn) -> bool {
    method.sig.receiver().is_some_and(|receiver| {
        receiver.reference.is_some()
            && receiver.mutability.is_none()
            && receiver.colon_token.is_none()
    })
}

fn typed_arg_types(method: &ImplItemFn) -> Vec<Type> {
    method
        .sig
        .inputs
        .iter()
        .filter_map(|arg| match arg {
            FnArg::Typed(pat) => Some((*pat.ty).clone()),
            FnArg::Receiver(_) => None,
        })
        .collect()
}

fn snapshot_state_ty(ty: &Type) -> Option<Type> {
    single_generic_arg(ty, "Snapshot")
}

fn server_result_ty(output: &ReturnType) -> syn::Result<Type> {
    let ty = match output {
        ReturnType::Type(_, ty) => ty.as_ref(),
        ReturnType::Default => {
            return Err(syn::Error::new(
                output_span(output),
                "server handlers must return `ServerResult<Resp>`",
            ));
        }
    };
    single_generic_arg(ty, "ServerResult").ok_or_else(|| {
        syn::Error::new_spanned(ty, "server handlers must return `ServerResult<Resp>`")
    })
}

fn output_span(output: &ReturnType) -> proc_macro2::Span {
    match output {
        ReturnType::Type(_, ty) => ty.span(),
        ReturnType::Default => proc_macro2::Span::call_site(),
    }
}

fn require_result_return(output: &ReturnType, what: &str) -> syn::Result<Type> {
    let ty = match output {
        ReturnType::Type(_, ty) => ty.as_ref(),
        ReturnType::Default => {
            return Err(syn::Error::new(output_span(output), what.to_string()));
        }
    };
    single_generic_arg(ty, "Result").ok_or_else(|| syn::Error::new_spanned(ty, what.to_string()))
}

/// Like [`require_result_return`], but requires the inner type to be a
/// 2-tuple (`Result<(Self, Self::Api)>`, `#[setup]`'s required return shape);
/// the tuple elements are not separately named - `Self`/`Self::Api` are fixed
/// by the trait.
fn require_tuple_result_return(output: &ReturnType) -> syn::Result<()> {
    let ty = match output {
        ReturnType::Type(_, ty) => ty.as_ref(),
        ReturnType::Default => {
            return Err(syn::Error::new(
                output_span(output),
                "#[setup] must return `Result<(Self, Self::Api)>`",
            ));
        }
    };
    let inner = single_generic_arg(ty, "Result").ok_or_else(|| {
        syn::Error::new_spanned(ty, "#[setup] must return `Result<(Self, Self::Api)>`")
    })?;
    match inner {
        Type::Tuple(t) if t.elems.len() == 2 => Ok(()),
        other => Err(syn::Error::new_spanned(
            other,
            "#[setup] must return `Result<(Self, Self::Api)>`",
        )),
    }
}

fn single_generic_arg(ty: &Type, name: &str) -> Option<Type> {
    let Type::Path(path) = ty else {
        return None;
    };
    let seg = path.path.segments.last()?;
    if seg.ident != name {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &seg.arguments else {
        return None;
    };
    let mut args = args.args.iter();
    match (args.next(), args.next()) {
        (Some(GenericArgument::Type(t)), None) => Some(t.clone()),
        _ => None,
    }
}

fn type_ends_with(ty: &Type, name: &str) -> bool {
    let Type::Path(path) = ty else {
        return false;
    };
    path.path
        .segments
        .last()
        .is_some_and(|segment| segment.ident == name)
}

fn ref_type_ends_with(ty: &Type, name: &str) -> bool {
    let inner = match ty {
        Type::Reference(r) => r.elem.as_ref(),
        other => other,
    };
    type_ends_with(inner, name)
}

/// `true` iff `ty` is exactly `&mut Self::Api` (a reference to the 2-segment
/// path `Self::Api`).
fn type_is_self_api_mut_ref(ty: &Type) -> bool {
    let Type::Reference(r) = ty else { return false };
    r.mutability.is_some() && is_self_api_path(&r.elem)
}

/// `true` iff `ty` is exactly `&Self::Api` (shared reference).
fn type_is_self_api_ref(ty: &Type) -> bool {
    let Type::Reference(r) = ty else { return false };
    r.mutability.is_none() && is_self_api_path(&r.elem)
}

fn is_self_api_path(ty: &Type) -> bool {
    let Type::Path(path) = ty else { return false };
    let mut segments = path.path.segments.iter();
    matches!(
        (segments.next(), segments.next(), segments.next()),
        (Some(first), Some(second), None)
            if first.ident == "Self" && second.ident == "Api"
    )
}

fn take_lifecycle_attr(method: &mut ImplItemFn) -> syn::Result<Option<(Lifecycle, Span)>> {
    let mut found: Option<(usize, Lifecycle, Span)> = None;

    for (idx, attr) in method.attrs.iter().enumerate() {
        let Some(name) = attr.path().get_ident().map(|i| i.to_string()) else {
            continue;
        };
        let kind = match name.as_str() {
            "setup" => {
                expect_path_only(attr, "setup")?;
                Lifecycle::Setup
            }
            "step" => Lifecycle::Step(parse_step_hz(attr)?),
            "shutdown" => {
                expect_path_only(attr, "shutdown")?;
                Lifecycle::Shutdown
            }
            "server" => Lifecycle::Server(parse_api_field(attr, "server")?),
            "server_snapshot" => {
                Lifecycle::ServerSnapshot(parse_api_field(attr, "server_snapshot")?)
            }
            "snapshot" => {
                expect_path_only(attr, "snapshot")?;
                Lifecycle::Snapshot
            }
            _ => continue,
        };
        if found.is_some() {
            return Err(syn::Error::new_spanned(
                attr,
                "a method may carry at most one phoxal lifecycle attribute",
            ));
        }
        found = Some((idx, kind, attr.span()));
    }

    if let Some((idx, kind, span)) = found {
        method.attrs.remove(idx);
        Ok(Some((kind, span)))
    } else {
        Ok(None)
    }
}

fn expect_path_only(attr: &Attribute, name: &str) -> syn::Result<()> {
    match &attr.meta {
        Meta::Path(_) => Ok(()),
        _ => Err(syn::Error::new_spanned(
            attr,
            format!("#[{name}] takes no arguments"),
        )),
    }
}

/// Parse `#[server(api = field_name)]` / `#[server_snapshot(api = field_name)]`
/// (the new model's replacement for the old `topic = …` expr - see the
/// module docs).
fn parse_api_field(attr: &Attribute, name: &str) -> syn::Result<Ident> {
    let mut field: Option<Ident> = None;
    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("api") {
            field = Some(meta.value()?.parse()?);
            Ok(())
        } else {
            Err(meta.error("expected `api = <Api struct field name>`"))
        }
    })?;
    field.ok_or_else(|| {
        syn::Error::new_spanned(
            attr,
            format!("#[{name}(...)] requires `api = <Api struct field name>`"),
        )
    })
}

fn parse_step_hz(attr: &Attribute) -> syn::Result<f64> {
    let mut hz: Option<f64> = None;
    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("hz") {
            let value: Expr = meta.value()?.parse()?;
            hz = Some(expr_to_f64(&value)?);
            Ok(())
        } else {
            Err(meta.error("unknown #[step(...)] key (expected `hz`)"))
        }
    })?;
    let hz = hz.ok_or_else(|| {
        syn::Error::new_spanned(
            attr,
            "#[step(hz = N)] requires a frequency, e.g. #[step(hz = 50)]",
        )
    })?;
    if !(hz.is_finite() && hz > 0.0) {
        return Err(syn::Error::new_spanned(
            attr,
            "#[step(hz = N)] frequency must be positive and finite",
        ));
    }
    Ok(hz)
}

// ---- `Self::Api` / `Self::Config` -> type-alias rewrite --------------------

/// Rewrites every plain `Self::Api` / `Self::Config` path - in type position
/// (`TypePath`) and in expression position, including the struct-literal head
/// (`ExprPath`/`ExprStruct`) and generic-argument position (the visitor
/// recurses) - to the single-segment alias name `expand` defines at module
/// scope (`type #api_alias = <ConcreteTy as Participant>::Api;`). See
/// `expand`'s call-site comment for why a plain alias, not a qualified path.
///
/// Caveat: it only rewrites the exact 2-segment `Self::Api` / `Self::Config`.
/// It does NOT touch 3+-segment paths like `Self::Api::foo()` (leading
/// `Self::Api::` is the `E0223` ambiguous form) or occurrences buried inside a
/// nested macro invocation's token stream (`VisitMut` does not descend into
/// `TokenStream` macro bodies). In those spots write `<Self::Api>::foo()`
/// yourself - the caveat is documented rather than handled because neither
/// appears in the authored surface the companion doc's fixtures use.
struct SelfAssocRewriter {
    api_alias: Ident,
    config_alias: Ident,
}

impl VisitMut for SelfAssocRewriter {
    fn visit_type_path_mut(&mut self, node: &mut TypePath) {
        self.rewrite(&mut node.qself, &mut node.path);
        syn::visit_mut::visit_type_path_mut(self, node);
    }

    fn visit_expr_path_mut(&mut self, node: &mut ExprPath) {
        self.rewrite(&mut node.qself, &mut node.path);
        syn::visit_mut::visit_expr_path_mut(self, node);
    }

    fn visit_expr_struct_mut(&mut self, node: &mut ExprStruct) {
        self.rewrite(&mut node.qself, &mut node.path);
        syn::visit_mut::visit_expr_struct_mut(self, node);
    }
}

impl SelfAssocRewriter {
    fn rewrite(&self, qself: &mut Option<QSelf>, path: &mut Path) {
        let Some(assoc) = bare_self_assoc(qself, path) else {
            return;
        };
        let alias = match assoc.as_str() {
            "Api" => &self.api_alias,
            "Config" => &self.config_alias,
            _ => unreachable!("bare_self_assoc only returns \"Api\" or \"Config\""),
        };
        *path = Path::from(alias.clone());
    }
}

/// `Some("Api")` / `Some("Config")` iff `path` is exactly the unqualified
/// 2-segment `Self::Api` / `Self::Config` (no existing qself, no generic args
/// on either segment) - the shape the doc's authoring surface writes.
/// `None` otherwise (left untouched).
fn bare_self_assoc(qself: &Option<QSelf>, path: &Path) -> Option<String> {
    if qself.is_some() || path.segments.len() != 2 {
        return None;
    }
    let first = &path.segments[0];
    let second = &path.segments[1];
    if first.ident != "Self" || !first.arguments.is_empty() || !second.arguments.is_empty() {
        return None;
    }
    match second.ident.to_string().as_str() {
        "Api" | "Config" => Some(second.ident.to_string()),
        _ => None,
    }
}

/// A filesystem/identifier-safe fragment derived from the impl's `Self` type,
/// for the alias names above (`Ident`'s `Display`/`to_token_stream` string is
/// fine for a plain-path `self_ty`, which is the only shape `#[phoxal::behavior]`
/// accepts - see `ItemImpl::self_ty` usage throughout this module).
fn self_ty_ident_string(self_ty: &Type) -> String {
    quote!(#self_ty)
        .to_string()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn expr_to_f64(expr: &Expr) -> syn::Result<f64> {
    if let Expr::Lit(lit) = expr {
        match &lit.lit {
            Lit::Int(i) => return i.base10_parse::<f64>(),
            Lit::Float(f) => return f.base10_parse::<f64>(),
            _ => {}
        }
    }
    Err(syn::Error::new_spanned(
        expr,
        "expected a numeric literal for `hz`",
    ))
}
