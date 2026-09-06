//! Narrow SDK for one per-Robot Live simulator controller.
//!
//! A controller joins one supervised execution, observes its source-bound
//! world attachment, stands in for simulated component drivers, and uses the
//! ordinary typed bus lanes for device IO. It never owns or replaces execution
//! time. Every Live transition is stamped from the execution's existing
//! monotonic timeline, and `simulation/step` is passive progress published
//! after that transition's outputs.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::bus::session::{BusConfig, BusOwner};
use crate::bus::{
    BusCloseReport, BusError, BusHandle, DEFAULT_QUERY_TIMEOUT, Endpoint, EventPublisher,
    FixedSourceLease, KeyLivelinessToken, LocalInstant, Observed, ParticipantReadyEvents,
    ParticipantReadyToken, Publish, Querier, QueryError, ReceiveTerminal, RobotEndpoint,
    RobotInstant, Sample, SamplePublisher, Setpoint, SetpointReceiver, SourceLabel,
    SourceLabelError, State, StatePublisher, Subscribe, Topic,
};
use crate::identity::{ExecutionId, ParticipantId, ProducerId};
use crate::model::world::{LiveAttachmentBoundary, WorldInstanceId, WorldProgress};
use crate::simulation::api::StepEvent;
use crate::supervisor::api::simulation::attach::{AttachRequest, AttachResponse};
use crate::supervisor::api::simulation::end::{EndRequest, EndResponse};
use crate::supervisor::api::simulation::{
    SimulationAttachmentPhase, SimulationAttachmentState, SimulationEndReason,
};
use crate::supervisor::api::time_domain::{TimeDomain, TimeMode};

/// A failure while attaching or operating one Live controller session.
#[derive(Debug, thiserror::Error)]
pub enum SimulatorError {
    #[error(
        "no Phoxal execution is reachable at {connect}; start the supervisor before the simulation"
    )]
    NoExecution { connect: String },
    #[error(
        "{count} Phoxal executions are reachable at {connect}, which must identify exactly one: {executions:?}"
    )]
    MultipleExecutions {
        connect: String,
        count: usize,
        executions: Vec<ExecutionId>,
    },
    #[error(transparent)]
    SourceLabel(#[from] SourceLabelError),
    #[error(transparent)]
    Bus(#[from] BusError),
    #[error(transparent)]
    Query(#[from] QueryError),
    #[error("execution attachment bootstrap failed: {detail}")]
    Bootstrap { detail: String },
    #[error("Live simulation requires an unchanged monotonic execution time domain")]
    NonMonotonicTimeDomain,
    #[error("the host monotonic clock is unavailable")]
    ClockUnavailable,
    #[error("the controller has no Active simulation attachment")]
    AttachmentInactive,
    #[error("attachment is bound to controller {expected}, not this session {observed}")]
    WrongController {
        expected: crate::identity::ProducerId,
        observed: crate::identity::ProducerId,
    },
    #[error("the Live attachment observer failed: {detail}")]
    AttachmentObserver { detail: String },
    #[error("the transition stamp no longer names the current Active attachment")]
    StaleTransition,
    #[error("StepEvent index {observed} does not match transition progress {expected}")]
    StepIndexMismatch { expected: u64, observed: u64 },
    #[error("world progress step {observed} does not immediately follow completed step {previous}")]
    NonMonotonicProgress { previous: u64, observed: u64 },
    #[error(transparent)]
    InvalidProgress(#[from] crate::model::world::WorldProgressError),
    #[error("the supervisor returned an invalid Live attachment: {detail}")]
    AttachmentProtocol { detail: String },
    #[error("the host attachment transaction task stopped: {detail}")]
    AttachmentTask { detail: String },
}

const ATTACHMENT_TRANSITION_CAPACITY: usize = 32;

/// Framework-owned transport and immutable facts common to every Live role.
///
/// Role-specific sessions retain separate authority after this point. The
/// controller owns device I/O, while the host owns attachment management.
struct LiveBootstrap {
    owner: BusOwner,
    bus: BusHandle,
    bootstrap: crate::execution::ExecutionBootstrap,
    robot: crate::model::Robot,
    assets: crate::bundle::ParticipantAssets,
}

async fn open_live_bootstrap(
    connect: String,
    label: String,
) -> Result<LiveBootstrap, SimulatorError> {
    let execution = crate::execution::resolve_execution(&connect)
        .await
        .map_err(simulator_bootstrap_error)?;
    let label = SourceLabel::new(label)?;
    let (owner, bus) = BusOwner::open(BusConfig::for_external(
        execution,
        Some(label),
        vec![connect],
    ))
    .await?;
    let result = async {
        let bootstrap = crate::execution::attach_execution(&bus)
            .await
            .map_err(|error| SimulatorError::Bootstrap {
                detail: error.to_string(),
            })?;
        if bootstrap.time_domain.mode != TimeMode::Monotonic {
            return Err(SimulatorError::NonMonotonicTimeDomain);
        }
        let robot = bootstrap.info.manifest.clone().into_robot();
        let assets =
            crate::bundle::ParticipantAssets::from_supervisor(bus.clone()).map_err(|error| {
                SimulatorError::Bootstrap {
                    detail: error.to_string(),
                }
            })?;
        Ok((bootstrap, robot, assets))
    }
    .await;
    match result {
        Ok((bootstrap, robot, assets)) => Ok(LiveBootstrap {
            owner,
            bus,
            bootstrap,
            robot,
            assets,
        }),
        Err(error) => {
            let _ = owner.close().await;
            Err(error)
        }
    }
}

fn simulator_bootstrap_error(error: crate::execution::BootstrapError) -> SimulatorError {
    match error {
        crate::execution::BootstrapError::NoExecution { endpoint } => {
            SimulatorError::NoExecution { connect: endpoint }
        }
        crate::execution::BootstrapError::MultipleExecutions {
            endpoint,
            count,
            executions,
        } => SimulatorError::MultipleExecutions {
            connect: endpoint,
            count,
            executions,
        },
        error => SimulatorError::Bootstrap {
            detail: error.to_string(),
        },
    }
}

/// The simulator session closed, but a close stage left evidence.
#[derive(Debug, thiserror::Error)]
#[error("the simulator session did not close cleanly: {report}")]
pub struct SimulatorCloseError {
    pub report: BusCloseReport,
}

/// Inputs for one controller session against one execution.
#[derive(Clone, Debug)]
pub struct SimulatorConnectOptions {
    pub connect: String,
    pub label: String,
}

/// Inputs for one source-bound world host session against one execution.
#[derive(Clone, Debug)]
pub struct SimulationHostConnectOptions {
    pub connect: String,
    pub label: String,
}

impl SimulationHostConnectOptions {
    #[must_use]
    pub fn new(connect: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            connect: connect.into(),
            label: label.into(),
        }
    }
}

impl SimulatorConnectOptions {
    #[must_use]
    pub fn new(connect: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            connect: connect.into(),
            label: label.into(),
        }
    }
}

/// One exact monotonic correlation shared by every output of a native
/// transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LiveTransitionStamp {
    instant: RobotInstant,
    world: WorldInstanceId,
    revision: u64,
    attached_at: LiveAttachmentBoundary,
    progress: WorldProgress,
}

