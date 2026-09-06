use super::*;

pub(super) async fn serve_attachments(bus: BusHandle, state: ExecutionState) -> Result<()> {
    let publisher =
        StreamPublisher::new(bus, &supervisor::topics().simulation().attachment().owner())?;
    let mut attachments = state
        .take_attachment_updates()
        .context("the supervisor attachment authority is already being served")?;
    while let Some(attachment) = attachments.recv().await {
        publisher
            .send(supervisor::simulation::attachment::SimulationAttachmentStream { attachment })?;
    }
    bail!("the supervisor attachment authority closed")
}

pub(super) async fn serve_current_attachment(bus: BusHandle, state: ExecutionState) -> Result<()> {
    let server = declare(
        &bus,
        &supervisor::topics()
            .simulation()
            .attachment()
            .current()
            .owner(),
    )
    .await?;
    loop {
        let incoming = server.recv().await?;
        let _: supervisor::simulation::attachment::CurrentRequest = match decode(&incoming).await? {
            Some(request) => request,
            None => continue,
        };
        reply(
            &incoming,
            &bus,
            &supervisor::simulation::attachment::CurrentResponse {
                attachment: state.attachment(),
            },
        )
        .await?;
    }
}

/// Serialize a Live attachment, holding the query until the exact controller
/// acknowledges the Preparing revision through its execution-scoped lease.
pub(super) async fn serve_attach(
    bus: BusHandle,
    state: ExecutionState,
    shutdown: CancellationToken,
) -> Result<()> {
    let server = declare(&bus, &supervisor::topics().simulation().attach().owner()).await?;
    loop {
        let incoming = server.recv().await?;
        let request: supervisor::simulation::attach::AttachRequest = match decode(&incoming).await?
        {
            Some(request) => request,
            None => continue,
        };
        let host = incoming.request_metadata()?.source.producer();
        let (preparing, time_domain) = match state.prepare_attachment(host, request) {
            Ok(attached) => attached,
            Err(error) => {
                incoming
                    .reply_err(&QueryFailure::invalid_argument(error.to_string()))
                    .await?;
                continue;
            }
        };
        let (attachment, time_domain) = match preparing.phase {
            supervisor::simulation::SimulationAttachmentPhase::Active => (preparing, time_domain),
            supervisor::simulation::SimulationAttachmentPhase::Preparing => {
                if let Err(error) = wait_for_prepared_controller(
                    &bus,
                    preparing,
                    &shutdown,
                )
                .await
                {
                    match state.abort_preparing_attachment(host, preparing.revision) {
                        Ok(removing) => {
                            tracing::warn!(
                                revision = removing.revision,
                                world = %removing.world,
                                host = %removing.host,
                                controller = %removing.controller,
                                error = %error,
                                "simulation attachment preparation was rolled back"
                            );
                        }
                        Err(super::super::state::AttachmentStateError::NotPreparing) => {}
                        Err(state_error) => return Err(state_error.into()),
                    }
                    incoming
                        .reply_err(&QueryFailure::unavailable(error.to_string()))
                        .await?;
                    shutdown.cancel();
                    continue;
                }
                match state.activate_attachment(host, preparing.revision) {
                    Ok(active) => active,
                    Err(error) => {
                        match state.abort_preparing_attachment(host, preparing.revision) {
                            Ok(removing) => {
                                tracing::warn!(
                                    revision = removing.revision,
                                    controller = %removing.controller,
                                    error = %error,
                                    "simulation attachment failed its final Active admission recheck"
                                );
                            }
                            Err(super::super::state::AttachmentStateError::NotPreparing) => {}
                            Err(state_error) => return Err(state_error.into()),
                        }
                        incoming
                            .reply_err(&QueryFailure::unavailable(error.to_string()))
                            .await?;
                        shutdown.cancel();
                        continue;
                    }
                }
            }
            supervisor::simulation::SimulationAttachmentPhase::Removing => {
                incoming
                    .reply_err(&QueryFailure::unavailable(
                        "the existing simulation attachment is being removed",
                    ))
                    .await?;
                continue;
            }
        };
        reply(
            &incoming,
            &bus,
            &supervisor::simulation::attach::AttachResponse {
                attachment,
                time_domain,
            },
        )
        .await?;
    }
}

