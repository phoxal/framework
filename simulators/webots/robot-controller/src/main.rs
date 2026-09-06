//! Per-Robot Webots controller and narrow simulator-SDK bridge.

#[cfg(any(target_env = "musl", all(target_os = "linux", target_arch = "aarch64")))]
compile_error!(
    "the Webots R2025a controller SDK is dynamically linked and unsupported on musl or Linux aarch64"
);

use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use phoxal::SampleSchedule;
use phoxal::api;
use phoxal::bus::{FixedSourceLease, LeaseDecision, LeaseRejection, ParticipantReadyEvents};
use phoxal::drive::authority::DriveCommandAuthority;
use phoxal::identity::ParticipantId;
use phoxal::model::component::capability::{
    Capability as DeclaredCapability, CapabilityKind, MotorCommand,
};
use phoxal::model::world::{WorldProgress, WorldProgressError};
use phoxal::simulation::api::step::StepEvent;
use phoxal::simulator::{
    ActiveBoundaryStamp, LiveSamplePublisher, LiveSetpointReceiver, LiveTransitionStamp,
};
use phoxal::simulator::{SimulatorConnectOptions, SimulatorError, SimulatorSession};
use phoxal::supervisor::api::simulation::SimulationAttachmentPhase;
use phoxal_simulator_webots_shared::plan::{
    CapabilityBinding as PlannedBinding, RobotSimulationPlan,
};
use phoxal_simulator_webots_shared::protocol::{
    ActuationDecision, ActuationEvidence, ActuationSelection, AppliedActuation, ControllerEvent,
    ControllerFault, ControllerLink, ControllerRole, HostDirective, NativeMotion,
    NoActuationReason, OfferedActuation,
};
use tracing_subscriber::EnvFilter;
use webots_rs::Webots;

mod sensors;

use sensors::SensorSet;

const PARKED_POLL: Duration = Duration::from_millis(10);

#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Supervisor endpoint identifying exactly one robot execution.
    #[arg(long, value_name = "SUPERVISOR_ENDPOINT")]
    connect: String,
    /// Loopback-only endpoint owned by the world-session host.
    #[arg(long, value_name = "LOCAL_ENDPOINT")]
    host_connect: String,
}

// Zenoh requires a multi-thread runtime. Tokio drives this root future on the calling
// thread, keeping every Webots SDK call on the controller's native main thread.
#[tokio::main(flavor = "multi_thread", worker_threads = 1)]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    run(Args::parse()).await
}