/// One current Active attachment boundary for command selection immediately
/// before a native transition.
///
/// This stamp intentionally carries no [`WorldProgress`] and does not
/// implement [`crate::bus::StepStamp`]. It can filter commands and anchor
/// monotonic lease selection, but it cannot publish simulator output or a
/// [`StepEvent`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActiveBoundaryStamp {
    local: LocalInstant,
    instant: RobotInstant,
    world: WorldInstanceId,
    revision: u64,
    attached_at: LiveAttachmentBoundary,
}

/// A simulator sample publisher that can emit only under the exact current
/// Active controller binding.
pub struct LiveSamplePublisher<E: RobotEndpoint + Endpoint<Semantics = Sample>> {
    inner: SamplePublisher<E>,
    bus: BusHandle,
}

impl<E> Clone for LiveSamplePublisher<E>
where
    E: RobotEndpoint + Endpoint<Semantics = Sample>,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            bus: self.bus.clone(),
        }
    }
}

impl<E> LiveSamplePublisher<E>
where
    E: RobotEndpoint + Endpoint<Semantics = Sample>,
{
    /// Publish one sample from `transition` only while its exact controller
    /// and supervisor revision remain Active.
    pub fn publish(&self, transition: &LiveTransitionStamp, body: E) -> Result<(), SimulatorError> {
        let admitted = self.inner.publish_active_simulation(
            self.bus.producer(),
            transition.revision,
            crate::bus::CaptureStamp::exact(transition.instant()),
            body,
        )?;
        ensure_live_publication(admitted)
    }
}

/// A simulator state publisher that can emit only under the exact current
/// Active controller binding.
pub struct LiveStatePublisher<E: RobotEndpoint + Endpoint<Semantics = State>> {
    inner: StatePublisher<E>,
    bus: BusHandle,
}

impl<E> Clone for LiveStatePublisher<E>
where
    E: RobotEndpoint + Endpoint<Semantics = State>,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            bus: self.bus.clone(),
        }
    }
}

impl<E> LiveStatePublisher<E>
where
    E: RobotEndpoint + Endpoint<Semantics = State>,
{
    /// Publish state from `transition` only while its exact controller and
    /// supervisor revision remain Active.
    pub fn publish(&self, transition: &LiveTransitionStamp, body: E) -> Result<(), SimulatorError> {
        let admitted = self.inner.publish_active_simulation(
            self.bus.producer(),
            transition.revision,
            transition,
            body,
        )?;
        ensure_live_publication(admitted)
    }
}

impl ActiveBoundaryStamp {
    /// The execution's current monotonic robot instant.
    #[must_use]
    pub const fn instant(&self) -> RobotInstant {
        self.instant
    }

    /// The host-monotonic reading captured for lease selection at the same
    /// boundary as [`Self::instant`].
    #[must_use]
    pub const fn local_instant(&self) -> LocalInstant {
        self.local
    }

    #[must_use]
    pub const fn world(&self) -> WorldInstanceId {
        self.world
    }

    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub const fn attached_at(&self) -> LiveAttachmentBoundary {
        self.attached_at
    }
}

/// One source-bound host transaction after its ordered Preparing replacement
/// has been observed and before the supervisor returns the Active commit.
pub struct SimulationAttachTransaction {
    initial: SimulationAttachmentState,
    request: AttachRequest,
    host: ProducerId,
    time_domain: TimeDomain,
    response: Option<AttachTransactionResponse>,
    transaction_liveliness: Option<KeyLivelinessToken>,
    attachment_liveliness: Arc<tokio::sync::Mutex<Option<KeyLivelinessToken>>>,
    end: Querier<EndRequest>,
}

enum AttachTransactionResponse {
    Pending(tokio::task::JoinHandle<Result<AttachResponse, QueryError>>),
    Complete(AttachResponse),
}

impl SimulationAttachTransaction {
    /// The ordered attachment state observed before this handle was returned.
    /// A new transaction is Preparing. An idempotent retry may already be
    /// Active and completes immediately.
    #[must_use]
    pub const fn initial(&self) -> SimulationAttachmentState {
        self.initial
    }

    /// Await and validate the supervisor's Active commit.
    pub async fn commit(mut self) -> Result<AttachResponse, SimulatorError> {
        let response = match self
            .response
            .take()
            .ok_or_else(|| SimulatorError::AttachmentTask {
                detail: "the attachment transaction response was already consumed".to_owned(),
            })? {
            AttachTransactionResponse::Pending(task) => {
                task.await
                    .map_err(|error| SimulatorError::AttachmentTask {
                        detail: error.to_string(),
                    })??
            }
            AttachTransactionResponse::Complete(response) => response,
        };
        validate_attach_response(response, self.request, self.host, self.time_domain)?;
        let lease =
            self.transaction_liveliness
                .take()
                .ok_or_else(|| SimulatorError::AttachmentTask {
                    detail: "the attachment transaction lease was already consumed".to_owned(),
                })?;
        *self.attachment_liveliness.lock().await = Some(lease);
        Ok(response)
    }

    /// Abort this Preparing transaction, revoke its no-late-commit lease, and
    /// await the supervisor's source-bound Removing response.
    pub async fn abort(
        mut self,
        reason: SimulationEndReason,
    ) -> Result<EndResponse, SimulatorError> {
        if let Some(AttachTransactionResponse::Pending(task)) = self.response.take() {
            task.abort();
        }
        drop(self.transaction_liveliness.take());
        let response = self.end.query(EndRequest { reason }).await?;
        if response.attachment.phase != SimulationAttachmentPhase::Removing
            || response.attachment.host != self.host
        {
            return Err(SimulatorError::AttachmentProtocol {
                detail: "attachment abort did not converge to Removing under this host producer"
                    .to_owned(),
            });
        }
        Ok(response)
    }
}

impl Drop for SimulationAttachTransaction {
    fn drop(&mut self) {
        if let Some(AttachTransactionResponse::Pending(task)) = &self.response {
            task.abort();
        }
        // Dropping this token is the synchronous cancellation fence. The
        // supervisor observes it while Preparing and cannot activate after it
        // disappears, even though Zenoh may still deliver the abandoned query.
        self.transaction_liveliness.take();
    }
}

impl LiveTransitionStamp {
    #[must_use]
    pub const fn instant(&self) -> RobotInstant {
        self.instant
    }

    #[must_use]
    pub const fn world(&self) -> WorldInstanceId {
        self.world
    }

    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// The immutable progress-to-execution correlation captured when the
    /// execution joined this world.
    #[must_use]
    pub const fn attached_at(&self) -> LiveAttachmentBoundary {
        self.attached_at
    }

    /// The validated world progress completed by this native transition.
    #[must_use]
    pub const fn progress(&self) -> WorldProgress {
        self.progress
    }
}

impl crate::bus::handle::stamp::sealed::Sealed for LiveTransitionStamp {}

impl crate::bus::StepStamp for LiveTransitionStamp {
    fn instant(&self) -> RobotInstant {
        self.instant
    }
}

