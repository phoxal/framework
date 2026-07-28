//! Webots controller simulator artifact.
//!
//! Binds one Webots-owned controller process to a robot's component
//! capabilities and publishes/subscribes exactly the `component::*` contracts
//! those capabilities need. The process bootstraps the normal framework runner,
//! mints one opaque timeline, and runs only the external Webots step loop. Each
//! step applies the actuator inputs, publishes all component outputs, and then
//! the matching `simulation::Clock`.
//!
//! Every capability kind a component may declare is simulated here except one:
//! Webots has no button, switch, or toggle node, so nothing in a simulated
//! world can engage or release an `emergency_stop`. That capability is
//! deliberately left unpublished rather than driven from a static config, which
//! would assert a state no one in the world can change.

use crate::capabilities;
use phoxal::api;
use phoxal::bus::ContractBody;
use phoxal::bus::{StepStamp, TimelineId, WorldStepToken};
use phoxal::model::component::v0::CapabilityRef;
use phoxal::model::simulation::Simulation as SimulationFile;
use phoxal::model::simulation::v0::Simulation as SimulationSpec;
use phoxal::model::v0::Robot;
use phoxal::prelude::*;
// `TimelineAuthority` and `WorldClockPublisher` are deliberately not part of
// `phoxal::bus`/`phoxal::prelude`: they are world-clock
// authority types only this simulator legitimately names, so they live behind
// the explicit `phoxal::raw` opt-in instead - see that module's docs.
use phoxal::raw::{TimelineAuthority, WorldClockPublisher};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};

use crate::capabilities::accelerometer::{AccelerometerSpec, NativeAccelerometer};
use crate::capabilities::battery::{BatterySpec, NativeBattery};
use crate::capabilities::camera::{CameraSpec, NativeCamera};
use crate::capabilities::depth::{DepthSpec, NativeDepth};
use crate::capabilities::encoder::{EncoderSpec, NativeEncoder};
use crate::capabilities::gnss::{GnssSpec, NativeGnss};
use crate::capabilities::gyroscope::{GyroscopeSpec, NativeGyroscope};
use crate::capabilities::imu::{ImuSpec, NativeImu};
use crate::capabilities::led::{LedSpec, NativeLed};
use crate::capabilities::lidar::{LidarSpec, NativeLidar};
use crate::capabilities::magnetometer::NativeMagnetometer;
use crate::capabilities::microphone::NativeMicrophone;
use crate::capabilities::mmwave::NativeMmwave;
use crate::capabilities::motor::{MotorSpec, NativeMotor};
use crate::capabilities::range::{NativeRange, RangeSpec};
use crate::capabilities::speaker::{NativeSpeaker, SpeakerSpec};

const STEP_HZ: f64 = 100.0;
const COMPONENTS_DIR: &str = "components";
const SIMULATION_FILE: &str = "simulation.yaml";

#[derive(Clone)]
pub struct Api {
    clock: WorldClockPublisher<api::simulation::Clock>,
    motor_commands: Vec<Subscriber<api::component::motor::Command>>,
    encoders: Vec<MeasurementPublisher<api::component::encoder::Sample>>,
    imus: Vec<MeasurementPublisher<api::component::imu::Sample>>,
    accelerometers: Vec<MeasurementPublisher<api::component::accelerometer::Sample>>,
    gyroscopes: Vec<MeasurementPublisher<api::component::gyroscope::Sample>>,
    ranges: Vec<MeasurementPublisher<api::component::range::Sample>>,
    cameras: Vec<MeasurementPublisher<api::component::camera::Frame>>,
    depths: Vec<MeasurementPublisher<api::component::depth::Frame>>,
    gnss: Vec<MeasurementPublisher<api::component::gnss::Sample>>,
    magnetometers: Vec<MeasurementPublisher<api::component::magnetometer::Sample>>,
    lidars: Vec<MeasurementPublisher<api::component::lidar::Scan>>,
    mmwaves: Vec<MeasurementPublisher<api::component::mmwave::Scan>>,
    microphones: Vec<MeasurementPublisher<api::component::microphone::Frame>>,
    batteries: Vec<StatePublisher<api::component::battery::State>>,
    led_commands: Vec<Subscriber<api::component::led::Command>>,
    speaker_streams: Vec<Subscriber<api::component::speaker::Chunk>>,
}

struct ControllerRuntime {
    /// This controller's exclusive ownership of the world's timeline. It is
    /// the only way anything in this process can express a robot instant.
    authority: TimelineAuthority,
    step_index: u64,
    backend: SharedBackend,
}

type SharedBackend = Arc<Mutex<BackendControl>>;

struct BackendControl {
    backend: Backend,
    motor_specs: Vec<MotorSpec>,
}

struct BlockingStep {
    now_ns: u64,
    outputs: BackendOutput,
}

/// Bootstrap the Webots-owned controller.
///
/// The controller joins the supervised run through `PHOXAL_EXECUTION_ID`, which
/// the supervisor puts in the Webots application's environment and Webots
/// passes through to this child process. It mints its own [`ProducerId`] if the
/// supervisor did not pre-mint one, and it always mints its own timeline: a
/// world history belongs to the controller process that runs it, never to the
/// CLI (#952 section B).
pub fn run() -> Result<()> {
    if has_explicit_producer_arg(std::env::args_os()) {
        bail!(
            "--producer-id is not accepted by the Webots-owned controller; it mints its own \
             process identity"
        );
    }
    phoxal::run::<WebotsControllerSimulator>()
}

fn has_explicit_producer_arg(args: impl IntoIterator<Item = OsString>) -> bool {
    args.into_iter().any(|arg| {
        arg == "--producer-id"
            || arg
                .to_str()
                .is_some_and(|arg| arg.starts_with("--producer-id="))
    })
}

pub struct WebotsControllerState {
    backend: SharedBackend,
}

#[phoxal::simulator(state = WebotsControllerState, api = Api)]
pub struct WebotsControllerSimulator;

impl Participant for WebotsControllerSimulator {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let robot = ctx.robot()?;
        let root = ctx.robot_root()?;
        let catalog = CapabilityCatalog::from_robot(root, robot)?;
        let clock = ctx
            .world_clock_publisher(api::topic::owner().simulation().clock())
            .await?;

