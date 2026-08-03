//! Participant contexts: `SetupContext` (IO construction), `ResetContext`
//! (simulation execution replacement), and `StepContext` (logical time per
//! scheduled step).

use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use crate::__private::surface::{ComponentBoundSurface, TypedIoSurface, WorldAuthoritySurface};
use crate::AssetResolver;
use crate::bus::{
    AskQuery, CommandContract, CommandPublisher, ContractBody, DEFAULT_QUERY_TIMEOUT,
    DiagnosticContract, DiagnosticPublisher, Latest, MeasurementContract, MeasurementPublisher,
    Publish, Querier, RobotInstant, ServeQuery, StateContract, StatePublisher, StepToken,
    Subscribe, Subscriber, TimelineId, Topic, WorldClockContract,
};
use crate::model::Robot;
use crate::participant::api::{Participant, QueryRegistration};
use crate::participant::managed::{ManagedTaskPolicy, ManagedTasks};
use phoxal_bus::{Bus, TimelineAuthority, WorldClockPublisher};

pub(crate) type TimelineRetention = Box<dyn Fn(TimelineId) + Send + Sync>;

/// The sole IO-construction point, handed to `Participant::setup`.
pub struct SetupContext<R: Participant> {
    bus: Bus,
    robot: Option<Arc<Robot>>,
    assets: Option<AssetResolver>,
    component_instance: Option<String>,
    managed_tasks: ManagedTasks,
    timeline_retentions: Vec<TimelineRetention>,
    queries: Vec<QueryRegistration<R>>,
    _runtime: PhantomData<fn() -> R>,
}

impl<R: Participant> SetupContext<R> {
    pub(crate) fn new(
        bus: Bus,
        robot: Option<Arc<Robot>>,
        assets: Option<AssetResolver>,
        component_instance: Option<String>,
    ) -> Self {
        SetupContext {
            bus,
            robot,
            assets,
            component_instance,
            managed_tasks: ManagedTasks::default(),
            timeline_retentions: Vec::new(),
            queries: Vec::new(),
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
    /// lose the participant Liveliness token) exactly as it would a `Participant::step` bug it
    /// cannot recover from. At shutdown the runner cancels every managed task
    /// as the shutdown sequence starts and joins it within the same
    /// runner-enforced grace budget as `Participant::shutdown`, before the bus
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

    /// Hand the managed-task registry accumulated during `Participant::setup` to the
    /// runner, which then owns watching/cancelling/joining them for the rest of
    /// the participant's lifetime. Called exactly once, after `Participant::setup`
    /// returns.
    pub(crate) fn take_managed_tasks(&mut self) -> ManagedTasks {
        std::mem::take(&mut self.managed_tasks)
    }

    pub(crate) fn register_timeline_retention(
        &mut self,
        retention: impl Fn(TimelineId) + Send + Sync + 'static,
    ) {
        self.timeline_retentions.push(Box::new(retention));
    }

    pub(crate) fn take_timeline_retentions(&mut self) -> Vec<TimelineRetention> {
        std::mem::take(&mut self.timeline_retentions)
    }

    pub(crate) fn register_query(&mut self, registration: QueryRegistration<R>) {
        self.queries.push(registration);
    }

    pub(crate) fn query_registrations(&self) -> &[QueryRegistration<R>] {
        &self.queries
    }

    pub(crate) fn take_query_registrations(&mut self) -> Vec<QueryRegistration<R>> {
        std::mem::take(&mut self.queries)
    }

    /// The bound `robot.components` instance, if any. In-crate accessor for the
    /// driver/simulator `component()` builders (`participant::api`).
    pub(crate) fn component_instance(&self) -> Option<&str> {
        self.component_instance.as_deref()
    }

    /// The resolved robot model, if bound. In-crate accessor for
    /// [`SetupContext::robot`](super::api::SetupContext::robot).
    pub(crate) fn robot_ref(&self) -> Option<&Robot> {
        self.robot.as_deref()
    }

    /// The immutable canonical model decoded from `<bundle>/robot.json`.
    pub fn robot(&self) -> crate::Result<&Robot> {
        self.robot_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "no robot model is bound (this participant was launched without a bundle root)"
            )
        })
    }

    /// The validated assets compiled into this participant's runtime bundle.
    pub fn assets(&self) -> crate::Result<&AssetResolver> {
        self.assets.as_ref().ok_or_else(|| {
            anyhow::anyhow!("no compiled assets are bound (this participant has no bundle root)")
        })
    }
}

impl<R: Participant + TypedIoSurface> SetupContext<R> {
    pub async fn state_publisher<B: StateContract>(
        &self,
        topic: Topic<Publish<B>>,
    ) -> crate::Result<StatePublisher<B>> {
        Ok(StatePublisher::new(self.bus.clone(), &topic)?)
    }

    pub async fn measurement_publisher<B: MeasurementContract>(
        &self,
        topic: Topic<Publish<B>>,
    ) -> crate::Result<MeasurementPublisher<B>> {
        Ok(MeasurementPublisher::new(self.bus.clone(), &topic)?)
    }

    pub async fn command_publisher<B: CommandContract>(
        &self,
        topic: Topic<Publish<B>>,
    ) -> crate::Result<CommandPublisher<B>> {
        Ok(CommandPublisher::new(self.bus.clone(), &topic)?)
    }

