//! Webots controller simulator artifact.
//!
//! Binds one Webots-owned controller process to
//! a robot's component capabilities (motor, encoder, IMU, accelerometer,
//! gyroscope, range, camera, depth, GNSS) and publishes/subscribes exactly the
//! `component::*` contracts those capabilities need. The process bootstraps the
//! normal framework runner, mints one opaque nonzero epoch, and runs only the
//! external Webots step loop. Each step publishes all component outputs and
//! then the matching `simulation::Clock`.

mod capabilities;

use phoxal::api;
use phoxal::bus::ContractBody;
use phoxal::model::component::v0::CapabilityRef;
use phoxal::model::simulation::Simulation as SimulationFile;
use phoxal::model::simulation::v0::Simulation as SimulationSpec;
use phoxal::model::v0::Robot;
use phoxal::prelude::*;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};

use capabilities::accelerometer::{AccelerometerSpec, NativeAccelerometer};
use capabilities::camera::{CameraSpec, NativeCamera};
use capabilities::depth::{DepthSpec, NativeDepth};
use capabilities::encoder::{EncoderSpec, NativeEncoder};
use capabilities::gnss::{GnssSpec, NativeGnss};
use capabilities::gyroscope::{GyroscopeSpec, NativeGyroscope};
use capabilities::imu::{ImuSpec, NativeImu};
use capabilities::motor::{MotorSpec, NativeMotor};
use capabilities::range::{NativeRange, RangeSpec};

const STEP_HZ: f64 = 100.0;
const COMPONENTS_DIR: &str = "components";
const SIMULATION_FILE: &str = "simulation.yaml";
const BATTERY_PUBLISH_NS: u64 = 1_000_000_000;
const FULL_VOLTAGE_V: f64 = 16.8;
const EMPTY_VOLTAGE_V: f64 = 12.0;
const DRAW_CURRENT_A: f64 = 2.0;
const CAPACITY_AH: f64 = 10.0;

#[derive(Clone, Debug, serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
pub struct WebotsControllerConfig {
    #[serde(default = "default_require_native")]
    require_native: bool,
    /// Deterministic test/simulation input for every bound component e-stop.
    #[serde(default)]
    emergency_stop_engaged: bool,
}

impl Default for WebotsControllerConfig {
    fn default() -> Self {
        Self {
            require_native: default_require_native(),
            emergency_stop_engaged: false,
        }
    }
}

fn default_require_native() -> bool {
    true
}

#[derive(phoxal::Api)]
pub struct Api {
    clock: Publisher<api::simulation::Clock>,
    motor_commands: Vec<Subscriber<api::component::motor::Command>>,
    encoders: Vec<Publisher<api::component::encoder::Sample>>,
    imus: Vec<Publisher<api::component::imu::Sample>>,
    accelerometers: Vec<Publisher<api::component::accelerometer::Sample>>,
    gyroscopes: Vec<Publisher<api::component::gyroscope::Sample>>,
    ranges: Vec<Publisher<api::component::range::Sample>>,
    cameras: Vec<Publisher<api::component::camera::Frame>>,
    depths: Vec<Publisher<api::component::depth::Frame>>,
    gnss: Vec<Publisher<api::component::gnss::Sample>>,
    emergency_stops: Vec<Publisher<api::component::emergency_stop::State>>,
    battery: Publisher<api::battery::State>,
}

#[phoxal::simulator(id = "webots-controller", config = Option<WebotsControllerConfig>)]
pub struct WebotsControllerSimulator {
    backend: SharedBackend,
}

struct ControllerRuntime {
    epoch: u64,
    step_index: u64,
    backend: SharedBackend,
    emergency_stop_states: Vec<bool>,
    battery_charge_ratio: f64,
    last_battery_update: Option<LogicalTime>,
    last_battery_publish: Option<LogicalTime>,
}

type SharedBackend = Arc<Mutex<BackendControl>>;

struct BackendControl {
    backend: Backend,
    motor_specs: Vec<MotorSpec>,
}

struct BlockingStep {
    step_ns: u64,
    now_ns: u64,
    outputs: BackendOutput,
    is_stub: bool,
}

