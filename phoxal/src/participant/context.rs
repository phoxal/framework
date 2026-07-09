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
use crate::model::v0::Robot;
use crate::participant::managed::{ManagedTaskPolicy, ManagedTasks};
use crate::participant::spec::{
    DeclaresPublish, DeclaresQuery, DeclaresSubscribe, IsDriver, IsTool, ParticipantSpec,
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
/// `R` carries no bound at the struct level (widened for the NEW authoring
/// model, `phoxal::participant::api`): the OLD `ParticipantSpec`-bound
/// builders below still require it on their own `impl` block, and the NEW
/// `SetupContextApiExt` extension impl in `participant::api` requires
/// `R: Participant` on its own `impl` block instead - so `SetupContext<R>` is
/// usable for either authoring model without a struct-level bound picking one
/// (RECONCILIATION correction #7's "or `SetupContext` goes generic").
pub struct SetupContext<R> {
    bus: Bus,
    owner_cap: OwnerCap,
    robot: Option<Arc<Robot>>,
    robot_root: Option<PathBuf>,
    component_instance: Option<String>,
    managed_tasks: ManagedTasks,
    _runtime: PhantomData<fn() -> R>,
}

impl<R: ParticipantSpec> SetupContext<R> {
    /// The resolved robot model (`robot.yaml` + components + structure). Official
    /// participants build their typed state from this (D33); it is present only when
    /// the runner was launched with a robot root. Returns an error otherwise.
    pub fn robot(&self) -> crate::Result<&Robot> {
        self.robot.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "no robot model is bound (this participant was launched without a robot root)"
            )
        })
    }

    /// The robot root directory (holds the robot model + assets). Present only
    /// when launched with a robot root.
    pub fn robot_root(&self) -> crate::Result<&Path> {
        self.robot_root.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "no robot root is bound (this participant was launched without a robot root)"
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
}

/// Unbounded on `R` (no `ParticipantSpec`/`Participant` bound needed - these
/// just store/read raw fields or spawn/track tasks generically), so both the
/// OLD builders above and the NEW model's `SetupContextApiExt`
/// (`participant::api`, a sibling module) can call them. [`Self::new`],
/// [`Self::spawn_managed`], [`Self::spawn_managed_with`], and
/// [`Self::take_managed_tasks`] in particular moved here (F-runtime slice) so
/// the NEW-model runner (`participant::runner_v2`) can construct and drive a
/// `SetupContext<R>` for an `R: Participant` too - none of them ever needed
/// the `ParticipantSpec` bound, they were only grouped with the OLD-model
/// builders above for organizational reasons.
impl<R> SetupContext<R> {
    pub(crate) fn new(
        bus: Bus,
        owner_cap: OwnerCap,
        robot: Option<Arc<Robot>>,
        robot_root: Option<PathBuf>,
        component_instance: Option<String>,
    ) -> Self {
        SetupContext {
            bus,
            owner_cap,
            robot,
            robot_root,
            component_instance,
            managed_tasks: ManagedTasks::default(),
            _runtime: PhantomData,
        }
    }

    /// Spawn a runner-owned, long-lived background task (sensor polling loop,
    /// serial/USB reader, async IO pump) under the default
    /// [`ManagedTaskPolicy::FaultOnExit`] policy.
    ///
    /// This is the framework-tracked alternative to a raw `tokio::spawn`:
    /// **checked participants must not `tokio::spawn` long-lived work**, because
    /// the runner cannot observe, cancel, or join a detached task. A managed
    /// task, by contrast, is watched for the rest of the participant's
    /// lifetime - if it panics or returns while `FaultOnExit` applies, the
    /// runner treats that as a runtime fault (participant marked `Failed`,
    /// see the presence heartbeat) exactly as it would a `#[step]` bug it
    /// cannot recover from. At shutdown the runner cancels every managed task
    /// as the shutdown sequence starts and joins it within the same grace
    /// budget as `#[shutdown]` (see [`ShutdownContext::grace`]), before the bus
    /// closes.
    ///
    /// `name` is a short diagnostic label (e.g. `"serial-reader"`) surfaced in
    /// runner logs on fault or on an unjoined-at-shutdown report; it does not
    /// need to be unique. Use [`Self::spawn_managed_with`] for setup-time work
    /// that is expected to finish on its own ([`ManagedTaskPolicy::AllowExit`]).
    pub fn spawn_managed<F>(&mut self, name: impl Into<String>, future: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        self.spawn_managed_with(name, ManagedTaskPolicy::FaultOnExit, future);
    }

