//! Participant contexts: `SetupContext` (IO construction), `ResetContext`
//! (simulation execution replacement), and `StepContext` (logical time per
//! scheduled step).

use std::marker::PhantomData;
use std::time::Duration;

use crate::__private::surface::{ComponentBoundSurface, TypedIoSurface, WorldAuthoritySurface};
use crate::ParticipantAssetResolver;
use crate::bus::{
    AskQuery, CommandContract, CommandPublisher, ContractBody, DEFAULT_QUERY_TIMEOUT,
    DiagnosticContract, DiagnosticPublisher, Latest, MeasurementContract, MeasurementPublisher,
    Publish, Querier, RobotInstant, ServeQuery, StateContract, StatePublisher, StepToken,
    StreamContract, StreamPublisher, Subscribe, Subscriber, TimelineId, Topic, WorldClockContract,
};
use crate::model::Robot;
use crate::participant::api::{Participant, QueryRegistration};
use crate::participant::managed::{ManagedTaskOutput, ManagedTaskPolicy, ManagedTasks};
use crate::participant::runner::inputs::ParticipantBundleInputs;
use phoxal_bus::{BusHandle, TimelineAuthority, WorldClockPublisher};

pub(crate) type TimelineRetention = Box<dyn Fn(TimelineId) + Send + Sync>;

/// Trusted requester provenance for one admitted query.
///
/// The runner constructs this only after decoding and validating the bus
/// metadata attachment.  Request bodies therefore never need to carry a
/// caller identity field that could disagree with the source session.  The
/// transport's participant/topology label deliberately stays inside the
/// framework boundary; handlers receive only the producer identity that is
/// authoritative for requester ownership.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryContext {
    producer: phoxal_bus::ProducerId,
}

impl QueryContext {
    pub(crate) fn new(producer: phoxal_bus::ProducerId) -> Self {
        Self { producer }
    }

    /// The exact producer/session that sent this query.
    pub fn producer(&self) -> phoxal_bus::ProducerId {
        self.producer
    }
}

/// The sole IO-construction point, handed to `Participant::setup`.
pub struct SetupContext<R: Participant> {
    bus: BusHandle,
    /// The finalized bundle this participant was launched against, if it was
    /// launched with one. Model and assets travel together because they are two
    /// views of the same load: there is no launch that binds one without the
    /// other.
    bundle: Option<ParticipantBundleInputs>,
    component_instance: Option<String>,
    managed_tasks: ManagedTasks,
    timeline_retentions: Vec<TimelineRetention>,
    queries: Vec<QueryRegistration<R>>,
    _runtime: PhantomData<fn() -> R>,
}

impl<R: Participant> SetupContext<R> {
    /// The producer identity of this participant's unique bus owner.
    pub fn producer(&self) -> phoxal_bus::ProducerId {
        self.bus.producer()
    }

    /// Subscribe to the execution-scoped exact Ready source set.  The
    /// returned stream is bounded and must be retained in the participant API
    /// so its observer remains alive for the participant lifetime.
    pub async fn participant_ready_events(
        &self,
    ) -> crate::Result<phoxal_bus::ParticipantReadyEvents> {
        Ok(self.bus.participant_ready_events().await?)
    }

    pub(crate) fn new(
        bus: BusHandle,
        bundle: Option<ParticipantBundleInputs>,
        component_instance: Option<String>,
    ) -> Self {
        SetupContext {
            bus,
            bundle,
            component_instance,
            managed_tasks: ManagedTasks::default(),
            timeline_retentions: Vec::new(),
            queries: Vec::new(),
            _runtime: PhantomData,
        }
    }

    /// Spawn a runner-owned, long-lived background task (sensor polling loop,
    /// serial/USB reader, async IO pump) under the default
    /// [`ManagedTaskPolicy::Critical`] policy.
    ///
    /// This is the framework-tracked alternative to a raw `tokio::spawn`:
    /// **checked participants must not `tokio::spawn` long-lived work**, because
    /// the runner cannot observe, cancel, or join a detached task. A managed
    /// task, by contrast, is watched for the rest of the participant's
    /// lifetime - if it panics or returns while `Critical` applies, the
    /// runner treats that as a runtime fault (participant marked `Failed`,
    /// lose the participant Liveliness token) exactly as it would a `Participant::step` bug it
    /// cannot recover from. After `Participant::shutdown` has had the required
    /// I/O available, the runner cancels every managed task and joins it within
    /// the same runner-enforced grace budget, before the bus closes.
    ///
    /// `name` is a short diagnostic label (e.g. `"serial-reader"`) surfaced in
    /// runner logs on fault or on an unjoined-at-shutdown report; it does not
    /// need to be unique. Use [`Self::spawn_managed_with`] for setup-time work
    /// that is expected to finish on its own ([`ManagedTaskPolicy::Finite`]).
    pub fn spawn_managed<F>(&mut self, name: impl Into<String>, future: F)
    where
        F: std::future::Future + Send + 'static,
        F::Output: ManagedTaskOutput,
    {
        self.spawn_managed_with(name, ManagedTaskPolicy::Critical, future);
    }

