//! Participant contexts: `SetupContext` (IO construction), `ResetContext`
//! (simulation execution replacement), and `StepContext` (logical time per
//! scheduled step).

use std::marker::PhantomData;
use std::time::Duration;

use crate::__private::surface::{ComponentBoundSurface, TypedIoSurface};
use crate::ParticipantAssetResolver;
use crate::bundle::RuntimeBundle;
use crate::bus::{
    AskQuery, DEFAULT_QUERY_TIMEOUT, EventContract, EventPublisher, EventReceiver, Observed,
    Publish, Querier, QueryEndpointDescriptor, RobotInstant, SampleContract,
    SampleDeliveryContract, SamplePublisher, SampleReceiver, ServeQuery, SetpointContract,
    SetpointDeliveryContract, SetpointPublisher, SetpointReceiver, StateContract,
    StateDeliveryContract, StatePublisher, StateView, StepToken, StreamContract,
    StreamDeliveryContract, StreamPublisher, StreamReceiver, Subscribe, TimelineId, Topic,
};
use crate::bus::{BusHandle, ParticipantId};
use crate::model::Robot;
use crate::participant::api::Participant;
use crate::participant::managed::{ManagedTaskOutput, ManagedTaskPolicy, ManagedTasks};
use crate::participant::query::QueryRegistration;

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
    producer: crate::bus::ProducerId,
}

impl QueryContext {
    pub(crate) fn new(producer: crate::bus::ProducerId) -> Self {
        Self { producer }
    }

    /// The exact producer/session that sent this query.
    pub fn producer(&self) -> crate::bus::ProducerId {
        self.producer
    }
}

/// The sole IO-construction point, handed to `Participant::setup`.
pub struct SetupContext<R: Participant> {
    bus: BusHandle,
    /// The bundle this participant was launched against, if it was launched
    /// with one. Model and assets travel together because they are two views of
    /// the same load: there is no launch that binds one without the other.
    bundle: Option<RuntimeBundle>,
    /// The identity this process was launched under. It is what a driver's
    /// component binding is looked up by: a driver's participant id *is* its
    /// component instance id.
    participant_id: ParticipantId,
    managed_tasks: ManagedTasks,
    timeline_retentions: Vec<TimelineRetention>,
    queries: Vec<QueryRegistration<R>>,
    _runtime: PhantomData<fn() -> R>,
}

impl<R: Participant> SetupContext<R> {
    /// The producer identity of this participant's unique bus owner.
    pub fn producer(&self) -> crate::bus::ProducerId {
        self.bus.producer()
    }

    /// Subscribe to the execution-scoped exact Ready source set.  The
    /// returned stream is bounded and must be retained in the participant API
    /// so its observer remains alive for the participant lifetime.
    pub async fn participant_ready_events(
        &self,
    ) -> crate::Result<crate::bus::ParticipantReadyEvents> {
        Ok(self.bus.participant_ready_events().await?)
    }

    /// Subscribe only to the exact producer-qualified Ready keys for one
    /// fixed participant. This keeps unrelated participant churn outside the
    /// bounded authority-evidence queue.
    pub async fn participant_ready_events_for(
        &self,
        participant: &crate::bus::ParticipantId,
    ) -> crate::Result<crate::bus::ParticipantReadyEvents> {
        Ok(self.bus.participant_ready_events_for(participant).await?)
    }

    /// Observe one participant's Ready keys directly on the transport
    /// callback. Use this only for ingress admission that must see Ready loss
    /// before the next sample can enter a retained state view.
    pub async fn observe_participant_ready_for(
        &self,
        participant: &crate::bus::ParticipantId,
        callback: impl Fn(crate::bus::ParticipantReadyEvent) + Send + Sync + 'static,
    ) -> crate::Result<crate::bus::ParticipantReadyObserver> {
        Ok(self
            .bus
            .observe_participant_ready_for(participant, callback)
            .await?)
    }

