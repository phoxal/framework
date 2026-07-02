//! Participant contexts: `SetupContext` (IO construction), `StepContext` (logical
//! time per scheduled step), and `ShutdownContext`.

use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::bus::{
    AskQuery, ContractBody, DEFAULT_QUERY_TIMEOUT, Latest, LogicalTime, OwnerCap, Publish,
    Publisher, Querier, Subscribe, Subscriber, Topic,
};
use crate::model::v1::Robot;
use crate::participant::spec::{
    DeclaresPublish, DeclaresQuery, DeclaresSubscribe, IsDriver, ParticipantSpec,
};
use phoxal_bus::Bus;

/// Default drop-oldest ring depth for a `Subscriber` built in `#[setup]`.
const DEFAULT_SUBSCRIBER_DEPTH: usize = 32;

/// The sole IO-construction point, handed to `#[setup]` (D18).
///
/// Builders are bound `B: ContractBody<Api = R::Api>` (one API version - D60) and
/// a **direction-specific** declaration marker
/// (`DeclaresPublish`/`DeclaresSubscribe`/`DeclaresQuery`, D44): a body from
/// another API version, an undeclared family, or a declared family used in an
/// undeclared direction is a compile error. Normal participant code never opens Zenoh
/// directly; the runner opened the bus before `#[setup]`.
///
/// On top of those gates, each builder accepts only the matching **side brand**
/// (L1, plan #00): `publisher` takes `Topic<Publish<B>>`, the subscription
/// builders take `Topic<Subscribe<B>>`, and `querier` takes
/// `Topic<AskQuery<Req, Resp>>`. The api tree's PUBLIC `topic::new()...` chain
/// yields the client brands and its `topic::internal::new(cap)...` chain yields the
/// owner brands, so taking the WRONG side of a topic (e.g. publishing a `state`
/// the public chain hands you as `Subscribe`) fails to compile - the owner side is
/// reachable only through the explicit `internal` builder, whose entry requires the
/// runner-minted owner capability from [`Self::owner_capability`] (L2, plan #00).
pub struct SetupContext<R: ParticipantSpec> {
    bus: Bus,
    owner_cap: OwnerCap,
    robot: Option<Arc<Robot>>,
    bundle_root: Option<PathBuf>,
    component_instance: Option<String>,
    _runtime: PhantomData<fn() -> R>,
}

impl<R: ParticipantSpec> SetupContext<R> {
    pub(crate) fn new(
        bus: Bus,
        owner_cap: OwnerCap,
        robot: Option<Arc<Robot>>,
        bundle_root: Option<PathBuf>,
        component_instance: Option<String>,
    ) -> Self {
        SetupContext {
            bus,
            owner_cap,
            robot,
            bundle_root,
            component_instance,
            _runtime: PhantomData,
        }
    }