    /// [`Self::spawn_managed`] with an explicit [`ManagedTaskPolicy`].
    ///
    /// Use [`ManagedTaskPolicy::Finite`] for setup-time-only work (a
    /// background warm-up, a best-effort cache prime) whose completion should
    /// never fault the participant; anything meant to run for the participant's
    /// whole lifetime should keep the [`ManagedTaskPolicy::Critical`]
    /// default from [`Self::spawn_managed`].
    pub fn spawn_managed_with<F>(
        &mut self,
        name: impl Into<String>,
        policy: ManagedTaskPolicy,
        future: F,
    ) where
        F: std::future::Future + Send + 'static,
        F::Output: ManagedTaskOutput,
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

    pub(crate) fn take_query_registrations(&mut self) -> Vec<QueryRegistration<R>> {
        std::mem::take(&mut self.queries)
    }

    /// The immutable canonical model loaded from the finalized bundle.
    pub fn robot(&self) -> crate::Result<&Robot> {
        let bundle = self.bundle.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "no robot model is bound (this participant was launched without a bundle root)"
            )
        })?;
        Ok(&bundle.robot)
    }

    /// The validated assets this participant's runtime bundle declares.
    pub fn assets(&self) -> crate::Result<&ParticipantAssetResolver> {
        let bundle = self.bundle.as_ref().ok_or_else(|| {
            anyhow::anyhow!("no bundle assets are bound (this participant has no bundle root)")
        })?;
        Ok(&bundle.assets)
    }
}

/// Every typed-IO builder below binds its body's `ContractBody::Api` to
/// `R::ContractApi`, the one revision the role attribute fixed for this
/// participant. A body from any other API - a second revision, or another
/// `phoxal_api_tree!` tree such as a process-boundary protocol - is a compile
/// error at the builder call, not a runtime mismatch: this is what makes the
/// `api` field of the participant's embedded metadata record truthful.
impl<R: Participant + TypedIoSurface> SetupContext<R> {
    pub fn state_publisher<B: StateContract<Api = R::ContractApi>>(
        &self,
        topic: Topic<Publish<B>>,
    ) -> crate::Result<StatePublisher<B>> {
        Ok(StatePublisher::new(self.bus.clone(), &topic)?)
    }

    pub fn measurement_publisher<B: MeasurementContract<Api = R::ContractApi>>(
        &self,
        topic: Topic<Publish<B>>,
    ) -> crate::Result<MeasurementPublisher<B>> {
        Ok(MeasurementPublisher::new(self.bus.clone(), &topic)?)
    }

    pub fn command_publisher<B: CommandContract<Api = R::ContractApi>>(
        &self,
        topic: Topic<Publish<B>>,
    ) -> crate::Result<CommandPublisher<B>> {
        Ok(CommandPublisher::new(self.bus.clone(), &topic)?)
    }

    pub fn stream_publisher<B: StreamContract<Api = R::ContractApi>>(
        &self,
        topic: Topic<Publish<B>>,
    ) -> crate::Result<StreamPublisher<B>> {
        Ok(StreamPublisher::new(self.bus.clone(), &topic)?)
    }

    pub fn diagnostic_publisher<B: DiagnosticContract<Api = R::ContractApi>>(
        &self,
        topic: Topic<Publish<B>>,
    ) -> crate::Result<DiagnosticPublisher<B>> {
        Ok(DiagnosticPublisher::new(self.bus.clone(), &topic)?)
    }

    pub async fn latest<B: ContractBody<Api = R::ContractApi>>(
        &mut self,
        topic: Topic<Subscribe<B>>,
    ) -> crate::Result<Latest<B>> {
        let handle = Latest::new(&self.bus, &topic).await?;
        let retained = handle.clone();
        self.register_timeline_retention(move |timeline| {
            retained.retain_timeline(timeline);
        });
        Ok(handle)
    }