/// A setpoint receiver that exposes only intent produced under the exact
/// Active attachment revision used by a native transition.
pub struct LiveSetpointReceiver<E: RobotEndpoint + Endpoint<Semantics = Setpoint>> {
    inner: SetpointReceiver<E>,
    attachment: tokio::sync::watch::Receiver<Option<SimulationAttachmentState>>,
}

impl<E> LiveSetpointReceiver<E>
where
    E: RobotEndpoint + Endpoint<Semantics = Setpoint>,
{
    /// Take the next buffered command that belongs to `transition`.
    /// Commands from Preparing, Removing, a prior Active revision, or without
    /// revision evidence are discarded and can never become live later.
    pub fn try_recv_for(&self, transition: &LiveTransitionStamp) -> Option<Observed<E>> {
        self.try_recv_revision(transition.world, transition.revision)
    }

    /// Take the next buffered command for a current pre-transition Active
    /// boundary without inventing world progress.
    pub fn try_recv_at(&self, boundary: &ActiveBoundaryStamp) -> Option<Observed<E>> {
        self.try_recv_revision(boundary.world, boundary.revision)
    }

    fn try_recv_revision(&self, world: WorldInstanceId, revision: u64) -> Option<Observed<E>> {
        let active = self.attachment.borrow().is_some_and(|state| {
            state.phase == SimulationAttachmentPhase::Active
                && state.world == world
                && state.revision == revision
        });
        if !active {
            self.flush();
            return None;
        }
        while let Some(observed) = self.inner.try_recv() {
            if observed.metadata.attachment_revision == Some(revision) {
                return Some(observed);
            }
        }
        None
    }

    /// Drain every currently buffered command for `transition` through the
    /// capability's fixed-source lease.
    ///
    /// The lease remains the owner of source liveness, monotonic silence and
    /// hold expiry, stale sequence rejection, and fail-closed selection. Feed
    /// it [`ParticipantReadyEvents`] from [`SimulatorSession::participant_ready_events`],
    /// call this immediately before a native transition, then select with
    /// [`FixedSourceLease::live_host`] at the transition's host-monotonic
    /// boundary.
    pub fn drain_into(
        &self,
        transition: &LiveTransitionStamp,
        lease: &mut FixedSourceLease<E>,
    ) -> usize {
        let mut offered = 0;
        while let Some(observed) = self.try_recv_for(transition) {
            lease.offer(
                observed.metadata.source.participant_source(),
                observed.metadata.sequence,
                observed.observed_at,
                observed.body,
            );
            offered += 1;
        }
        offered
    }

    /// Drain commands for a pre-transition Active boundary through the typed
    /// source lease. Select the result with
    /// `lease.live_host(boundary.local_instant())` immediately before entering
    /// the native transition.
    pub fn drain_at(
        &self,
        boundary: &ActiveBoundaryStamp,
        lease: &mut FixedSourceLease<E>,
    ) -> usize {
        let mut offered = 0;
        while let Some(observed) = self.try_recv_at(boundary) {
            lease.offer(
                observed.metadata.source.participant_source(),
                observed.metadata.sequence,
                observed.observed_at,
                observed.body,
            );
            offered += 1;
        }
        offered
    }

    /// Discard every retained command, returning how many values were cleared.
    pub fn flush(&self) -> usize {
        let mut discarded = 0;
        while self.inner.try_recv().is_some() {
            discarded += 1;
        }
        discarded
    }

    pub fn terminal(&self) -> Option<ReceiveTerminal> {
        self.inner.terminal()
    }
}

/// One controller process attached to one execution.
pub struct SimulatorSession {
    presence: BTreeMap<ParticipantId, ParticipantReadyToken>,
    preparation: tokio::sync::Mutex<Option<(u64, KeyLivelinessToken)>>,
    attachment: tokio::sync::watch::Receiver<Option<SimulationAttachmentState>>,
    attachment_fault: Arc<Mutex<Option<String>>>,
    attachment_task: Option<tokio::task::JoinHandle<()>>,
    step: EventPublisher<StepEvent>,
    time_domain: TimeDomain,
    progress: Mutex<Option<(u64, WorldProgress)>>,
    robot: crate::model::Robot,
    assets: crate::bundle::ParticipantAssets,
    bus: BusHandle,
    execution: ExecutionId,
    owner: Option<BusOwner>,
}

impl std::fmt::Debug for SimulatorSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SimulatorSession")
            .field("execution", &self.execution)
            .field("presented", &self.presence.len())
            .field("attachment", &*self.attachment.borrow())
            .finish_non_exhaustive()
    }
}

impl SimulatorSession {
    pub async fn probe(connect: &str) -> Result<Vec<ExecutionId>, SimulatorError> {
        Ok(BusOwner::probe_routers(connect).await?)
    }

    /// Join the sole execution at `connect` and complete the frozen supervisor
    /// bootstrap before exposing any controller capability.
    pub async fn connect(options: SimulatorConnectOptions) -> Result<Self, SimulatorError> {
        let LiveBootstrap {
            owner,
            bus,
            bootstrap,
            robot,
            assets,
        } = open_live_bootstrap(options.connect, options.label).await?;
        let execution = bootstrap.execution;
        let step = match EventPublisher::new(
            bus.clone(),
            &crate::simulation::api::topics().step().owner(),
        ) {
            Ok(step) => step,
            Err(error) => {
                let _ = owner.close().await;
                return Err(error.into());
            }
        };
        let (attachment_tx, attachment) = tokio::sync::watch::channel(bootstrap.attachment);
        let attachment_fault = Arc::new(Mutex::new(None));
        let task_fault = Arc::clone(&attachment_fault);
        let time_domain = bootstrap.time_domain;
        install_active_controller_binding(&bus, bootstrap.attachment);
        let task_bus = bus.clone();
        let attachment_task = tokio::spawn(async move {
            observe_attachment(
                attachment_tx,
                None,
                bootstrap.attachments,
                bootstrap.time_domains,
                time_domain,
                task_fault,
                task_bus,
            )
            .await;
        });
        Ok(Self {
            presence: BTreeMap::new(),
            preparation: tokio::sync::Mutex::new(None),
            attachment,
            attachment_fault,
            attachment_task: Some(attachment_task),
            step,
            time_domain,
            progress: Mutex::new(None),
            robot,
            assets,
            bus,
            execution,
            owner: Some(owner),
        })
    }

    #[must_use]
    pub fn execution(&self) -> ExecutionId {
        self.execution
    }

    /// The producer identity that a host binds as the attachment controller.
    #[must_use]
    pub fn producer(&self) -> ProducerId {
        self.bus.producer()
    }

    /// The immutable robot model returned by the supervisor bootstrap.
    #[must_use]
    pub fn robot(&self) -> &crate::model::Robot {
        &self.robot
    }

    /// Lazy supervisor-backed access to the execution bundle's immutable
    /// assets.
    #[must_use]
    pub fn assets(&self) -> &crate::bundle::ParticipantAssets {
        &self.assets
    }

    /// The newest complete attachment state known to this controller.
    pub async fn attachment(&self) -> Result<Option<SimulationAttachmentState>, SimulatorError> {
        self.check_attachment_observer()?;
        Ok(*self.attachment.borrow())
    }

