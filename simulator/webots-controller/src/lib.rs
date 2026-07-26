//! Webots controller simulator artifact.
//!
//! Binds one Webots-owned controller process to
//! a robot's component capabilities (motor, encoder, IMU, accelerometer,
//! gyroscope, range, camera, depth, GNSS) and publishes/subscribes exactly the
//! `component::*` contracts those capabilities need. The process bootstraps the
//! normal framework runner, mints one opaque timeline, and runs only the
//! external Webots step loop. Each step publishes all component outputs and
//! then the matching `simulation::Clock`.

mod capabilities;

use phoxal::api;
use phoxal::bus::ContractBody;
use phoxal::bus::{StepStamp, TimelineAuthority, TimelineId, WorldStepToken};
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

#[cfg(phoxal_require_native_webots)]
const _: () = assert!(
    webots_rs::WEBOTS_RUNTIME_LINKED,
    "release packaging requires a native Webots runtime"
);

#[derive(Clone, Debug, serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
pub struct WebotsControllerConfig {
    #[serde(default = "default_require_native")]
    require_native: bool,
}

impl Default for WebotsControllerConfig {
    fn default() -> Self {
        Self {
            require_native: default_require_native(),
        }
    }
}

fn default_require_native() -> bool {
    true
}

#[derive(phoxal::Api)]
pub struct Api {
    clock: StatePublisher<api::simulation::Clock>,
    motor_commands: Vec<Subscriber<api::component::motor::Command>>,
    encoders: Vec<MeasurementPublisher<api::component::encoder::Sample>>,
    imus: Vec<MeasurementPublisher<api::component::imu::Sample>>,
    accelerometers: Vec<MeasurementPublisher<api::component::accelerometer::Sample>>,
    gyroscopes: Vec<MeasurementPublisher<api::component::gyroscope::Sample>>,
    ranges: Vec<MeasurementPublisher<api::component::range::Sample>>,
    cameras: Vec<MeasurementPublisher<api::component::camera::Frame>>,
    depths: Vec<MeasurementPublisher<api::component::depth::Frame>>,
    gnss: Vec<MeasurementPublisher<api::component::gnss::Sample>>,
}

#[phoxal::simulator(id = "webots-controller", config = Option<WebotsControllerConfig>)]
pub struct WebotsControllerSimulator {
    backend: SharedBackend,
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
    step_ns: u64,
    now_ns: u64,
    outputs: BackendOutput,
    is_stub: bool,
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
            .state_publisher(api::topic::internal::new(cap).simulation().clock())
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
                ctx.measurement_publisher(
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
                ctx.measurement_publisher(
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
                ctx.measurement_publisher(
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
                ctx.measurement_publisher(
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
                ctx.measurement_publisher(
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
                ctx.measurement_publisher(
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
                ctx.measurement_publisher(
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
                ctx.measurement_publisher(
                    api::topic::internal::new(cap)
                        .component(&spec.sampled.reference.component_id)
                        .gnss(&spec.sampled.reference.capability_id)
                        .sample(),
                )
                .await?,
            );
        }

        let backend = Arc::new(Mutex::new(BackendControl {
            backend: Backend::open(&config, &catalog)?,
            motor_specs: catalog.motors.clone(),
        }));
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
        };

        let runtime = ControllerRuntime {
            authority: TimelineAuthority::__mint(TimelineId::mint())?,
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
        if step.is_stub {
            tokio::time::sleep(Duration::from_nanos(step.step_ns)).await;
        }
        Ok(())
    }

    fn latest_motor_commands(&self, api: &Api) -> Vec<Option<api::component::motor::Command>> {
        api.motor_commands.iter().map(drain_latest).collect()
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
    Ok(())
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
                    | Capability::EmergencyStop(_)
                    | Capability::Led(_) => {
                        tracing::debug!(
                            target: "simulator_webots_controller",
                            capability = %reference,
                            kind = capability.kind_name(),
                            "capability is intentionally left for a later Webots port slice"
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
    use phoxal::raw::{Bus, BusConfig, MeasurementPublisher, OwnerCap, StatePublisher, Subscriber};
    use std::time::Duration;

    #[test]
    fn api_declares_clock_and_component_contracts() {
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
            "controller must declare one clock output and 9 component contracts, got {contracts:?}"
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

    fn clock_publisher(bus: &Bus, cap: OwnerCap) -> StatePublisher<api::simulation::Clock> {
        StatePublisher::new(
            bus.clone(),
            &api::topic::internal::new(cap).simulation().clock(),
        )
        .expect("clock publisher should attach")
    }

    fn empty_api(clock: StatePublisher<api::simulation::Clock>) -> Api {
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
        let cap = OwnerCap::__mint();
        let clock_subscriber = Subscriber::<api::simulation::Clock>::new(
            &bus,
            &api::topic::new().simulation().clock(),
            1,
        )
        .await
        .expect("clock subscriber should attach");
        let encoder_subscriber = Subscriber::<api::component::encoder::Sample>::new(
            &bus,
            &api::topic::new()
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
                    &api::topic::internal::new(cap)
                        .component("left_drive")
                        .encoder("encoder")
                        .sample(),
                )
                .expect("encoder publisher should attach"),
            ],
            ..empty_api(clock_publisher(&bus, cap))
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
