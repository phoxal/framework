//! Webots controller simulator artifact.
//!
//! The per-robot substitution provider: binds one Webots controller process to
//! a robot's component capabilities (motor, encoder, IMU, accelerometer,
//! gyroscope, range, camera, depth, GNSS) and publishes/subscribes exactly the
//! `component::*` contracts those capabilities need. This binary never touches
//! the Webots Supervisor API and never publishes `simulation::*` topics - that
//! is `phoxal-simulator-webots-supervisor`'s job (see `simulator/webots-supervisor`).

mod capabilities;

use phoxal::bus::ContractBody;
use phoxal::model::component::v0::CapabilityRef;
use phoxal::model::robot::v0::ArtifactPin;
use phoxal::model::simulation::Simulation as SimulationFile;
use phoxal::model::simulation::v0::Simulation as SimulationSpec;
use phoxal::model::v0::Robot;
use phoxal::prelude::*;
use phoxal_api::y2026_1 as api;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
const DEFAULT_DT_NS: u64 = 10_000_000;
const COMPONENTS_DIR: &str = "components";
const SIMULATION_FILE: &str = "simulation.yaml";

#[derive(Clone, Debug, serde::Deserialize, phoxal::schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct WebotsControllerConfig {
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

#[derive(phoxal::Simulator)]
#[phoxal(id = "webots-controller", api = y2026_1, config = Option<WebotsControllerConfig>)]
struct WebotsControllerSimulator {
    step_index: u64,
    time_ns: u64,
    dt_ns: u64,
    backend: Backend,
    motor_commands: Vec<Subscriber<api::component::motor::Command>>,
    motor_specs: Vec<MotorSpec>,
    encoders: Vec<Publisher<api::component::encoder::Sample>>,
    imus: Vec<Publisher<api::component::imu::Sample>>,
    accelerometers: Vec<Publisher<api::component::accelerometer::Sample>>,
    gyroscopes: Vec<Publisher<api::component::gyroscope::Sample>>,
    ranges: Vec<Publisher<api::component::range::Sample>>,
    cameras: Vec<Publisher<api::component::camera::Frame>>,
    depths: Vec<Publisher<api::component::depth::Frame>>,
    gnss: Vec<Publisher<api::component::gnss::Sample>>,
}

