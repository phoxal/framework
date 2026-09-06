use super::*;

pub(super) async fn run(args: Args) -> Result<()> {
    let mut session = SimulatorSession::connect(SimulatorConnectOptions::new(
        args.connect,
        "webots-robot-controller",
    ))
    .await
    .context("failed to join the supervised robot execution")?;
    let execution = session.execution();
    let mut link = ControllerLink::connect(&args.host_connect, ControllerRole::Robot { execution })
        .context("failed to join the private Webots world host")?;
    let plan = link.take_robot_plan()?;
    ensure!(
        plan.robot == session.robot().id().to_string(),
        "host plan names robot '{}', but supervisor bootstrap returned '{}'",
        plan.robot,
        session.robot().id()
    );
    let webots = Webots::new().context("failed to initialize the Webots R2025a controller")?;
    let step_ms = exact_step_ms(webots.get_basic_time_step()?)?;
    ensure!(
        plan.basic_time_step_ms == step_ms,
        "host plan basicTimeStep {} does not match Webots {step_ms}",
        plan.basic_time_step_ms
    );
    let step_ns = u64::try_from(step_ms)
        .context("Webots basicTimeStep is negative")?
        .checked_mul(1_000_000)
        .context("Webots basicTimeStep overflows nanoseconds")?;
    let source_start_ns = observed_progress(webots.get_time()?, step_ns)?.elapsed_ns();
    let mut devices = DeviceSet::bind(&session, &webots, &plan, source_start_ns).await?;
    devices.invalidate_and_park()?;
    synchronize_devices(&webots)?;
    for substitution in &plan.substitutions {
        session
            .present(&ParticipantId::new(substitution.participant.as_str())?)
            .await
            .with_context(|| {
                format!(
                    "failed to present substituted driver {} after every native and typed binding succeeded",
                    substitution.participant
                )
            })?;
    }
    link.exchange(ControllerEvent::RobotReady {
        controller: session.producer(),
    })?;
    let mut active_revision = None;
    let mut completed_progress = observed_progress(webots.get_time()?, step_ns)?;
    let mut entered_motion = NativeMotion::Paused;
    let mut pending_native_entry = false;

    let exit = loop {
        let attachment = match session.attachment().await {
            Ok(attachment) => attachment,
            Err(error) => {
                break ControllerLoopExit::SupervisorLost {
                    detail: format!("attachment authority failed: {error}"),
                };
            }
        };
        match attachment {
            Some(attachment) if attachment.phase == SimulationAttachmentPhase::Preparing => {
                if let Err(error) = devices.invalidate_and_park() {
                    break ControllerLoopExit::ControllerFault(ControllerFault::Device {
                        detail: format!("failed to fence Preparing devices: {error:#}"),
                    });
                }
                if let Err(error) = session.acknowledge_preparing().await {
                    break ControllerLoopExit::SupervisorLost {
                        detail: format!(
                            "failed to acknowledge the exact Preparing revision: {error}"
                        ),
                    };
                }
            }
            Some(attachment) if attachment.phase == SimulationAttachmentPhase::Removing => {
                break ControllerLoopExit::Removing;
            }
            Some(attachment) if attachment.phase == SimulationAttachmentPhase::Active => {
                if let Some(attached_at) = activation_progress(
                    active_revision,
                    attachment.revision,
                    attachment.attached_at.world,
                ) {
                    devices.invalidate_and_park()?;
                    completed_progress = attached_at;
                    link.exchange(ControllerEvent::RobotActive {
                        revision: attachment.revision,
                    })?;
                    active_revision = Some(attachment.revision);
                }
            }
            Some(_) | None => {}
        }
        match link.directive()? {
            HostDirective::Continue {
                motion: NativeMotion::RealTime,
            } => {
                pending_native_entry = true;
                entered_motion = NativeMotion::RealTime;
                // Select commands at the exact Active revision and current monotonic boundary
                // immediately before entering Webots. This also expires commands while paused,
                // so the first resumed transition cannot reuse stale intent.
                let boundary = match session.active_boundary() {
                    Ok(boundary) => boundary,
                    Err(error) => {
                        break authority_exit(
                            error,
                            session
                                .attachment()
                                .await
                                .ok()
                                .flatten()
                                .map(|state| state.phase),
                            "Active boundary",
                        );
                    }
                };
                let pending_evidence = match devices
                    .prepare_transition(&boundary, completed_progress)
                {
                    Ok(evidence) => evidence,
                    Err(error) => {
                        break ControllerLoopExit::ControllerFault(ControllerFault::Device {
                            detail: format!("pre-transition device selection failed: {error:#}"),
                        });
                    }
                };
                let stepped = match webots.step(step_ms) {
                    Ok(stepped) => stepped,
                    Err(error) => {
                        break ControllerLoopExit::ControllerFault(ControllerFault::Device {
                            detail: format!("Webots transition failed: {error}"),
                        });
                    }
                };
                if !stepped {
                    link.exchange(ControllerEvent::Stopped)?;
                    break ControllerLoopExit::Clean;
                }
                pending_native_entry = false;
                let progress = match webots
                    .get_time()
                    .map_err(anyhow::Error::from)
                    .and_then(|seconds| observed_progress(seconds, step_ns))
                {
                    Ok(progress) => progress,
                    Err(error) => {
                        break ControllerLoopExit::ControllerFault(
                            ControllerFault::InvalidProgress {
                                detail: format!("invalid completed Webots transition: {error:#}"),
                            },
                        );
                    }
                };
                completed_progress = progress;
                let transition = match session.live_transition(progress) {
                    Ok(transition) => transition,
                    Err(error) => {
                        break authority_exit(
                            error,
                            session
                                .attachment()
                                .await
                                .ok()
                                .flatten()
                                .map(|state| state.phase),
                            "completed transition",
                        );
                    }
                };
                let evidence = pending_evidence
                    .into_iter()
                    .map(|pending| pending.complete(&transition))
                    .collect();
                link.exchange(ControllerEvent::ActuationEvidence(evidence))?;
                if let Err(fault) = publish_completed_transition(
                    || devices.publish_outputs(&transition),
                    || {
                        session.publish_step(
                            &transition,
                            StepEvent {
                                index: transition.progress().completed_step(),
                            },
                        )
                    },
                ) {
                    if session
                        .attachment()
                        .await
                        .ok()
                        .flatten()
                        .is_some_and(|state| state.phase == SimulationAttachmentPhase::Removing)
                    {
                        break ControllerLoopExit::Removing;
                    }
                    break ControllerLoopExit::ControllerFault(fault);
                }
                // The bounded observation closes each native boundary. Its host response carries
                // the next Pause/Stop directive before another synchronized transition begins.
                link.exchange(ControllerEvent::RobotBoundary {
                    progress: completed_progress,
                    motion: NativeMotion::RealTime,
                })?;
            }
            HostDirective::Continue {
                motion: NativeMotion::Paused,
            }
            | HostDirective::Park => {
                // Stay outside `wb_robot_step` while parked so removal and resume directives can
                // be observed without breaking Webots synchronization.
                if let Err(error) = devices.stop_native() {
                    break ControllerLoopExit::ControllerFault(ControllerFault::Device {
                        detail: format!("failed to park native devices: {error:#}"),
                    });
                }
                synchronize_devices(&webots)?;
                entered_motion = NativeMotion::Paused;
                link.exchange(ControllerEvent::RobotBoundary {
                    progress: completed_progress,
                    motion: NativeMotion::Paused,
                })?;
                tokio::time::sleep(PARKED_POLL).await;
            }
            HostDirective::Mutate(_) => {
                bail!("the host sent a world-only scene mutation to a Robot controller");
            }
            HostDirective::Stop { reason } => {
                tracing::info!(%reason, "stopping the per-Robot Webots controller");
                devices.invalidate_and_park()?;
                link.exchange(ControllerEvent::RobotParked)?;
                break ControllerLoopExit::Clean;
            }
        }
        pending_native_entry = matches!(
            link.directive()?,
            HostDirective::Continue {
                motion: NativeMotion::RealTime
            }
        );
    };
    match exit {
        ControllerLoopExit::Removing => {
            park_after_cooperative_failure(
                &mut devices,
                &webots,
                &link,
                ControllerEvent::RobotStopping,
                completed_progress,
                entered_motion,
                pending_native_entry,
                step_ms,
            )
            .await?;
            session
                .close()
                .await
                .context("failed to close removed simulator session")
        }
        ControllerLoopExit::Clean => session
            .close()
            .await
            .context("failed to close the simulator session"),
        ControllerLoopExit::ControllerFault(fault) => {
            tracing::error!(?fault, "parking a recoverably faulted Robot member");
            park_after_cooperative_failure(
                &mut devices,
                &webots,
                &link,
                ControllerEvent::Fault(fault),
                completed_progress,
                entered_motion,
                pending_native_entry,
                step_ms,
            )
            .await?;
            if let Err(error) = session.close().await {
                tracing::warn!(%error, "simulator session close failed after member fault");
            }
            Ok(())
        }
        ControllerLoopExit::SupervisorLost { detail } => {
            tracing::warn!(%detail, "parking after supervisor authority loss");
            park_after_cooperative_failure(
                &mut devices,
                &webots,
                &link,
                ControllerEvent::RobotSupervisorLost,
                completed_progress,
                entered_motion,
                pending_native_entry,
                step_ms,
            )
            .await?;
            if let Err(error) = session.close().await {
                tracing::debug!(%error, "supervisor session was already unavailable at close");
            }
            Ok(())
        }
    }
}

