//! The participant authoring model (`Api`/`Config`/`Participant`).
//! `#[derive(phoxal::Api)]` / `#[derive(phoxal::Config)]` /
//! `#[phoxal::service|driver|simulator|tool]` / `#[phoxal::behavior]` target
//! the traits here.
//!
//! `Api` names a participant-authored struct of bus handles, and a participant
//! may mix contract generations freely across its `Api` fields - there is no
//! per-participant API version ceiling.
//!
//! The runner's own system contracts (heartbeat/presence/simulation clock) do
//! not resolve a version through this trait hierarchy either:
//! `participant::runner` hardcodes `use phoxal_api::y2026_1 as api;`,
//! independent of any participant's chosen `Api`.
//!
//! # What this slice defers
//!
//! - **Config schema.** [`ParticipantConfig::SCHEMA_JSON`] is a placeholder
//!   (`"{}"`) emitted by `#[derive(phoxal::Config)]`; materializing the real
//!   `schemars` schema needs a host-side `build.rs` step (a recursive runtime
//!   walk a proc-macro cannot reproduce across crate boundaries) - a later
//!   slice (RECONCILIATION correction #12).
//! - **`Declares*<B>`-style compile-time gating** on the `SetupContext`
//!   builders below (`publisher`/`latest`/`server`/…) - "you must declare this
//!   family before building a handle for it" (D44). This slice's builders are
//!   **open** (any `ContractBody` compiles), matching "this slice only needs
//!   the contract-identity metadata embedded and the const available."
//!
//!   Consequence for tooling: because the builders are open AND a handle can
//!   be built and then stashed somewhere other than a scanned `Api` field
//!   (e.g. moved into a `ctx.spawn_managed(...)` task), [`ParticipantApi::CONTRACTS`]
//!   is the set of contracts named by the `Api` struct's fields, **not** a
//!   guaranteed-complete picture of everything the participant actually
//!   touches on the bus. It is a sound *lower bound* on the compatibility
//!   surface, and for a participant authored to the target model (all handles
//!   live as `Api` fields, nothing bypasses them) it is exact - but until
//!   `Declares*<B>` gating lands to make "every handle is a declared `Api`
//!   field" a compile-time guarantee, downstream tooling
//!   (xtask/catalog/graph-check) must treat `CONTRACTS` as authoritative for
//!   *what is declared*, not as a proof that nothing else is used.
//! - **`#[server(api = field)]` field validation.** The `field` identifier is
//!   parsed and recorded but not cross-checked against the `Api` struct's
//!   actual `Server<Req, Resp>` field of that name - a proc-macro on the
//!   `impl` block cannot see the separate `Api` struct definition it did not
//!   derive, so binding the handler to its declared slot is deferred to a
//!   later slice that owns both together.
//! - **Deferred hardening for `#[server_snapshot]`.** The generated
//!   [`ParticipantLifecycle::__serve_snapshot`] takes `Arc<Self::Api>`
//!   (read-only, D3): the runner constructs that `Arc` (one clone of the
//!   `#[setup]`-returned `api`) and hands `Arc::clone`s to each spawned
//!   `#[server_snapshot]` task on a live bus, alongside the owned `api` the
//!   main task keeps for `&mut Self::Api`. `Arc<Self::Api>` (not
//!   `&Self::Api`) is the chosen "api snapshot" shape (D3 offers either):
//!   every bus handle type's real operations already take `&self` (see
//!   `phoxal-bus/src/handle.rs`), so an `Arc` costs nothing extra and - unlike
//!   a borrowed reference - is `'static` and can be moved into the
//!   spawned/boxed future `__serve_snapshot` returns. This is sound for
//!   `Publisher`/`Latest`/`Querier`/`Server` (non-destructive reads / fresh
//!   publishes); a `Subscriber` field, however, is a *destructive* shared
//!   queue, so a `#[server_snapshot]` handler must read committed `Snapshot`
//!   state and never `recv` a `Subscriber` (see `runner`'s module docs and
//!   `Subscriber`'s rustdoc). **Deferred hardening:** this slice does not yet
//!   *enforce* that structurally - rejecting a snapshot handler that drains a
//!   `Subscriber` at compile time would need an invasive `Api`-projection
//!   redesign (a separate read-only snapshot view type excluding `Subscriber`
//!   fields); until it lands, P-convert must uphold the rule per participant.
//!
//! The owner-capability / component / raw-bus setup accessors are **not**
//! deferred: the surface exposes `owner_capability()` for all participants,
//! `component()` for drivers and simulators, and `raw_bus()` for tools (see
//! [`SetupContextApiExt`], [`SetupContextDriverExt`],
//! [`SetupContextSimulatorExt`], [`SetupContextToolExt`] below).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::bus::{
    AskQuery, ContractBody, DEFAULT_QUERY_TIMEOUT, Latest, OwnerCap, Publish, Publisher, Querier,
    Subscribe, Subscriber, Topic,
};
use crate::participant::context::{SetupContext, ShutdownContext, StepContext};
use crate::participant::server::ServerOutcome;
use crate::participant::spec::{IsDriver, IsSimulator, IsTool, StepSchedule};
use phoxal_bus::Bus;