        let mut motor_commands = Vec::new();
        for spec in &catalog.motors {
            motor_commands.push(
                ctx.subscriber(
                    api::topic::owner()
                        .component(&spec.reference.component_id)
                        .motor(&spec.reference.capability_id)
                        .command(),
                    32,
                )
                .await?,
            );
        }

        let mut encoders = Vec::new();
        for spec in &catalog.encoders {
            encoders.push(
                ctx.measurement_publisher(
                    api::topic::owner()
                        .component(&spec.reference.component_id)
                        .encoder(&spec.reference.capability_id)
                        .sample(),
                )
                .await?,
            );
        }

        let mut imus = Vec::new();
        for spec in &catalog.imus {
            imus.push(
                ctx.measurement_publisher(
                    api::topic::owner()
                        .component(&spec.reference.component_id)
                        .imu(&spec.reference.capability_id)
                        .sample(),
                )
                .await?,
            );
        }

        let mut accelerometers = Vec::new();
        for spec in &catalog.accelerometers {
            accelerometers.push(
                ctx.measurement_publisher(
                    api::topic::owner()
                        .component(&spec.reference.component_id)
                        .accelerometer(&spec.reference.capability_id)
                        .sample(),
                )
                .await?,
            );
        }

        let mut gyroscopes = Vec::new();
        for spec in &catalog.gyroscopes {
            gyroscopes.push(
                ctx.measurement_publisher(
                    api::topic::owner()
                        .component(&spec.reference.component_id)
                        .gyroscope(&spec.reference.capability_id)
                        .sample(),
                )
                .await?,
            );
        }

        let mut ranges = Vec::new();
        for spec in &catalog.ranges {
            ranges.push(
                ctx.measurement_publisher(
                    api::topic::owner()
                        .component(&spec.sampled.reference.component_id)
                        .range(&spec.sampled.reference.capability_id)
                        .sample(),
                )
                .await?,
            );
        }

        let mut cameras = Vec::new();
        for spec in &catalog.cameras {
            cameras.push(
                ctx.measurement_publisher(
                    api::topic::owner()
                        .component(&spec.sampled.reference.component_id)
                        .camera(&spec.sampled.reference.capability_id)
                        .frame(),
                )
                .await?,
            );
        }

        let mut depths = Vec::new();
        for spec in &catalog.depths {
            depths.push(
                ctx.measurement_publisher(
                    api::topic::owner()
                        .component(&spec.sampled.reference.component_id)
                        .depth(&spec.sampled.reference.capability_id)
                        .frame(),
                )
                .await?,
            );
        }

        let mut gnss = Vec::new();
        for spec in &catalog.gnss {
            gnss.push(
                ctx.measurement_publisher(
                    api::topic::owner()
                        .component(&spec.sampled.reference.component_id)
                        .gnss(&spec.sampled.reference.capability_id)
                        .sample(),
                )
                .await?,
            );
        }

        let mut magnetometers = Vec::new();
        for spec in &catalog.magnetometers {
            magnetometers.push(
                ctx.measurement_publisher(
                    api::topic::owner()
                        .component(&spec.reference.component_id)
                        .magnetometer(&spec.reference.capability_id)
                        .sample(),
                )
                .await?,
            );
        }

        let mut lidars = Vec::new();
        for spec in &catalog.lidars {
            lidars.push(
                ctx.measurement_publisher(
                    api::topic::owner()
                        .component(&spec.sampled.reference.component_id)
                        .lidar(&spec.sampled.reference.capability_id)
                        .scan(),
                )
                .await?,
            );
        }

        let mut mmwaves = Vec::new();
        for spec in &catalog.mmwaves {
            mmwaves.push(
                ctx.measurement_publisher(
                    api::topic::owner()
                        .component(&spec.reference.component_id)
                        .mmwave(&spec.reference.capability_id)
                        .scan(),
                )
                .await?,
            );
        }

        let mut microphones = Vec::new();
        for spec in &catalog.microphones {
            microphones.push(
                ctx.measurement_publisher(
                    api::topic::owner()
                        .component(&spec.reference.component_id)
                        .microphone(&spec.reference.capability_id)
                        .frame(),
                )
                .await?,
            );
        }

        let mut batteries = Vec::new();
        for spec in &catalog.batteries {
            batteries.push(
                ctx.state_publisher(
                    api::topic::owner()
                        .component(&spec.reference.component_id)
                        .battery(&spec.reference.capability_id)
                        .state(),
                )
                .await?,
            );
        }

        let mut led_commands = Vec::new();
        for spec in &catalog.leds {
            led_commands.push(
                ctx.subscriber(
                    api::topic::owner()
                        .component(&spec.reference.component_id)
                        .led(&spec.reference.capability_id)
                        .command(),
                    32,
                )
                .await?,
            );
        }

        let mut speaker_streams = Vec::new();
        for spec in &catalog.speakers {
            speaker_streams.push(
                ctx.subscriber(
                    api::topic::owner()
                        .component(&spec.reference.component_id)
                        .speaker(&spec.reference.capability_id)
                        .stream(),
                    // A stream arrives as many chunks in a row, and dropping
                    // one silently corrupts the sound rather than shortening
                    // it, so this queue is deeper than a command queue.
                    256,
                )
                .await?,
            );
        }

        let backend = Arc::new(Mutex::new(BackendControl {
            backend: Backend::open(&catalog)?,
            motor_specs: catalog.motors.clone(),
        }));
        tracing::info!(
            target: "simulator_webots_controller",
            motors = catalog.motors.len(),
            encoders = catalog.encoders.len(),
            imus = catalog.imus.len(),
            accelerometers = catalog.accelerometers.len(),
            gyroscopes = catalog.gyroscopes.len(),
            ranges = catalog.ranges.len(),
            cameras = catalog.cameras.len(),
            depths = catalog.depths.len(),
            gnss = catalog.gnss.len(),
            magnetometers = catalog.magnetometers.len(),
            lidars = catalog.lidars.len(),
            mmwaves = catalog.mmwaves.len(),
            microphones = catalog.microphones.len(),
            batteries = catalog.batteries.len(),
            leds = catalog.leds.len(),
            speakers = catalog.speakers.len(),
            "webots controller simulator ready"
        );