trait EpochSource {
    fn next_candidate(&mut self) -> Result<u64>;
}

struct SystemEpochSource;

impl EpochSource for SystemEpochSource {
    fn next_candidate(&mut self) -> Result<u64> {
        let mut bytes = [0u8; std::mem::size_of::<u64>()];
        getrandom::fill(&mut bytes).context("failed to generate simulation epoch")?;
        Ok(u64::from_ne_bytes(bytes))
    }
}

fn mint_epoch(source: &mut impl EpochSource) -> Result<u64> {
    loop {
        let epoch = source.next_candidate()?;
        if epoch != 0 {
            return Ok(epoch);
        }
    }
}

/// Bootstrap the Webots-owned controller with a process-local nonzero
/// incarnation. Stable project arguments may configure routing/model inputs,
/// but lifecycle identity is never supplied by the CLI.
pub fn run() -> Result<()> {
    if has_explicit_incarnation_arg(std::env::args_os()) {
        bail!(
            "--incarnation is not accepted by the Webots-owned controller; it mints its own process identity"
        );
    }
    let incarnation = mint_epoch(&mut SystemEpochSource)?;
    // SAFETY: this is the binary's single-threaded entrypoint, before the
    // framework constructs its Tokio runtime or any other thread.
    unsafe {
        std::env::set_var(
            phoxal::participant::launch::env::INCARNATION,
            incarnation.to_string(),
        );
    }
    phoxal::run::<WebotsControllerSimulator>()
}

fn has_explicit_incarnation_arg(args: impl IntoIterator<Item = OsString>) -> bool {
    args.into_iter().any(|arg| {
        arg == "--incarnation"
            || arg
                .to_str()
                .is_some_and(|arg| arg.starts_with("--incarnation="))
    })
}