/// The role a [`ParticipantApi`] handle field plays on the bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContractRole {
    /// A `Publisher<B>` field (or `Vec`/map of one).
    Publish,
    /// A `Subscriber<B>`/`Latest<B>` field (or `Vec`/map of one).
    Subscribe,
    /// A `Server<Req, Resp>` field (or `Vec`/map of one); contributes one
    /// entry for `Req` and one for `Resp`.
    Serve,
    /// A `Querier<Req, Resp>` field (or `Vec`/map of one) - the CLIENT/asking
    /// side of a query contract (the counterpart to [`Serve`](Self::Serve));
    /// contributes one entry for `Req` and one for `Resp`.
    Ask,
}

/// One contract a `Api` struct field uses: its generation-qualified wire key
/// (D1) plus the role that field plays. Built by `#[derive(phoxal::Api)]` from
/// each field's `<Body as ContractBody>::TOPIC`.
#[derive(Clone, Copy, Debug)]
pub struct ApiContractUse {
    /// The generation-qualified wire key.
    pub topic: &'static str,
    /// The role this field plays for that contract.
    pub role: ContractRole,
}

/// Emitted by `#[derive(phoxal::Api)]`: the bus-facing contract surface of a
/// participant's `Api` handle struct.
///
/// `Clone`: every bus handle type (`Publisher`/`Latest`/`Subscriber`/
/// `Querier`/[`Server`]) is cheaply `Clone` - a second handle to the same
/// underlying subscription/publish key/session, never a deep copy - because
/// every real operation on them takes `&self` (`phoxal-bus/src/handle.rs`'s
/// module docs). `#[derive(phoxal::Api)]` emits a field-wise `Clone` impl
/// alongside the `ParticipantApi` impl, so this bound is satisfied
/// automatically for every derived `Api` struct. The runner
/// (`participant::runner`) uses it to give concurrent `#[server_snapshot]`
/// tasks their own `Arc<Self::Api>` - a full clone made once after
/// `#[setup]`, independent of the `&mut Self::Api` the main task keeps for
/// `#[step]`/`#[server]`/`#[shutdown]` - rather than one value shared behind
/// `&mut`/`Arc` at once, which Rust's aliasing rules forbid without unsafe
/// code. Because every clone is a handle to the same live state (shared
/// `Bus`/`ArcSwapOption`/subscription task), the two never diverge, so this
/// is exactly D3's "read-only `&Self::Api`, or an api snapshot" - here
/// realized as a cloned api snapshot.
pub trait ParticipantApi: Send + Sync + Clone + 'static {
    /// Every contract this `Api` struct's fields use, deduplicated.
    const CONTRACTS: &'static [ApiContractUse];
}

/// `Api = ()` for participants that opt out of a typed bus surface (tools,
/// per decision - "Tools stay raw-bus only", `remove-emit-apis-api-authoring/readme.md`).
impl ParticipantApi for () {
    const CONTRACTS: &'static [ApiContractUse] = &[];
}

/// Emitted by `#[derive(phoxal::Config)]`: participant config identity.
///
/// `SCHEMA_JSON` is a placeholder in this slice (see the module docs); treat
/// it as "the trait exists and is available for later wiring," not as a
/// materialized schema.
pub trait ParticipantConfig: serde::de::DeserializeOwned + Send + 'static {
    /// Placeholder config JSON schema (real materialization is a later,
    /// host-side `build.rs` slice - see the module docs).
    const SCHEMA_JSON: &'static str;
}

impl ParticipantConfig for () {
    const SCHEMA_JSON: &'static str = "{}";
}

