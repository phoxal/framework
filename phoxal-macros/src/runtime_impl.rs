//! `#[phoxal::runtime]` — lifecycle + server dispatch from the inherent impl.
//!
//! Reads the lifecycle/server helper attributes on the methods of an inherent
//! impl and emits a `RuntimeBehavior` impl that the runner drives, re-emitting
//! the original methods verbatim (helper attributes stripped).
//!
//! Attributes: `#[setup]` (mandatory, once), `#[step(hz = N)]` (≤ 1),
//! `#[shutdown]` (≤ 1), `#[server(topic = …)]` (exclusive, `&mut self`),
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
             #[derive(phoxal::Runtime)] #[phoxal(...)]",
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
                step = Some(StepFn {
                    name: method.sig.ident.clone(),
                    hz,
                });
            }
            Lifecycle::Shutdown => {
                if shutdown.is_some() {
                    return Err(syn::Error::new(method.sig.span(), "duplicate #[shutdown]"));
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
    let topic_assertions = topic_assertions(&servers, &snapshot_servers);

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
                request: &[u8],
            ) -> ::phoxal::runtime::ServerOutcome {
                #serve_exclusive
            }

            fn __serve_snapshot(
                snapshot: ::std::sync::Arc<Self::Snapshot>,
                topic: ::std::string::String,
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
    let entries = req_tys.map(|ty| quote!(<#ty as ::phoxal::api::ContractBody>::TOPIC));
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
            api_version: <<#ty as ::phoxal::api::ContractBody>::Api as ::phoxal::api::ApiVersion>::ID,
            family: <#ty as ::phoxal::api::ContractBody>::FAMILY,
            topic: <#ty as ::phoxal::api::ContractBody>::TOPIC,
            direction: ::phoxal::runtime::Direction::#direction,
        }
    }
}

fn serve_exclusive(servers: &[ServerFn]) -> TokenStream {
    let arms = servers.iter().map(|s| {
        let name = &s.name;
        let req_ty = &s.req_ty;
        let encode = encode_reply(&s.resp_ty);
        quote! {
            if topic == <#req_ty as ::phoxal::api::ContractBody>::TOPIC {
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

fn serve_snapshot(snapshot_servers: &[SnapshotServerFn]) -> TokenStream {
    if snapshot_servers.is_empty() {
        return quote! {
            let _ = (snapshot, &request);
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
        let encode = encode_reply(resp_ty);
        quote! {
            if topic == <#req_ty as ::phoxal::api::ContractBody>::TOPIC {
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
                family: <#resp_ty as ::phoxal::api::ContractBody>::FAMILY,
                api_version: <<#resp_ty as ::phoxal::api::ContractBody>::Api as ::phoxal::api::ApiVersion>::ID,
            }),
            ::core::result::Result::Err(e) => ::core::result::Result::Err(
                ::phoxal::bus::QueryFailure::internal(::std::format!("encode response: {e}")),
            ),
        }
    }
}

fn topic_assertions(servers: &[ServerFn], snapshot_servers: &[SnapshotServerFn]) -> TokenStream {
    let mut checks = Vec::new();
    for (i, s) in servers.iter().enumerate() {
        let req = &s.req_ty;
        let resp = &s.resp_ty;
        let topic = &s.topic;
        let id = syn::Ident::new(&format!("__assert_server_topic_{i}"), s.name.span());
        checks.push(quote! {
            fn #id() {
                let _t: ::phoxal::bus::Topic<::phoxal::bus::Query<#req, #resp>> = #topic;
            }
        });
    }
    for (i, s) in snapshot_servers.iter().enumerate() {
        let req = &s.req_ty;
        let resp = &s.resp_ty;
        let topic = &s.topic;
        let id = syn::Ident::new(
            &format!("__assert_snapshot_server_topic_{i}"),
            s.name.span(),
        );
        checks.push(quote! {
            fn #id() {
                let _t: ::phoxal::bus::Topic<::phoxal::bus::Query<#req, #resp>> = #topic;
            }
        });
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
    fn parse_setup(method: &ImplItemFn) -> syn::Result<Self> {
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
        let extra = method.sig.inputs.len() >= 2;
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
        if method.sig.receiver().is_none() {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[server] takes `&mut self` (exclusive, serialized with #[step] — D16)",
            ));
        }
        let typed = typed_arg_types(method);
        let req_ty = typed.first().cloned().ok_or_else(|| {
            syn::Error::new(method.sig.span(), "#[server] must take a request argument")
        })?;
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
        if typed.len() < 2 {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[server_snapshot] takes `state: Snapshot<State>` and a request argument",
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
        if method.sig.receiver().is_none() {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[snapshot] takes `&self` and returns the committed state",
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

/// If `ty` is `Name<Arg>` (by last path segment), return `Arg`.
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
    args.args.iter().find_map(|a| match a {
        GenericArgument::Type(t) => Some(t.clone()),
        _ => None,
    })
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
