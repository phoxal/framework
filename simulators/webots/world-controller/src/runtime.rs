use super::*;

pub(super) fn run(args: Args) -> Result<()> {
    let webots = Webots::new().context("failed to initialize the Webots R2025a controller")?;
    let supervisor = webots.get_supervisor();
    let result = run_initialized(args, &webots, &supervisor);
    converge_on_error(result, || {
        let _ = supervisor.simulation_quit(1);
    })
}

fn run_initialized(args: Args, webots: &Webots, supervisor: &Supervisor) -> Result<()> {
    let step_ms = exact_step_ms(webots.get_basic_time_step()?)?;
    let step_ns = u64::try_from(step_ms)
        .context("Webots basicTimeStep is negative")?
        .checked_mul(1_000_000)
        .context("Webots basicTimeStep overflows nanoseconds")?;
    let link = ControllerLink::connect(&args.host_connect, ControllerRole::World)
        .context("failed to join the private world host")?;

    set_motion(webots, supervisor, NativeMotion::Paused)?;
    link.exchange(ControllerEvent::WorldReady {
        time_step_ns: step_ns,
        mode: ObservedNativeMode::Paused,
    })?;

    let mut observed = NativeMotion::Paused;
    let mut completed_step = 0_u64;
    let mut completed_mutation = None;
    loop {
        let directive = match link.directive() {
            Ok(directive) => directive,
            Err(error) => {
                return Err(error).context(
                    "private world-host authority was lost; forced Webots process convergence",
                );
            }
        };
        match directive {
            HostDirective::Continue { motion } => {
                if motion != observed {
                    set_motion(webots, supervisor, motion)?;
                    observed = motion;
                    link.exchange(ControllerEvent::WorldMode {
                        mode: observed_mode(motion),
                    })?;
                }
                match motion {
                    NativeMotion::Paused => poll_while_paused(webots, supervisor, &link)?,
                    NativeMotion::RealTime => {
                        validate_native_mode(supervisor, &link)?;
                        if !webots.step(step_ms)? {
                            link.exchange(ControllerEvent::Stopped)?;
                            return Ok(());
                        }
                        completed_step = completed_step
                            .checked_add(1)
                            .context("Webots completed-step counter exhausted")?;
                        link.exchange(ControllerEvent::WorldProgress(NativeProgressObservation {
                            completed_step,
                            elapsed_ns: observed_elapsed_ns(webots.get_time()?)?,
                            mode: ObservedNativeMode::RealTime,
                        }))?;
                    }
                }
            }
            HostDirective::Park => {
                if observed != NativeMotion::Paused {
                    set_motion(webots, supervisor, NativeMotion::Paused)?;
                    observed = NativeMotion::Paused;
                    link.exchange(ControllerEvent::WorldMode {
                        mode: ObservedNativeMode::Paused,
                    })?;
                }
                poll_while_paused(webots, supervisor, &link)?;
            }
            HostDirective::Mutate(mutation) => {
                if observed != NativeMotion::Paused {
                    set_motion(webots, supervisor, NativeMotion::Paused)?;
                    observed = NativeMotion::Paused;
                    link.exchange(ControllerEvent::WorldMode {
                        mode: ObservedNativeMode::Paused,
                    })?;
                }
                let transaction = mutation.transaction();
                if completed_mutation == Some(transaction) {
                    poll_while_paused(webots, supervisor, &link)?;
                    continue;
                }
                if matches!(mutation, NativeMutation::StartRobotController { .. }) {
                    start_imported_controller(webots, supervisor, &link, transaction)?;
                    link.exchange(ControllerEvent::MutationCompleted {
                        transaction,
                        error: None,
                    })?;
                    completed_mutation = Some(transaction);
                    continue;
                }
                let importing = matches!(mutation, NativeMutation::ImportRobot { .. });
                let error = apply_mutation(supervisor, mutation)
                    .err()
                    .map(|error| format!("{error:#}"));
                if importing && error.is_none() {
                    link.exchange(ControllerEvent::RobotImported { transaction })?;
                    continue;
                }
                link.exchange(ControllerEvent::MutationCompleted { transaction, error })?;
                completed_mutation = Some(transaction);
            }
            HostDirective::Stop { reason } => {
                tracing::info!(%reason, "stopping the shared Webots world controller");
                set_motion(webots, supervisor, NativeMotion::Paused)?;
                link.exchange(ControllerEvent::Stopped)?;
                return Ok(());
            }
        }
    }
}

pub(super) fn converge_on_error<T>(result: Result<T>, quit: impl FnOnce()) -> Result<T> {
    if let Err(error) = &result {
        tracing::error!(error = %format!("{error:#}"), "native world controller failed");
        quit();
    }
    result
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

pub(super) fn observed_elapsed_ns(seconds: f64) -> Result<u64> {
    ensure!(
        seconds.is_finite() && seconds >= 0.0,
        "Webots returned invalid simulation time"
    );
    let nanoseconds = seconds * 1_000_000_000.0;
    let rounded = nanoseconds.round();
    ensure!(
        (nanoseconds - rounded).abs() <= 0.25,
        "Webots simulation time cannot be represented as whole nanoseconds"
    );
    ensure!(
        rounded <= u64::MAX as f64,
        "Webots simulation time overflows nanoseconds"
    );
    Ok(rounded as u64)
}