/// An optional config is a config: `config = Option<T>` (a participant whose
/// `PHOXAL_CONFIG` may be absent, deserializing to `None`) works whenever
/// `T: ParticipantConfig`; `Option<T>`'s own `Deserialize` maps `null`/absent
/// to `None` and a present object to `Some(T)`. A participant crate cannot
/// write `impl ParticipantConfig for Option<LocalConfig>` itself (orphan
/// rule: both the trait and `Option` are foreign), so the blanket lives here.
impl<T: ParticipantConfig> ParticipantConfig for Option<T> {
    const SCHEMA_JSON: &'static str = T::SCHEMA_JSON;
}

/// Emitted by `#[phoxal::service]` / `#[phoxal::driver]` /
/// `#[phoxal::simulator]` / `#[phoxal::tool]`: participant identity plus the
/// linked `Config`/`Api` types.
pub trait Participant: Sized + Send + 'static {
    /// The authoring kind that produced this artifact (`"service"`,
    /// `"driver"`, `"simulator"`, or `"tool"`).
    const KIND: &'static str;
    /// Whether normal graph topology applies to this participant.
    const PARTICIPANT_CLASS: &'static str;
    /// The participant id (`id = "…"`, default kebab of the type name).
    const ID: &'static str;
    /// The participant's typed config (`robot.yaml` input).
    type Config: ParticipantConfig;
    /// The participant's bus-facing contract surface (`()` for a raw-bus
    /// tool).
    type Api: ParticipantApi;
}

/// Lifecycle dispatch + server-side metadata, emitted by `#[phoxal::behavior]`
/// for a `#[setup]` returning `Result<(Self, Self::Api)>`, threading
/// `Self::Api` through every callback (D3):
///
/// - `#[step]` / the exclusive `#[server(api = …)]` get `&mut Self::Api`
///   (same task as the caller, so exclusive access is free);
/// - the concurrent `#[server_snapshot(api = …)]` gets a shared
///   `Arc<Self::Api>`, not `&mut` - it may run concurrently with `#[step]`/an
///   exclusive server (D3's "read-only … or an api snapshot"; see the module
///   docs for why `Arc` is this slice's chosen shape).
#[allow(async_fn_in_trait)]
pub trait ParticipantLifecycle: Participant {
    /// Contracts derived from `#[server]`/`#[server_snapshot]` handler
    /// signatures.
    const SERVER_CONTRACTS: &'static [ApiContractUse];

    /// The committed-snapshot state type (`()` when there is no
    /// `#[snapshot]`).
    type Snapshot: Send + Sync + 'static;

    /// Whether the participant provides a committed snapshot (`#[snapshot]`).
    const HAS_SNAPSHOT: bool;

    /// The versionless topic keys of exclusive `#[server]` handlers.
    fn __exclusive_server_topics() -> &'static [&'static str];

    /// The versionless topic keys of concurrent `#[server_snapshot]`
    /// handlers.
    fn __snapshot_server_topics() -> &'static [&'static str];

    /// Reject a duplicate server topic before startup declares queryables.
    fn __validate_server_topics() -> Result<(), String>;

    /// The scheduled-step cadence, or `None` if the participant has no
    /// `#[step]`.
    fn __step_schedule() -> Option<StepSchedule>;

    /// Construct the participant and its `Api` (`#[setup]`).
    async fn __setup(
        ctx: &mut SetupContext<Self>,
        config: Self::Config,
    ) -> crate::Result<(Self, Self::Api)>;

    /// Run one scheduled step (`#[step]`; a no-op when none is declared).
    async fn __step(&mut self, api: &mut Self::Api, step: StepContext) -> crate::Result<()>;

    /// Graceful shutdown (`#[shutdown]`; a no-op when none is declared).
    async fn __shutdown(&mut self, api: &mut Self::Api, ctx: ShutdownContext) -> crate::Result<()>;

    /// Commit the current state as a snapshot (calls the `#[snapshot]`
    /// provider; returns `()` when there is none).
    fn __take_snapshot(&self) -> Self::Snapshot;

    /// Serve one exclusive `#[server]` query (holds `&mut self` and
    /// `&mut Self::Api`, serialized with `#[step]`).
    async fn __serve_exclusive(
        &mut self,
        api: &mut Self::Api,
        topic: &str,
        request: &[u8],
    ) -> ServerOutcome;

    /// Serve one concurrent `#[server_snapshot]` query against a committed
    /// state snapshot and a shared, read-only `Api` snapshot (D3). Returns a
    /// boxed `Send` future so the caller can spawn it.
    fn __serve_snapshot(
        snapshot: Arc<Self::Snapshot>,
        api: Arc<Self::Api>,
        topic: String,
        request: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = ServerOutcome> + Send>>;
}