        let api = Api {
            clock,
            motor_commands,
            encoders,
            imus,
            accelerometers,
            gyroscopes,
            ranges,
            cameras,
            depths,
            gnss,
            magnetometers,
            lidars,
            mmwaves,
            microphones,
            batteries,
            led_commands,
            speaker_streams,
        };

        let runtime = ControllerRuntime {
            authority: ctx.timeline_authority(TimelineId::mint())?,
            step_index: 0,
            backend: Arc::clone(&backend),
        };
        let loop_api = api.clone();
        ctx.spawn_managed("webots-step-loop", async move {
            if let Err(error) = runtime.run(loop_api).await {
                tracing::error!(
                    target: "simulator_webots_controller",
                    error = %error,
                    "external Webots step loop stopped"
                );
            }
        });

        Ok((WebotsControllerState { backend }, api))
    }

    async fn shutdown(&self, api: &Self::Api, state: &mut Self::State) -> Result<()> {
        for subscriber in &api.motor_commands {
            let _latest = drain_latest(subscriber);
        }
        park_backend(Arc::clone(&state.backend)).await
    }
}

impl ControllerRuntime {
    async fn run(mut self, api: Api) -> Result<()> {
        loop {
            if let Err(error) = self.step_once(&api).await {
                if let Err(park_error) = park_backend(Arc::clone(&self.backend)).await {
                    tracing::warn!(
                        target: "simulator_webots_controller",
                        error = %park_error,
                        "failed to park motors after the Webots step loop stopped"
                    );
                }
                return Err(error);
            }
        }
    }

    async fn step_once(&mut self, api: &Api) -> Result<()> {
        let inputs = latest_inputs(api);
        let next_step = self.step_index.saturating_add(1);
        let step_index = self.step_index;
        let backend = Arc::clone(&self.backend);
        let step = tokio::task::spawn_blocking(move || {
            let mut control = lock_backend(&backend)?;
            let step_ns = control.backend.step_ns()?;
            let now_ns = next_step.saturating_mul(step_ns);
            let outputs = control.backend.advance(step_index, now_ns, inputs)?;
            Ok::<_, anyhow::Error>(BlockingStep { now_ns, outputs })
        })
        .await
        .context("Webots step worker failed to join")??;

        // One completed world advance mints one token, and every output of
        // that advance is stamped with it. There is no other way for this
        // process to express a robot instant.
        let world_step = self.authority.completed_step(step.now_ns);
        commit_step(api, &world_step, next_step, step.outputs)?;
        self.step_index = next_step;
        tracing::trace!(
            target: "simulator_webots_controller",
            timeline = %self.authority.timeline(),
            step = self.step_index,
            ticks = step.now_ns,
            "external Webots step committed"
        );
        Ok(())
    }
}

/// Everything the graph asked this world's actuators to do since the previous
/// step.
///
/// Motors and LEDs keep only the newest command - a superseded setpoint has no
/// effect worth applying. A speaker keeps every chunk in order, because its
/// chunks are not alternatives: dropping one corrupts the sound rather than
/// replacing it.
struct BackendInput {
    motors: Vec<Option<api::component::motor::Command>>,
    leds: Vec<Option<api::component::led::Command>>,
    speakers: Vec<Vec<api::component::speaker::Chunk>>,
}

fn latest_inputs(api: &Api) -> BackendInput {
    BackendInput {
        motors: api.motor_commands.iter().map(drain_latest).collect(),
        leds: api.led_commands.iter().map(drain_latest).collect(),
        speakers: api.speaker_streams.iter().map(drain_all).collect(),
    }
}

/// Publishes everything one completed world advance produced, then the clock
/// that closes it. The order is the contract: a reader that has seen the clock
/// for a step has already seen that step's outputs.
fn commit_step(
    api: &Api,
    world_step: &WorldStepToken,
    step: u64,
    outputs: BackendOutput,
) -> Result<()> {
    publish_outputs(api, world_step, outputs)?;
    api.clock
        .publish(world_step, api::simulation::Clock { step })?;
    Ok(())
}

fn publish_outputs(api: &Api, world_step: &WorldStepToken, outputs: BackendOutput) -> Result<()> {
    // Simulated sensors read the world at exactly the instant the world
    // advanced to, so their capture is exact rather than uncertain.
    let captured_at = CaptureStamp::exact(world_step.instant());
    for (publisher, sample) in api.encoders.iter().zip(outputs.encoders) {
        if let Some(sample) = sample {
            publisher.publish(captured_at, sample)?;
        }
    }
    for (publisher, sample) in api.imus.iter().zip(outputs.imus) {
        if let Some(sample) = sample {
            publisher.publish(captured_at, sample)?;
        }
    }
    for (publisher, sample) in api.accelerometers.iter().zip(outputs.accelerometers) {
        if let Some(sample) = sample {
            publisher.publish(captured_at, sample)?;
        }
    }
    for (publisher, sample) in api.gyroscopes.iter().zip(outputs.gyroscopes) {
        if let Some(sample) = sample {
            publisher.publish(captured_at, sample)?;
        }
    }
    for (publisher, sample) in api.ranges.iter().zip(outputs.ranges) {
        if let Some(sample) = sample {
            publisher.publish(captured_at, sample)?;
        }
    }
    for (publisher, frame) in api.cameras.iter().zip(outputs.cameras) {
        if let Some(frame) = frame {
            publisher.publish(captured_at, frame)?;
        }
    }
    for (publisher, frame) in api.depths.iter().zip(outputs.depths) {
        if let Some(frame) = frame {
            publisher.publish(captured_at, frame)?;
        }
    }
    for (publisher, sample) in api.gnss.iter().zip(outputs.gnss) {
        if let Some(sample) = sample {
            publisher.publish(captured_at, sample)?;
        }
    }
    for (publisher, sample) in api.magnetometers.iter().zip(outputs.magnetometers) {
        if let Some(sample) = sample {
            publisher.publish(captured_at, sample)?;
        }
    }
    for (publisher, scan) in api.lidars.iter().zip(outputs.lidars) {
        if let Some(scan) = scan {
            publisher.publish(captured_at, scan)?;
        }
    }
    for (publisher, scan) in api.mmwaves.iter().zip(outputs.mmwaves) {
        if let Some(scan) = scan {
            publisher.publish(captured_at, scan)?;
        }
    }
    for (publisher, frame) in api.microphones.iter().zip(outputs.microphones) {
        if let Some(frame) = frame {
            publisher.publish(captured_at, frame)?;
        }
    }
    // A battery reports what the pack is, not what a sensor saw at an instant,
    // so it is state stamped with the world step like the clock itself.
    for (publisher, state) in api.batteries.iter().zip(outputs.batteries) {
        if let Some(state) = state {
            publisher.publish(world_step, state)?;
        }
    }
    Ok(())
}