async fn wait_for_prepared_controller(
    bus: &BusHandle,
    attachment: supervisor::simulation::SimulationAttachmentState,
    shutdown: &CancellationToken,
) -> std::result::Result<(), PreparationWaitError> {
    tokio::time::timeout(
        SIMULATION_PREPARATION_GRACE,
        wait_for_prepared_controller_inner(bus, attachment, shutdown),
    )
    .await
    .map_err(|_| PreparationWaitError::TimedOut)?
}

async fn wait_for_prepared_controller_inner(
    bus: &BusHandle,
    attachment: supervisor::simulation::SimulationAttachmentState,
    shutdown: &CancellationToken,
) -> std::result::Result<(), PreparationWaitError> {
    let prepared_key = supervisor::simulation::preparation_liveliness_key(
        attachment.revision,
        attachment.controller,
    );
    let host_key = supervisor::simulation::host_liveliness_key(attachment.host);
    let transaction_key = supervisor::simulation::transaction_liveliness_key(
        attachment.world,
        attachment.host,
        attachment.controller,
    );
    let (prepared_observer, mut prepared) = observe_status(bus, &prepared_key).await?;
    let (host_observer, mut host) = observe_status(bus, &host_key).await?;
    let (transaction_observer, mut transaction) = observe_status(bus, &transaction_key).await?;
    let mut prepared_alive = latest_status(&prepared_observer, &prepared) == LivelinessStatus::Alive;
    let mut host_alive = latest_status(&host_observer, &host) == LivelinessStatus::Alive;
    let mut transaction_alive =
        latest_status(&transaction_observer, &transaction) == LivelinessStatus::Alive;
    let mut prepared_was_alive = prepared_alive;
    let mut host_was_alive = host_alive;
    let mut transaction_was_alive = transaction_alive;

    loop {
        if prepared_alive && host_alive && transaction_alive {
            return Ok(());
        }
        tokio::select! {
            biased;
            changed = transaction.changed() => {
                changed.map_err(|_| PreparationWaitError::ObserverClosed)?;
                transaction_alive = *transaction.borrow_and_update() == Some(LivelinessStatus::Alive);
                if transaction_was_alive && !transaction_alive {
                    return Err(PreparationWaitError::TransactionAbandoned);
                }
                transaction_was_alive |= transaction_alive;
            }
            changed = host.changed() => {
                changed.map_err(|_| PreparationWaitError::ObserverClosed)?;
                host_alive = *host.borrow_and_update() == Some(LivelinessStatus::Alive);
                if host_was_alive && !host_alive {
                    return Err(PreparationWaitError::HostLost);
                }
                host_was_alive |= host_alive;
            }
            changed = prepared.changed() => {
                changed.map_err(|_| PreparationWaitError::ObserverClosed)?;
                prepared_alive = *prepared.borrow_and_update() == Some(LivelinessStatus::Alive);
                if prepared_was_alive && !prepared_alive {
                    return Err(PreparationWaitError::ControllerLost);
                }
                prepared_was_alive |= prepared_alive;
            }
            () = shutdown.cancelled() => return Err(PreparationWaitError::Cancelled),
        }
    }
}

async fn observe_status(
    bus: &BusHandle,
    key: &str,
) -> std::result::Result<
    (
        crate::bus::KeyLivelinessObserver,
        tokio::sync::watch::Receiver<Option<LivelinessStatus>>,
    ),
    PreparationWaitError,
> {
    let (status_tx, status_rx) = tokio::sync::watch::channel(None);
    let observer = bus
        .observe_liveliness_key(key, move |status| {
            status_tx.send_replace(Some(status));
        })
        .await
        .map_err(|error| PreparationWaitError::Observer(error.to_string()))?;
    Ok((observer, status_rx))
}