    pub async fn diagnostic_publisher<B: DiagnosticContract>(
        &self,
        topic: Topic<Publish<B>>,
    ) -> crate::Result<DiagnosticPublisher<B>> {
        Ok(DiagnosticPublisher::new(self.bus.clone(), &topic)?)
    }

    pub async fn latest<B: ContractBody>(
        &mut self,
        topic: Topic<Subscribe<B>>,
    ) -> crate::Result<Latest<B>> {
        let handle = Latest::new(&self.bus, &topic).await?;
        let retained = handle.clone();
        self.register_timeline_retention(move |timeline| {
            retained.__retain_timeline(timeline);
        });
        Ok(handle)
    }

    pub async fn subscriber<B: ContractBody>(
        &mut self,
        topic: Topic<Subscribe<B>>,
        depth: usize,
    ) -> crate::Result<Subscriber<B>> {
        let handle = Subscriber::new(&self.bus, &topic, depth).await?;
        let retained = handle.clone();
        self.register_timeline_retention(move |timeline| {
            retained.__retain_timeline(timeline);
        });
        Ok(handle)
    }

    pub async fn querier<Req: ContractBody, Resp: ContractBody>(
        &self,
        topic: Topic<AskQuery<Req, Resp>>,
    ) -> crate::Result<Querier<Req, Resp>> {
        Ok(Querier::new(
            self.bus.clone(),
            &topic,
            DEFAULT_QUERY_TIMEOUT,
        )?)
    }

    pub async fn query<Req, Resp, H>(
        &mut self,
        topic: Topic<ServeQuery<Req, Resp>>,
        handler: H,
    ) -> crate::Result<()>
    where
        Req: ContractBody,
        Resp: ContractBody,
        H: for<'a> std::ops::AsyncFn(
                &'a R,
                &'a R::Api,
                Req,
                &'a mut R::State,
            ) -> crate::bus::QueryResult<Resp>
            + Send
            + Sync
            + 'static,
    {
        let topic = topic.key().to_string();
        if self
            .query_registrations()
            .iter()
            .any(|registration| registration.topic() == topic)
        {
            anyhow::bail!("duplicate query binding for '{topic}'");
        }
        self.register_query(QueryRegistration::new(topic, handler));
        Ok(())
    }
}

impl<R: Participant + ComponentBoundSurface> SetupContext<R> {
    /// The compiled component instance bound to this driver or simulator.
    pub fn component(&self) -> crate::Result<&crate::model::ComponentInstance> {
        let id = self.component_instance().ok_or_else(|| {
            anyhow::anyhow!("no component instance is bound for this participant launch")
        })?;
        Ok(self.robot()?.component_instance(id)?)
    }
}

impl<R: Participant + WorldAuthoritySurface> SetupContext<R> {
    pub fn timeline_authority(&self, timeline: TimelineId) -> crate::Result<TimelineAuthority> {
        Ok(TimelineAuthority::__mint(timeline)?)
    }

    pub async fn world_clock_publisher<B: WorldClockContract>(
        &self,
        topic: Topic<Publish<B>>,
    ) -> crate::Result<WorldClockPublisher<B>> {
        Ok(WorldClockPublisher::__mint(self.bus.clone(), &topic)?)
    }
}

/// Per-step context: the robot instant this step reached, plus the capability
/// to publish state at it.
///
/// The [`StepToken`] is what a [`StatePublisher`](crate::bus::StatePublisher)
/// requires, and the runner is the only minter on the documented surface - so
/// a participant publishes state at the instant it actually reached, or not at
/// all (#952 section D; `phoxal-bus`'s docs state exactly how strong that is).
#[derive(Clone, Copy, Debug)]
pub struct StepContext {
    token: StepToken,
    step_index: u64,
    dt: Duration,
    missed_ticks: u32,
}

/// Context for `Participant::reset`: the runner observed a different timeline and is
/// about to begin releasing steps for that world history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResetContext {
    previous_timeline: TimelineId,
    new_timeline: TimelineId,
}

impl ResetContext {
    pub(crate) fn new(previous_timeline: TimelineId, new_timeline: TimelineId) -> Self {
        Self {
            previous_timeline,
            new_timeline,
        }
    }

    /// The world history whose derived state must be discarded.
    pub fn previous_timeline(&self) -> TimelineId {
        self.previous_timeline
    }

    /// The newly active world history.
    pub fn new_timeline(&self) -> TimelineId {
        self.new_timeline
    }
}

impl StepContext {
    pub(crate) fn new(token: StepToken, step_index: u64, dt: Duration, missed_ticks: u32) -> Self {
        StepContext {
            token,
            step_index,
            dt,
            missed_ticks,
        }
    }

    /// The capability to publish state at this step's instant.
    pub fn token(&self) -> &StepToken {
        &self.token
    }

    /// The robot instant this step reached.
    pub fn now(&self) -> RobotInstant {
        crate::bus::StepStamp::instant(&self.token)
    }

    /// The world history this step belongs to.
    pub fn timeline(&self) -> TimelineId {
        self.now().timeline()
    }

    /// Monotonic step counter within the timeline.
    pub fn step_index(&self) -> u64 {
        self.step_index
    }

    /// Robot time since the previous step.
    pub fn dt(&self) -> Duration {
        self.dt
    }

    /// Ticks collapsed into this step after an overrun (D34).
    pub fn missed_ticks(&self) -> u32 {
        self.missed_ticks
    }
}