fn drain_all<B: ContractBody>(subscriber: &Subscriber<B>) -> Vec<B> {
    let mut received = Vec::new();
    while let Some(message) = subscriber.try_recv() {
        received.push(message.body);
    }
    received
}

fn drain_latest<B: ContractBody>(subscriber: &Subscriber<B>) -> Option<B> {
    let mut latest = None;
    while let Some(received) = subscriber.try_recv() {
        latest = Some(received.body);
    }
    latest
}

fn lock_backend(backend: &SharedBackend) -> Result<std::sync::MutexGuard<'_, BackendControl>> {
    backend
        .lock()
        .map_err(|_| anyhow!("Webots backend mutex is poisoned"))
}

async fn park_backend(backend: SharedBackend) -> Result<()> {
    tokio::task::spawn_blocking(move || lock_backend(&backend)?.park())
        .await
        .context("Webots motor parking worker failed to join")?
}

/// The set of component-capability specs this robot's Webots model exposes,
/// read from the staged component catalog (`ctx.robot()`).
#[derive(Clone, Debug, Default)]
struct CapabilityCatalog {
    motors: Vec<MotorSpec>,
    encoders: Vec<EncoderSpec>,
    imus: Vec<ImuSpec>,
    accelerometers: Vec<AccelerometerSpec>,
    gyroscopes: Vec<GyroscopeSpec>,
    ranges: Vec<RangeSpec>,
    cameras: Vec<CameraSpec>,
    depths: Vec<DepthSpec>,
    gnss: Vec<GnssSpec>,
    magnetometers: Vec<capabilities::SampledSpec>,
    lidars: Vec<LidarSpec>,
    mmwaves: Vec<capabilities::SampledSpec>,
    microphones: Vec<capabilities::SampledSpec>,
    batteries: Vec<BatterySpec>,
    leds: Vec<LedSpec>,
    speakers: Vec<SpeakerSpec>,
}

impl CapabilityCatalog {
    fn from_robot(root: &Path, robot: &Robot) -> Result<Self> {
        use phoxal::model::component::v0::capability::Capability;

        let simulations = load_simulation_specs(root, robot)?;
        let mut catalog = Self::default();

        for (component_id, instance) in robot.manifest.components() {
            let component = robot.component_for_instance(component_id)?;
            let simulation = simulations.get(&instance.component);
            for (capability_id, capability) in &component.capabilities {
                let reference = CapabilityRef::new(component_id, capability_id);
                let simulation_capability =
                    simulation.and_then(|sim| sim.capabilities.get(capability_id));
                match capability {
                    Capability::Motor(config) => {
                        catalog.motors.push(MotorSpec {
                            reference,
                            actuator_type: config.command,
                            gear_ratio: config.gear_ratio,
                        });
                    }
                    Capability::Encoder(config) => {
                        let sampling_hz = simulation_capability
                            .and_then(simulation_sampling_rate)
                            .unwrap_or(config.publish_rate_hz);
                        catalog.encoders.push(EncoderSpec {
                            reference,
                            publish_every_steps: capabilities::publish_every_steps(
                                STEP_HZ,
                                config.publish_rate_hz,
                            )?,
                            sampling_period_ms: capabilities::sampling_period_ms(sampling_hz)?,
                            gear_ratio: config.gear_ratio,
                        });
                    }
                    Capability::Imu(config) => {
                        catalog.imus.push(sampled_spec(
                            reference,
                            config.publish_rate_hz,
                            simulation_capability,
                        )?);
                    }
                    Capability::Accelerometer(config) => {
                        catalog.accelerometers.push(sampled_spec(
                            reference,
                            config.publish_rate_hz,
                            simulation_capability,
                        )?);
                    }
                    Capability::Gyroscope(config) => {
                        catalog.gyroscopes.push(sampled_spec(
                            reference,
                            config.publish_rate_hz,
                            simulation_capability,
                        )?);
                    }
                    Capability::Range(config) => {
                        let sampled =
                            sampled_spec(reference, config.publish_rate_hz, simulation_capability)?;
                        catalog.ranges.push(RangeSpec {
                            sampled,
                            min_range_m: config.min_range_m as f32,
                            max_range_m: config.max_range_m as f32,
                        });
                    }
                    Capability::Camera(config) => {
                        let sampled =
                            sampled_spec(reference, config.publish_rate_hz, simulation_capability)?;
                        catalog.cameras.push(CameraSpec {
                            sampled,
                            mode: config.mode,
                            width: config.width_px,
                            height: config.height_px,
                        });
                    }
                    Capability::Depth(config) => {
                        let sampled =
                            sampled_spec(reference, config.publish_rate_hz, simulation_capability)?;
                        catalog.depths.push(DepthSpec {
                            sampled,
                            width: config.width_px,
                            height: config.height_px,
                        });
                    }
                    Capability::Gnss(config) => {
                        let sampled =
                            sampled_spec(reference, config.publish_rate_hz, simulation_capability)?;
                        catalog.gnss.push(GnssSpec {
                            sampled,
                            coordinate_system: config.coordinate_system,
                        });
                    }
                    Capability::Magnetometer(config) => {
                        catalog.magnetometers.push(sampled_spec(
                            reference,
                            config.publish_rate_hz,
                            simulation_capability,
                        )?);
                    }
                    Capability::Lidar(config) => {
                        let sampled =
                            sampled_spec(reference, config.publish_rate_hz, simulation_capability)?;
                        catalog.lidars.push(LidarSpec {
                            sampled,
                            output: config.output,
                        });
                    }
                    Capability::Mmwave(config) => {
                        catalog.mmwaves.push(sampled_spec(
                            reference,
                            config.publish_rate_hz,
                            simulation_capability,
                        )?);
                    }
                    Capability::Microphone(config) => {
                        catalog.microphones.push(sampled_spec(
                            reference,
                            config.publish_rate_hz,
                            simulation_capability,
                        )?);
                    }
                    Capability::Battery(config) => {
                        let sampling_hz = simulation_capability
                            .and_then(simulation_sampling_rate)
                            .unwrap_or(config.publish_rate_hz);
                        catalog.batteries.push(BatterySpec {
                            reference,
                            publish_every_steps: capabilities::publish_every_steps(
                                STEP_HZ,
                                config.publish_rate_hz,
                            )?,
                            sampling_period_ms: capabilities::sampling_period_ms(sampling_hz)?,
                            voltage_v: config.voltage_v,
                            capacity_ah: config.capacity_ah,
                        });
                    }
                    Capability::Led(_) => {
                        catalog.leds.push(LedSpec { reference });
                    }
                    Capability::Speaker(_) => {
                        catalog.speakers.push(SpeakerSpec { reference });
                    }
                    // Webots has no button, switch, or toggle node, so nothing
                    // in a simulated world can engage or release an e-stop.
                    // Leaving it unpublished is the honest state: `motion`
                    // fails closed on a component it never hears from.
                    Capability::EmergencyStop(_) => {
                        tracing::debug!(
                            target: "simulator_webots_controller",
                            capability = %reference,
                            kind = capability.kind_name(),
                            "Webots models no emergency-stop control, so this capability is \
                             not simulated"
                        );
                    }
                }
            }
        }

        Ok(catalog)
    }
}

