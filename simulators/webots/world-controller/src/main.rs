//! Shared Webots supervisor controller for one Phoxal world session.

#[cfg(any(target_env = "musl", all(target_os = "linux", target_arch = "aarch64")))]
compile_error!(
    "the Webots R2025a controller SDK is dynamically linked and unsupported on musl or Linux aarch64"
);

use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use phoxal_simulator_webots_shared::protocol::{
    ControllerEvent, ControllerFault, ControllerLink, ControllerRole, HostDirective, NativeMotion,
    NativeMutation, NativeProgressObservation, ObservedNativeMode,
};
use tracing_subscriber::EnvFilter;
use webots_rs::bindings::{
    WbSimulationMode, WbSimulationMode_WB_SUPERVISOR_SIMULATION_MODE_FAST,
    WbSimulationMode_WB_SUPERVISOR_SIMULATION_MODE_PAUSE,
    WbSimulationMode_WB_SUPERVISOR_SIMULATION_MODE_REAL_TIME,
};
use webots_rs::{
    Webots,
    supervisor::{Node, Supervisor},
};

const PAUSED_POLL: Duration = Duration::from_millis(10);

#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Loopback-only endpoint owned by the world-session host.
    #[arg(long, value_name = "LOCAL_ENDPOINT")]
    host_connect: String,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    run(Args::parse())
}