    /// The resolved robot model (`robot.yaml` + components + structure). Official
    /// participants build their typed state from this (D33); it is present only when
    /// the runner was launched with a bundle. Returns an error otherwise.
    pub fn robot(&self) -> crate::Result<&Robot> {
        self.robot.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "no robot model is bound (this participant was launched without a bundle)"
            )
        })
    }

    /// The bundle root directory (holds the robot model + assets). Present only
    /// when launched with a bundle.
    pub fn bundle_root(&self) -> crate::Result<&Path> {
        self.bundle_root.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "no bundle root is bound (this participant was launched without a bundle)"
            )
        })
    }

    /// Build a publisher for a declared pub/sub contract of this API version.
    ///
    /// The bounds are the static safety gate: `B` must belong to the participant's
    /// selected API version, and the participant must have declared `B` in the
    /// publish direction through a `Publisher<B>` field or
    /// `#[phoxal(contracts(publishes(B)))]`. A body from another API version or
    /// an undeclared publish family fails to compile.
    pub async fn publisher<B>(&self, topic: Topic<Publish<B>>) -> crate::Result<Publisher<B>>
    where
        B: ContractBody<Api = R::Api>,
        R: DeclaresPublish<B>,
    {
        Ok(Publisher::new(self.bus.clone(), &topic)?)
    }

    /// Begin building a subscription (`.latest()` or `.subscriber()`).
    ///
    /// The `B: ContractBody<Api = R::Api>` bound rejects bodies from other API
    /// versions. `R: DeclaresSubscribe<B>` rejects undeclared families and
    /// publish-only declarations. Use [`SubscribeBuilder::depth`] or
    /// [`Self::subscribe_with`] to override the `Subscriber` ring depth.
    pub fn subscribe<B>(&self, topic: Topic<Subscribe<B>>) -> SubscribeBuilder<R, B>
    where
        B: ContractBody<Api = R::Api>,
        R: DeclaresSubscribe<B>,
    {
        self.subscribe_with(topic, SubscribeOptions::default())
    }

    /// Begin building a subscription with explicit options.
    ///
    /// This is the options-shaped form of [`Self::subscribe`]. Today the only
    /// public option is `Subscriber` ring depth; `Latest` remains keep-last-1 by
    /// design. The same API-version and declared-subscribe compile-time gates
    /// apply as for [`Self::subscribe`].
    pub fn subscribe_with<B>(
        &self,
        topic: Topic<Subscribe<B>>,
        options: SubscribeOptions,
    ) -> SubscribeBuilder<R, B>
    where
        B: ContractBody<Api = R::Api>,
        R: DeclaresSubscribe<B>,
    {
        SubscribeBuilder {
            bus: self.bus.clone(),
            topic,
            depth: options.depth,
            _runtime: PhantomData,
        }
    }

    /// Build a querier for a declared query contract of this API version.
    ///
    /// Both request and response bodies must belong to `R::Api`, and the participant
    /// must have declared the exact query pair with `Querier<Req, Resp>` or
    /// `#[phoxal(contracts(queries(Req => Resp)))]`. The handle carries the
    /// Phoxal-pinned finite timeout (D31).
    pub async fn querier<Req, Resp>(
        &self,
        topic: Topic<AskQuery<Req, Resp>>,
    ) -> crate::Result<Querier<Req, Resp>>
    where
        Req: ContractBody<Api = R::Api>,
        Resp: ContractBody<Api = R::Api>,
        R: DeclaresQuery<Req, Resp>,
    {
        Ok(Querier::new(
            self.bus.clone(),
            &topic,
            DEFAULT_QUERY_TIMEOUT,
        )?)
    }

    /// The runner-minted owner capability (plan #00 Layer 2).
    ///
    /// This is the controlled path a participant uses to opt into being the OWNER of
    /// its own topics. Bind it once in `#[setup]` and pass it to the owner builder
    /// entry, `api::topic::internal::new(cap)`:
    ///
    /// ```ignore
    /// let cap = ctx.owner_capability();
    /// let pub_ = ctx.publisher(api::topic::internal::new(cap).drive().state()).await?;
    /// ```
    ///
    /// The runner mints the single [`OwnerCap`] it stores here before `#[setup]`
    /// runs, so owning a topic is a deliberate, greppable opt-in rather than
    /// something any participant can do silently. See [`OwnerCap`] for the honest
    /// residual: this is a not-by-accident gate, not a hard guarantee.
    pub fn owner_capability(&self) -> OwnerCap {
        self.owner_cap
    }

    /// The underlying bus. Not on the default checked-participant surface (plan #00
    /// DoD #11 / plan #07): the raw bus is `pub(crate)` so normal participants and
    /// examples cannot reach around the typed handle builders. Privileged
    /// participants that genuinely need raw access go through `phoxal::raw`
    /// (`Bus::open` + `run_with_bus`), not through this context.
    ///
    /// Retained as an in-crate accessor (no current caller) so privileged phoxal
    /// code/tests have the seam without re-widening the documented surface.
    #[allow(dead_code)]
    pub(crate) fn bus(&self) -> &Bus {
        &self.bus
    }
}

impl<R> SetupContext<R>
where
    R: ParticipantSpec + IsDriver,
{
    /// The `components.instances` entry this participant drives (D47/D53), for a
    /// component driver launched once per instance. Combine with [`Self::robot`]
    /// (`driver_binding`/`component_instance`) for the resolved component spec.
    pub fn component(&self) -> crate::Result<&str> {
        self.component_instance.as_deref().ok_or_else(|| {
            anyhow::anyhow!("no component instance is bound (this driver was launched without one)")
        })
    }
}