    /// Acknowledge the current Preparing revision after the controller has
    /// bound devices and flushed retained commands. The supervisor holds the
    /// attach query until this exact producer-qualified lease exists.
    pub async fn acknowledge_preparing(&self) -> Result<(), SimulatorError> {
        let attachment = self
            .attachment()
            .await?
            .ok_or(SimulatorError::AttachmentInactive)?;
        if attachment.phase != SimulationAttachmentPhase::Preparing {
            return Err(SimulatorError::AttachmentInactive);
        }
        self.ensure_controller(attachment)?;
        let mut preparation = self.preparation.lock().await;
        if preparation
            .as_ref()
            .is_some_and(|(revision, _)| *revision == attachment.revision)
        {
            return Ok(());
        }
        let owner = self.owner.as_ref().ok_or(BusError::Closed)?;
        let key = crate::supervisor::api::simulation::preparation_liveliness_key(
            attachment.revision,
            attachment.controller,
        );
        let token = owner.declare_liveliness_key(&key).await?;
        *preparation = Some((attachment.revision, token));
        Ok(())
    }

    /// Capture one current host-monotonic instant for all outputs of a native
    /// transition. This succeeds only for the exact Active controller binding.
    pub fn live_transition(
        &self,
        progress: WorldProgress,
    ) -> Result<LiveTransitionStamp, SimulatorError> {
        self.check_attachment_observer()?;
        let attachment = (*self.attachment.borrow()).ok_or(SimulatorError::AttachmentInactive)?;
        if attachment.phase != SimulationAttachmentPhase::Active {
            return Err(SimulatorError::AttachmentInactive);
        }
        self.ensure_controller(attachment)?;
        let mut cursor = self
            .progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = cursor
            .filter(|(revision, _)| *revision == attachment.revision)
            .map_or(attachment.attached_at.world, |(_, progress)| progress);
        validate_next_progress(previous, progress)?;
        let now = LocalInstant::try_now().ok_or(SimulatorError::ClockUnavailable)?;
        *cursor = Some((attachment.revision, progress));
        Ok(LiveTransitionStamp {
            instant: RobotInstant::new(self.time_domain.timeline, now.boot_ns()),
            world: attachment.world,
            revision: attachment.revision,
            attached_at: attachment.attached_at,
            progress,
        })
    }

    /// Capture a current Active monotonic boundary for command selection before
    /// a native transition. This does not inspect or advance world progress.
    pub fn active_boundary(&self) -> Result<ActiveBoundaryStamp, SimulatorError> {
        self.check_attachment_observer()?;
        let attachment = (*self.attachment.borrow()).ok_or(SimulatorError::AttachmentInactive)?;
        if attachment.phase != SimulationAttachmentPhase::Active {
            return Err(SimulatorError::AttachmentInactive);
        }
        self.ensure_controller(attachment)?;
        let local = LocalInstant::try_now().ok_or(SimulatorError::ClockUnavailable)?;
        Ok(ActiveBoundaryStamp {
            local,
            instant: RobotInstant::new(self.time_domain.timeline, local.boot_ns()),
            world: attachment.world,
            revision: attachment.revision,
            attached_at: attachment.attached_at,
        })
    }

    /// Publish passive progress after every output for the same transition.
    pub fn publish_step(
        &self,
        transition: &LiveTransitionStamp,
        event: StepEvent,
    ) -> Result<(), SimulatorError> {
        self.validate_transition(transition)?;
        let expected = transition.progress.completed_step();
        if event.index != expected {
            return Err(SimulatorError::StepIndexMismatch {
                expected,
                observed: event.index,
            });
        }
        admit_step_event(&self.step, &self.bus, transition, event)
    }

    pub fn sample_publisher<E>(
        &self,
        topic: Topic<Publish<E>>,
    ) -> Result<LiveSamplePublisher<E>, SimulatorError>
    where
        E: RobotEndpoint + Endpoint<Semantics = Sample>,
    {
        Ok(LiveSamplePublisher {
            inner: SamplePublisher::new(self.bus.clone(), &topic)?,
            bus: self.bus.clone(),
        })
    }

    pub fn state_publisher<E>(
        &self,
        topic: Topic<Publish<E>>,
    ) -> Result<LiveStatePublisher<E>, SimulatorError>
    where
        E: RobotEndpoint + Endpoint<Semantics = State>,
    {
        Ok(LiveStatePublisher {
            inner: StatePublisher::new(self.bus.clone(), &topic)?,
            bus: self.bus.clone(),
        })
    }

    pub async fn setpoint_receiver<E>(
        &self,
        topic: Topic<Subscribe<E>>,
    ) -> Result<LiveSetpointReceiver<E>, SimulatorError>
    where
        E: RobotEndpoint + Endpoint<Semantics = Setpoint>,
    {
        Ok(LiveSetpointReceiver {
            inner: SetpointReceiver::new(&self.bus, &topic).await?,
            attachment: self.attachment.clone(),
        })
    }

    pub async fn participant_ready_events(
        &self,
        participant: &ParticipantId,
    ) -> Result<ParticipantReadyEvents, SimulatorError> {
        Ok(self.bus.participant_ready_events_for(participant).await?)
    }

    pub async fn present(&mut self, participant: &ParticipantId) -> Result<(), SimulatorError> {
        if self.presence.contains_key(participant) {
            return Ok(());
        }
        let owner = self.owner.as_ref().ok_or(BusError::Closed)?;
        let token = owner.declare_participant_ready_as(participant).await?;
        self.presence.insert(participant.clone(), token);
        Ok(())
    }

    pub async fn close(mut self) -> Result<(), SimulatorCloseError> {
        self.presence.clear();
        *self.preparation.lock().await = None;
        if let Some(task) = self.attachment_task.take() {
            task.abort();
            let _ = task.await;
        }
        let Some(owner) = self.owner.take() else {
            return Ok(());
        };
        let report = owner.close().await;
        if report.is_clean() {
            Ok(())
        } else {
            Err(SimulatorCloseError { report })
        }
    }

    fn validate_transition(
        &self,
        transition: &LiveTransitionStamp,
    ) -> Result<SimulationAttachmentState, SimulatorError> {
        self.check_attachment_observer()?;
        let attachment = (*self.attachment.borrow()).ok_or(SimulatorError::StaleTransition)?;
        if attachment.phase != SimulationAttachmentPhase::Active
            || attachment.world != transition.world
            || attachment.revision != transition.revision
            || transition.instant.timeline() != self.time_domain.timeline
        {
            return Err(SimulatorError::StaleTransition);
        }
        self.ensure_controller(attachment)?;
        Ok(attachment)
    }

    fn ensure_controller(
        &self,
        attachment: SimulationAttachmentState,
    ) -> Result<(), SimulatorError> {
        let observed = self.bus.producer();
        if attachment.controller == observed {
            Ok(())
        } else {
            Err(SimulatorError::WrongController {
                expected: attachment.controller,
                observed,
            })
        }
    }

    fn check_attachment_observer(&self) -> Result<(), SimulatorError> {
        let fault = self
            .attachment_fault
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        match fault {
            Some(detail) => Err(SimulatorError::AttachmentObserver { detail }),
            None => Ok(()),
        }
    }
}