fn run(args: Args) -> Result<()> {
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

fn converge_on_error<T>(result: Result<T>, quit: impl FnOnce()) -> Result<T> {
    if let Err(error) = &result {
        tracing::error!(error = %format!("{error:#}"), "native world controller failed");
        quit();
    }
    result
}

fn apply_mutation(supervisor: &Supervisor, mutation: NativeMutation) -> Result<()> {
    ensure!(
        supervisor.simulation_get_mode()? == WbSimulationMode_WB_SUPERVISOR_SIMULATION_MODE_PAUSE,
        "native scene mutation is permitted only while Webots is paused"
    );
    match mutation {
        NativeMutation::StartRobotController { .. } => {
            bail!("controller bootstrap is not a scene mutation")
        }
        NativeMutation::ImportRobot {
            definition, source, ..
        } => {
            ensure!(
                Node::from_def(&definition).is_err(),
                "Robot DEF {definition} already exists"
            );
            supervisor
                .get_root()?
                .field("children")?
                .import_mf_node_from_string(-1, &source)?;
            let imported = Node::from_def(&definition)
                .with_context(|| format!("imported Robot DEF {definition} is not addressable"))?;
            ensure!(
                imported.base_type_name()? == "Robot",
                "imported DEF {definition} is not a Robot"
            );
        }
        NativeMutation::RemoveRobot { definition, .. } => {
            Node::from_def(&definition)
                .with_context(|| format!("Robot DEF {definition} is absent during removal"))?
                .remove()?;
            ensure!(
                Node::from_def(&definition).is_err(),
                "Robot DEF {definition} remained after removal"
            );
        }
        NativeMutation::RollbackRobot { definition, .. } => {
            if let Ok(node) = Node::from_def(&definition) {
                node.remove()?;
            }
            ensure!(
                Node::from_def(&definition).is_err(),
                "Robot DEF {definition} remained after rollback"
            );
        }
    }
    Ok(())
}

fn start_imported_controller(
    webots: &Webots,
    supervisor: &Supervisor,
    link: &ControllerLink,
    transaction: u64,
) -> Result<()> {
    // R2025a starts imported controllers from its running event loop. Zero-duration
    // controller requests do not authorize physics, so bootstrap can preserve this boundary.
    // The installed-runtime proof covers startup and return to PAUSE without a time change.
    let before = webots.get_time()?;
    set_motion(webots, supervisor, NativeMotion::RealTime)?;
    let deadline = std::time::Instant::now() + Duration::from_secs(25);
    let result = (|| -> Result<()> {
        loop {
            synchronize_control(webots)?;
            ensure!(
                webots.get_time()? == before,
                "controller bootstrap advanced native physics"
            );
            validate_native_mode(supervisor, link)?;
            match link.directive()? {
                HostDirective::Mutate(NativeMutation::StartRobotController {
                    transaction: current,
                    ready,
                    ..
                }) if current == transaction => {
                    if ready {
                        return Ok(());
                    }
                }
                directive => bail!("native import bootstrap lost authority: {directive:?}"),
            }
            ensure!(
                std::time::Instant::now() < deadline,
                "imported controller bootstrap timed out"
            );
            link.exchange(ControllerEvent::Heartbeat)?;
            std::thread::sleep(PAUSED_POLL);
        }
    })();
    set_motion(webots, supervisor, NativeMotion::Paused)?;
    ensure!(
        webots.get_time()? == before,
        "controller bootstrap changed the paused boundary"
    );
    result
}

fn poll_while_paused(
    webots: &Webots,
    supervisor: &Supervisor,
    link: &ControllerLink,
) -> Result<()> {
    synchronize_control(webots)?;
    validate_paused_mode(supervisor, link)?;
    link.exchange(ControllerEvent::Heartbeat)?;
    std::thread::sleep(PAUSED_POLL);
    Ok(())
}

fn validate_paused_mode(supervisor: &Supervisor, link: &ControllerLink) -> Result<()> {
    let raw = supervisor.simulation_get_mode()?;
    match map_mode(raw) {
        Some(ObservedNativeMode::Paused) => Ok(()),
        Some(observed @ (ObservedNativeMode::Run | ObservedNativeMode::Fast)) => {
            link.exchange(ControllerEvent::Fault(ControllerFault::UnsupportedMode {
                observed,
            }))?;
            bail!("Webots entered unsupported native mode {observed:?} while paused")
        }
        Some(observed) => {
            link.exchange(ControllerEvent::Fault(ControllerFault::Protocol {
                detail: format!("expected PAUSE outside wb_robot_step, observed {observed:?}"),
            }))?;
            bail!("Webots left PAUSE without host authority")
        }
        None => {
            link.exchange(ControllerEvent::Fault(ControllerFault::UnsupportedMode {
                observed: ObservedNativeMode::Run,
            }))?;
            bail!("Webots returned unknown simulation mode {raw} while paused")
        }
    }
}

fn exact_step_ms(value: f64) -> Result<i32> {
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

fn set_motion(webots: &Webots, supervisor: &Supervisor, motion: NativeMotion) -> Result<()> {
    supervisor
        .simulation_set_mode(match motion {
            NativeMotion::Paused => WbSimulationMode_WB_SUPERVISOR_SIMULATION_MODE_PAUSE,
            NativeMotion::RealTime => WbSimulationMode_WB_SUPERVISOR_SIMULATION_MODE_REAL_TIME,
        })
        .context("failed to change Webots simulation mode")?;
    synchronize_control(webots)
}

fn synchronize_control(webots: &Webots) -> Result<()> {
    // R2025a may return the cached previous mode after simulation_set_mode.
    // A zero-duration step refreshes control state without advancing physics,
    // including while paused. A positive-duration step would block in PAUSE.
    let before = webots.get_time()?;
    ensure!(
        webots.step(0)?,
        "Webots stopped during control synchronization"
    );
    ensure!(
        webots.get_time()? == before,
        "Webots advanced physics during control synchronization"
    );
    Ok(())
}

fn validate_native_mode(supervisor: &Supervisor, link: &ControllerLink) -> Result<()> {
    let raw = supervisor.simulation_get_mode()?;
    match map_mode(raw) {
        Some(ObservedNativeMode::RealTime) => Ok(()),
        Some(observed @ (ObservedNativeMode::Run | ObservedNativeMode::Fast)) => {
            link.exchange(ControllerEvent::Fault(ControllerFault::UnsupportedMode {
                observed,
            }))?;
            bail!("Webots entered unsupported native mode {observed:?}")
        }
        Some(observed) => {
            link.exchange(ControllerEvent::Fault(ControllerFault::Protocol {
                detail: format!("expected REAL_TIME before a native step, observed {observed:?}"),
            }))?;
            bail!("Webots was not in REAL_TIME before a native step")
        }
        None => {
            link.exchange(ControllerEvent::Fault(ControllerFault::UnsupportedMode {
                observed: ObservedNativeMode::Run,
            }))?;
            bail!("Webots returned unknown simulation mode {raw}")
        }
    }
}

const fn map_mode(mode: WbSimulationMode) -> Option<ObservedNativeMode> {
    if mode == WbSimulationMode_WB_SUPERVISOR_SIMULATION_MODE_PAUSE {
        Some(ObservedNativeMode::Paused)
    } else if mode == WbSimulationMode_WB_SUPERVISOR_SIMULATION_MODE_REAL_TIME {
        Some(ObservedNativeMode::RealTime)
    } else if mode == WbSimulationMode_WB_SUPERVISOR_SIMULATION_MODE_FAST {
        Some(ObservedNativeMode::Fast)
    } else {
        None
    }
}

const fn observed_mode(motion: NativeMotion) -> ObservedNativeMode {
    match motion {
        NativeMotion::Paused => ObservedNativeMode::Paused,
        NativeMotion::RealTime => ObservedNativeMode::RealTime,
    }
}

fn observed_elapsed_ns(seconds: f64) -> Result<u64> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_time_step_must_be_a_positive_integer() {
        assert_eq!(exact_step_ms(12.0).expect("valid step"), 12);
        for invalid in [0.0, -1.0, 12.5, f64::NAN] {
            assert!(exact_step_ms(invalid).is_err());
        }
    }

    #[test]
    fn progress_time_is_quantized_to_nanoseconds() {
        assert_eq!(observed_elapsed_ns(0.012).expect("valid time"), 12_000_000);
        assert!(observed_elapsed_ns(-1.0).is_err());
    }

    #[test]
    fn every_post_initialization_failure_forces_native_convergence() {
        let mut quit = false;
        let result = converge_on_error::<()>(Err(anyhow::anyhow!("host bootstrap failed")), || {
            quit = true;
        });
        assert!(result.is_err());
        assert!(quit);

        let mut quit = false;
        converge_on_error(Ok(()), || quit = true).expect("normal controller exit");
        assert!(!quit);
    }
}