fn sampled_spec(
    reference: CapabilityRef,
    publish_rate_hz: f64,
    simulation_capability: Option<&phoxal::model::simulation::capability::Capability>,
) -> Result<capabilities::SampledSpec> {
    let sampling_hz = simulation_capability
        .and_then(simulation_sampling_rate)
        .unwrap_or(publish_rate_hz);
    Ok(capabilities::SampledSpec {
        reference,
        publish_every_steps: capabilities::publish_every_steps(STEP_HZ, publish_rate_hz)?,
        sampling_period_ms: capabilities::sampling_period_ms(sampling_hz)?,
    })
}

fn simulation_sampling_rate(
    capability: &phoxal::model::simulation::capability::Capability,
) -> Option<f64> {
    use phoxal::model::simulation::capability::Capability as SimCapability;
    match capability {
        SimCapability::Encoder(config) => Some(config.sampling_period_hz),
        SimCapability::Accelerometer(config) => Some(config.sampling_period_hz),
        SimCapability::Gyroscope(config) => Some(config.sampling_period_hz),
        SimCapability::Magnetometer(config) => Some(config.sampling_period_hz),
        SimCapability::Imu(config) => Some(config.sampling_period_hz),
        SimCapability::Gnss(config) => Some(config.sampling_period_hz),
        SimCapability::Camera(config) => Some(config.sampling_period_hz),
        SimCapability::Depth(config) => Some(config.sampling_period_hz),
        SimCapability::Range(config) => Some(config.sampling_period_hz),
        SimCapability::Lidar(config) => Some(config.sampling_period_hz),
        SimCapability::Mmwave(config) => Some(config.sampling_period_hz),
        SimCapability::Microphone(config) => Some(config.sampling_period_hz),
        SimCapability::Motor(_)
        | SimCapability::EmergencyStop
        | SimCapability::Speaker
        | SimCapability::Battery
        | SimCapability::Led => None,
    }
}

fn load_simulation_specs(root: &Path, robot: &Robot) -> Result<BTreeMap<String, SimulationSpec>> {
    let mut simulations = BTreeMap::new();
    for component_type in robot.manifest.used_component_types() {
        let path = component_simulation_path(root, component_type);
        if !path.join(SIMULATION_FILE).is_file() {
            continue;
        }
        let simulation = SimulationFile::read_from_dir(&path)
            .with_context(|| {
                format!("failed to read Webots simulation config for {component_type}")
            })?
            .as_v0()
            .context("webots simulator only supports simulation.yaml version v0")?
            .clone();
        simulations.insert(component_type.to_string(), simulation);
    }
    Ok(simulations)
}

/// Resolves the staged directory for a used component type's simulation
/// config. Cargo dependency discovery selects component crates; the assembled
/// runtime bundle exposes their assets under `components/<type>`.
fn component_simulation_path(bundle_root: &Path, component_type: &str) -> PathBuf {
    bundle_root.join(COMPONENTS_DIR).join(component_type)
}

/// Native `webots-rs` linkage is the only backend a shipped binary can
/// construct. The `#[cfg(test)]` stub fakes the simulator runtime (not the
/// linking) so `step_once`'s clock behavior can be unit-tested without a live
/// Webots controller process.
enum Backend {
    Native(Box<NativeBackend>),
    #[cfg(test)]
    Stub(StubBackend),
}

impl BackendControl {
    /// Leaves the world quiet: every motor stopped and every speaker silent.
    /// A simulation that stopped must not keep driving or keep playing.
    fn park(&mut self) -> Result<()> {
        let mut first_error = None;
        for spec in &self.motor_specs {
            if let Err(error) = self
                .backend
                .apply_motor_command(spec, &api::component::motor::Command::Stop)
            {
                tracing::warn!(
                    target: "simulator_webots_controller",
                    capability = %spec.reference,
                    error = %error,
                    "failed to park motor while stopping"
                );
                first_error.get_or_insert(error);
            }
        }
        if let Err(error) = self.backend.silence_speakers() {
            tracing::warn!(
                target: "simulator_webots_controller",
                error = %error,
                "failed to silence speakers while stopping"
            );
            first_error.get_or_insert(error);
        }
        first_error.map_or(Ok(()), Err)
    }
}

impl Backend {
    fn open(catalog: &CapabilityCatalog) -> Result<Self> {
        Ok(Self::Native(Box::new(NativeBackend::new(catalog)?)))
    }

    fn advance(
        &mut self,
        step_index: u64,
        time_ns: u64,
        inputs: BackendInput,
    ) -> Result<BackendOutput> {
        match self {
            Self::Native(backend) => backend.advance(step_index, time_ns, inputs),
            #[cfg(test)]
            Self::Stub(backend) => backend.advance(step_index, time_ns, inputs),
        }
    }

    fn step_ns(&self) -> Result<u64> {
        match self {
            Self::Native(backend) => step_ns_from_ms(backend.step_ms),
            #[cfg(test)]
            Self::Stub(_) => Ok(10_000_000),
        }
    }