impl Drop for SimulatorSession {
    fn drop(&mut self) {
        if let Some(task) = &self.attachment_task {
            task.abort();
        }
    }
}

/// One world-host transport bound to one execution by its own producer
/// identity.
///
/// This is deliberately distinct from [`SimulatorSession`]. The host performs
/// the source-bound supervisor attachment transaction, while the per-Robot
/// controller owns native device IO and acknowledges its own Preparing lease.
pub struct SimulationHostSession {
    attachment: tokio::sync::watch::Receiver<Option<SimulationAttachmentState>>,
    attachment_transitions: tokio::sync::broadcast::Sender<SimulationAttachmentState>,
    attachment_fault: Arc<Mutex<Option<String>>>,
    attachment_task: Option<tokio::task::JoinHandle<()>>,
    removal_acknowledgement: tokio::sync::Mutex<Option<(u64, KeyLivelinessToken)>>,
    host_liveliness: Option<KeyLivelinessToken>,
    attachment_liveliness: Arc<tokio::sync::Mutex<Option<KeyLivelinessToken>>>,
    attach: Querier<AttachRequest>,
    end: Querier<EndRequest>,
    time_domain: TimeDomain,
    robot: crate::model::Robot,
    assets: crate::bundle::ParticipantAssets,
    bus: BusHandle,
    execution: ExecutionId,
    owner: Option<BusOwner>,
}

impl std::fmt::Debug for SimulationHostSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SimulationHostSession")
            .field("execution", &self.execution)
            .field("producer", &self.bus.producer())
            .field("attachment", &*self.attachment.borrow())
            .finish_non_exhaustive()
    }
}

impl SimulationHostSession {
    /// Join the sole execution at `connect` as the world host and complete the
    /// frozen supervisor bootstrap before exposing attachment authority.
    pub async fn connect(options: SimulationHostConnectOptions) -> Result<Self, SimulatorError> {
        let LiveBootstrap {
            owner,
            bus,
            bootstrap,
            robot,
            assets,
        } = open_live_bootstrap(options.connect, options.label).await?;
        let execution = bootstrap.execution;
        let attach = match Querier::new(
            bus.clone(),
            &crate::supervisor::api::topics()
                .simulation()
                .attach()
                .client(),
            DEFAULT_QUERY_TIMEOUT,
        ) {
            Ok(attach) => attach,
            Err(error) => {
                let _ = owner.close().await;
                return Err(error.into());
            }
        };
        let end = match Querier::new(
            bus.clone(),
            &crate::supervisor::api::topics().simulation().end().client(),
            DEFAULT_QUERY_TIMEOUT,
        ) {
            Ok(end) => end,
            Err(error) => {
                let _ = owner.close().await;
                return Err(error.into());
            }
        };
        let host_liveliness = match owner
            .declare_liveliness_key(&crate::supervisor::api::simulation::host_liveliness_key(
                bus.producer(),
            ))
            .await
        {
            Ok(token) => token,
            Err(error) => {
                let _ = owner.close().await;
                return Err(error.into());
            }
        };
        let (attachment_tx, attachment) = tokio::sync::watch::channel(bootstrap.attachment);
        let (attachment_transitions, _) =
            tokio::sync::broadcast::channel(ATTACHMENT_TRANSITION_CAPACITY);
        let task_transitions = attachment_transitions.clone();
        let attachment_fault = Arc::new(Mutex::new(None));
        let task_fault = Arc::clone(&attachment_fault);
        let time_domain = bootstrap.time_domain;
        let attachment_liveliness = Arc::new(tokio::sync::Mutex::new(None));
        let task_bus = bus.clone();
        let attachment_task = tokio::spawn(async move {
            observe_attachment(
                attachment_tx,
                Some(task_transitions),
                bootstrap.attachments,
                bootstrap.time_domains,
                time_domain,
                task_fault,
                task_bus,
            )
            .await;
        });
        Ok(Self {
            attachment,
            attachment_transitions,
            attachment_fault,
            attachment_task: Some(attachment_task),
            removal_acknowledgement: tokio::sync::Mutex::new(None),
            host_liveliness: Some(host_liveliness),
            attachment_liveliness,
            attach,
            end,
            time_domain,
            robot,
            assets,
            bus,
            execution,
            owner: Some(owner),
        })
    }

    #[must_use]
    pub fn execution(&self) -> ExecutionId {
        self.execution
    }

    /// The producer identity that the supervisor source-binds as host.
    #[must_use]
    pub fn producer(&self) -> ProducerId {
        self.bus.producer()
    }

    /// The immutable robot model returned by the supervisor bootstrap.
    #[must_use]
    pub fn robot(&self) -> &crate::model::Robot {
        &self.robot
    }

    /// Lazy supervisor-backed access to the execution bundle's immutable
    /// assets.
    #[must_use]
    pub fn assets(&self) -> &crate::bundle::ParticipantAssets {
        &self.assets
    }

    /// The execution domain that must remain unchanged for this session.
    #[must_use]
    pub const fn time_domain(&self) -> TimeDomain {
        self.time_domain
    }

    /// The newest complete supervisor attachment state.
    pub async fn attachment(&self) -> Result<Option<SimulationAttachmentState>, SimulatorError> {
        self.check_attachment_observer()?;
        Ok(*self.attachment.borrow())
    }

    /// Wait until the supervisor requests native removal for this bound host.
    ///
    /// The supervisor keeps its bus alive for a bounded grace after publishing
    /// Removing. The adapter must park the controller, remove the native Robot,
    /// release world membership, and then call [`Self::acknowledge_removal`].
    pub async fn wait_for_removing(&self) -> Result<SimulationAttachmentState, SimulatorError> {
        let mut attachment = self.attachment.clone();
        loop {
            self.check_attachment_observer()?;
            if let Some(current) = *attachment.borrow_and_update()
                && current.phase == SimulationAttachmentPhase::Removing
            {
                if current.host != self.producer() {
                    return Err(SimulatorError::AttachmentProtocol {
                        detail: "Removing was bound to another world host producer".to_owned(),
                    });
                }
                return Ok(current);
            }
            attachment
                .changed()
                .await
                .map_err(|_| SimulatorError::AttachmentObserver {
                    detail: "the supervisor attachment authority closed before Removing".to_owned(),
                })?;
        }
    }

    /// Acknowledge one Removing revision after native member cleanup is
    /// complete. Repeating the acknowledgement for the same revision is
    /// idempotent.
    pub async fn acknowledge_removal(&self) -> Result<SimulationAttachmentState, SimulatorError> {
        let removing = self.wait_for_removing().await?;
        let mut acknowledgement = self.removal_acknowledgement.lock().await;
        if acknowledgement
            .as_ref()
            .is_some_and(|(revision, _)| *revision == removing.revision)
        {
            return Ok(removing);
        }
        let owner = self.owner.as_ref().ok_or(BusError::Closed)?;
        let key = crate::supervisor::api::simulation::removal_liveliness_key(
            removing.revision,
            removing.host,
        );
        let token = owner.declare_liveliness_key(&key).await?;
        *acknowledgement = Some((removing.revision, token));
        Ok(removing)
    }