#[phoxal::behavior]
impl WebotsControllerSimulator {
    #[setup]
    async fn setup(
        ctx: &mut SetupContext<Self>,
        config: Option<WebotsControllerConfig>,
    ) -> Result<Self> {
        let config = config.unwrap_or_default();
        let cap = ctx.owner_capability();
        let robot = ctx.robot()?;
        let root = ctx.robot_root()?;
        let catalog = CapabilityCatalog::from_robot(root, robot)?;

        let mut motor_commands = Vec::new();
        for spec in &catalog.motors {
            motor_commands.push(
                ctx.subscribe(
                    api::topic::internal::new(cap)
                        .component(&spec.reference.component_id)
                        .motor(&spec.reference.capability_id)
                        .command(),
                )
                .subscriber()
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

        let backend = Backend::open(&config, &catalog)?;
        let dt_ns = backend.dt_ns();

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

        Ok(Self {
            step_index: 0,
            time_ns: 0,
            dt_ns,
            backend,
            motor_commands,
            motor_specs: catalog.motors,
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

    #[step(hz = 100)]
    async fn step(&mut self, _step: StepContext) -> Result<()> {
        let now = LogicalTime::new(0, self.time_ns);
        let commands = self.latest_motor_commands();
        let outputs = self
            .backend
            .advance(self.step_index, self.time_ns, &commands)?;
        self.time_ns = self.time_ns.saturating_add(self.dt_ns);
        self.step_index = self.step_index.saturating_add(1);
        let at = LogicalTime::new(0, self.time_ns);
        self.publish_outputs(at, outputs).await?;
        tracing::trace!(target: "simulator_webots_controller", time_ns = now.time_ns(), "controller step complete");
        Ok(())
    }

    #[shutdown]
    async fn shutdown(&mut self, _ctx: ShutdownContext) -> Result<()> {
        for (subscriber, spec) in self.motor_commands.iter().zip(&self.motor_specs) {
            let _latest = drain_latest(subscriber);
            let stop = api::component::motor::Command::Stop;
            if let Err(error) = self.backend.apply_motor_command(spec, &stop) {
                tracing::warn!(
                    target: "simulator_webots_controller",
                    capability = %spec.reference,
                    error = %error,
                    "failed to park motor on shutdown"
                );
            }
        }
        Ok(())
    }
}

impl WebotsControllerSimulator {
    fn latest_motor_commands(&self) -> Vec<Option<api::component::motor::Command>> {
        self.motor_commands.iter().map(drain_latest).collect()
    }

    async fn publish_outputs(&self, at: LogicalTime, outputs: BackendOutput) -> Result<()> {
        for (publisher, sample) in self.encoders.iter().zip(outputs.encoders) {
            if let Some(sample) = sample {
                publisher.publish_at(at, sample).await?;
            }
        }
        for (publisher, sample) in self.imus.iter().zip(outputs.imus) {
            if let Some(sample) = sample {
                publisher.publish_at(at, sample).await?;
            }
        }
        for (publisher, sample) in self.accelerometers.iter().zip(outputs.accelerometers) {
            if let Some(sample) = sample {
                publisher.publish_at(at, sample).await?;
            }
        }
        for (publisher, sample) in self.gyroscopes.iter().zip(outputs.gyroscopes) {
            if let Some(sample) = sample {
                publisher.publish_at(at, sample).await?;
            }
        }
        for (publisher, sample) in self.ranges.iter().zip(outputs.ranges) {
            if let Some(sample) = sample {
                publisher.publish_at(at, sample).await?;
            }
        }
        for (publisher, frame) in self.cameras.iter().zip(outputs.cameras) {
            if let Some(frame) = frame {
                publisher.publish_at(at, frame).await?;
            }
        }
        for (publisher, frame) in self.depths.iter().zip(outputs.depths) {
            if let Some(frame) = frame {
                publisher.publish_at(at, frame).await?;
            }
        }
        for (publisher, sample) in self.gnss.iter().zip(outputs.gnss) {
            if let Some(sample) = sample {
                publisher.publish_at(at, sample).await?;
            }
        }
        Ok(())
    }
}

fn drain_latest<B: ContractBody>(subscriber: &Subscriber<B>) -> Option<B> {
    let mut latest = None;
    while let Some(received) = subscriber.try_recv() {
        latest = Some(received.body);
    }
    latest
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
                    | Capability::Led(_)
                    | Capability::EmergencyStop(_) => {
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
        | SimCapability::Speaker
        | SimCapability::Battery
        | SimCapability::Led => None,
    }
}

fn load_simulation_specs(root: &Path, robot: &Robot) -> Result<BTreeMap<String, SimulationSpec>> {
    let mut simulations = BTreeMap::new();
    for component_type in robot.manifest.used_component_types() {
        let path = component_simulation_path(root, &robot.manifest, component_type);
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

/// Resolves the on-disk directory for a used component type's simulation
/// config, mirroring `phoxal::model::v0::Robot`'s component-assets
/// resolution: the staged `<bundle_root>/components/<type>` directory by
/// default, overridden by an `artifacts.pins` path pin keyed by the
/// component's provider-qualified assets package id
/// (`phoxal/component-<type>-assets`) when that override actually contains a
/// `simulation.yaml`.
fn component_simulation_path(
    bundle_root: &Path,
    manifest: &phoxal::model::robot::v0::Robot,
    component_type: &str,
) -> PathBuf {
    let staged_path = bundle_root.join(COMPONENTS_DIR).join(component_type);
    let assets_pin_key = format!("phoxal/component-{component_type}-assets");
    let Some(ArtifactPin::Path(path_pin)) = manifest.artifacts.pins.get(&assets_pin_key) else {
        return staged_path;
    };

    let source_path = bundle_root.join(&path_pin.path);
    if source_path.join(SIMULATION_FILE).is_file() {
        source_path
    } else {
        staged_path
    }
}

/// Webots backend selection: native `webots-rs` linkage when the runtime is
/// present, or a metadata-only stub otherwise (headless `cargo test`/CI).
enum Backend {
    Native(Box<NativeBackend>),
    Stub(StubBackend),
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

    fn dt_ns(&self) -> u64 {
        match self {
            Self::Native(backend) => backend.dt_ns,
            Self::Stub(backend) => backend.dt_ns,
        }
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

struct StubBackend {
    dt_ns: u64,
    counts: OutputCounts,
}

impl StubBackend {
    fn new(catalog: &CapabilityCatalog) -> Self {
        Self {
            dt_ns: DEFAULT_DT_NS,
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

/// Owns the Webots controller-process handle: base handle open, per-step
/// `webots.step()`, and every component device wrapper. This crate never
/// opens a `webots_rs::Supervisor` - see `phoxal-simulator-webots-supervisor`
/// for the world/session authority half of the old monolith.
struct NativeBackend {
    webots: webots_rs::Webots,
    dt_ns: u64,
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
        let dt_ns = u64::try_from(std::time::Duration::from_millis(step_ms as u64).as_nanos())
            .unwrap_or(u64::MAX);

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
            dt_ns,
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

fn main() -> phoxal::Result<()> {
    phoxal::run::<WebotsControllerSimulator>()
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ContractMapping {
    family: &'static str,
    schema_id: &'static str,
}

#[cfg(test)]
fn contract_mappings() -> Vec<ContractMapping> {
    vec![
        mapping::<api::component::motor::Command>(),
        mapping::<api::component::encoder::Sample>(),
        mapping::<api::component::imu::Sample>(),
        mapping::<api::component::accelerometer::Sample>(),
        mapping::<api::component::gyroscope::Sample>(),
        mapping::<api::component::range::Sample>(),
        mapping::<api::component::camera::Frame>(),
        mapping::<api::component::depth::Frame>(),
        mapping::<api::component::gnss::Sample>(),
    ]
}

#[cfg(test)]
fn mapping<B: ContractBody>() -> ContractMapping {
    ContractMapping {
        family: B::FAMILY,
        schema_id: B::SCHEMA_ID,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emit_apis_reports_exactly_the_nine_component_contracts() {
        let json = phoxal::participant::emit_apis_json::<WebotsControllerSimulator>();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["artifact"]["id"], "webots-controller");
        assert_eq!(value["artifact"]["kind"], "simulator");
        assert_eq!(value["participant_class"], "checked");
        let contracts = value["required_contracts"].as_array().unwrap();

        let expected = contract_mappings();
        assert_eq!(
            contracts.len(),
            expected.len(),
            "controller must emit exactly the 9 component contracts, got {contracts:?}"
        );
        for mapping in &expected {
            assert!(
                contracts.iter().any(|contract| {
                    contract["family"] == mapping.family
                        && contract["schema_id"] == mapping.schema_id
                }),
                "missing mapping for {}",
                mapping.family
            );
        }

        // Never leaks the supervisor's simulation::* contracts.
        assert!(
            !contracts.iter().any(|contract| contract["family"]
                .as_str()
                .unwrap_or_default()
                .starts_with("simulation::")),
            "controller must not report simulation::* contracts: {contracts:?}"
        );
    }
}