    fn apply_motor_command(
        &mut self,
        spec: &MotorSpec,
        command: &api::component::motor::Command,
    ) -> Result<()> {
        match self {
            Self::Native(backend) => backend.apply_motor_command(spec, command),
            #[cfg(test)]
            Self::Stub(_) => Ok(()),
        }
    }

    fn silence_speakers(&self) -> Result<()> {
        match self {
            Self::Native(backend) => backend.silence_speakers(),
            #[cfg(test)]
            Self::Stub(_) => Ok(()),
        }
    }
}

fn step_ns_from_ms(step_ms: i32) -> Result<u64> {
    let step_ms =
        u64::try_from(step_ms).context("Webots basicTimeStep must be a positive integer")?;
    if step_ms == 0 {
        bail!("Webots basicTimeStep must be > 0");
    }
    step_ms
        .checked_mul(1_000_000)
        .context("Webots basicTimeStep overflows nanoseconds")
}

#[cfg(test)]
struct StubBackend {
    counts: OutputCounts,
}

#[cfg(test)]
impl StubBackend {
    fn new(catalog: &CapabilityCatalog) -> Self {
        Self {
            counts: OutputCounts::from_catalog(catalog),
        }
    }

    fn advance(
        &mut self,
        _step_index: u64,
        _time_ns: u64,
        _inputs: BackendInput,
    ) -> Result<BackendOutput> {
        Ok(BackendOutput::empty(&self.counts))
    }
}

/// Owns the Webots controller-process handle: base handle open, the sole
/// `webots.step()` loop, and every component device wrapper. This crate never
/// opens a `webots_rs::Supervisor`.
struct NativeBackend {
    webots: webots_rs::Webots,
    step_ms: i32,
    motors: Vec<NativeMotor>,
    encoders: Vec<NativeEncoder>,
    imus: Vec<NativeImu>,
    accelerometers: Vec<NativeAccelerometer>,
    gyroscopes: Vec<NativeGyroscope>,
    ranges: Vec<NativeRange>,
    cameras: Vec<NativeCamera>,
    depths: Vec<NativeDepth>,
    gnss: Vec<NativeGnss>,
    magnetometers: Vec<NativeMagnetometer>,
    lidars: Vec<NativeLidar>,
    mmwaves: Vec<NativeMmwave>,
    microphones: Vec<NativeMicrophone>,
    batteries: Vec<NativeBattery>,
    leds: Vec<NativeLed>,
    speakers: Vec<NativeSpeaker>,
}

impl NativeBackend {
    fn new(catalog: &CapabilityCatalog) -> Result<Self> {
        let webots = webots_rs::Webots::new().map_err(|error| anyhow!(error))?;
        let step_ms = webots
            .get_basic_time_step()
            .map_err(|error| anyhow!(error))?
            .round() as i32;
        if step_ms <= 0 {
            bail!("Webots basicTimeStep must be > 0");
        }
        let mut motors = Vec::new();
        let mut encoders = Vec::new();
        let mut imus = Vec::new();
        let mut accelerometers = Vec::new();
        let mut gyroscopes = Vec::new();
        let mut ranges = Vec::new();
        let mut cameras = Vec::new();
        let mut depths = Vec::new();
        let mut gnss = Vec::new();
        let mut magnetometers = Vec::new();
        let mut lidars = Vec::new();
        let mut mmwaves = Vec::new();
        let mut microphones = Vec::new();
        let mut batteries = Vec::new();
        let mut leds = Vec::new();
        let mut speakers = Vec::new();

        for spec in &catalog.motors {
            motors.push(NativeMotor::new(&webots, spec)?);
        }
        for spec in &catalog.encoders {
            encoders.push(NativeEncoder::new(&webots, spec)?);
        }
        for spec in &catalog.imus {
            imus.push(NativeImu::new(&webots, spec)?);
        }
        for spec in &catalog.accelerometers {
            accelerometers.push(NativeAccelerometer::new(&webots, spec)?);
        }
        for spec in &catalog.gyroscopes {
            gyroscopes.push(NativeGyroscope::new(&webots, spec)?);
        }
        for spec in &catalog.ranges {
            ranges.push(NativeRange::new(&webots, spec)?);
        }
        for spec in &catalog.cameras {
            cameras.push(NativeCamera::new(&webots, spec)?);
        }
        for spec in &catalog.depths {
            depths.push(NativeDepth::new(&webots, spec)?);
        }
        for spec in &catalog.gnss {
            gnss.push(NativeGnss::new(&webots, spec)?);
        }
        for spec in &catalog.magnetometers {
            magnetometers.push(NativeMagnetometer::new(&webots, spec)?);
        }
        for spec in &catalog.lidars {
            lidars.push(NativeLidar::new(&webots, spec)?);
        }
        for spec in &catalog.mmwaves {
            mmwaves.push(NativeMmwave::new(&webots, spec)?);
        }
        for spec in &catalog.microphones {
            microphones.push(NativeMicrophone::new(&webots, spec)?);
        }
        // Webots gives a robot exactly one battery sensor, so a second battery
        // capability would report the first one's energy under another name.
        if catalog.batteries.len() > 1 {
            let declared = catalog
                .batteries
                .iter()
                .map(|spec| spec.reference.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "Webots models one battery per robot, but this robot declares {}: {declared}. \
                 Keep exactly one battery capability for a simulated robot.",
                catalog.batteries.len()
            );
        }
        for spec in &catalog.batteries {
            batteries.push(NativeBattery::new(spec)?);
        }
        for spec in &catalog.leds {
            leds.push(NativeLed::new(&webots, spec)?);
        }
        for spec in &catalog.speakers {
            speakers.push(NativeSpeaker::new(&webots, spec)?);
        }

        Ok(Self {
            webots,
            step_ms,
            motors,
            encoders,
            imus,
            accelerometers,
            gyroscopes,
            ranges,
            cameras,
            depths,
            gnss,
            magnetometers,
            lidars,
            mmwaves,
            microphones,
            batteries,
            leds,
            speakers,
        })
    }