fn latest_status(
    observer: &crate::bus::KeyLivelinessObserver,
    status: &tokio::sync::watch::Receiver<Option<LivelinessStatus>>,
) -> LivelinessStatus {
    (*status.borrow()).unwrap_or_else(|| observer.initial())
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
enum PreparationWaitError {
    #[error("simulation attachment preparation exceeded its 4 s deadline")]
    TimedOut,
    #[error("the world host disappeared during simulation attachment preparation")]
    HostLost,
    #[error("the controller withdrew its preparation acknowledgement")]
    ControllerLost,
    #[error("the world host abandoned the simulation attachment transaction")]
    TransactionAbandoned,
    #[error("the execution began shutting down during simulation attachment preparation")]
    Cancelled,
    #[error("a simulation attachment liveliness observer closed")]
    ObserverClosed,
    #[error("failed to establish a simulation attachment liveliness observer: {0}")]
    Observer(String),
}

pub(super) async fn serve_attachment_liveness(
    bus: BusHandle,
    state: ExecutionState,
    shutdown: CancellationToken,
) -> Result<()> {
    let mut attachments = state.subscribe_attachment();
    loop {
        let Some(active) = *attachments.borrow_and_update() else {
            attachments
                .changed()
                .await
                .context("the supervisor attachment authority closed")?;
            continue;
        };
        if active.phase != supervisor::simulation::SimulationAttachmentPhase::Active {
            attachments
                .changed()
                .await
                .context("the supervisor attachment authority closed")?;
            continue;
        }
        let Some(reason) = monitor_active_attachment(
            &bus,
            &state,
            &mut attachments,
            active,
        )
        .await?
        else {
            continue;
        };
        match state.fail_active_attachment(active.revision, reason) {
            Ok(removing) => {
                tracing::error!(
                    ?reason,
                    revision = removing.revision,
                    world = %removing.world,
                    host = %removing.host,
                    controller = %removing.controller,
                    "Active simulation attachment lost a bound authority"
                );
                shutdown.cancel();
                std::future::pending::<()>().await;
            }
            Err(super::super::state::AttachmentStateError::NotActiveRevision) => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

async fn monitor_active_attachment(
    bus: &BusHandle,
    state: &ExecutionState,
    attachments: &mut tokio::sync::watch::Receiver<
        Option<supervisor::simulation::SimulationAttachmentState>,
    >,
    active: supervisor::simulation::SimulationAttachmentState,
) -> Result<Option<supervisor::simulation::SimulationEndReason>> {
    let host_key = supervisor::simulation::host_liveliness_key(active.host);
    let (status_tx, mut host_status) = tokio::sync::watch::channel(None);
    let host_observer = bus
        .observe_liveliness_key(&host_key, move |status| {
            status_tx.send_replace(Some(status));
        })
        .await?;
    let transaction_key = supervisor::simulation::transaction_liveliness_key(
        active.world,
        active.host,
        active.controller,
    );
    let (transaction_tx, mut transaction_status) = tokio::sync::watch::channel(None);
    let transaction_observer = bus
        .observe_liveliness_key(&transaction_key, move |status| {
            transaction_tx.send_replace(Some(status));
        })
        .await?;
    if state.attachment() != Some(active) {
        return Ok(None);
    }
    if latest_status(&host_observer, &host_status) != LivelinessStatus::Alive {
        return Ok(Some(supervisor::simulation::SimulationEndReason::HostLost));
    }
    if latest_status(&transaction_observer, &transaction_status) != LivelinessStatus::Alive {
        return Ok(Some(
            supervisor::simulation::SimulationEndReason::ProtocolViolation,
        ));
    }
    if !state.controller_is_exclusive(active.controller) {
        return Ok(Some(
            supervisor::simulation::SimulationEndReason::ControllerLost,
        ));
    }
    let mut snapshots = state.subscribe();
    loop {
        tokio::select! {
            biased;
            changed = attachments.changed() => {
                changed.context("the supervisor attachment authority closed")?;
                return Ok(None);
            }
            changed = host_status.changed() => {
                changed.context("the world-host liveness observer closed")?;
                if *host_status.borrow_and_update() == Some(LivelinessStatus::Lost) {
                    return Ok(Some(supervisor::simulation::SimulationEndReason::HostLost));
                }
            }
            changed = transaction_status.changed() => {
                changed.context("the attachment transaction liveness observer closed")?;
                if *transaction_status.borrow_and_update() == Some(LivelinessStatus::Lost) {
                    return Ok(Some(
                        supervisor::simulation::SimulationEndReason::ProtocolViolation,
                    ));
                }
            }
            changed = snapshots.changed() => {
                changed.context("the supervisor presence authority closed")?;
                let _ = snapshots.borrow_and_update();
                if !state.controller_is_exclusive(active.controller) {
                    return Ok(Some(
                        supervisor::simulation::SimulationEndReason::ControllerLost,
                    ));
                }
            }
        }
    }
}

pub(super) async fn serve_simulation_end(
    bus: BusHandle,
    state: ExecutionState,
    shutdown: CancellationToken,
) -> Result<()> {
    let server = declare(&bus, &supervisor::topics().simulation().end().owner()).await?;
    loop {
        let incoming = server.recv().await?;
        let request: supervisor::simulation::end::EndRequest = match decode(&incoming).await? {
            Some(request) => request,
            None => continue,
        };
        let host = incoming.request_metadata()?.source.producer();
        let attachment = match state.remove_attachment(host) {
            Ok(attachment) => attachment,
            Err(error) => {
                incoming
                    .reply_err(&QueryFailure::invalid_argument(error.to_string()))
                    .await?;
                continue;
            }
        };
        reply(
            &incoming,
            &bus,
            &supervisor::simulation::end::EndResponse { attachment },
        )
        .await?;
        tracing::info!(
            reason = ?request.reason,
            revision = attachment.revision,
            %host,
            "the world host ended this simulation attachment"
        );
        // A host-reported terminal attachment outcome ends this fresh Live
        // execution. The reply above is sent first, then the ordinary shutdown
        // path retains the bus for the same bounded removal acknowledgement as
        // a robot-initiated stop.
        shutdown.cancel();
    }
}

/// Publish Removing and retain the control plane until both sides of native
/// cleanup are observable. The host acknowledges only after it has removed the
/// native member; controller Ready loss independently proves that the delegated
/// process no longer presents this execution's drivers.
pub(super) async fn finish_clean_simulation_removal(bus: &BusHandle, state: &ExecutionState) -> Result<()> {
    let Some(removing) = state.begin_shutdown_attachment()? else {
        return Ok(());
    };
    let key = supervisor::simulation::removal_liveliness_key(removing.revision, removing.host);
    let (status_tx, mut status_rx) = tokio::sync::watch::channel(LivelinessStatus::Lost);
    let observer = bus
        .observe_liveliness_key(&key, move |status| {
            status_tx.send_replace(status);
        })
        .await?;
    let mut host_acknowledged = observer.initial() == LivelinessStatus::Alive;
    let mut snapshots = state.subscribe();
    let deadline = tokio::time::Instant::now() + SIMULATION_REMOVAL_GRACE;

    loop {
        let controller_withdrawn = !state.producer_is_present(removing.controller);
        if host_acknowledged && controller_withdrawn {
            tracing::info!(
                revision = removing.revision,
                world = %removing.world,
                host = %removing.host,
                controller = %removing.controller,
                "clean simulation removal was acknowledged"
            );
            return match state.attachment_failure() {
                Some(reason) => Err(ActiveAttachmentFailure::Clean { reason }.into()),
                None => Ok(()),
            };
        }

        tokio::select! {
            () = tokio::time::sleep_until(deadline) => {
                if let Some(reason) = state.attachment_failure() {
                    return Err(ActiveAttachmentFailure::Cleanup {
                        reason,
                        cleanup: format!(
                            "removal revision {} exceeded {:?}: host_acknowledged={}, controller_withdrawn={}",
                            removing.revision,
                            SIMULATION_REMOVAL_GRACE,
                            host_acknowledged,
                            controller_withdrawn,
                        ),
                    }
                    .into());
                }
                bail!(
                    "simulation removal revision {} exceeded {:?}: host_acknowledged={}, controller_withdrawn={}",
                    removing.revision,
                    SIMULATION_REMOVAL_GRACE,
                    host_acknowledged,
                    controller_withdrawn,
                );
            }
            changed = status_rx.changed(), if !host_acknowledged => {
                changed.context("the host removal acknowledgement observer closed")?;
                host_acknowledged =
                    *status_rx.borrow_and_update() == LivelinessStatus::Alive;
            }
            changed = snapshots.changed(), if !controller_withdrawn => {
                changed.context("the supervisor presence authority closed during removal")?;
                let _ = snapshots.borrow_and_update();
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum ActiveAttachmentFailure {
    #[error("Active simulation attachment failed with {reason:?}")]
    Clean {
        reason: supervisor::simulation::SimulationEndReason,
    },
    #[error("Active simulation attachment failed with {reason:?}; {cleanup}")]
    Cleanup {
        reason: supervisor::simulation::SimulationEndReason,
        cleanup: String,
    },
}