    /// Start the source-bound attachment query and return only after observing
    /// its ordered Preparing replacement.
    ///
    /// This split lets the world host publish truthful Preparing membership
    /// before awaiting controller acknowledgement and the Active commit.
    pub async fn begin_attach(
        &self,
        request: AttachRequest,
    ) -> Result<SimulationAttachTransaction, SimulatorError> {
        self.check_attachment_observer()?;
        let mut transitions = self.attachment_transitions.subscribe();
        let owner = self.owner.as_ref().ok_or(BusError::Closed)?;
        let transaction_key = crate::supervisor::api::simulation::transaction_liveliness_key(
            request.world(),
            self.producer(),
            request.controller(),
        );
        let transaction_liveliness = owner.declare_liveliness_key(&transaction_key).await?;
        let attach = self.attach.clone();
        let mut response = tokio::spawn(async move { attach.query(request).await });
        loop {
            tokio::select! {
                biased;
                transition = transitions.recv() => {
                    let transition = transition.map_err(|error| {
                        response.abort();
                        SimulatorError::AttachmentObserver {
                            detail: format!("the ordered attachment transition feed failed: {error}"),
                        }
                    })?;
                    if transition.host != self.producer()
                        || transition.controller != request.controller()
                        || transition.world != request.world()
                        || transition.attached_at.world != request.progress()
                    {
                        continue;
                    }
                    if transition.phase != SimulationAttachmentPhase::Preparing {
                        response.abort();
                        return Err(SimulatorError::AttachmentProtocol {
                            detail: "a new attachment reached a non-Preparing phase before the host observed Preparing".to_owned(),
                        });
                    }
                    return Ok(SimulationAttachTransaction {
                        initial: transition,
                        request,
                        host: self.producer(),
                        time_domain: self.time_domain,
                        response: Some(AttachTransactionResponse::Pending(response)),
                        transaction_liveliness: Some(transaction_liveliness),
                        attachment_liveliness: Arc::clone(&self.attachment_liveliness),
                        end: self.end.clone(),
                    });
                }
                joined = &mut response => {
                    let response = joined
                        .map_err(|error| SimulatorError::AttachmentTask {
                            detail: error.to_string(),
                        })??;
                    validate_attach_response(
                        response,
                        request,
                        self.producer(),
                        self.time_domain,
                    )?;
                    return Ok(SimulationAttachTransaction {
                        initial: response.attachment,
                        request,
                        host: self.producer(),
                        time_domain: self.time_domain,
                        response: Some(AttachTransactionResponse::Complete(response)),
                        transaction_liveliness: Some(transaction_liveliness),
                        attachment_liveliness: Arc::clone(&self.attachment_liveliness),
                        end: self.end.clone(),
                    });
                }
            }
        }
    }

    /// Perform the complete source-bound Preparing-to-Active transaction.
    pub async fn attach(&self, request: AttachRequest) -> Result<AttachResponse, SimulatorError> {
        self.begin_attach(request).await?.commit().await
    }

    /// Enter Removing for this host's current attachment.
    pub async fn end(&self, reason: SimulationEndReason) -> Result<EndResponse, SimulatorError> {
        self.check_attachment_observer()?;
        let response = self.end.query(EndRequest { reason }).await?;
        if response.attachment.phase != SimulationAttachmentPhase::Removing
            || response.attachment.host != self.producer()
        {
            return Err(SimulatorError::AttachmentProtocol {
                detail: "end response was not Removing under this host producer".to_owned(),
            });
        }
        Ok(response)
    }

    pub async fn close(mut self) -> Result<(), SimulatorCloseError> {
        *self.removal_acknowledgement.lock().await = None;
        *self.attachment_liveliness.lock().await = None;
        self.host_liveliness.take();
        if let Some(task) = self.attachment_task.take() {
            task.abort();
            let _ = task.await;
        }
        let Some(owner) = self.owner.take() else {
            return Ok(());
        };
        let report = owner.close().await;
        if report.is_clean() {
            Ok(())
        } else {
            Err(SimulatorCloseError { report })
        }
    }

    fn check_attachment_observer(&self) -> Result<(), SimulatorError> {
        let fault = self
            .attachment_fault
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        match fault {
            Some(detail) => Err(SimulatorError::AttachmentObserver { detail }),
            None => Ok(()),
        }
    }
}

impl Drop for SimulationHostSession {
    fn drop(&mut self) {
        if let Some(task) = &self.attachment_task {
            task.abort();
        }
    }
}

fn validate_attach_response(
    response: AttachResponse,
    request: AttachRequest,
    host: ProducerId,
    time_domain: TimeDomain,
) -> Result<(), SimulatorError> {
    let attachment = response.attachment;
    if response.time_domain != time_domain
        || attachment.phase != SimulationAttachmentPhase::Active
        || attachment.host != host
        || attachment.controller != request.controller()
        || attachment.world != request.world()
        || attachment.attached_at.world != request.progress()
    {
        return Err(SimulatorError::AttachmentProtocol {
            detail: "attach response did not preserve the requested source binding, progress boundary, and monotonic domain".to_owned(),
        });
    }
    Ok(())
}

fn validate_next_progress(
    previous: WorldProgress,
    observed: WorldProgress,
) -> Result<(), SimulatorError> {
    let expected =
        previous
            .completed_step()
            .checked_add(1)
            .ok_or(SimulatorError::NonMonotonicProgress {
                previous: previous.completed_step(),
                observed: observed.completed_step(),
            })?;
    if observed.completed_step() != expected || observed.elapsed_ns() <= previous.elapsed_ns() {
        return Err(SimulatorError::NonMonotonicProgress {
            previous: previous.completed_step(),
            observed: observed.completed_step(),
        });
    }
    let time_step_ns = if previous.completed_step() == 0 {
        observed
            .elapsed_ns()
            .checked_sub(previous.elapsed_ns())
            .ok_or(SimulatorError::NonMonotonicProgress {
                previous: previous.completed_step(),
                observed: observed.completed_step(),
            })?
    } else {
        let completed = previous.completed_step();
        previous.elapsed_ns() / completed
    };
    previous.validate(time_step_ns)?;
    observed.validate(time_step_ns)?;
    Ok(())
}