    fn advance(
        &mut self,
        step_index: u64,
        time_ns: u64,
        inputs: BackendInput,
    ) -> Result<BackendOutput> {
        for (motor, command) in self.motors.iter().zip(inputs.motors) {
            if let Some(command) = command {
                motor.apply(&command)?;
            }
        }
        for (led, command) in self.leds.iter().zip(inputs.leds) {
            if let Some(command) = command {
                led.apply(&command)?;
            }
        }
        // Audio chunks move rather than copy: a stream is large, and nothing
        // downstream needs them again.
        for (speaker, chunks) in self.speakers.iter_mut().zip(inputs.speakers) {
            for chunk in chunks {
                speaker.apply(chunk)?;
            }
        }

        if !self
            .webots
            .step(self.step_ms)
            .map_err(|error| anyhow!(error))?
        {
            bail!("Webots requested controller shutdown");
        }

        Ok(BackendOutput {
            // The encoder is the one sensor that still needs the instant: it
            // differentiates position over the step to report velocity.
            encoders: self
                .encoders
                .iter_mut()
                .map(|sensor| sensor.read_if_due(step_index, time_ns))
                .collect::<Result<_>>()?,
            imus: self
                .imus
                .iter()
                .map(|sensor| sensor.read_if_due(step_index))
                .collect::<Result<_>>()?,
            accelerometers: self
                .accelerometers
                .iter()
                .map(|sensor| sensor.read_if_due(step_index))
                .collect::<Result<_>>()?,
            gyroscopes: self
                .gyroscopes
                .iter()
                .map(|sensor| sensor.read_if_due(step_index))
                .collect::<Result<_>>()?,
            ranges: self
                .ranges
                .iter()
                .map(|sensor| sensor.read_if_due(step_index))
                .collect::<Result<_>>()?,
            cameras: self
                .cameras
                .iter()
                .map(|sensor| sensor.read_if_due(step_index))
                .collect::<Result<_>>()?,
            depths: self
                .depths
                .iter()
                .map(|sensor| sensor.read_if_due(step_index))
                .collect::<Result<_>>()?,
            gnss: self
                .gnss
                .iter()
                .map(|sensor| sensor.read_if_due(step_index))
                .collect::<Result<_>>()?,
            magnetometers: self
                .magnetometers
                .iter()
                .map(|sensor| sensor.read_if_due(step_index))
                .collect::<Result<_>>()?,
            lidars: self
                .lidars
                .iter()
                .map(|sensor| sensor.read_if_due(step_index))
                .collect::<Result<_>>()?,
            mmwaves: self
                .mmwaves
                .iter()
                .map(|sensor| sensor.read_if_due(step_index))
                .collect::<Result<_>>()?,
            microphones: self
                .microphones
                .iter()
                .map(|sensor| sensor.read_if_due(step_index))
                .collect::<Result<_>>()?,
            // The battery differentiates energy over the step to report
            // current, so like the encoder it needs the instant.
            batteries: self
                .batteries
                .iter_mut()
                .map(|sensor| sensor.read_if_due(step_index, time_ns))
                .collect::<Result<_>>()?,
        })
    }