    pub(crate) fn new(
        bus: BusHandle,
        bundle: Option<RuntimeBundle>,
        participant_id: ParticipantId,
    ) -> Self {
        SetupContext {
            bus,
            bundle,
            participant_id,
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
    /// revoke the participant Ready token) exactly as it would a `Participant::step` bug it
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

    /// The immutable canonical model read from the bundle's `manifest.json`.
    pub fn robot(&self) -> crate::Result<&Robot> {
        Ok(self.bundle()?.robot())
    }

    /// The assets under the bundle's `assets/` directory.
    pub fn assets(&self) -> crate::Result<&ParticipantAssetResolver> {
        Ok(self.bundle()?.assets())
    }

    fn bundle(&self) -> crate::Result<&RuntimeBundle> {
        self.bundle.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "no bundle is bound (this participant was launched without a bundle root)"
            )
        })
    }
}

/// Every typed-IO builder below binds its endpoint descriptor's `Api` to
/// `R::ContractApi`, the one contract family the role attribute fixed for this
/// participant. A descriptor from any other family is a compile error at the
/// builder call, not a runtime mismatch.
impl<R: Participant + TypedIoSurface> SetupContext<R> {
    pub fn state_publisher<B: StateContract<Api = R::ContractApi>>(
        &self,
        topic: Topic<Publish<B>>,
    ) -> crate::Result<StatePublisher<B>> {
        Ok(StatePublisher::new(self.bus.clone(), &topic)?)
    }

    pub fn sample_publisher<B: SampleContract<Api = R::ContractApi>>(
        &self,
        topic: Topic<Publish<B>>,
    ) -> crate::Result<SamplePublisher<B>> {
        Ok(SamplePublisher::new(self.bus.clone(), &topic)?)
    }

    pub fn setpoint_publisher<B: SetpointContract<Api = R::ContractApi>>(
        &self,
        topic: Topic<Publish<B>>,
    ) -> crate::Result<SetpointPublisher<B>> {
        Ok(SetpointPublisher::new(self.bus.clone(), &topic)?)
    }

    pub fn event_publisher<B: EventContract<Api = R::ContractApi>>(
        &self,
        topic: Topic<Publish<B>>,
    ) -> crate::Result<EventPublisher<B>> {
        Ok(EventPublisher::new(self.bus.clone(), &topic)?)
    }

    pub fn stream_publisher<B: StreamContract<Api = R::ContractApi>>(
        &self,
        topic: Topic<Publish<B>>,
    ) -> crate::Result<StreamPublisher<B>> {
        Ok(StreamPublisher::new(self.bus.clone(), &topic)?)
    }

    pub async fn state_view<
        B: StateContract<Api = R::ContractApi> + StateDeliveryContract<Api = R::ContractApi>,
    >(
        &mut self,
        topic: Topic<Subscribe<B>>,
    ) -> crate::Result<StateView<B>> {
        let handle = StateView::new(&self.bus, &topic).await?;
        let retained = handle.timeline_retention();
        self.register_timeline_retention(move |timeline| {
            retained.retain(timeline);
        });
        Ok(handle)
    }

    /// Build a state view whose synchronous source admission runs before the
    /// keep-last slot coalesces observations.
    #[doc(hidden)]
    pub async fn state_view_with_admission<
        B: StateContract<Api = R::ContractApi> + StateDeliveryContract<Api = R::ContractApi>,
        F: Fn(&Observed<B::Payload>) -> bool + Send + Sync + 'static,
    >(
        &mut self,
        topic: Topic<Subscribe<B>>,
        admission: F,
    ) -> crate::Result<StateView<B>> {
        let handle = StateView::new_with_admission(&self.bus, &topic, admission).await?;
        let retained = handle.timeline_retention();
        self.register_timeline_retention(move |timeline| {
            retained.retain(timeline);
        });
        Ok(handle)
    }

    pub async fn setpoint_receiver<
        B: SetpointContract<Api = R::ContractApi> + SetpointDeliveryContract<Api = R::ContractApi>,
    >(
        &mut self,
        topic: Topic<Subscribe<B>>,
    ) -> crate::Result<SetpointReceiver<B>> {
        let handle = SetpointReceiver::new(&self.bus, &topic).await?;
        let retained = handle.timeline_retention();
        self.register_timeline_retention(move |timeline| {
            retained.retain(timeline);
        });
        Ok(handle)
    }