async fn observe_attachment(
    attachment: tokio::sync::watch::Sender<Option<SimulationAttachmentState>>,
    transitions: Option<tokio::sync::broadcast::Sender<SimulationAttachmentState>>,
    attachments: crate::bus::StreamReceiver<
        crate::supervisor::api::simulation::attachment::SimulationAttachmentStream,
    >,
    time_domains: crate::bus::StreamReceiver<crate::supervisor::api::time_domain::TimeDomainStream>,
    initial_domain: TimeDomain,
    fault: Arc<Mutex<Option<String>>>,
    bus: BusHandle,
) {
    let controller_bus = transitions.is_none().then_some(&bus);
    let result: Result<(), String> = async {
        // A retained stream does not close when its router disappears. Observe the
        // execution-scoped supervisor identity separately, as ordinary sessions do.
        let (lost_tx, mut lost) = tokio::sync::watch::channel(false);
        let identity = bus
            .observe_liveliness_key(
                crate::supervisor::api::connect::PRESENCE_KEY,
                move |status| {
                    if status == crate::bus::LivelinessStatus::Lost {
                        lost_tx.send_replace(true);
                    }
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        if identity.initial() == crate::bus::LivelinessStatus::Lost || *lost.borrow() {
            return Err("the supervisor identity was lost".to_owned());
        }
        let mut transport_check = tokio::time::interval(std::time::Duration::from_millis(250));
        transport_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = transport_check.tick() => {
                    // Client reconnection can retain remote tokens after a hard router exit.
                    // Local transport identity must also remain present, independently of physics.
                    if !bus.execution_router_connected().await.map_err(|error| error.to_string())? {
                        return Err("the supervisor identity lost its execution router".to_owned());
                    }
                }
                _ = lost.changed() => {
                    return Err("the supervisor identity was lost".to_owned());
                }
                update = attachments.recv() => {
                    let update = update.map_err(|error| error.to_string())?;
                    let replacement = update.body.attachment;
                    let current = *attachment.borrow();
                    match (replacement, current) {
                        (Some(replacement), Some(installed))
                            if replacement.revision > installed.revision =>
                        {
                            if let Some(bus) = &controller_bus {
                                install_active_controller_binding(bus, Some(replacement));
                            }
                            attachment.send_replace(Some(replacement));
                            if let Some(transitions) = &transitions {
                                let _ = transitions.send(replacement);
                            }
                        }
                        (Some(replacement), None) => {
                            if let Some(bus) = &controller_bus {
                                install_active_controller_binding(bus, Some(replacement));
                            }
                            attachment.send_replace(Some(replacement));
                            if let Some(transitions) = &transitions {
                                let _ = transitions.send(replacement);
                            }
                        }
                        // Absence is only the initial empty authority in Live
                        // v0. Removing remains retained terminal evidence.
                        (None, _) | (Some(_), Some(_)) => {}
                    }
                }
                update = time_domains.recv() => {
                    let update = update.map_err(|error| error.to_string())?.body.domain;
                    if update.revision > initial_domain.revision {
                        return Err(format!(
                            "the execution time domain changed from revision {} to {} during Live attachment",
                            initial_domain.revision,
                            update.revision,
                        ));
                    }
                }
            }
        }
    }
    .await;
    if let Err(detail) = result {
        if let Some(bus) = &controller_bus {
            bus.set_active_simulation_binding(None);
        }
        *fault
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(detail);
    }
}

fn install_active_controller_binding(
    bus: &BusHandle,
    attachment: Option<SimulationAttachmentState>,
) {
    let binding = attachment.and_then(|state| {
        (state.phase == SimulationAttachmentPhase::Active)
            .then_some((state.controller, state.revision))
    });
    bus.set_active_simulation_binding(binding);
}

fn ensure_live_publication(admitted: bool) -> Result<(), SimulatorError> {
    if admitted {
        Ok(())
    } else {
        Err(SimulatorError::StaleTransition)
    }
}