    /// [`Self::spawn_managed`] with an explicit [`ManagedTaskPolicy`].
    ///
    /// Use [`ManagedTaskPolicy::AllowExit`] for setup-time-only work (a
    /// background warm-up, a best-effort cache prime) whose completion should
    /// never fault the participant; anything meant to run for the participant's
    /// whole lifetime should keep the [`ManagedTaskPolicy::FaultOnExit`]
    /// default from [`Self::spawn_managed`].
    pub fn spawn_managed_with<F>(
        &mut self,
        name: impl Into<String>,
        policy: ManagedTaskPolicy,
        future: F,
    ) where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        self.managed_tasks.spawn(name, policy, future);
    }

    /// Hand the managed-task registry accumulated during `#[setup]` to the
    /// runner, which then owns watching/cancelling/joining them for the rest of
    /// the participant's lifetime. Called exactly once, after `#[setup]`
    /// returns.
    pub(crate) fn take_managed_tasks(&mut self) -> ManagedTasks {
        std::mem::take(&mut self.managed_tasks)
    }

    /// The underlying bus. Not on the default checked-participant surface (plan #00
    /// DoD #11 / plan #07): normal participants and examples cannot reach around
    /// the typed handle builders. Privileged participants that genuinely need raw
    /// access go through `phoxal::raw` (`Bus::open` + `run_with_bus`) or the
    /// tool-only [`Self::raw_bus`] accessor.
    pub(crate) fn bus(&self) -> &Bus {
        &self.bus
    }

    /// The runner-minted owner capability (plan #00 L2). In-crate accessor
    /// mirroring [`bus`](Self::bus): the OLD-model public
    /// [`owner_capability`](Self::owner_capability) is `ParticipantSpec`-bound,
    /// so the NEW model's `SetupContextApiExt::owner_capability`
    /// (`participant::api`) reads the raw field through this unbounded seam
    /// instead.
    pub(crate) fn owner_cap(&self) -> OwnerCap {
        self.owner_cap
    }

    /// The bound `robot.components` instance, if any. In-crate accessor for the
    /// NEW model's driver/simulator `component()` builders
    /// (`participant::api`); the OLD-model [`component`](Self::component) is
    /// `ParticipantSpec + IsDriver`-bound.
    pub(crate) fn component_instance(&self) -> Option<&str> {
        self.component_instance.as_deref()
    }
}

impl<R> SetupContext<R>
where
    R: ParticipantSpec + IsDriver,
{
    /// The `robot.components` entry this participant drives (D47/D53), for a
    /// component driver launched once per instance. Combine with [`Self::robot`]
    /// (`driver_binding`/`component_instance`) for the resolved component spec.
    pub fn component(&self) -> crate::Result<&str> {
        self.component_instance.as_deref().ok_or_else(|| {
            anyhow::anyhow!("no component instance is bound (this driver was launched without one)")
        })
    }
}

impl<R> SetupContext<R>
where
    R: ParticipantSpec + IsTool,
{
    /// Clone the runner-owned raw bus for privileged tool internals.
    ///
    /// The bus has already been opened from the clap/env launch contract, so tools
    /// that need raw access do not reparse launch env or open an unrelated session.
    pub fn raw_bus(&self) -> Bus {
        self.bus.clone()
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