/// A declared server slot in an `Api` struct (`#[derive(phoxal::Api)]`
/// recognizes it as a [`ContractRole::Serve`] contract). Unlike
/// [`Publisher`]/[`Latest`]/[`Subscriber`]/[`Querier`], this carries no live
/// bus connection: serving is runner-dispatched from the generated
/// `ParticipantLifecycle::__serve_*` methods keyed on
/// `<Req as ContractBody>::TOPIC` - the field exists purely so `Api` can
/// *declare* the contract ("`Api` declares the bus contract; `behavior`
/// implements runtime logic").
pub struct Server<Req, Resp> {
    _p: std::marker::PhantomData<fn(Req) -> Resp>,
}

impl<Req, Resp> Server<Req, Resp> {
    /// Framework-internal (macro-only) constructor; the author-facing path is
    /// `ctx.server(...)` in `#[setup]`. `#[doc(hidden)]`.
    #[doc(hidden)]
    pub fn new() -> Self {
        Server {
            _p: std::marker::PhantomData,
        }
    }
}

impl<Req, Resp> Default for Server<Req, Resp> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Req, Resp> Clone for Server<Req, Resp> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Req, Resp> Copy for Server<Req, Resp> {}

// `PhantomData<fn(Req) -> Resp>` is `Send`/`Sync`/`Copy` regardless of
// `Req`/`Resp` (a fn-pointer phantom, same trick the other handle types'
// `PhantomData<fn() -> B>` markers use), so `Server<Req, Resp>` needs no
// bounds on `Req`/`Resp` to satisfy `ParticipantApi: Send + Sync + Clone`
// (see that trait's docs).

/// `SetupContext<R>` builders (`R: Participant`), added as an extension trait
/// rather than an inherent `impl<R: Participant> SetupContext<R>` block so
/// `context.rs` can stay free of a `Participant` bound on `SetupContext`
/// itself. Bring it into scope with `use phoxal::prelude::*;`.
#[allow(async_fn_in_trait)]
pub trait SetupContextApiExt<R: Participant> {
    /// Build a publisher for `B` (any generation - no per-participant API
    /// version ceiling).
    async fn publisher<B: ContractBody>(
        &self,
        topic: Topic<Publish<B>>,
    ) -> crate::Result<Publisher<B>>;

    /// A keep-last-1 view of `B`.
    async fn latest<B: ContractBody>(&self, topic: Topic<Subscribe<B>>)
    -> crate::Result<Latest<B>>;

    /// A drop-oldest ring subscription of `B` at `depth`.
    async fn subscriber<B: ContractBody>(
        &self,
        topic: Topic<Subscribe<B>>,
        depth: usize,
    ) -> crate::Result<Subscriber<B>>;

    /// Build a querier for a declared query contract.
    async fn querier<Req: ContractBody, Resp: ContractBody>(
        &self,
        topic: Topic<AskQuery<Req, Resp>>,
    ) -> crate::Result<Querier<Req, Resp>>;

    /// Declare an `Api` server slot for a query contract this participant
    /// serves. See [`Server`] - no live connection is opened here; the
    /// runner dispatches served queries to the generated
    /// `ParticipantLifecycle::__serve_*` methods.
    async fn server<Req: ContractBody, Resp: ContractBody>(
        &self,
        topic: Topic<AskQuery<Req, Resp>>,
    ) -> crate::Result<Server<Req, Resp>>;

    /// The runner-minted owner capability (plan #00 Layer 2) - the controlled
    /// path a participant takes to OWN its own topics. Bind it once in
    /// `#[setup]` and pass it to the owner builder entry
    /// `api::topic::internal::new(cap)`:
    ///
    /// ```ignore
    /// let cap = ctx.owner_capability();
    /// let state = ctx
    ///     .publisher(api::topic::internal::new(cap).drive().state())
    ///     .await?;
    /// ```
    ///
    /// Every real participant starts here, so it is on the base surface (all
    /// `Participant` kinds).
    fn owner_capability(&self) -> OwnerCap;

    /// The resolved robot model (`robot.yaml` + components + structure, D33):
    /// participants build their typed state from it. Present only when the
    /// runner was launched with a robot root; errors otherwise.
    fn robot(&self) -> crate::Result<&crate::model::v0::Robot>;

    /// The robot root directory (holds the robot model + assets). Present only
    /// when launched with a robot root.
    fn robot_root(&self) -> crate::Result<&std::path::Path>;
}