fn admit_step_event(
    publisher: &EventPublisher<StepEvent>,
    bus: &BusHandle,
    transition: &LiveTransitionStamp,
    event: StepEvent,
) -> Result<(), SimulatorError> {
    let admitted = publisher.publish_active_simulation(
        bus.producer(),
        transition.revision,
        transition,
        event,
    )?;
    ensure_live_publication(admitted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::DeliveryFamily;
    use crate::identity::TimelineId;
    use crate::model::identity::CapabilityId;
    use crate::model::world::WorldProgress;

    #[serial_test::serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn supervisor_identity_loss_ends_observation_even_when_streams_stay_open() {
        for controller in [true, false] {
            let (owner, bus) = BusOwner::open(BusConfig::for_external(
                ExecutionId::mint(),
                None,
                Vec::new(),
            ))
            .await
            .unwrap();
            let identity = owner
                .declare_liveliness_key(crate::supervisor::api::connect::PRESENCE_KEY)
                .await
                .unwrap();
            let attachments = crate::bus::StreamReceiver::new(
                &bus,
                &crate::supervisor::api::topics()
                    .simulation()
                    .attachment()
                    .client(),
            )
            .await
            .unwrap();
            let domains = crate::bus::StreamReceiver::new(
                &bus,
                &crate::supervisor::api::topics().time_domain().client(),
            )
            .await
            .unwrap();
            let (attachment, _current) = tokio::sync::watch::channel(None);
            let transitions = (!controller).then(|| tokio::sync::broadcast::channel(8).0);
            let fault = Arc::new(Mutex::new(None));
            bus.set_active_simulation_binding(Some((bus.producer(), 7)));
            let observer = tokio::spawn(observe_attachment(
                attachment,
                transitions,
                attachments,
                domains,
                TimeDomain {
                    revision: 1,
                    timeline: TimelineId::mint(),
                    mode: TimeMode::Monotonic,
                },
                Arc::clone(&fault),
                bus.clone(),
            ));
            tokio::task::yield_now().await;
            drop(identity);
            tokio::time::timeout(std::time::Duration::from_secs(5), observer)
                .await
                .expect("identity loss cannot wait for a retained stream")
                .unwrap();
            assert!(
                fault
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .contains("supervisor identity")
            );
            if controller {
                assert!(
                    bus.active_simulation_delivery_metadata(
                        bus.producer(),
                        7,
                        DeliveryFamily::Sample,
                        None,
                    )
                    .unwrap()
                    .is_none()
                );
            }
            let _ = owner.close().await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn live_publishers_fail_closed_outside_the_exact_active_binding() {
        let (owner, bus) = BusOwner::open(BusConfig::for_external(
            ExecutionId::mint(),
            None,
            Vec::new(),
        ))
        .await
        .expect("test simulator bus opens");
        let progress = WorldProgress::at(1, 12).expect("valid progress");
        let transition = LiveTransitionStamp {
            instant: RobotInstant::new(TimelineId::mint(), 1),
            world: WorldInstanceId::mint(),
            revision: 7,
            attached_at: LiveAttachmentBoundary {
                world: WorldProgress::zero(12).expect("valid initial progress"),
                execution: RobotInstant::new(TimelineId::mint(), 0),
            },
            progress,
        };

        assert!(
            bus.active_simulation_delivery_metadata(
                bus.producer(),
                transition.revision,
                crate::bus::DeliveryFamily::Sample,
                Some(crate::bus::TimeWindow::exact(transition.instant())),
            )
            .expect("metadata check succeeds")
            .is_none()
        );
        bus.set_active_simulation_binding(Some((bus.producer(), 6)));
        assert!(
            bus.active_simulation_delivery_metadata(
                bus.producer(),
                transition.revision,
                crate::bus::DeliveryFamily::Sample,
                Some(crate::bus::TimeWindow::exact(transition.instant())),
            )
            .expect("metadata check succeeds")
            .is_none()
        );
        bus.set_active_simulation_binding(Some((bus.producer(), 7)));
        let metadata = bus
            .active_simulation_delivery_metadata(
                bus.producer(),
                transition.revision,
                crate::bus::DeliveryFamily::Sample,
                Some(crate::bus::TimeWindow::exact(transition.instant())),
            )
            .expect("metadata check succeeds")
            .expect("the exact current Active binding admits publication");
        assert_eq!(metadata.attachment_revision, Some(transition.revision));
        bus.set_active_simulation_binding(None);
        assert!(
            bus.active_simulation_delivery_metadata(
                bus.producer(),
                transition.revision,
                crate::bus::DeliveryFamily::Sample,
                Some(crate::bus::TimeWindow::exact(transition.instant())),
            )
            .expect("metadata check succeeds")
            .is_none()
        );

        let _ = owner.close().await;
    }

    #[serial_test::serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_live_transition_admits_outputs_then_step_with_one_exact_instant() {
        let execution = ExecutionId::mint();
        let (owner, bus) = BusOwner::open(BusConfig::for_external(execution, None, Vec::new()))
            .await
            .expect("test simulator bus opens");
        let state = LiveStatePublisher {
            inner: StatePublisher::new(bus.clone(), &crate::api::topics().drive().state().owner())
                .expect("state publisher"),
            bus: bus.clone(),
        };
        let component =
            crate::identity::ComponentInstanceId::new("accelerometer").expect("component id");
        let capability = CapabilityId::new("linear").expect("capability id");
        let sample_topic = crate::api::topics()
            .component(&component)
            .expect("component topic")
            .accelerometer(&capability)
            .expect("accelerometer topic")
            .sample()
            .owner();
        let sample = LiveSamplePublisher {
            inner: SamplePublisher::new(bus.clone(), &sample_topic).expect("sample publisher"),
            bus: bus.clone(),
        };
        let step = EventPublisher::new(
            bus.clone(),
            &crate::simulation::api::topics().step().owner(),
        )
        .expect("step publisher");
        let pause = bus
            .test_pause_outbound_drain()
            .await
            .expect("the one bus drain can be held before admission");
        let timeline = TimelineId::mint();
        let revision = 7;
        let transition = LiveTransitionStamp {
            instant: RobotInstant::new(timeline, 123),
            world: WorldInstanceId::mint(),
            revision,
            attached_at: LiveAttachmentBoundary {
                world: WorldProgress::zero(12).expect("initial world progress"),
                execution: RobotInstant::new(timeline, 100),
            },
            progress: WorldProgress::at(1, 12).expect("completed world transition"),
        };
        bus.set_active_simulation_binding(Some((bus.producer(), revision)));

        state
            .publish(
                &transition,
                crate::api::drive::State::Stopped {
                    target: crate::api::drive::Target::stopped(),
                    reason: crate::api::drive::StopReason::Fault,
                },
            )
            .expect("state output is admitted without draining");
        sample
            .publish(
                &transition,
                crate::api::component::accelerometer::Sample::try_new([1.0, 2.0, 3.0])
                    .expect("finite sample"),
            )
            .expect("sample output is admitted without draining");
        admit_step_event(
            &step,
            &bus,
            &transition,
            StepEvent {
                index: transition.progress().completed_step(),
            },
        )
        .expect("StepEvent is admitted without draining");

        let mut queued = bus.test_queued_delivery_metadata();
        queued.sort_by_key(|(_, _, metadata)| metadata.sequence);
        assert_eq!(
            queued.len(),
            3,
            "all publications use the one bus scheduler"
        );
        assert_eq!(
            queued
                .iter()
                .map(|(_, family, _)| *family)
                .collect::<Vec<_>>(),
            vec![
                DeliveryFamily::State,
                DeliveryFamily::Sample,
                DeliveryFamily::Stream,
            ]
        );
        assert!(queued[0].0.ends_with("robot/drive/state"));
        assert!(
            queued[1]
                .0
                .ends_with("robot/component/accelerometer/accelerometer/linear/sample")
        );
        assert!(queued[2].0.ends_with("simulation/step"));
        assert_eq!(
            queued
                .iter()
                .map(|(_, _, metadata)| metadata.sequence)
                .collect::<Vec<_>>(),
            vec![0, 1, 2],
            "the StepEvent is admitted after every output in local producer order"
        );
        for (_, _, metadata) in &queued {
            assert_eq!(metadata.produced_exactly_at(), Some(transition.instant()));
            assert_eq!(metadata.attachment_revision, Some(revision));
        }

        drop(pause);
        let _ = owner
            .close_until(tokio::time::Instant::now() + std::time::Duration::from_secs(10))
            .await;
    }

    #[test]
    fn a_transition_stamp_keeps_execution_and_world_time_separate() {
        let timeline = TimelineId::mint();
        let world = WorldInstanceId::mint();
        let attached_at = LiveAttachmentBoundary {
            world: WorldProgress::at(4, 12).unwrap(),
            execution: RobotInstant::new(timeline, 90),
        };
        let stamp = LiveTransitionStamp {
            instant: RobotInstant::new(timeline, 100),
            world,
            revision: 7,
            attached_at,
            progress: WorldProgress::at(5, 12).unwrap(),
        };
        assert_eq!(stamp.instant(), RobotInstant::new(timeline, 100));
        assert_eq!(stamp.world(), world);
        assert_eq!(stamp.revision(), 7);
        assert_eq!(stamp.attached_at(), attached_at);
        assert_eq!(stamp.progress().completed_step(), 5);
    }

    #[test]
    fn an_active_boundary_carries_no_world_progress_or_step_authority() {
        let timeline = TimelineId::mint();
        let world = WorldInstanceId::mint();
        let local = LocalInstant::from_boot_ns(100);
        let attached_at = LiveAttachmentBoundary {
            world: WorldProgress::at(4, 12).unwrap(),
            execution: RobotInstant::new(timeline, 90),
        };
        let boundary = ActiveBoundaryStamp {
            local,
            instant: RobotInstant::new(timeline, 100),
            world,
            revision: 7,
            attached_at,
        };
        assert_eq!(boundary.local_instant(), local);
        assert_eq!(boundary.instant(), RobotInstant::new(timeline, 100));
        assert_eq!(boundary.world(), world);
        assert_eq!(boundary.revision(), 7);
        assert_eq!(boundary.attached_at(), attached_at);
    }

    #[test]
    fn transition_progress_must_advance_by_one_exact_quantum() {
        let previous = WorldProgress::at(4, 12).expect("valid progress");
        validate_next_progress(
            previous,
            WorldProgress::at(5, 12).expect("the next exact quantum"),
        )
        .expect("one exact transition is accepted");
        assert!(matches!(
            validate_next_progress(
                previous,
                WorldProgress::at(6, 12).expect("valid but skipped progress")
            ),
            Err(SimulatorError::NonMonotonicProgress {
                previous: 4,
                observed: 6,
            })
        ));
        let inconsistent: WorldProgress = serde_json::from_value(serde_json::json!({
            "completed_step": 5,
            "elapsed_ns": 65,
        }))
        .expect("the fields imply a positive quantum before session validation");
        assert!(matches!(
            validate_next_progress(previous, inconsistent),
            Err(SimulatorError::InvalidProgress(
                crate::model::world::WorldProgressError::Inconsistent { .. }
            ))
        ));
    }
}
