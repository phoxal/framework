//! Host-side Live attachment session ownership.

use super::*;

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

/// One world-host transport bound to one execution by its own producer identity.
///
/// This is deliberately distinct from [`SimulatorSession`].
/// The host performs the source-bound supervisor attachment transaction, while
/// the per-Robot controller owns native device I/O and its Preparing lease.
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