    pub async fn event_receiver<B: EventContract<Api = R::ContractApi>>(
        &mut self,
        topic: Topic<Subscribe<B>>,
    ) -> crate::Result<EventReceiver<B>> {
        let handle = EventReceiver::new(&self.bus, &topic).await?;
        let retained = handle.timeline_retention();
        self.register_timeline_retention(move |timeline| {
            retained.retain(timeline);
        });
        Ok(handle)
    }

    pub async fn sample_receiver<
        B: SampleContract<Api = R::ContractApi> + SampleDeliveryContract<Api = R::ContractApi>,
    >(
        &mut self,
        topic: Topic<Subscribe<B>>,
    ) -> crate::Result<SampleReceiver<B>> {
        let handle = SampleReceiver::new(&self.bus, &topic).await?;
        let retained = handle.timeline_retention();
        self.register_timeline_retention(move |timeline| {
            retained.retain(timeline);
        });
        Ok(handle)
    }

    pub async fn stream_receiver<
        B: StreamContract<Api = R::ContractApi> + StreamDeliveryContract<Api = R::ContractApi>,
    >(
        &mut self,
        topic: Topic<Subscribe<B>>,
    ) -> crate::Result<StreamReceiver<B>> {
        let handle = StreamReceiver::new(&self.bus, &topic).await?;
        let retained = handle.timeline_retention();
        self.register_timeline_retention(move |timeline| {
            retained.retain(timeline);
        });
        Ok(handle)
    }

    /// Build the client handle for a descriptor-declared query.
    ///
    /// Requests express no robot time, calls have a finite timeout, no local
    /// backlog is exposed, and incomplete or ambiguous outcomes are typed
    /// [`crate::bus::QueryError`] values.
    pub fn querier<E: QueryEndpointDescriptor>(
        &self,
        topic: Topic<AskQuery<E>>,
    ) -> crate::Result<Querier<E::Request, E::Response>> {
        Ok(Querier::new(
            self.bus.clone(),
            &topic,
            DEFAULT_QUERY_TIMEOUT,
        )?)
    }

    /// Register the endpoint owner's synchronous typed query handler.
    ///
    /// Requests express no robot time. The runner admits and serializes handler
    /// evaluation with participant state; it emits one success response or a
    /// structured failure, and duplicate bindings are rejected at setup.
    pub fn query<E, H>(&mut self, topic: Topic<ServeQuery<E>>, handler: H) -> crate::Result<()>
    where
        E: QueryEndpointDescriptor,
        H: for<'a> Fn(
                &'a R,
                &'a R::Api,
                QueryContext,
                E::Request,
                &'a mut R::State,
            ) -> crate::bus::QueryResult<E::Response>
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
        self.queries
            .push(QueryRegistration::new::<E::Request, E::Response, H>(
                topic, handler,
            ));
        Ok(())
    }
}

impl<R: Participant + ComponentBoundSurface> SetupContext<R> {
    /// The compiled component this driver is bound to: the mounted instance,
    /// the type behind it, and how a simulated world models that type.
    ///
    /// The binding is the identity, not a separate field: a driver is launched
    /// once per `components` entry under that entry's own id, so the lookup is
    /// this process's participant id. An id the robot mounts no component for
    /// is a launch mistake, and it fails here.
    pub fn component(&self) -> crate::Result<crate::model::robot::ComponentView<'_>> {
        let id = &self.participant_id;
        self.robot()?.component(id.as_str()).ok_or_else(|| {
            anyhow::anyhow!("this driver's id '{id}' is not a component instance of the robot")
        })
    }
}

/// Per-step context: the robot instant this step reached, plus the capability
/// to publish state at it.
///
/// The [`StepToken`] is what a [`StatePublisher`](crate::bus::StatePublisher)
/// requires, and the runner is the only minter on the documented surface - so
/// a participant publishes state at the instant it actually reached, or not at
/// all ([`crate::bus`]'s docs state exactly how strong that is).
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