    fn silence_speakers(&self) -> Result<()> {
        let mut first_error = None;
        for speaker in &self.speakers {
            if let Err(error) = speaker.stop() {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn apply_motor_command(
        &mut self,
        spec: &MotorSpec,
        command: &api::component::motor::Command,
    ) -> Result<()> {
        let Some(index) = self
            .motors
            .iter()
            .position(|motor| motor.reference == spec.reference)
        else {
            return Ok(());
        };
        self.motors[index].apply(command)
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug)]
struct OutputCounts {
    encoders: usize,
    imus: usize,
    accelerometers: usize,
    gyroscopes: usize,
    ranges: usize,
    cameras: usize,
    depths: usize,
    gnss: usize,
    magnetometers: usize,
    lidars: usize,
    mmwaves: usize,
    microphones: usize,
    batteries: usize,
}

#[cfg(test)]
impl OutputCounts {
    fn from_catalog(catalog: &CapabilityCatalog) -> Self {
        Self {
            encoders: catalog.encoders.len(),
            imus: catalog.imus.len(),
            accelerometers: catalog.accelerometers.len(),
            gyroscopes: catalog.gyroscopes.len(),
            ranges: catalog.ranges.len(),
            cameras: catalog.cameras.len(),
            depths: catalog.depths.len(),
            gnss: catalog.gnss.len(),
            magnetometers: catalog.magnetometers.len(),
            lidars: catalog.lidars.len(),
            mmwaves: catalog.mmwaves.len(),
            microphones: catalog.microphones.len(),
            batteries: catalog.batteries.len(),
        }
    }
}

struct BackendOutput {
    encoders: Vec<Option<api::component::encoder::Sample>>,
    imus: Vec<Option<api::component::imu::Sample>>,
    accelerometers: Vec<Option<api::component::accelerometer::Sample>>,
    gyroscopes: Vec<Option<api::component::gyroscope::Sample>>,
    ranges: Vec<Option<api::component::range::Sample>>,
    cameras: Vec<Option<api::component::camera::Frame>>,
    depths: Vec<Option<api::component::depth::Frame>>,
    gnss: Vec<Option<api::component::gnss::Sample>>,
    magnetometers: Vec<Option<api::component::magnetometer::Sample>>,
    lidars: Vec<Option<api::component::lidar::Scan>>,
    mmwaves: Vec<Option<api::component::mmwave::Scan>>,
    microphones: Vec<Option<api::component::microphone::Frame>>,
    batteries: Vec<Option<api::component::battery::State>>,
}

#[cfg(test)]
impl BackendOutput {
    fn empty(counts: &OutputCounts) -> Self {
        Self {
            encoders: none_vec(counts.encoders),
            imus: none_vec(counts.imus),
            accelerometers: none_vec(counts.accelerometers),
            gyroscopes: none_vec(counts.gyroscopes),
            ranges: none_vec(counts.ranges),
            cameras: none_vec(counts.cameras),
            depths: none_vec(counts.depths),
            gnss: none_vec(counts.gnss),
            magnetometers: none_vec(counts.magnetometers),
            lidars: none_vec(counts.lidars),
            mmwaves: none_vec(counts.mmwaves),
            microphones: none_vec(counts.microphones),
            batteries: none_vec(counts.batteries),
        }
    }
}

#[cfg(test)]
fn none_vec<T>(len: usize) -> Vec<Option<T>> {
    std::iter::repeat_with(|| None).take(len).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoxal::participant::{Participant, ParticipantSpec};
    use phoxal::raw::{Bus, BusConfig, MeasurementPublisher, Subscriber};
    use std::time::Duration;

    #[test]
    fn identity_and_class_are_reported() {
        assert_eq!(
            <WebotsControllerSimulator as ParticipantSpec>::ID,
            "webots-controller"
        );
        assert_eq!(
            <WebotsControllerSimulator as ParticipantSpec>::KIND,
            "simulator"
        );
        assert_eq!(
            <WebotsControllerSimulator as ParticipantSpec>::PARTICIPANT_CLASS,
            "checked"
        );
        assert!(
            <WebotsControllerSimulator as Participant>::__step_schedule().is_none(),
            "the controller must not wrap Webots in a framework step loop"
        );
    }

    #[test]
    fn controller_rejects_cli_producer_overrides() {
        assert!(has_explicit_producer_arg([
            OsString::from("webots-controller"),
            OsString::from("--producer-id"),
            OsString::from("00112233445566778899aabbccddeeff"),
        ]));
        assert!(has_explicit_producer_arg([
            OsString::from("webots-controller"),
            OsString::from("--producer-id=00112233445566778899aabbccddeeff"),
        ]));
        assert!(!has_explicit_producer_arg([
            OsString::from("webots-controller"),
            OsString::from("--robot-id"),
            OsString::from("rover"),
        ]));
    }

    #[test]
    fn webots_step_duration_must_be_positive() {
        assert_eq!(step_ns_from_ms(10).expect("10 ms is valid"), 10_000_000);
        assert!(step_ns_from_ms(0).is_err());
        assert!(step_ns_from_ms(-1).is_err());
    }

    fn test_namespace(label: &str) -> String {
        format!(
            "test/webots-controller-{label}/{}/{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| since.as_nanos())
                .unwrap_or_default()
        )
    }

    fn clock_publisher(bus: &Bus) -> WorldClockPublisher<api::simulation::Clock> {
        WorldClockPublisher::__mint(bus.clone(), &api::topic::owner().simulation().clock())
            .expect("clock publisher should attach")
    }

    fn empty_api(clock: WorldClockPublisher<api::simulation::Clock>) -> Api {
        Api {
            clock,
            motor_commands: Vec::new(),
            encoders: Vec::new(),
            imus: Vec::new(),
            accelerometers: Vec::new(),
            gyroscopes: Vec::new(),
            ranges: Vec::new(),
            cameras: Vec::new(),
            depths: Vec::new(),
            gnss: Vec::new(),
            magnetometers: Vec::new(),
            lidars: Vec::new(),
            mmwaves: Vec::new(),
            microphones: Vec::new(),
            batteries: Vec::new(),
            led_commands: Vec::new(),
            speaker_streams: Vec::new(),
        }
    }

    // One process may mint exactly one timeline authority, so the controller's
    // step behaviour is covered by this single test rather than one per
    // assertion.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn controller_publishes_outputs_before_matching_clock() {
        let bus = Bus::open(BusConfig::in_process(test_namespace("order"), "robot"))
            .await
            .expect("bus should open");
        let clock_subscriber = Subscriber::<api::simulation::Clock>::new(
            &bus,
            &api::topic::client().simulation().clock(),
            1,
        )
        .await
        .expect("clock subscriber should attach");
        let encoder_subscriber = Subscriber::<api::component::encoder::Sample>::new(
            &bus,
            &api::topic::client()
                .component("left_drive")
                .encoder("encoder")
                .sample(),
            1,
        )
        .await
        .expect("encoder subscriber should attach");
        let api = Api {
            encoders: vec![
                MeasurementPublisher::new(
                    bus.clone(),
                    &api::topic::owner()
                        .component("left_drive")
                        .encoder("encoder")
                        .sample(),
                )
                .expect("encoder publisher should attach"),
            ],
            ..empty_api(clock_publisher(&bus))
        };
        let timeline = TimelineId::from_raw(77).expect("test timeline must be nonzero");
        let mut runtime = ControllerRuntime {
            authority: TimelineAuthority::__mint(timeline).expect("authority should mint"),
            step_index: 0,
            backend: Arc::new(Mutex::new(BackendControl {
                backend: Backend::Stub(StubBackend::new(&CapabilityCatalog::default())),
                motor_specs: Vec::new(),
            })),
        };

        // A stub step produces no sensor output, so it only proves the clock
        // the controller reached.
        runtime
            .step_once(&api)
            .await
            .expect("stub step should complete");
        let clock = tokio::time::timeout(Duration::from_secs(2), clock_subscriber.recv())
            .await
            .expect("clock should arrive")
            .expect("clock should decode");
        assert_eq!(
            clock.metadata.produced_exactly_at(),
            Some(RobotInstant::new(timeline, 10_000_000))
        );
        assert_eq!(clock.body.step, 1);
        assert_eq!(runtime.step_index, 1);

        // A step that did produce sensor output commits it ahead of that step's
        // clock.
        let world_step = runtime.authority.completed_step(20_000_000);
        let mut outputs =
            BackendOutput::empty(&OutputCounts::from_catalog(&CapabilityCatalog::default()));
        outputs.encoders = vec![Some(api::component::encoder::Sample {
            position_rad: 1.0,
            velocity_radps: 0.5,
        })];
        commit_step(&api, &world_step, 2, outputs).expect("commit should publish");

        let encoder = tokio::time::timeout(Duration::from_secs(2), encoder_subscriber.recv())
            .await
            .expect("encoder output should arrive")
            .expect("encoder output should decode");
        let clock = tokio::time::timeout(Duration::from_secs(2), clock_subscriber.recv())
            .await
            .expect("clock should arrive")
            .expect("clock should decode");

        // Every output of one completed world step shares that step's exact
        // instant, and it rides in the envelope rather than in any body.
        let expected = RobotInstant::new(timeline, 20_000_000);
        assert_eq!(encoder.metadata.produced_exactly_at(), Some(expected));
        assert_eq!(clock.metadata.produced_exactly_at(), Some(expected));
        assert_eq!(clock.body.step, 2);
        assert!(
            encoder.metadata.sequence < clock.metadata.sequence,
            "all completed-world outputs must enqueue before the matching clock"
        );
        bus.close().await.expect("bus should close");
    }
}