#[phoxal::behavior]
impl WebotsControllerSimulator {
    #[setup]
    async fn setup(
        ctx: &mut SetupContext<Self>,
        config: Option<WebotsControllerConfig>,
    ) -> Result<(Self, Self::Api)> {
        let config = config.unwrap_or_default();
        let cap = ctx.owner_capability();
        let robot = ctx.robot()?;
        let root = ctx.robot_root()?;
        let catalog = CapabilityCatalog::from_robot(root, robot)?;
        let clock = ctx
            .publisher(api::topic::internal::new(cap).simulation().clock())
            .await?;
        let battery = ctx
            .publisher(api::topic::internal::new(cap).battery().state())
            .await?;

        let mut motor_commands = Vec::new();
        for spec in &catalog.motors {
            motor_commands.push(
                ctx.subscriber(
                    api::topic::internal::new(cap)
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
                ctx.publisher(
                    api::topic::internal::new(cap)
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
                ctx.publisher(
                    api::topic::internal::new(cap)
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
                ctx.publisher(
                    api::topic::internal::new(cap)
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
                ctx.publisher(
                    api::topic::internal::new(cap)
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
                ctx.publisher(
                    api::topic::internal::new(cap)
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
                ctx.publisher(
                    api::topic::internal::new(cap)
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
                ctx.publisher(
                    api::topic::internal::new(cap)
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
                ctx.publisher(
                    api::topic::internal::new(cap)
                        .component(&spec.sampled.reference.component_id)
                        .gnss(&spec.sampled.reference.capability_id)
                        .sample(),
                )
                .await?,
            );
        }

        let mut emergency_stops = Vec::new();
        for spec in &catalog.emergency_stops {
            emergency_stops.push(
                ctx.publisher(
                    api::topic::internal::new(cap)
                        .component(&spec.reference.component_id)
                        .emergency_stop(&spec.reference.capability_id)
                        .state(),
                )
                .await?,
            );
        }

        let backend = Arc::new(Mutex::new(BackendControl {
            backend: Backend::open(&config, &catalog)?,
            motor_specs: catalog.motors.clone(),
        }));
        let emergency_stop_states = catalog
            .emergency_stops
            .iter()
            .map(|spec| config.emergency_stop_engaged || spec.engaged)
            .collect();
        tracing::info!(
            target: "simulator_webots_controller",
            webots_runtime_linked = webots_rs::WEBOTS_RUNTIME_LINKED,
            motors = catalog.motors.len(),
            encoders = catalog.encoders.len(),
            imus = catalog.imus.len(),
            accelerometers = catalog.accelerometers.len(),
            gyroscopes = catalog.gyroscopes.len(),
            ranges = catalog.ranges.len(),
            cameras = catalog.cameras.len(),
            depths = catalog.depths.len(),
            gnss = catalog.gnss.len(),
            "webots controller simulator ready"
        );

        let api = Self::Api {
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
            emergency_stops,
            battery,
        };
        let mut epoch_source = SystemEpochSource;
        let runtime = ControllerRuntime {
            epoch: mint_epoch(&mut epoch_source)?,
            step_index: 0,
            backend: Arc::clone(&backend),
            emergency_stop_states,
            battery_charge_ratio: 1.0,
            last_battery_update: None,
            last_battery_publish: None,
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

        Ok((Self { backend }, api))
    }

    #[shutdown]
    async fn shutdown(&mut self, api: &mut Self::Api, _ctx: ShutdownContext) -> Result<()> {
        for subscriber in &api.motor_commands {
            let _latest = drain_latest(subscriber);
        }
        park_backend(Arc::clone(&self.backend)).await
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
        let commands = self.latest_motor_commands(api);
        let next_step = self.step_index.saturating_add(1);
        let step_index = self.step_index;
        let backend = Arc::clone(&self.backend);
        let step = tokio::task::spawn_blocking(move || {
            let mut control = lock_backend(&backend)?;
            let step_ns = control.backend.step_ns()?;
            let now_ns = next_step.saturating_mul(step_ns);
            let outputs = control.backend.advance(step_index, now_ns, &commands)?;
            Ok::<_, anyhow::Error>(BlockingStep {
                step_ns,
                now_ns,
                outputs,
                is_stub: control.backend.is_stub(),
            })
        })
        .await
        .context("Webots step worker failed to join")??;

        let at = LogicalTime::new(self.epoch, step.now_ns);
        self.publish_outputs(api, at, step.outputs).await?;
        api.clock
            .publish_at(
                at,
                api::simulation::Clock {
                    epoch: self.epoch,
                    now_ns: step.now_ns,
                    step: next_step,
                },
            )
            .await?;
        self.step_index = next_step;
        tracing::trace!(
            target: "simulator_webots_controller",
            epoch = self.epoch,
            step = self.step_index,
            time_ns = step.now_ns,
            "external Webots step committed"
        );
        if step.is_stub {
            tokio::time::sleep(Duration::from_nanos(step.step_ns)).await;
        }
        Ok(())
    }

    fn latest_motor_commands(&self, api: &Api) -> Vec<Option<api::component::motor::Command>> {
        api.motor_commands.iter().map(drain_latest).collect()
    }

    async fn publish_outputs(
        &mut self,
        api: &Api,
        at: LogicalTime,
        outputs: BackendOutput,
    ) -> Result<()> {
        for (publisher, sample) in api.encoders.iter().zip(outputs.encoders) {
            if let Some(sample) = sample {
                publisher.publish_at(at, sample).await?;
            }
        }
        for (publisher, sample) in api.imus.iter().zip(outputs.imus) {
            if let Some(sample) = sample {
                publisher.publish_at(at, sample).await?;
            }
        }
        for (publisher, sample) in api.accelerometers.iter().zip(outputs.accelerometers) {
            if let Some(sample) = sample {
                publisher.publish_at(at, sample).await?;
            }
        }
        for (publisher, sample) in api.gyroscopes.iter().zip(outputs.gyroscopes) {
            if let Some(sample) = sample {
                publisher.publish_at(at, sample).await?;
            }
        }
        for (publisher, sample) in api.ranges.iter().zip(outputs.ranges) {
            if let Some(sample) = sample {
                publisher.publish_at(at, sample).await?;
            }
        }
        for (publisher, frame) in api.cameras.iter().zip(outputs.cameras) {
            if let Some(frame) = frame {
                publisher.publish_at(at, frame).await?;
            }
        }
        for (publisher, frame) in api.depths.iter().zip(outputs.depths) {
            if let Some(frame) = frame {
                publisher.publish_at(at, frame).await?;
            }
        }
        for (publisher, sample) in api.gnss.iter().zip(outputs.gnss) {
            if let Some(sample) = sample {
                publisher.publish_at(at, sample).await?;
            }
        }
        for (publisher, engaged) in api.emergency_stops.iter().zip(&self.emergency_stop_states) {
            publisher
                .publish_at(
                    at,
                    api::component::emergency_stop::State { engaged: *engaged },
                )
                .await?;
        }
        if let Some(state) = self.battery_state(at) {
            api.battery.publish_at(at, state).await?;
        }
        Ok(())
    }

    fn battery_state(&mut self, at: LogicalTime) -> Option<api::battery::State> {
        if self
            .last_battery_update
            .is_some_and(|previous| previous.epoch() != at.epoch())
        {
            self.battery_charge_ratio = 1.0;
            self.last_battery_publish = None;
        }
        if let Some(previous) = self.last_battery_update
            && at.epoch() == previous.epoch()
            && at.time_ns() >= previous.time_ns()
        {
            self.battery_charge_ratio = discharge(
                self.battery_charge_ratio,
                DRAW_CURRENT_A,
                (at.time_ns() - previous.time_ns()) as f64 / 1_000_000_000.0,
                CAPACITY_AH,
            );
        }
        self.last_battery_update = Some(at);

        let due = self.last_battery_publish.is_none_or(|previous| {
            previous.epoch() != at.epoch()
                || at.time_ns().saturating_sub(previous.time_ns()) >= BATTERY_PUBLISH_NS
        });
        if !due {
            return None;
        }
        self.last_battery_publish = Some(at);
        Some(api::battery::State {
            voltage_v: voltage_for(self.battery_charge_ratio, EMPTY_VOLTAGE_V, FULL_VOLTAGE_V)
                as f32,
            current_a: if self.battery_charge_ratio > 0.0 {
                DRAW_CURRENT_A as f32
            } else {
                0.0
            },
            charge_ratio: self.battery_charge_ratio as f32,
        })
    }
}

fn discharge(ratio: f64, current_a: f64, dt_s: f64, capacity_ah: f64) -> f64 {
    if ratio <= 0.0 || current_a <= 0.0 || dt_s <= 0.0 || capacity_ah <= 0.0 {
        return ratio.clamp(0.0, 1.0);
    }
    (ratio.clamp(0.0, 1.0) - current_a * (dt_s / 3_600.0) / capacity_ah).clamp(0.0, 1.0)
}

fn voltage_for(ratio: f64, empty_v: f64, full_v: f64) -> f64 {
    empty_v + (full_v - empty_v) * ratio.clamp(0.0, 1.0)
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
    tokio::task::spawn_blocking(move || lock_backend(&backend)?.park_motors())
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
    emergency_stops: Vec<EmergencyStopSpec>,
}

#[derive(Clone, Debug)]
struct EmergencyStopSpec {
    reference: CapabilityRef,
    engaged: bool,
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
                    Capability::Lidar(_)
                    | Capability::Mmwave(_)
                    | Capability::Magnetometer(_)
                    | Capability::Microphone(_)
                    | Capability::Speaker(_)
                    | Capability::Battery(_)
                    | Capability::Led(_) => {
                        tracing::debug!(
                            target: "simulator_webots_controller",
                            capability = %reference,
                            kind = capability.kind_name(),
                            "capability is intentionally left for a later Webots port slice"
                        );
                    }
                    Capability::EmergencyStop(_) => {
                        let engaged = match simulation_capability {
                            Some(
                                phoxal::model::simulation::capability::Capability::EmergencyStop(
                                    config,
                                ),
                            ) => config.engaged,
                            _ => false,
                        };
                        catalog
                            .emergency_stops
                            .push(EmergencyStopSpec { reference, engaged });
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
        | SimCapability::EmergencyStop(_)
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

/// Webots backend selection: native `webots-rs` linkage when the runtime is
/// present, or a metadata-only stub otherwise (headless `cargo test`/CI).
enum Backend {
    Native(Box<NativeBackend>),
    Stub(StubBackend),
}

impl BackendControl {
    fn park_motors(&mut self) -> Result<()> {
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
        first_error.map_or(Ok(()), Err)
    }
}

impl Backend {
    fn open(config: &WebotsControllerConfig, catalog: &CapabilityCatalog) -> Result<Self> {
        if webots_rs::WEBOTS_RUNTIME_LINKED {
            return Ok(Self::Native(Box::new(NativeBackend::new(catalog)?)));
        }

        if config.require_native {
            bail!(
                "Webots runtime is not linked into this build. Rebuild with WEBOTS_HOME pointing at a Webots installation, or set PHOXAL_CONFIG require_native=false for metadata-only contract tests."
            );
        }

        Ok(Self::Stub(StubBackend::new(catalog)))
    }

    fn advance(
        &mut self,
        step_index: u64,
        time_ns: u64,
        commands: &[Option<api::component::motor::Command>],
    ) -> Result<BackendOutput> {
        match self {
            Self::Native(backend) => backend.advance(step_index, time_ns, commands),
            Self::Stub(backend) => backend.advance(step_index, time_ns, commands),
        }
    }

    fn step_ns(&self) -> Result<u64> {
        match self {
            Self::Native(backend) => step_ns_from_ms(backend.step_ms),
            Self::Stub(_) => Ok(10_000_000),
        }
    }

    fn is_stub(&self) -> bool {
        matches!(self, Self::Stub(_))
    }

    fn apply_motor_command(
        &mut self,
        spec: &MotorSpec,
        command: &api::component::motor::Command,
    ) -> Result<()> {
        match self {
            Self::Native(backend) => backend.apply_motor_command(spec, command),
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

struct StubBackend {
    counts: OutputCounts,
}

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
        _commands: &[Option<api::component::motor::Command>],
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
        })
    }

    fn advance(
        &mut self,
        step_index: u64,
        time_ns: u64,
        commands: &[Option<api::component::motor::Command>],
    ) -> Result<BackendOutput> {
        for (motor, command) in self.motors.iter().zip(commands) {
            if let Some(command) = command {
                motor.apply(command)?;
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
            encoders: self
                .encoders
                .iter_mut()
                .map(|sensor| sensor.read_if_due(step_index, time_ns))
                .collect::<Result<_>>()?,
            imus: self
                .imus
                .iter()
                .map(|sensor| sensor.read_if_due(step_index, time_ns))
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
                .map(|sensor| sensor.read_if_due(step_index, time_ns))
                .collect::<Result<_>>()?,
            cameras: self
                .cameras
                .iter()
                .map(|sensor| sensor.read_if_due(step_index, time_ns))
                .collect::<Result<_>>()?,
            depths: self
                .depths
                .iter()
                .map(|sensor| sensor.read_if_due(step_index, time_ns))
                .collect::<Result<_>>()?,
            gnss: self
                .gnss
                .iter()
                .map(|sensor| sensor.read_if_due(step_index))
                .collect::<Result<_>>()?,
        })
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
}

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
}

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
        }
    }
}

fn none_vec<T>(len: usize) -> Vec<Option<T>> {
    std::iter::repeat_with(|| None).take(len).collect()
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ContractMapping {
    topic: &'static str,
    role: phoxal::participant::ContractRole,
}

#[cfg(test)]
fn contract_mappings() -> Vec<ContractMapping> {
    use phoxal::participant::ContractRole;
    vec![
        mapping::<api::simulation::Clock>(ContractRole::Publish),
        mapping::<api::component::motor::Command>(ContractRole::Subscribe),
        mapping::<api::component::encoder::Sample>(ContractRole::Publish),
        mapping::<api::component::imu::Sample>(ContractRole::Publish),
        mapping::<api::component::accelerometer::Sample>(ContractRole::Publish),
        mapping::<api::component::gyroscope::Sample>(ContractRole::Publish),
        mapping::<api::component::range::Sample>(ContractRole::Publish),
        mapping::<api::component::camera::Frame>(ContractRole::Publish),
        mapping::<api::component::depth::Frame>(ContractRole::Publish),
        mapping::<api::component::gnss::Sample>(ContractRole::Publish),
        mapping::<api::component::emergency_stop::State>(ContractRole::Publish),
        mapping::<api::battery::State>(ContractRole::Publish),
    ]
}

#[cfg(test)]
fn mapping<B: ContractBody>(role: phoxal::participant::ContractRole) -> ContractMapping {
    ContractMapping {
        topic: B::TOPIC,
        role,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoxal::participant::{Participant, ParticipantApi, ParticipantLifecycle};
    use phoxal::raw::{Bus, BusConfig, OwnerCap, Publisher, Subscriber};
    use std::collections::VecDeque;
    use std::time::Duration;

    struct SequenceEpochSource {
        candidates: VecDeque<u64>,
    }

    impl EpochSource for SequenceEpochSource {
        fn next_candidate(&mut self) -> Result<u64> {
            self.candidates
                .pop_front()
                .context("test epoch sequence exhausted")
        }
    }

    #[test]
    fn api_declares_clock_component_and_battery_contracts() {
        assert_eq!(
            <WebotsControllerSimulator as Participant>::ID,
            "webots-controller"
        );
        assert_eq!(
            <WebotsControllerSimulator as Participant>::KIND,
            "simulator"
        );
        assert_eq!(
            <WebotsControllerSimulator as Participant>::PARTICIPANT_CLASS,
            "checked"
        );

        let contracts =
            <<WebotsControllerSimulator as Participant>::Api as ParticipantApi>::CONTRACTS;

        let expected = contract_mappings();
        assert_eq!(
            contracts.len(),
            expected.len(),
            "controller must declare one clock output, 9 component contracts, and battery state, got {contracts:?}"
        );
        for mapping in &expected {
            assert!(
                contracts
                    .iter()
                    .any(|c| c.topic == mapping.topic && c.role == mapping.role),
                "missing mapping for {} ({:?})",
                mapping.topic,
                mapping.role
            );
        }

        // The controller owns the only simulation clock in this process.
        assert!(
            contracts
                .iter()
                .filter(|c| c.topic.contains("/simulation/"))
                .all(|c| {
                    c.topic == api::simulation::Clock::TOPIC
                        && c.role == phoxal::participant::ContractRole::Publish
                }),
            "controller may only publish simulation/clock: {contracts:?}"
        );
        assert!(
            <WebotsControllerSimulator as ParticipantLifecycle>::__step_schedule().is_none(),
            "the controller must not wrap Webots in a framework #[step] loop"
        );
    }

    // The runner deserializes `Self::Config` from JSON `null` when
    // `PHOXAL_CONFIG` is unset (`ParticipantLaunch::config: Option<Value>` ->
    // `None` -> `serde_json::from_value(Value::Null)`); `Option<T>: ParticipantConfig`
    // (the blanket impl in `phoxal::participant::api`) is what makes that a
    // clean `None` -> `unwrap_or_default()` instead of a deserialize error.
    #[test]
    fn config_treats_null_as_absent_and_defaults() {
        let config: Option<WebotsControllerConfig> =
            serde_json::from_value(serde_json::Value::Null).unwrap();
        assert!(config.is_none());
        assert!(config.unwrap_or_default().require_native);
    }

    #[test]
    fn config_deserializes_an_object_as_present() {
        let config: Option<WebotsControllerConfig> =
            serde_json::from_value(serde_json::json!({"require_native": false})).unwrap();
        assert!(!config.unwrap().require_native);
    }

    #[test]
    fn epoch_minting_rejects_zero_and_keeps_process_and_simulation_identity_independent() {
        let mut source = SequenceEpochSource {
            candidates: VecDeque::from([41, 0, 52]),
        };

        let incarnation = mint_epoch(&mut source).expect("process incarnation should mint");
        let simulation_epoch = mint_epoch(&mut source).expect("simulation epoch should mint");
        assert_eq!(incarnation, 41);
        assert_eq!(simulation_epoch, 52);
        assert_ne!(incarnation, simulation_epoch);
    }

    #[test]
    fn controller_rejects_cli_incarnation_overrides() {
        assert!(has_explicit_incarnation_arg([
            OsString::from("webots-controller"),
            OsString::from("--incarnation"),
            OsString::from("42"),
        ]));
        assert!(has_explicit_incarnation_arg([
            OsString::from("webots-controller"),
            OsString::from("--incarnation=42"),
        ]));
        assert!(!has_explicit_incarnation_arg([
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn controller_publishes_outputs_before_matching_clock() {
        let namespace = format!(
            "test/webots-controller-order/{}/{}",
            std::process::id(),
            phoxal::raw::host_time().time_ns()
        );
        let bus = Bus::open(BusConfig::in_process(namespace, "robot"))
            .await
            .expect("bus should open");
        let cap = OwnerCap::__mint();
        let clock_subscriber = Subscriber::<api::simulation::Clock>::new(
            &bus,
            &api::topic::new().simulation().clock(),
            1,
        )
        .await
        .expect("clock subscriber should attach");
        let battery_subscriber =
            Subscriber::<api::battery::State>::new(&bus, &api::topic::new().battery().state(), 1)
                .await
                .expect("battery subscriber should attach");
        let api = Api {
            clock: Publisher::new(
                bus.clone(),
                &api::topic::internal::new(cap).simulation().clock(),
            )
            .expect("clock publisher should attach"),
            motor_commands: Vec::new(),
            encoders: Vec::new(),
            imus: Vec::new(),
            accelerometers: Vec::new(),
            gyroscopes: Vec::new(),
            ranges: Vec::new(),
            cameras: Vec::new(),
            depths: Vec::new(),
            gnss: Vec::new(),
            emergency_stops: Vec::new(),
            battery: Publisher::new(
                bus.clone(),
                &api::topic::internal::new(cap).battery().state(),
            )
            .expect("battery publisher should attach"),
        };
        let mut runtime = ControllerRuntime {
            epoch: 77,
            step_index: 0,
            backend: Arc::new(Mutex::new(BackendControl {
                backend: Backend::Stub(StubBackend::new(&CapabilityCatalog::default())),
                motor_specs: Vec::new(),
            })),
            emergency_stop_states: Vec::new(),
            battery_charge_ratio: 1.0,
            last_battery_update: None,
            last_battery_publish: None,
        };

        runtime
            .step_once(&api)
            .await
            .expect("stub step should complete");
        let battery = tokio::time::timeout(Duration::from_secs(2), battery_subscriber.recv())
            .await
            .expect("battery output should arrive")
            .expect("battery output should decode");
        let clock = tokio::time::timeout(Duration::from_secs(2), clock_subscriber.recv())
            .await
            .expect("clock should arrive")
            .expect("clock should decode");

        assert_eq!(battery.metadata.epoch, 77);
        assert_eq!(battery.metadata.produced_at_ns, 10_000_000);
        assert_eq!(clock.metadata.epoch, 77);
        assert_eq!(clock.metadata.produced_at_ns, 10_000_000);
        assert_eq!(clock.body.epoch, 77);
        assert_eq!(clock.body.now_ns, 10_000_000);
        assert_eq!(clock.body.step, 1);
        assert!(
            battery.metadata.source.sequence < clock.metadata.source.sequence,
            "all completed-world outputs must enqueue before the matching clock"
        );
        assert_eq!(runtime.step_index, 1);
        bus.close().await.expect("bus should close");
    }

    #[test]
    fn battery_model_discharges_and_clamps() {
        assert_eq!(discharge(0.5, 2.0, 0.0, 10.0), 0.5);
        assert!(discharge(1.0, 2.0, 3_600.0, 10.0) < 1.0);
        assert_eq!(discharge(0.01, 100.0, 3_600.0, 1.0), 0.0);
        assert_eq!(voltage_for(0.0, 12.0, 16.8), 12.0);
        assert_eq!(voltage_for(1.0, 12.0, 16.8), 16.8);
    }
}