/// Admit one completed native transition directly into the execution bus.
///
/// The output closure runs first and `StepEvent` runs only after it succeeds.
/// Both closures publish synchronously into the bus's bounded scheduler, so
/// this boundary adds no adapter-private transition queue.
pub(super) fn publish_completed_transition(
    publish_outputs: impl FnOnce() -> Result<()>,
    publish_step: impl FnOnce() -> Result<(), SimulatorError>,
) -> std::result::Result<(), ControllerFault> {
    publish_outputs().map_err(classify_output_failure)?;
    publish_step().map_err(|error| ControllerFault::Protocol {
        detail: format!("StepEvent publication failed: {error}"),
    })
}

fn classify_output_failure(error: anyhow::Error) -> ControllerFault {
    if error.downcast_ref::<SimulatorError>().is_some() {
        ControllerFault::Protocol {
            detail: format!("typed output publication failed: {error:#}"),
        }
    } else {
        ControllerFault::Device {
            detail: format!("typed output capture failed: {error:#}"),
        }
    }
}

pub(super) fn activation_progress(
    current_revision: Option<u64>,
    observed_revision: u64,
    attached_at: WorldProgress,
) -> Option<WorldProgress> {
    if current_revision == Some(observed_revision) {
        None
    } else {
        Some(attached_at)
    }
}

