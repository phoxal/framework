//! `#[phoxal::runtime]` — lifecycle + server dispatch from the inherent impl.
//!
//! Reads the lifecycle/server helper attributes on the methods of an inherent
//! impl and emits a `RuntimeBehavior` impl that the runner drives, re-emitting
//! the original methods verbatim (helper attributes stripped).
//!
//! Attributes: `#[setup]` (mandatory, once), `#[step(hz = N)]` (≤ 1),
//! `#[shutdown] async fn shutdown(&mut self, ctx: ShutdownContext)` (≤ 1),
//! `#[server(topic = …)]` (exclusive, `&mut self`),
//! `#[server_snapshot(topic = …)]` (concurrent, reads a committed `Snapshot`),
//! and `#[snapshot]` (the committed-snapshot provider, ≤ 1).

use proc_macro2::TokenStream;
use quote::quote;
use syn::spanned::Spanned;
use syn::{
    Attribute, Expr, FnArg, GenericArgument, ImplItem, ImplItemFn, ItemImpl, Lit, Meta,
    PathArguments, ReturnType, Type,
};

pub fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    if !attr.is_empty() {
        return Err(syn::Error::new_spanned(
            attr,
            "#[phoxal::runtime] takes no arguments; configure the runtime on the struct via \
             #[derive(phoxal::Service)] #[phoxal(...)] or \
             #[derive(phoxal::Driver)] #[phoxal(...)]",
        ));
    }

    let mut item_impl: ItemImpl = syn::parse2(item)?;
    let self_ty = (*item_impl.self_ty).clone();

    let mut setup: Option<LifecycleFn> = None;
    let mut step: Option<StepFn> = None;
    let mut shutdown: Option<LifecycleFn> = None;
    let mut servers: Vec<ServerFn> = Vec::new();
    let mut snapshot_servers: Vec<SnapshotServerFn> = Vec::new();
    let mut snapshot: Option<SnapshotFn> = None;

    for impl_item in &mut item_impl.items {
        let ImplItem::Fn(method) = impl_item else {
            continue;
        };
        let Some(kind) = take_lifecycle_attr(method)? else {
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
                setup = Some(LifecycleFn::parse_setup(method)?);
            }
            Lifecycle::Step(hz) => {
                if step.is_some() {
                    return Err(syn::Error::new(
                        method.sig.span(),
                        "duplicate #[step]: a runtime has at most one scheduled loop",
                    ));
                }
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
                shutdown = Some(LifecycleFn::parse_self_method(method)?);
            }
            Lifecycle::Server(topic) => servers.push(ServerFn::parse(method, topic)?),
            Lifecycle::ServerSnapshot(topic) => {
                snapshot_servers.push(SnapshotServerFn::parse(method, topic)?)
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
            "a runtime impl must declare exactly one #[setup] method (D22)",
        )
    })?;

    if !snapshot_servers.is_empty() && snapshot.is_none() {
        return Err(syn::Error::new_spanned(
            &item_impl.self_ty,
            "#[server_snapshot] requires a #[snapshot] provider on the same runtime",
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
    let topic_assertions = topic_assertions(&self_ty, &servers, &snapshot_servers);
    let validate_server_topics = validate_server_topics(&servers, &snapshot_servers);

    Ok(quote! {
        #item_impl

        #topic_assertions

        impl ::phoxal::runtime::RuntimeBehavior for #self_ty {
            const SERVER_CONTRACTS: &'static [::phoxal::runtime::ContractUse] = #server_contracts;

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

            fn __step_schedule() -> ::core::option::Option<::phoxal::runtime::StepSchedule> {
                #step_schedule
            }

            async fn __setup(
                ctx: &mut ::phoxal::runtime::SetupContext<Self>,
                config: <Self as ::phoxal::runtime::RuntimeFields>::Config,
            ) -> ::phoxal::Result<Self> {
                #setup_call
            }

            async fn __step(
                &mut self,
                step: ::phoxal::runtime::StepContext,
            ) -> ::phoxal::Result<()> {
                #step_call
            }

            async fn __shutdown(
                &mut self,
                ctx: ::phoxal::runtime::ShutdownContext,
            ) -> ::phoxal::Result<()> {
                #shutdown_call
            }

            fn __take_snapshot(&self) -> Self::Snapshot {
                #take_snapshot
            }

            async fn __serve_exclusive(
                &mut self,
                topic: &str,
                api_version: &str,
                family: &str,
                request: &[u8],
            ) -> ::phoxal::runtime::ServerOutcome {
                #serve_exclusive
            }

            fn __serve_snapshot(
                snapshot: ::std::sync::Arc<Self::Snapshot>,
                topic: ::std::string::String,
                api_version: ::std::string::String,
                family: ::std::string::String,
                request: ::std::vec::Vec<u8>,
            ) -> ::std::pin::Pin<
                ::std::boxed::Box<
                    dyn ::core::future::Future<Output = ::phoxal::runtime::ServerOutcome> + ::core::marker::Send,
                >,
            > {
                #serve_snapshot
            }
        }
    })
}

// ---- lifecycle plumbing (setup/step/shutdown) ------------------------------

fn setup_call(setup: &LifecycleFn) -> TokenStream {
    let name = &setup.name;
    if setup.takes_extra_arg {
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
        Some(s) => {
            let name = &s.name;
            quote!(self.#name(step).await)
        }
        None => quote!({
            let _ = step;
            ::core::result::Result::Ok(())
        }),
    }
}

fn step_schedule(step: Option<&StepFn>) -> TokenStream {
    match step {
        Some(s) => {
            let hz = s.hz;
            quote!(::core::option::Option::Some(::phoxal::runtime::StepSchedule::hz(#hz)))
        }
        None => quote!(::core::option::Option::None),
    }
}

fn shutdown_call(shutdown: Option<&LifecycleFn>) -> TokenStream {
    match shutdown {
        Some(s) => {
            let name = &s.name;
            if s.takes_extra_arg {
                quote!(self.#name(ctx).await)
            } else {
                quote!({
                    let _ = ctx;
                    self.#name().await
                })
            }
        }
        None => quote!({
            let _ = ctx;
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
        entries.push(contract_entry(&s.req_ty, quote!(ServerRequest)));
        entries.push(contract_entry(&s.resp_ty, quote!(ServerResponse)));
    }
    for s in snapshot_servers {
        entries.push(contract_entry(&s.req_ty, quote!(ServerRequest)));
        entries.push(contract_entry(&s.resp_ty, quote!(ServerResponse)));
    }
    quote!(&[ #(#entries),* ])
}

fn contract_entry(ty: &Type, direction: TokenStream) -> TokenStream {
    quote! {
        ::phoxal::runtime::ContractUse {
            api_version: <<#ty as ::phoxal::bus::ContractBody>::Api as ::phoxal::bus::ApiVersion>::ID,
            family: <#ty as ::phoxal::bus::ContractBody>::FAMILY,
            topic: <#ty as ::phoxal::bus::ContractBody>::TOPIC,
            direction: ::phoxal::runtime::Direction::#direction,
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
        .map(|(idx, s)| validate_one_server_topic(idx, "server", &s.req_ty, &s.resp_ty, &s.topic));
    let snapshot = snapshot_servers.iter().enumerate().map(|(idx, s)| {
        validate_one_server_topic(idx, "server_snapshot", &s.req_ty, &s.resp_ty, &s.topic)
    });
    quote! {
        let mut seen = ::std::collections::BTreeSet::<::std::string::String>::new();
        #(#exclusive)*
        #(#snapshot)*
        ::core::result::Result::Ok(())
    }
}

fn validate_one_server_topic(
    idx: usize,
    kind: &str,
    req_ty: &Type,
    resp_ty: &Type,
    topic: &Expr,
) -> TokenStream {
    let topic_var = syn::Ident::new(&format!("__phoxal_{kind}_topic_{idx}"), topic.span());
    quote! {
        // The `#[server(topic = …)]` expr is built with the PUBLIC api builder
        // (`api::topic::new()...`), so the query leaf is the client `AskQuery`
        // brand. This validation only reads the topic KEY (identical on both
        // sides) and type-checks Req/Resp; the live serve authority is
        // `<Req as ContractBody>::TOPIC` (runner-controlled), not this expr, so
        // owning the served topic does not need an `OwnerCap` here (plan #00 L2
        // Option C: runner-controlled).
        let #topic_var: ::phoxal::bus::Topic<::phoxal::bus::AskQuery<#req_ty, #resp_ty>> = #topic;
        let topic_key = #topic_var.key();
        let expected = <#req_ty as ::phoxal::bus::ContractBody>::TOPIC;
        if topic_key != expected {
            return ::core::result::Result::Err(::std::format!(
                "#[{kind}(topic = ...)] key '{}' does not match request body topic '{}'",
                topic_key,
                expected,
                kind = #kind,
            ));
        }
        if !seen.insert(topic_key.to_string()) {
            return ::core::result::Result::Err(::std::format!(
                "duplicate server topic '{}'",
                topic_key,
            ));
        }
    }
}

fn serve_exclusive(servers: &[ServerFn]) -> TokenStream {
    let arms = servers.iter().map(|s| {
        let name = &s.name;
        let req_ty = &s.req_ty;
        let validate = validate_request(req_ty);
        let encode = encode_reply(&s.resp_ty);
        quote! {
            if topic == <#req_ty as ::phoxal::bus::ContractBody>::TOPIC {
                #validate
                let request: #req_ty = match <::phoxal::bus::MessagePack as ::phoxal::bus::Codec>::decode::<#req_ty>(request) {
                    ::core::result::Result::Ok(r) => r,
                    ::core::result::Result::Err(e) => {
                        return ::core::result::Result::Err(
                            ::phoxal::bus::QueryFailure::invalid_argument(::std::format!("decode request: {e}")),
                        );
                    }
                };
                return match self.#name(request).await {
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

/// Validate the request's metadata (api_version + family) against the handler's
/// request body before decode — a wrong-API or wrong-family request is rejected
/// rather than silently decoded (server-side backstop for D59/D62).
fn validate_request(req_ty: &Type) -> TokenStream {
    quote! {
        if api_version != <<#req_ty as ::phoxal::bus::ContractBody>::Api as ::phoxal::bus::ApiVersion>::ID {
            return ::core::result::Result::Err(::phoxal::bus::QueryFailure::invalid_argument(
                ::std::format!(
                    "request api_version '{}' does not match server api_version '{}'",
                    api_version,
                    <<#req_ty as ::phoxal::bus::ContractBody>::Api as ::phoxal::bus::ApiVersion>::ID,
                ),
            ));
        }
        if family != <#req_ty as ::phoxal::bus::ContractBody>::FAMILY {
            return ::core::result::Result::Err(::phoxal::bus::QueryFailure::invalid_argument(
                ::std::format!(
                    "request family '{}' does not match server family '{}'",
                    family,
                    <#req_ty as ::phoxal::bus::ContractBody>::FAMILY,
                ),
            ));
        }
    }
}

fn serve_snapshot(snapshot_servers: &[SnapshotServerFn]) -> TokenStream {
    if snapshot_servers.is_empty() {
        return quote! {
            let _ = (snapshot, request, api_version, family);
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
        let validate = validate_request(req_ty);
        let encode = encode_reply(resp_ty);
        quote! {
            if topic == <#req_ty as ::phoxal::bus::ContractBody>::TOPIC {
                #validate
                let request: #req_ty = match <::phoxal::bus::MessagePack as ::phoxal::bus::Codec>::decode::<#req_ty>(&request) {
                    ::core::result::Result::Ok(r) => r,
                    ::core::result::Result::Err(e) => {
                        return ::core::result::Result::Err(
                            ::phoxal::bus::QueryFailure::invalid_argument(::std::format!("decode request: {e}")),
                        );
                    }
                };
                let state = ::phoxal::runtime::Snapshot::from_arc(snapshot);
                return match Self::#name(state, request).await {
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

fn encode_reply(resp_ty: &Type) -> TokenStream {
    quote! {
        match <::phoxal::bus::MessagePack as ::phoxal::bus::Codec>::encode(&response) {
            ::core::result::Result::Ok(payload) => ::core::result::Result::Ok(::phoxal::runtime::ServerReply {
                payload,
                family: <#resp_ty as ::phoxal::bus::ContractBody>::FAMILY,
                api_version: <<#resp_ty as ::phoxal::bus::ContractBody>::Api as ::phoxal::bus::ApiVersion>::ID,
            }),
            ::core::result::Result::Err(e) => ::core::result::Result::Err(
                ::phoxal::bus::QueryFailure::internal(::std::format!("encode response: {e}")),
            ),
        }
    }
}

fn topic_assertions(
    self_ty: &Type,
    servers: &[ServerFn],
    snapshot_servers: &[SnapshotServerFn],
) -> TokenStream {
    let mut checks = Vec::new();

    let mut emit = |idx: usize,
                    kind: &str,
                    req: &Type,
                    resp: &Type,
                    topic: &Expr,
                    span: proc_macro2::Span| {
        let topic_id = syn::Ident::new(&format!("__assert_{kind}_topic_{idx}"), span);
        let api_id = syn::Ident::new(&format!("__assert_{kind}_api_{idx}"), span);
        // The declared `topic = …` must agree with the handler's Req/Resp; and
        // both bodies must be `ContractBody<Api = R::Api>` so a server on a foreign
        // API version (which would emit mixed-version metadata) fails to compile
        // (D59/D60).
        checks.push(quote! {
            fn #topic_id() {
                // Built with the PUBLIC api builder (`api::topic::new()...`), so the
                // query leaf is the client `AskQuery` brand. This is a type-only
                // assertion that the `topic = …` expr agrees with Req/Resp; the
                // serve authority is the runner-controlled `<Req>::TOPIC`, not this
                // expr (plan #00 L2 Option C).
                let _t: ::phoxal::bus::Topic<::phoxal::bus::AskQuery<#req, #resp>> = #topic;
            }
            fn #api_id() {
                fn assert<R, B>()
                where
                    R: ::phoxal::runtime::RuntimeFields,
                    B: ::phoxal::bus::ContractBody<Api = <R as ::phoxal::runtime::RuntimeFields>::Api>,
                {}
                assert::<#self_ty, #req>();
                assert::<#self_ty, #resp>();
            }
        });
    };

    for (i, s) in servers.iter().enumerate() {
        emit(i, "server", &s.req_ty, &s.resp_ty, &s.topic, s.name.span());
    }
    for (i, s) in snapshot_servers.iter().enumerate() {
        emit(
            i,
            "snapshot_server",
            &s.req_ty,
            &s.resp_ty,
            &s.topic,
            s.name.span(),
        );
    }

    if checks.is_empty() {
        return quote!();
    }
    quote! {
        const _: () = {
            #(#[allow(unused)] #checks)*
        };
    }
}

// ---- parsed method descriptors ---------------------------------------------

enum Lifecycle {
    Setup,
    Step(f64),
    Shutdown,
    Server(Expr),
    ServerSnapshot(Expr),
    Snapshot,
}

struct LifecycleFn {
    name: syn::Ident,
    takes_extra_arg: bool,
}

impl LifecycleFn {
    fn parse_setup(method: &mut ImplItemFn) -> syn::Result<Self> {
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
        let typed = method.sig.inputs.len();
        if typed == 0 {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[setup] must take `ctx: &mut SetupContext<Self>` as its first argument",
            ));
        }
        if typed > 2 {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[setup] takes `ctx: &mut SetupContext<Self>` and optional runtime config",
            ));
        }
        let arg_types = typed_arg_types(method);
        if let Some(ctx_ty) = arg_types.first()
            && !ref_type_ends_with(ctx_ty, "SetupContext")
        {
            return Err(syn::Error::new_spanned(
                ctx_ty,
                "#[setup] first argument must be `ctx: &mut SetupContext<Self>`",
            ));
        }
        require_result_return(
            &method.sig.output,
            "#[setup] must return `Result<Self>` (D22)",
        )?;
        rewrite_setup_self_config(method);
        Ok(LifecycleFn {
            name: method.sig.ident.clone(),
            takes_extra_arg: typed >= 2,
        })
    }

    fn parse_self_method(method: &ImplItemFn) -> syn::Result<Self> {
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
        if method.sig.inputs.len() > 2 {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[shutdown] takes `&mut self` and optional `ctx: ShutdownContext`",
            ));
        }
        let typed = typed_arg_types(method);
        if let Some(ctx_ty) = typed.first()
            && !type_ends_with(ctx_ty, "ShutdownContext")
        {
            return Err(syn::Error::new_spanned(
                ctx_ty,
                "#[shutdown] context parameter must be `ctx: ShutdownContext`",
            ));
        }
        require_result_return(&method.sig.output, "#[shutdown] must return `Result<()>`")?;
        let extra = !typed.is_empty();
        Ok(LifecycleFn {
            name: method.sig.ident.clone(),
            takes_extra_arg: extra,
        })
    }
}

struct StepFn {
    name: syn::Ident,
    hz: f64,
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
        if typed.len() != 1 {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[step] takes exactly `&mut self` and `step: StepContext`",
            ));
        }
        if !type_ends_with(&typed[0], "StepContext") {
            return Err(syn::Error::new_spanned(
                &typed[0],
                "#[step] argument must be `step: StepContext`",
            ));
        }
        require_result_return(&method.sig.output, "#[step] must return `Result<()>`")?;
        Ok(StepFn {
            name: method.sig.ident.clone(),
            hz,
        })
    }
}

struct ServerFn {
    name: syn::Ident,
    topic: Expr,
    req_ty: Type,
    resp_ty: Type,
}

impl ServerFn {
    fn parse(method: &ImplItemFn, topic: Expr) -> syn::Result<Self> {
        if method.sig.asyncness.is_none() {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[server] must be `async`",
            ));
        }
        if !has_exclusive_receiver(method) {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[server] takes `&mut self` (exclusive, serialized with #[step] — D16)",
            ));
        }
        let typed = typed_arg_types(method);
        if typed.len() != 1 {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[server] takes exactly `&mut self` and one request argument",
            ));
        }
        let req_ty = typed[0].clone();
        let resp_ty = server_result_ty(&method.sig.output)?;
        Ok(ServerFn {
            name: method.sig.ident.clone(),
            topic,
            req_ty,
            resp_ty,
        })
    }
}