async fn run(args: Args) -> Result<()> {
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
fn publish_completed_transition(
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

fn activation_progress(
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
enum ControllerLoopExit {
    Clean,
    Removing,
    ControllerFault(ControllerFault),
    SupervisorLost { detail: String },
}

fn authority_exit(
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

#[allow(
    clippy::too_many_arguments,
    reason = "fault convergence retains the exact native boundary and any issued transition"
)]
async fn park_after_cooperative_failure(
    devices: &mut DeviceSet,
    webots: &Webots,
    link: &ControllerLink,
    event: ControllerEvent,
    mut progress: WorldProgress,
    mut motion: NativeMotion,
    mut pending_native_entry: bool,
    step_ms: i32,
) -> Result<()> {
    // A controller that cannot park is not isolated and must disconnect so the host classifies
    // the synchronization role as world-fatal. Once parked, stay outside `wb_robot_step` and keep
    // driving the private request/response link until the host retires this one Robot.
    devices.invalidate_and_park()?;
    synchronize_devices(webots)?;
    link.exchange(event)?;
    // A peer may already have entered the next synchronized quantum. Finish every
    // previously issued native transition with parked actuators, without publishing output,
    // until the common boundary selects PAUSE. Leaving that barrier early would strand peers.
    loop {
        if pending_native_entry {
            ensure!(
                webots.step(step_ms)?,
                "Webots stopped before cooperative parking completed"
            );
            progress = observed_progress(webots.get_time()?, u64::try_from(step_ms)? * 1_000_000)?;
            motion = NativeMotion::RealTime;
        }
        link.exchange(ControllerEvent::RobotBoundary { progress, motion })?;
        match link.directive()? {
            HostDirective::Continue {
                motion: NativeMotion::RealTime,
            } => pending_native_entry = true,
            HostDirective::Continue {
                motion: NativeMotion::Paused,
            }
            | HostDirective::Park => break,
            HostDirective::Stop { .. } => break,
            HostDirective::Mutate(_) => bail!("world mutation directed to parking Robot"),
        }
    }
    link.exchange(ControllerEvent::RobotParked)?;
    loop {
        match link.directive()? {
            HostDirective::Stop { reason } => {
                tracing::info!(%reason, "retiring the cooperatively parked Robot");
                link.exchange(ControllerEvent::Stopped)?;
                return Ok(());
            }
            HostDirective::Continue { .. } | HostDirective::Park => {
                link.exchange(ControllerEvent::Heartbeat)?;
                tokio::time::sleep(PARKED_POLL).await;
            }
            HostDirective::Mutate(_) => {
                bail!("the host sent a world-only scene mutation to a parked Robot controller");
            }
        }
    }
}

fn synchronize_devices(webots: &Webots) -> Result<()> {
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

struct DeviceSet {
    motors: Vec<MotorDevice>,
    encoders: Vec<EncoderDevice>,
    sensors: SensorSet,
}

struct MotorDevice {
    capability: phoxal::model::identity::CapabilityRef,
    native: webots_rs::device::motor::Motor,
    command: MotorCommand,
    gear_ratio: f64,
    position_velocity: f64,
    receiver: LiveSetpointReceiver<api::component::motor::Command>,
    authority: FixedSourceLease<api::component::motor::Command>,
    ready: ParticipantReadyEvents,
}

struct EncoderDevice {
    native: webots_rs::device::position_sensor::PositionSensor,
    gear_ratio: f64,
    schedule: SampleSchedule,
    last: Option<(f64, u64)>,
    publisher: LiveSamplePublisher<api::component::encoder::Sample>,
}

struct PendingActuationEvidence {
    capability: phoxal::model::identity::CapabilityRef,
    revision: u64,
    selected_at: phoxal::bus::RobotInstant,
    selected_from: WorldProgress,
    offered: Vec<OfferedActuation>,
    selected: Option<api::component::motor::Command>,
    selection: ActuationSelection,
    applied: AppliedActuation,
}

impl PendingActuationEvidence {
    fn complete(self, transition: &LiveTransitionStamp) -> ActuationEvidence {
        ActuationEvidence {
            capability: self.capability,
            revision: self.revision,
            selected_at: self.selected_at,
            selected_from: self.selected_from,
            progress: transition.progress(),
            instant: transition.instant(),
            offered: self.offered,
            selected: self.selected,
            selection: self.selection,
            applied: self.applied,
        }
    }
}

impl DeviceSet {
    async fn bind(
        session: &SimulatorSession,
        webots: &Webots,
        plan: &RobotSimulationPlan,
        source_start_ns: u64,
    ) -> Result<Self> {
        let mut motors = Vec::new();
        let mut encoders = Vec::new();
        let drive_authority = DriveCommandAuthority::standard()?;
        for binding in &plan.capabilities {
            if !matches!(
                binding.kind(),
                CapabilityKind::Motor | CapabilityKind::Encoder
            ) {
                continue;
            }
            let declared = session
                .robot()
                .capability(binding.reference())
                .with_context(|| format!("plan capability {} is absent", binding.reference()))?;
            let component = || api::topics().component(&binding.reference().component_id);
            let id = &binding.reference().capability_id;
            match (declared, binding.kind()) {
                (DeclaredCapability::Motor(config), CapabilityKind::Motor) => {
                    ensure_motor_plan(binding, config.command)?;
                    let native = webots.motor(binding.native_device())?;
                    let position_velocity = config.max_velocity_radps.map_or_else(
                        || native.get_max_velocity(),
                        |velocity| Ok(velocity / config.gear_ratio.abs()),
                    )?;
                    ensure!(
                        position_velocity.is_finite() && position_velocity > 0.0,
                        "motor {} has no positive finite position velocity",
                        binding.reference()
                    );
                    motors.push(MotorDevice {
                        capability: binding.reference().clone(),
                        native,
                        command: config.command,
                        gear_ratio: config.gear_ratio,
                        position_velocity,
                        receiver: session
                            .setpoint_receiver(component()?.motor(id)?.command().owner())
                            .await?,
                        authority: drive_authority.motor_lease(),
                        ready: session
                            .participant_ready_events(drive_authority.source())
                            .await?,
                    });
                }
                (DeclaredCapability::Encoder(config), CapabilityKind::Encoder) => {
                    let sampling = binding
                        .sampling()
                        .context("encoder plan has no sampling policy")?;
                    let native = webots.position_sensor(binding.native_device())?;
                    native.enable(sampling.native_period_ms)?;
                    encoders.push(EncoderDevice {
                        native,
                        gear_ratio: config.gear_ratio,
                        schedule: sensors::schedule(binding, source_start_ns)?,
                        last: None,
                        publisher: session
                            .sample_publisher(component()?.encoder(id)?.sample().owner())?,
                    });
                }
                _ => bail!(
                    "plan binding {} does not match its compiled capability kind",
                    binding.reference()
                ),
            }
        }
        let sensors = SensorSet::bind(session, webots, &plan.capabilities, source_start_ns)?;
        Ok(Self {
            motors,
            encoders,
            sensors,
        })
    }

    fn prepare_transition(
        &mut self,
        boundary: &ActiveBoundaryStamp,
        selected_from: WorldProgress,
    ) -> Result<Vec<PendingActuationEvidence>> {
        let mut evidence = Vec::with_capacity(self.motors.len());
        for motor in &mut self.motors {
            if motor.receiver.terminal().is_some() {
                motor.apply_action(MotorAction::Stop)?;
                evidence.push(PendingActuationEvidence {
                    capability: motor.capability.clone(),
                    revision: boundary.revision(),
                    selected_at: boundary.instant(),
                    selected_from,
                    offered: Vec::new(),
                    selected: None,
                    selection: ActuationSelection::None {
                        reason: NoActuationReason::ReceiverClosed,
                    },
                    applied: AppliedActuation::Stop,
                });
                continue;
            }
            while let Some(event) = motor.ready.try_recv() {
                motor.authority.update_ready_event(&event);
            }
            if motor.ready.overflowed() {
                motor.authority.mark_ready_overflow();
            }
            let mut offered = Vec::new();
            while let Some(observed) = motor.receiver.try_recv_at(boundary) {
                let command = observed.body.clone();
                let decision = motor.authority.offer(
                    observed.metadata.source.participant_source(),
                    observed.metadata.sequence,
                    observed.observed_at,
                    observed.body,
                );
                offered.push(OfferedActuation {
                    producer: observed
                        .metadata
                        .source
                        .participant_source()
                        .map(|source| source.producer),
                    sequence: observed.metadata.sequence,
                    command,
                    decision: evidence_decision(decision),
                });
            }
            let held_before_selection = motor.authority.producer().is_some();
            let ready_count = motor.authority.ready_count();
            let accepted_new = offered.iter().any(|offered| {
                matches!(
                    offered.decision,
                    ActuationDecision::Acquired | ActuationDecision::Renewed
                )
            });
            let selected = motor.authority.live_host(boundary.local_instant()).cloned();
            let selection = classify_selection(
                selected.is_some(),
                accepted_new,
                held_before_selection,
                ready_count,
                !offered.is_empty(),
            );
            let action = selected
                .as_ref()
                .map(|command| dispatch_motor(motor.command, command, motor.gear_ratio))
                .transpose()?
                .unwrap_or(MotorAction::Stop);
            motor.apply_action(action)?;
            evidence.push(PendingActuationEvidence {
                capability: motor.capability.clone(),
                revision: boundary.revision(),
                selected_at: boundary.instant(),
                selected_from,
                offered,
                selected,
                selection,
                applied: action.into(),
            });
        }
        Ok(evidence)
    }

    fn publish_outputs(&mut self, transition: &LiveTransitionStamp) -> Result<()> {
        self.sensors.publish_outputs(transition)?;
        let elapsed_ns = transition.progress().elapsed_ns();
        for encoder in &mut self.encoders {
            if !encoder.schedule.is_due_at(elapsed_ns)? {
                continue;
            }
            let position = encoder.native.value()? * encoder.gear_ratio;
            let velocity = encoder
                .last
                .map(|(previous, time)| {
                    let delta = elapsed_ns.saturating_sub(time);
                    if delta == 0 {
                        0.0
                    } else {
                        (position - previous) * 1_000_000_000.0 / delta as f64
                    }
                })
                .unwrap_or(0.0);
            encoder.last = Some((position, elapsed_ns));
            encoder.publisher.publish(
                transition,
                api::component::encoder::Sample::try_new(position, velocity as f32)?,
            )?;
        }
        Ok(())
    }

    fn invalidate_and_park(&mut self) -> Result<()> {
        for motor in &mut self.motors {
            motor.receiver.flush();
            motor.authority.clear();
        }
        self.stop_all_motors()
    }

    fn stop_native(&mut self) -> Result<()> {
        self.stop_all_motors()
    }

    /// Attempt every independent native motor stop before reporting cleanup failure.
    fn stop_all_motors(&mut self) -> Result<()> {
        stop_every(
            &mut self.motors,
            |motor| motor.capability.to_string(),
            |motor| motor.stop(),
        )
    }
}

/// Complete every independent stop attempt before returning their aggregate failure.
fn stop_every<T>(
    targets: &mut [T],
    label: impl Fn(&T) -> String,
    mut stop: impl FnMut(&mut T) -> Result<()>,
) -> Result<()> {
    let mut failures = Vec::new();
    for target in targets {
        let target_label = label(target);
        if let Err(error) = stop(target) {
            failures.push(format!("{target_label}: {error:#}"));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        bail!("failed to stop native motors: {}", failures.join("; "));
    }
}

const fn classify_selection(
    selected: bool,
    accepted_new: bool,
    held_before_selection: bool,
    ready_count: usize,
    had_offers: bool,
) -> ActuationSelection {
    if selected {
        return if accepted_new {
            ActuationSelection::SelectedNew
        } else {
            ActuationSelection::Reused
        };
    }
    ActuationSelection::None {
        reason: if ready_count == 0 {
            NoActuationReason::SourceAbsent
        } else if ready_count > 1 {
            NoActuationReason::SourceConflict
        } else if held_before_selection {
            NoActuationReason::Expired
        } else if had_offers {
            NoActuationReason::Rejected
        } else {
            NoActuationReason::Missing
        },
    }
}

impl MotorDevice {
    fn apply_action(&self, action: MotorAction) -> Result<()> {
        match action {
            MotorAction::Position(value) => {
                self.native.set_velocity(self.position_velocity)?;
                self.native.set_position(value)?;
            }
            MotorAction::Velocity(value) => {
                self.native.set_position(f64::INFINITY)?;
                self.native.set_velocity(value)?;
            }
            MotorAction::Torque(value) => {
                self.native.set_position(f64::INFINITY)?;
                self.native.set_torque(value)?;
            }
            MotorAction::Stop => self.stop()?,
        }
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        match self.command {
            MotorCommand::Position => self.native.set_velocity(0.0)?,
            MotorCommand::Velocity => {
                self.native.set_position(f64::INFINITY)?;
                self.native.set_velocity(0.0)?;
            }
            MotorCommand::Torque => {
                self.native.set_position(f64::INFINITY)?;
                self.native.set_torque(0.0)?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum MotorAction {
    Position(f64),
    Velocity(f64),
    Torque(f64),
    Stop,
}

impl From<MotorAction> for AppliedActuation {
    fn from(action: MotorAction) -> Self {
        match action {
            MotorAction::Position(value) => Self::Position(value),
            MotorAction::Velocity(value) => Self::Velocity(value),
            MotorAction::Torque(value) => Self::Torque(value),
            MotorAction::Stop => Self::Stop,
        }
    }
}

fn evidence_decision(decision: LeaseDecision) -> ActuationDecision {
    match decision {
        LeaseDecision::Acquired => ActuationDecision::Acquired,
        LeaseDecision::Renewed => ActuationDecision::Renewed,
        LeaseDecision::Rejected(rejection) => match rejection {
            LeaseRejection::WrongParticipant => ActuationDecision::WrongParticipant,
            LeaseRejection::ParticipantSource => ActuationDecision::ParticipantSource,
            LeaseRejection::SourceAbsent => ActuationDecision::SourceAbsent,
            LeaseRejection::SourceConflict => ActuationDecision::SourceConflict,
            LeaseRejection::StaleSequence { accepted, observed } => {
                ActuationDecision::StaleSequence { accepted, observed }
            }
            LeaseRejection::AuthorityHeld { owner } => ActuationDecision::AuthorityHeld { owner },
            LeaseRejection::NotOwner { owner, requested } => {
                ActuationDecision::NotOwner { owner, requested }
            }
            LeaseRejection::ReadyStateOverflow => ActuationDecision::ReadyStateOverflow,
        },
    }
}

fn dispatch_motor(
    configured: MotorCommand,
    command: &api::component::motor::Command,
    gear_ratio: f64,
) -> Result<MotorAction> {
    if matches!(command, api::component::motor::Command::Stop) {
        return Ok(MotorAction::Stop);
    }
    ensure!(
        gear_ratio.is_finite() && gear_ratio != 0.0,
        "motor gear ratio must be finite and nonzero"
    );
    let (mode, value) = match command {
        api::component::motor::Command::Position(value) => {
            (MotorCommand::Position, f64::from(*value))
        }
        api::component::motor::Command::Velocity(value) => {
            (MotorCommand::Velocity, f64::from(*value))
        }
        api::component::motor::Command::Torque(value) => (MotorCommand::Torque, f64::from(*value)),
        api::component::motor::Command::Stop => unreachable!(),
    };
    ensure!(
        mode == configured,
        "motor command mode does not match its plan"
    );
    let value = match mode {
        MotorCommand::Position | MotorCommand::Velocity => value / gear_ratio,
        MotorCommand::Torque => value * gear_ratio,
    };
    ensure!(
        value.is_finite(),
        "motor command becomes non-finite after gearing"
    );
    Ok(match mode {
        MotorCommand::Position => MotorAction::Position(value),
        MotorCommand::Velocity => MotorAction::Velocity(value),
        MotorCommand::Torque => MotorAction::Torque(value),
    })
}

fn ensure_motor_plan(binding: &PlannedBinding, command: MotorCommand) -> Result<()> {
    let planned = binding
        .motor_command()
        .context("motor binding has no command contract")?;
    ensure!(
        planned == command,
        "motor binding command mode does not match its plan"
    );
    Ok(())
}

fn observed_progress(seconds: f64, step_ns: u64) -> Result<WorldProgress> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use phoxal::bus::{BusError, LocalInstant, ParticipantReadyStatus, ParticipantSourceIdentity};
    use phoxal::identity::ProducerId;

    #[test]
    fn removing_during_a_transition_keeps_the_orderly_shutdown_handshake() {
        assert!(matches!(
            authority_exit(
                SimulatorError::AttachmentInactive,
                Some(SimulationAttachmentPhase::Removing),
                "completed transition",
            ),
            ControllerLoopExit::Removing
        ));
        for phase in [None, Some(SimulationAttachmentPhase::Active)] {
            assert!(matches!(
                authority_exit(SimulatorError::AttachmentInactive, phase, "boundary"),
                ControllerLoopExit::SupervisorLost { .. }
            ));
        }
    }

    #[test]
    fn binary_abi_has_exactly_two_required_flags() {
        let parsed = Args::try_parse_from([
            "robot-controller",
            "--connect",
            "tcp/127.0.0.1:7447",
            "--host-connect",
            "tcp://127.0.0.1:1234",
        ])
        .expect("fixed ABI parses");
        assert_eq!(parsed.host_connect, "tcp://127.0.0.1:1234");
    }

    #[test]
    fn drive_policy_distinguishes_new_reused_expired_source_loss_and_missing() {
        assert_eq!(
            classify_selection(true, true, false, 1, true),
            ActuationSelection::SelectedNew
        );
        assert_eq!(
            classify_selection(true, false, true, 1, false),
            ActuationSelection::Reused
        );
        assert_eq!(
            classify_selection(false, false, true, 1, false),
            ActuationSelection::None {
                reason: NoActuationReason::Expired
            }
        );
        assert_eq!(
            classify_selection(false, false, false, 0, false),
            ActuationSelection::None {
                reason: NoActuationReason::SourceAbsent
            }
        );
        assert_eq!(
            classify_selection(false, false, false, 1, false),
            ActuationSelection::None {
                reason: NoActuationReason::Missing
            }
        );
    }

    #[test]
    fn paused_drive_intent_expires_by_host_time_and_a_later_command_stays_fresh() {
        let authority = DriveCommandAuthority::standard().expect("drive authority");
        let participant = authority.source().clone();
        let producer = ProducerId::try_from(0x1000_0000_0000_0000_0000_0000_0000_0001)
            .expect("canonical producer");
        let source = ParticipantSourceIdentity::new(participant.clone(), producer);
        let mut lease = authority.motor_lease();
        lease.update_ready(&source, ParticipantReadyStatus::Ready);
        let paused_at = LocalInstant::from_boot_ns(1_000_000_000);
        assert_eq!(
            lease.offer(
                Some(&source),
                1,
                paused_at,
                api::component::motor::Command::Velocity(1.0),
            ),
            LeaseDecision::Acquired
        );
        assert!(
            lease
                .live_host(paused_at.saturating_add(DriveCommandAuthority::silence()))
                .is_none(),
            "a command that aged through the full pause bound must expire"
        );

        let later = paused_at.saturating_add(Duration::from_millis(250));
        assert_eq!(
            lease.offer(
                Some(&source),
                2,
                later,
                api::component::motor::Command::Velocity(2.0),
            ),
            LeaseDecision::Renewed
        );
        assert!(
            lease
                .live_host(later.saturating_add(Duration::from_millis(149)))
                .is_some(),
            "a later paused command remains live for the first resumed transition"
        );
        lease.update_ready(&source, ParticipantReadyStatus::Lost);
        assert!(lease.live_host(later).is_none());
    }

    #[test]
    fn late_attachment_begins_from_its_immutable_world_boundary() {
        let attached = WorldProgress::at(42, 12_000_000).expect("late world boundary");
        assert_eq!(activation_progress(None, 7, attached), Some(attached));
        assert_eq!(activation_progress(Some(7), 7, attached), None);
    }

    #[test]
    fn geared_motor_commands_use_the_same_native_domain_as_rendered_limits() {
        assert_eq!(
            dispatch_motor(
                MotorCommand::Position,
                &api::component::motor::Command::Position(6.0),
                3.0,
            )
            .expect("position command"),
            MotorAction::Position(2.0)
        );
        assert_eq!(
            dispatch_motor(
                MotorCommand::Velocity,
                &api::component::motor::Command::Velocity(6.0),
                3.0,
            )
            .expect("velocity command"),
            MotorAction::Velocity(2.0)
        );
        assert_eq!(
            dispatch_motor(
                MotorCommand::Torque,
                &api::component::motor::Command::Torque(2.0),
                3.0,
            )
            .expect("torque command"),
            MotorAction::Torque(6.0)
        );
    }

    #[test]
    fn parking_attempts_every_motor_after_one_stop_fails() {
        struct Probe {
            name: &'static str,
            parked: bool,
        }
        let mut motors = [
            Probe {
                name: "first",
                parked: false,
            },
            Probe {
                name: "second",
                parked: false,
            },
        ];
        let result = stop_every(
            &mut motors,
            |motor| motor.name.to_owned(),
            |motor| {
                motor.parked = true;
                if motor.name == "first" {
                    anyhow::bail!("injected motor failure");
                }
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(motors.iter().all(|motor| motor.parked));
    }

    #[test]
    fn completed_transition_publishes_outputs_before_step() {
        let order = std::cell::RefCell::new(Vec::new());
        publish_completed_transition(
            || {
                order.borrow_mut().push("output");
                Ok(())
            },
            || {
                order.borrow_mut().push("step");
                Ok(())
            },
        )
        .expect("both publications succeed");
        assert_eq!(*order.borrow(), ["output", "step"]);
    }

    #[test]
    fn lossless_publication_refusal_is_a_controller_local_protocol_fault() {
        let output_fault = publish_completed_transition(
            || {
                Err(anyhow::Error::new(SimulatorError::Bus(
                    BusError::WouldBlock {
                        topic: "robot/joint/elbow/state".to_owned(),
                    },
                )))
            },
            || panic!("StepEvent must not be attempted after an output refusal"),
        )
        .expect_err("a refused output faults the controller");
        assert!(matches!(
            output_fault,
            ControllerFault::Protocol { ref detail }
                if detail.contains("typed output publication failed")
                    && detail.contains("would block")
        ));

        let step_fault = publish_completed_transition(
            || Ok(()),
            || {
                Err(SimulatorError::Bus(BusError::WouldBlock {
                    topic: "simulation/step".to_owned(),
                }))
            },
        )
        .expect_err("a refused StepEvent faults the controller");
        assert!(matches!(
            step_fault,
            ControllerFault::Protocol { ref detail }
                if detail.contains("StepEvent publication failed")
                    && detail.contains("would block")
        ));
    }
}