#[derive(Debug)]
pub(super) enum ControllerLoopExit {
    Clean,
    Removing,
    ControllerFault(ControllerFault),
    SupervisorLost { detail: String },
}

pub(super) fn authority_exit(
    error: SimulatorError,
    phase: Option<SimulationAttachmentPhase>,
    stage: &str,
) -> ControllerLoopExit {
    // Removing may arrive during a native transition. Its intentional loss of Active
    // authority must finish the removal handshake, not discard the host acknowledgement.
    if matches!(error, SimulatorError::AttachmentInactive)
        && phase == Some(SimulationAttachmentPhase::Removing)
    {
        ControllerLoopExit::Removing
    } else {
        ControllerLoopExit::SupervisorLost {
            detail: format!("{stage} authority failed: {error}"),
        }
    }
}

pub(super) fn synchronize_devices(webots: &Webots) -> Result<()> {
    let before = webots.get_time()?;
    ensure!(
        webots.step(0)?,
        "Webots stopped during device synchronization"
    );
    ensure!(
        webots.get_time()? == before,
        "device synchronization advanced physics"
    );
    Ok(())
}

pub(super) fn observed_progress(seconds: f64, step_ns: u64) -> Result<WorldProgress> {
    ensure!(
        seconds.is_finite() && seconds >= 0.0,
        "Webots returned invalid simulation time"
    );
    let elapsed = (seconds * 1_000_000_000.0).round();
    ensure!(
        elapsed <= u64::MAX as f64,
        "Webots simulation time overflows"
    );
    let elapsed = elapsed as u64;
    ensure!(
        elapsed.is_multiple_of(step_ns),
        "Webots simulation time is off the declared physics grid"
    );
    WorldProgress::at(elapsed / step_ns, step_ns).map_err(|error: WorldProgressError| error.into())
}

pub(super) fn exact_step_ms(value: f64) -> Result<i32> {
    ensure!(
        value.is_finite() && value > 0.0,
        "Webots basicTimeStep must be finite and positive"
    );
    ensure!(
        value.fract() == 0.0,
        "Webots basicTimeStep must be an exact whole millisecond"
    );
    ensure!(
        value <= f64::from(i32::MAX),
        "Webots basicTimeStep exceeds the controller ABI"
    );
    Ok(value as i32)
}