impl<R: Participant> SetupContextApiExt<R> for SetupContext<R> {
    async fn publisher<B: ContractBody>(
        &self,
        topic: Topic<Publish<B>>,
    ) -> crate::Result<Publisher<B>> {
        Ok(Publisher::new(self.bus().clone(), &topic)?)
    }

    async fn latest<B: ContractBody>(
        &self,
        topic: Topic<Subscribe<B>>,
    ) -> crate::Result<Latest<B>> {
        Ok(Latest::new(self.bus(), &topic).await?)
    }

    async fn subscriber<B: ContractBody>(
        &self,
        topic: Topic<Subscribe<B>>,
        depth: usize,
    ) -> crate::Result<Subscriber<B>> {
        Ok(Subscriber::new(self.bus(), &topic, depth).await?)
    }

    async fn querier<Req: ContractBody, Resp: ContractBody>(
        &self,
        topic: Topic<AskQuery<Req, Resp>>,
    ) -> crate::Result<Querier<Req, Resp>> {
        Ok(Querier::new(
            self.bus().clone(),
            &topic,
            DEFAULT_QUERY_TIMEOUT,
        )?)
    }

    async fn server<Req: ContractBody, Resp: ContractBody>(
        &self,
        topic: Topic<AskQuery<Req, Resp>>,
    ) -> crate::Result<Server<Req, Resp>> {
        // No live bus op: the topic argument only pins `Req`/`Resp` to the
        // declared query contract at the call site (a wrong pairing fails to
        // compile here); dispatch itself is runner-side (see `Server`'s docs).
        let _ = topic;
        Ok(Server::new())
    }

    fn owner_capability(&self) -> OwnerCap {
        self.owner_cap()
    }

    fn robot(&self) -> crate::Result<&crate::model::v0::Robot> {
        self.robot_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "no robot model is bound (this participant was launched without a robot root)"
            )
        })
    }

    fn robot_root(&self) -> crate::Result<&std::path::Path> {
        self.robot_root_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "no robot root is bound (this participant was launched without a robot root)"
            )
        })
    }
}

/// Driver-only `SetupContext` accessor (`R: Participant + IsDriver`).
///
/// `component()` lives on a separate extension trait per marker (not a single
/// trait with two blanket impls) precisely because two blanket impls -
/// `IsDriver` and `IsSimulator` - of one trait would overlap under coherence
/// (a type *could* implement both markers, even though none does). Splitting
/// per marker keeps each `ctx.component()` call resolvable to exactly one
/// impl. See also [`SetupContextSimulatorExt`].
pub trait SetupContextDriverExt {
    /// The `robot.components` entry this driver drives (D47/D53), launched once
    /// per instance. Errors if the driver was launched without one.
    fn component(&self) -> crate::Result<&str>;
}

impl<R: Participant + IsDriver> SetupContextDriverExt for SetupContext<R> {
    fn component(&self) -> crate::Result<&str> {
        self.component_instance().ok_or_else(|| {
            anyhow::anyhow!("no component instance is bound (this driver was launched without one)")
        })
    }
}

/// Simulator-only `SetupContext` accessor
/// (`R: Participant + IsSimulator`); the simulator-marker twin of
/// [`SetupContextDriverExt`] (see its docs for why the two markers get
/// separate traits). A simulator that owns a per-component instance reads it
/// the same way a driver does.
pub trait SetupContextSimulatorExt {
    /// The bound `robot.components` instance, if the simulator was launched
    /// per instance. Errors otherwise.
    fn component(&self) -> crate::Result<&str>;
}

impl<R: Participant + IsSimulator> SetupContextSimulatorExt for SetupContext<R> {
    fn component(&self) -> crate::Result<&str> {
        self.component_instance().ok_or_else(|| {
            anyhow::anyhow!(
                "no component instance is bound (this simulator was launched without one)"
            )
        })
    }
}

/// Tool-only `SetupContext` accessor (`R: Participant + IsTool`). Tools stay
/// raw-bus only (decided 2026-07-09), so this is their sole IO seam.
pub trait SetupContextToolExt {
    /// Clone the runner-owned raw bus for privileged tool internals. The bus is
    /// already open from the launch contract, so a tool does not reparse launch
    /// env or open an unrelated session.
    fn raw_bus(&self) -> Bus;
}

impl<R: Participant + IsTool> SetupContextToolExt for SetupContext<R> {
    fn raw_bus(&self) -> Bus {
        self.bus().clone()
    }
}