/// Options for [`SetupContext::subscribe_with`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubscribeOptions {
    depth: usize,
}

impl SubscribeOptions {
    /// Create options using the default drop-oldest ring depth.
    pub const fn new() -> Self {
        SubscribeOptions {
            depth: DEFAULT_SUBSCRIBER_DEPTH,
        }
    }

    /// Set the ring depth used by [`SubscribeBuilder::subscriber`].
    ///
    /// Values below 1 are clamped by the subscriber implementation. `Latest`
    /// ignores this option because it is intentionally keep-last-1.
    pub const fn depth(mut self, depth: usize) -> Self {
        self.depth = depth;
        self
    }
}

impl Default for SubscribeOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder returned by [`SetupContext::subscribe`]; pick `latest()` (keep-last-1)
/// or `subscriber()` (drop-oldest ring).
pub struct SubscribeBuilder<R: ParticipantSpec, B> {
    bus: Bus,
    topic: Topic<Subscribe<B>>,
    depth: usize,
    _runtime: PhantomData<fn() -> R>,
}

impl<R, B> SubscribeBuilder<R, B>
where
    R: ParticipantSpec,
    B: ContractBody<Api = R::Api>,
{
    /// Override the ring depth for a `subscriber()` (ignored by `latest()`).
    pub fn depth(mut self, depth: usize) -> Self {
        self.depth = depth;
        self
    }

    /// A keep-last-1 view of the topic.
    pub async fn latest(self) -> crate::Result<Latest<B>> {
        Ok(Latest::new(&self.bus, &self.topic).await?)
    }

    /// A drop-oldest ring subscription.
    pub async fn subscriber(self) -> crate::Result<Subscriber<B>> {
        Ok(Subscriber::new(&self.bus, &self.topic, self.depth).await?)
    }
}

/// Per-step logical-time context (D34). `time()` is in the same domain as every
/// bus `produced_at_ns`.
#[derive(Clone, Copy, Debug)]
pub struct StepContext {
    epoch: u64,
    step_index: u64,
    time_ns: u64,
    dt_ns: u64,
    missed_ticks: u32,
}

impl StepContext {
    pub(crate) fn new(
        epoch: u64,
        step_index: u64,
        time_ns: u64,
        dt_ns: u64,
        missed_ticks: u32,
    ) -> Self {
        StepContext {
            epoch,
            step_index,
            time_ns,
            dt_ns,
            missed_ticks,
        }
    }

    /// Logical robot time for this step.
    pub fn time(&self) -> LogicalTime {
        LogicalTime::new(self.epoch, self.time_ns)
    }

    /// The clock epoch.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Monotonic step counter within the epoch.
    pub fn step_index(&self) -> u64 {
        self.step_index
    }

    /// Nanoseconds since the previous step.
    pub fn dt_ns(&self) -> u64 {
        self.dt_ns
    }

    /// `dt` as a [`Duration`].
    pub fn dt(&self) -> Duration {
        Duration::from_nanos(self.dt_ns)
    }

    /// Ticks collapsed into this step after an overrun (D34).
    pub fn missed_ticks(&self) -> u32 {
        self.missed_ticks
    }
}

/// Context for `#[shutdown]`: graceful park/stop/flush before bus close (D24/D43i).
///
/// The runner bounds the whole `#[shutdown]` hook by [`grace`](Self::grace): if the
/// hook is still running at the deadline, the runner logs, drops the hook, and
/// proceeds to bus close anyway so the process never leaks. Treat [`grace`](Self::grace)
/// as a budget for any internal flush/park deadlines and return before it elapses.
#[derive(Clone, Copy, Debug)]
pub struct ShutdownContext {
    grace: Duration,
}

impl ShutdownContext {
    pub(crate) fn new(grace: Duration) -> Self {
        ShutdownContext { grace }
    }

    /// The bounded grace period the runner allows the hook before it forces bus
    /// close. Sourced from `ParticipantLaunch::shutdown_grace_ms`.
    pub fn grace(&self) -> Duration {
        self.grace
    }
}