    pub async fn subscriber<B: ContractBody<Api = R::ContractApi>>(
        &mut self,
        topic: Topic<Subscribe<B>>,
    ) -> crate::Result<Subscriber<B>> {
        let handle = Subscriber::new(&self.bus, &topic).await?;
        let retained = handle.clone();
        self.register_timeline_retention(move |timeline| {
            retained.retain_timeline(timeline);
        });
        Ok(handle)
    }

    pub fn querier<
        Req: ContractBody<Api = R::ContractApi>,
        Resp: ContractBody<Api = R::ContractApi>,
    >(
        &self,
        topic: Topic<AskQuery<Req, Resp>>,
    ) -> crate::Result<Querier<Req, Resp>> {
        Ok(Querier::new(
            self.bus.clone(),
            &topic,
            DEFAULT_QUERY_TIMEOUT,
        )?)
    }

    pub fn query<Req, Resp, H>(
        &mut self,
        topic: Topic<ServeQuery<Req, Resp>>,
        handler: H,
    ) -> crate::Result<()>
    where
        Req: ContractBody<Api = R::ContractApi>,
        Resp: ContractBody<Api = R::ContractApi>,
        H: for<'a> Fn(
                &'a R,
                &'a R::Api,
                QueryContext,
                Req,
                &'a mut R::State,
            ) -> crate::bus::QueryResult<Resp>
            + Send
            + Sync
            + 'static,
    {
        let topic = topic.key().to_string();
        if self
            .queries
            .iter()
            .any(|registration| registration.topic() == topic)
        {
            anyhow::bail!("duplicate query binding for '{topic}'");
        }
        self.queries.push(QueryRegistration::new(topic, handler));
        Ok(())
    }
}

impl<R: Participant + ComponentBoundSurface> SetupContext<R> {
    /// The compiled component instance bound to this driver or simulator.
    pub fn component(&self) -> crate::Result<&crate::model::robot::ComponentInstance> {
        let id = self.component_instance.as_deref().ok_or_else(|| {
            anyhow::anyhow!("no component instance is bound for this participant record")
        })?;
        self.robot()?.component_instance(id).ok_or_else(|| {
            anyhow::anyhow!("the bound component instance '{id}' is not in the robot model")
        })
    }
}

impl<R: Participant + WorldAuthoritySurface> SetupContext<R> {
    pub fn timeline_authority(&self, timeline: TimelineId) -> crate::Result<TimelineAuthority> {
        Ok(TimelineAuthority::mint(timeline)?)
    }

    pub fn world_clock_publisher<B: WorldClockContract<Api = R::ContractApi>>(
        &self,
        topic: Topic<Publish<B>>,
    ) -> crate::Result<WorldClockPublisher<B>> {
        Ok(WorldClockPublisher::mint(self.bus.clone(), &topic)?)
    }
}

/// Per-step context: the robot instant this step reached, plus the capability
/// to publish state at it.
///
/// The [`StepToken`] is what a [`StatePublisher`](crate::bus::StatePublisher)
/// requires, and the runner is the only minter on the documented surface - so
/// a participant publishes state at the instant it actually reached, or not at
/// all (`phoxal-bus`'s docs state exactly how strong that is).
///
/// The fields are public because there is nothing here to protect: this is a
/// per-step `Copy` carrier the runner fills once and hands over, and the
/// guarantee lives in the [`StepToken`] itself rather than in any invariant
/// between these four values.
#[derive(Clone, Copy, Debug)]
pub struct StepContext {
    /// The capability to publish state at this step's instant.
    pub token: StepToken,
    /// Monotonic step counter within the timeline.
    pub step_index: u64,
    /// Robot time since the previous step.
    pub dt: Duration,
    /// Ticks collapsed into this step after an overrun.
    pub missed_ticks: u32,
}

/// Context for `Participant::reset`: the runner observed a different timeline and is
/// about to begin releasing steps for that world history.
///
/// Public fields for the same reason as [`StepContext`]: two opaque identities
/// the runner fills once, with nothing to keep consistent between them.
/// Timelines have no generation order, so "previous" and "new" are roles in this
/// one transition, not a relation the type could enforce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResetContext {
    /// The world history whose derived state must be discarded.
    pub previous_timeline: TimelineId,
    /// The newly active world history.
    pub new_timeline: TimelineId,
}

impl StepContext {
    /// The robot instant this step reached, as the token records it.
    pub fn now(&self) -> RobotInstant {
        crate::bus::StepStamp::instant(&self.token)
    }

    /// The world history this step belongs to.
    pub fn timeline(&self) -> TimelineId {
        self.now().timeline()
    }
}