struct SnapshotServerFn {
    name: syn::Ident,
    topic: Expr,
    req_ty: Type,
    resp_ty: Type,
}

impl SnapshotServerFn {
    fn parse(method: &ImplItemFn, topic: Expr) -> syn::Result<Self> {
        if method.sig.asyncness.is_none() {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[server_snapshot] must be `async`",
            ));
        }
        if method.sig.receiver().is_some() {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[server_snapshot] is an associated function: it takes `state: Snapshot<…>` and \
                 `request`, not `self` (concurrent, read-only — D16)",
            ));
        }
        let typed = typed_arg_types(method);
        if typed.len() != 2 {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[server_snapshot] takes exactly `state: Snapshot<State>` and one request argument",
            ));
        }
        if snapshot_state_ty(&typed[0]).is_none() {
            return Err(syn::Error::new_spanned(
                &typed[0],
                "#[server_snapshot] first argument must be `state: Snapshot<State>`",
            ));
        }
        let req_ty = typed[1].clone();
        let resp_ty = server_result_ty(&method.sig.output)?;
        Ok(SnapshotServerFn {
            name: method.sig.ident.clone(),
            topic,
            req_ty,
            resp_ty,
        })
    }
}

struct SnapshotFn {
    name: syn::Ident,
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

/// Types of the typed (non-receiver) arguments of a method, in order.
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

fn rewrite_setup_self_config(method: &mut ImplItemFn) {
    let Some(FnArg::Typed(arg)) = method.sig.inputs.iter_mut().nth(1) else {
        return;
    };
    if type_is_self_config(&arg.ty) {
        *arg.ty = syn::parse_quote!(<Self as ::phoxal::runtime::RuntimeFields>::Config);
    }
}

fn type_is_self_config(ty: &Type) -> bool {
    let Type::Path(path) = ty else {
        return false;
    };
    let mut segments = path.path.segments.iter();
    matches!(
        (segments.next(), segments.next(), segments.next()),
        (Some(first), Some(second), None)
            if first.ident == "Self" && second.ident == "Config"
    )
}

fn snapshot_state_ty(ty: &Type) -> Option<Type> {
    single_generic_arg(ty, "Snapshot")
}

/// Extract `T` from a `-> ServerResult<T>` return type.
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

/// Require a `-> Result<Inner>` return (by last path segment), reporting `what`
/// in the error so each lifecycle method names its canonical shape. Matches the
/// surface form (`Result<…>` / `phoxal::Result<…>` / `crate::Result<…>` /
/// `anyhow::Result<…>`), the single-generic alias re-exported as `phoxal::Result`.
fn require_result_return(output: &ReturnType, what: &str) -> syn::Result<Type> {
    let ty = match output {
        ReturnType::Type(_, ty) => ty.as_ref(),
        ReturnType::Default => {
            return Err(syn::Error::new(output_span(output), what.to_string()));
        }
    };
    single_generic_arg(ty, "Result").ok_or_else(|| syn::Error::new_spanned(ty, what.to_string()))
}

/// If `ty` is exactly `Name<Arg>` (by last path segment), return `Arg`.
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

/// Like [`type_ends_with`], but peels one leading reference (`&T` / `&mut T`)
/// first — so `&mut SetupContext<Self>` matches `SetupContext`.
fn ref_type_ends_with(ty: &Type, name: &str) -> bool {
    let inner = match ty {
        Type::Reference(r) => r.elem.as_ref(),
        other => other,
    };
    type_ends_with(inner, name)
}

/// Find and remove the single phoxal helper attribute on a method.
fn take_lifecycle_attr(method: &mut ImplItemFn) -> syn::Result<Option<Lifecycle>> {
    let mut found: Option<(usize, Lifecycle)> = None;

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
            "server" => Lifecycle::Server(parse_topic(attr)?),
            "server_snapshot" => Lifecycle::ServerSnapshot(parse_topic(attr)?),
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
        found = Some((idx, kind));
    }

    if let Some((idx, kind)) = found {
        method.attrs.remove(idx);
        Ok(Some(kind))
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

/// Parse `#[server(topic = EXPR)]` / `#[server_snapshot(topic = EXPR)]`.
fn parse_topic(attr: &Attribute) -> syn::Result<Expr> {
    let mut topic: Option<Expr> = None;
    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("topic") {
            topic = Some(meta.value()?.parse()?);
            Ok(())
        } else {
            Err(meta.error("expected `topic = <api-local topic builder>`"))
        }
    })?;
    topic.ok_or_else(|| {
        syn::Error::new_spanned(
            attr,
            "server attributes require an explicit `topic = …` (server topics are explicit in v1)",
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
