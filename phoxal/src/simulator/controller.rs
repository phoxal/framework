//! Controller-side Live simulator session ownership.

use super::*;

/// Inputs for one controller session against one execution.
#[derive(Clone, Debug)]
pub struct SimulatorConnectOptions {
    pub connect: String,
    pub label: String,
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
