use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use phoxal::bus::ContractBody;
use phoxal::model::component::v1::CapabilityRef;
use phoxal::model::component::v1::capability::{
    CameraMode, Capability, GnssCoordinateSystem, MotorCommand,
};
use phoxal::model::robot::v1::ComponentSource;
use phoxal::model::simulation::Simulation as SimulationFile;
use phoxal::model::simulation::v1::Simulation as SimulationSpec;
use phoxal::model::v1::Robot;
use phoxal::prelude::*;
use phoxal_api::y2026_1 as api;

const STEP_HZ: f64 = 100.0;
const DEFAULT_DT_NS: u64 = 10_000_000;
const COMPONENTS_DIR: &str = "components";
const SIMULATION_FILE: &str = "simulation.yaml";

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, phoxal::schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
enum WebotsMode {
    #[default]
    Bridge,
    Controller,
    Supervisor,
}

impl WebotsMode {
    const fn binds_component_devices(self) -> bool {
        matches!(self, Self::Bridge | Self::Controller)
    }

    const fn owns_simulation_topics(self) -> bool {
        matches!(self, Self::Bridge | Self::Supervisor)
    }
}

#[derive(Clone, Debug, serde::Deserialize, phoxal::schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct WebotsConfig {
    #[serde(default)]
    mode: WebotsMode,
    #[serde(default = "default_require_native")]
    require_native: bool,
}

impl Default for WebotsConfig {
    fn default() -> Self {
        Self {
            mode: WebotsMode::default(),
            require_native: default_require_native(),
        }
    }
}

fn default_require_native() -> bool {
    true
}

#[derive(phoxal::Simulator)]
#[phoxal(id = "webots", api = y2026_1, config = Option<WebotsConfig>)]
struct WebotsSimulator {
    mode: WebotsMode,
    running: bool,
    epoch: u64,
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
    clock: Publisher<api::simulation::Clock>,
    control: Subscriber<api::simulation::Control>,
    robot_pose: Publisher<api::simulation::RobotPose>,
    contact: Publisher<api::simulation::Contact>,
}

#[phoxal::behavior]
impl WebotsSimulator {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>, config: Option<WebotsConfig>) -> Result<Self> {
        let config = config.unwrap_or_default();
        let cap = ctx.owner_capability();
        let robot = ctx.robot()?;
        let root = ctx.robot_root()?;
        let catalog = CapabilityCatalog::from_robot(root, robot)?;

        let mut motor_commands = Vec::new();
        if config.mode.binds_component_devices() {
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
        }

        let mut encoders = Vec::new();
        if config.mode.binds_component_devices() {
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
        }

        let mut imus = Vec::new();
        if config.mode.binds_component_devices() {
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
        }

        let mut accelerometers = Vec::new();
        if config.mode.binds_component_devices() {
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
        }

        let mut gyroscopes = Vec::new();
        if config.mode.binds_component_devices() {
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
        }

        let mut ranges = Vec::new();
        if config.mode.binds_component_devices() {
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
        }

        let mut cameras = Vec::new();
        if config.mode.binds_component_devices() {
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
        }

        let mut depths = Vec::new();
        if config.mode.binds_component_devices() {
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
        }

        let mut gnss = Vec::new();
        if config.mode.binds_component_devices() {
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
        }

        let clock = ctx
            .publisher(api::topic::internal::new(cap).simulation().clock())
            .await?;
        let control = ctx
            .subscribe(api::topic::internal::new(cap).simulation().control())
            .subscriber()
            .await?;
        let robot_pose = ctx
            .publisher(api::topic::internal::new(cap).simulation().robot_pose())
            .await?;
        let contact = ctx
            .publisher(api::topic::internal::new(cap).simulation().contact())
            .await?;

        let backend = Backend::open(&config, &catalog)?;
        let dt_ns = backend.dt_ns();

        tracing::info!(
            target: "simulator_webots",
            mode = ?config.mode,
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
            "webots simulator bridge ready"
        );

        Ok(Self {
            mode: config.mode,
            running: true,
            epoch: 0,
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
            clock,
            control,
            robot_pose,
            contact,
        })
    }

    #[step(hz = 100)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        self.apply_control_commands()?;
        let now = LogicalTime::new(self.epoch, self.time_ns);

        if self.running {
            let commands = self.latest_motor_commands();
            let outputs =
                self.backend
                    .advance(self.mode, self.step_index, self.time_ns, &commands)?;
            self.time_ns = self.time_ns.saturating_add(self.dt_ns);
            self.step_index = self.step_index.saturating_add(1);
            let at = LogicalTime::new(self.epoch, self.time_ns);
            self.publish_outputs(at, outputs).await?;
        } else {
            self.dt_ns = self.dt_ns.max(step.dt_ns());
        }

        self.clock
            .publish_at(
                LogicalTime::new(self.epoch, self.time_ns),
                api::simulation::Clock {
                    now_ns: self.time_ns,
                    running: self.running,
                },
            )
            .await?;

        if !self.running {
            tracing::trace!(target: "simulator_webots", time_ns = now.time_ns(), "simulation paused");
        }

        Ok(())
    }

    #[shutdown]
    async fn shutdown(&mut self, _ctx: ShutdownContext) -> Result<()> {
        for (subscriber, spec) in self.motor_commands.iter().zip(&self.motor_specs) {
            let _latest = drain_latest(subscriber);
            let stop = api::component::motor::Command::Stop;
            if let Err(error) = self.backend.apply_motor_command(spec, &stop) {
                tracing::warn!(
                    target: "simulator_webots",
                    capability = %spec.reference,
                    error = %error,
                    "failed to park motor on shutdown"
                );
            }
        }
        Ok(())
    }
}

impl WebotsSimulator {
    fn apply_control_commands(&mut self) -> Result<()> {
        while let Some(received) = self.control.try_recv() {
            match received.body {
                api::simulation::Control::Pause => self.running = false,
                api::simulation::Control::Resume => self.running = true,
                api::simulation::Control::Reset => {
                    self.backend.reset()?;
                    self.epoch = self.epoch.saturating_add(1);
                    self.time_ns = 0;
                    self.step_index = 0;
                    self.running = true;
                }
            }
        }
        Ok(())
    }

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
        if let Some(pose) = outputs.robot_pose {
            self.robot_pose.publish_at(at, pose).await?;
        }
        if let Some(contact) = outputs.contact {
            self.contact.publish_at(at, contact).await?;
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

#[derive(Clone, Debug, Default)]
struct CapabilityCatalog {
    motors: Vec<MotorSpec>,
    encoders: Vec<EncoderSpec>,
    imus: Vec<SampledSpec>,
    accelerometers: Vec<SampledSpec>,
    gyroscopes: Vec<SampledSpec>,
    ranges: Vec<RangeSpec>,
    cameras: Vec<CameraSpec>,
    depths: Vec<DepthSpec>,
    gnss: Vec<GnssSpec>,
}

impl CapabilityCatalog {
    fn from_robot(root: &Path, robot: &Robot) -> Result<Self> {
        let simulations = load_simulation_specs(root, robot)?;
        let mut catalog = Self::default();

        for (component_id, instance) in &robot.manifest.components {
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
                            publish_every_steps: publish_every_steps(
                                STEP_HZ,
                                config.publish_rate_hz,
                            )?,
                            sampling_period_ms: sampling_period_ms(sampling_hz)?,
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
                            target: "simulator_webots",
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

#[derive(Clone, Debug)]
struct MotorSpec {
    reference: CapabilityRef,
    actuator_type: MotorCommand,
    gear_ratio: f64,
}

#[derive(Clone, Debug)]
struct EncoderSpec {
    reference: CapabilityRef,
    publish_every_steps: u64,
    sampling_period_ms: i32,
    gear_ratio: f64,
}

#[derive(Clone, Debug)]
struct SampledSpec {
    reference: CapabilityRef,
    publish_every_steps: u64,
    sampling_period_ms: i32,
}

#[derive(Clone, Debug)]
struct RangeSpec {
    sampled: SampledSpec,
    min_range_m: f32,
    max_range_m: f32,
}

#[derive(Clone, Debug)]
struct CameraSpec {
    sampled: SampledSpec,
    mode: CameraMode,
    width: u32,
    height: u32,
}

#[derive(Clone, Debug)]
struct DepthSpec {
    sampled: SampledSpec,
    width: u32,
    height: u32,
}

#[derive(Clone, Debug)]
struct GnssSpec {
    sampled: SampledSpec,
    coordinate_system: GnssCoordinateSystem,
}

fn sampled_spec(
    reference: CapabilityRef,
    publish_rate_hz: f64,
    simulation_capability: Option<&phoxal::model::simulation::capability::Capability>,
) -> Result<SampledSpec> {
    let sampling_hz = simulation_capability
        .and_then(simulation_sampling_rate)
        .unwrap_or(publish_rate_hz);
    Ok(SampledSpec {
        reference,
        publish_every_steps: publish_every_steps(STEP_HZ, publish_rate_hz)?,
        sampling_period_ms: sampling_period_ms(sampling_hz)?,
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
            .as_v1()
            .context("webots simulator only supports simulation.yaml version v1")?
            .clone();
        simulations.insert(component_type.to_string(), simulation);
    }
    Ok(simulations)
}

fn component_simulation_path(
    bundle_root: &Path,
    manifest: &phoxal::model::robot::v1::Robot,
    component_type: &str,
) -> PathBuf {
    let staged_path = bundle_root.join(COMPONENTS_DIR).join(component_type);
    let Some(source) = manifest.components.sources.get(component_type) else {
        return staged_path;
    };

    let source_path = match source {
        ComponentSource::Path(path_source) => bundle_root.join(&path_source.path),
        ComponentSource::Git(_) => staged_path.clone(),
    };
    if source_path.join(SIMULATION_FILE).is_file() {
        source_path
    } else {
        staged_path
    }
}

fn publish_every_steps(step_hz: f64, publish_rate_hz: f64) -> Result<u64> {
    if !step_hz.is_finite() || step_hz <= 0.0 {
        bail!("step_hz must be finite and > 0");
    }
    if !publish_rate_hz.is_finite() || publish_rate_hz <= 0.0 {
        bail!("publish_rate_hz must be finite and > 0");
    }
    Ok((step_hz / publish_rate_hz).round().max(1.0) as u64)
}

fn sampling_period_ms(rate_hz: f64) -> Result<i32> {
    if !rate_hz.is_finite() || rate_hz <= 0.0 {
        bail!("sampling_period_hz must be finite and > 0");
    }
    Ok((1000.0 / rate_hz).round().max(1.0) as i32)
}

fn is_due(step_index: u64, publish_every_steps: u64) -> bool {
    publish_every_steps <= 1 || step_index % publish_every_steps == 0
}

enum Backend {
    Native(Box<NativeBackend>),
    Stub(StubBackend),
}

impl Backend {
    fn open(config: &WebotsConfig, catalog: &CapabilityCatalog) -> Result<Self> {
        if webots_rs::WEBOTS_RUNTIME_LINKED {
            return Ok(Self::Native(Box::new(NativeBackend::new(
                config.mode,
                catalog,
            )?)));
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
        mode: WebotsMode,
        step_index: u64,
        time_ns: u64,
        commands: &[Option<api::component::motor::Command>],
    ) -> Result<BackendOutput> {
        match self {
            Self::Native(backend) => backend.advance(mode, step_index, time_ns, commands),
            Self::Stub(backend) => backend.advance(mode, step_index, time_ns, commands),
        }
    }

    fn reset(&mut self) -> Result<()> {
        match self {
            Self::Native(backend) => backend.reset(),
            Self::Stub(backend) => {
                backend.reset();
                Ok(())
            }
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
        mode: WebotsMode,
        _step_index: u64,
        _time_ns: u64,
        _commands: &[Option<api::component::motor::Command>],
    ) -> Result<BackendOutput> {
        Ok(BackendOutput::empty(mode, &self.counts))
    }

    fn reset(&mut self) {}
}

struct NativeBackend {
    webots: webots_rs::Webots,
    supervisor: webots_rs::Supervisor,
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
    fn new(mode: WebotsMode, catalog: &CapabilityCatalog) -> Result<Self> {
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
        let supervisor = webots.get_supervisor();

        let mut motors = Vec::new();
        let mut encoders = Vec::new();
        let mut imus = Vec::new();
        let mut accelerometers = Vec::new();
        let mut gyroscopes = Vec::new();
        let mut ranges = Vec::new();
        let mut cameras = Vec::new();
        let mut depths = Vec::new();
        let mut gnss = Vec::new();

        if mode.binds_component_devices() {
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
        }

        Ok(Self {
            webots,
            supervisor,
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
        mode: WebotsMode,
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

        let mut output = BackendOutput {
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
            robot_pose: None,
            contact: None,
        };

        if mode.owns_simulation_topics() {
            output.robot_pose = self.read_robot_pose().ok();
            output.contact = self.read_contact().ok();
        }

        Ok(output)
    }

    fn reset(&mut self) -> Result<()> {
        self.supervisor
            .simulation_reset()
            .map_err(|error| anyhow!(error))
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

    fn read_robot_pose(&self) -> Result<api::simulation::RobotPose> {
        let node = self.supervisor.get_self().map_err(|error| anyhow!(error))?;
        let position = node.position().map_err(|error| anyhow!(error))?;
        let rotation = node
            .field("rotation")
            .and_then(|field| field.sf_rotation())
            .map_err(|error| anyhow!(error))?;
        Ok(api::simulation::RobotPose {
            x_m: position[0],
            y_m: position[1],
            yaw_rad: yaw_from_axis_angle(rotation),
        })
    }

    fn read_contact(&self) -> Result<api::simulation::Contact> {
        let node = self.supervisor.get_self().map_err(|error| anyhow!(error))?;
        let count = node
            .number_of_contact_points(true)
            .map_err(|error| anyhow!(error))?;
        Ok(api::simulation::Contact {
            in_contact: count > 0,
            detail: (count > 0).then(|| format!("{count} contact point(s)")),
        })
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
    robot_pose: Option<api::simulation::RobotPose>,
    contact: Option<api::simulation::Contact>,
}

impl BackendOutput {
    fn empty(mode: WebotsMode, counts: &OutputCounts) -> Self {
        Self {
            encoders: none_vec(counts.encoders),
            imus: none_vec(counts.imus),
            accelerometers: none_vec(counts.accelerometers),
            gyroscopes: none_vec(counts.gyroscopes),
            ranges: none_vec(counts.ranges),
            cameras: none_vec(counts.cameras),
            depths: none_vec(counts.depths),
            gnss: none_vec(counts.gnss),
            robot_pose: mode
                .owns_simulation_topics()
                .then_some(api::simulation::RobotPose {
                    x_m: 0.0,
                    y_m: 0.0,
                    yaw_rad: 0.0,
                }),
            contact: mode
                .owns_simulation_topics()
                .then_some(api::simulation::Contact {
                    in_contact: false,
                    detail: None,
                }),
        }
    }
}

fn none_vec<T>(len: usize) -> Vec<Option<T>> {
    std::iter::repeat_with(|| None).take(len).collect()
}

struct NativeMotor {
    reference: CapabilityRef,
    motor: webots_rs::device::motor::Motor,
    actuator_type: MotorCommand,
    gear_ratio: f64,
}

impl NativeMotor {
    fn new(webots: &webots_rs::Webots, spec: &MotorSpec) -> Result<Self> {
        let motor = webots
            .motor(spec.reference.to_string())
            .map_err(|error| anyhow!(error))?;
        Ok(Self {
            reference: spec.reference.clone(),
            motor,
            actuator_type: spec.actuator_type,
            gear_ratio: spec.gear_ratio,
        })
    }

    fn apply(&self, command: &api::component::motor::Command) -> Result<()> {
        match (self.actuator_type, command) {
            (MotorCommand::Velocity, api::component::motor::Command::Velocity(value)) => {
                self.motor
                    .set_position(f64::INFINITY)
                    .map_err(|error| anyhow!(error))?;
                self.motor
                    .set_velocity(actuator_to_joint_value(f64::from(*value), self.gear_ratio))
                    .map_err(|error| anyhow!(error))?;
            }
            (MotorCommand::Torque, api::component::motor::Command::Torque(value)) => {
                self.motor
                    .set_position(f64::INFINITY)
                    .map_err(|error| anyhow!(error))?;
                self.motor
                    .set_torque(actuator_to_joint_value(f64::from(*value), self.gear_ratio))
                    .map_err(|error| anyhow!(error))?;
            }
            (MotorCommand::Velocity, api::component::motor::Command::Stop) => {
                self.motor
                    .set_position(f64::INFINITY)
                    .map_err(|error| anyhow!(error))?;
                self.motor
                    .set_velocity(0.0)
                    .map_err(|error| anyhow!(error))?;
            }
            (MotorCommand::Torque, api::component::motor::Command::Stop) => {
                self.motor
                    .set_position(f64::INFINITY)
                    .map_err(|error| anyhow!(error))?;
                self.motor.set_torque(0.0).map_err(|error| anyhow!(error))?;
            }
            (MotorCommand::Position, api::component::motor::Command::Stop) => {
                self.motor
                    .set_velocity(0.0)
                    .map_err(|error| anyhow!(error))?;
            }
            (actual, command) => {
                bail!("motor {actual:?} does not support command {command:?}");
            }
        }
        Ok(())
    }
}

struct NativeEncoder {
    sensor: webots_rs::device::position_sensor::PositionSensor,
    spec: EncoderSpec,
    last: Option<(f64, u64)>,
}

impl NativeEncoder {
    fn new(webots: &webots_rs::Webots, spec: &EncoderSpec) -> Result<Self> {
        let sensor = webots
            .position_sensor(spec.reference.to_string())
            .map_err(|error| anyhow!(error))?;
        sensor
            .enable(spec.sampling_period_ms)
            .map_err(|error| anyhow!(error))?;
        Ok(Self {
            sensor,
            spec: spec.clone(),
            last: None,
        })
    }

    fn read_if_due(
        &mut self,
        step_index: u64,
        time_ns: u64,
    ) -> Result<Option<api::component::encoder::Sample>> {
        if !is_due(step_index, self.spec.publish_every_steps) {
            return Ok(None);
        }
        let position_rad = joint_to_actuator_position(
            self.sensor.value().map_err(|error| anyhow!(error))?,
            self.spec.gear_ratio,
        );
        let velocity_radps = self
            .last
            .map(|(last_position, last_time)| {
                velocity_radps(position_rad, last_position, time_ns, last_time)
            })
            .unwrap_or(0.0);
        self.last = Some((position_rad, time_ns));
        Ok(Some(api::component::encoder::Sample {
            position_rad,
            velocity_radps: velocity_radps as f32,
        }))
    }
}

struct NativeImu {
    inertial_unit: webots_rs::device::inertial_unit::InertialUnit,
    accelerometer: webots_rs::device::accelerometer::Accelerometer,
    gyro: webots_rs::device::gyro::Gyro,
    spec: SampledSpec,
}

impl NativeImu {
    fn new(webots: &webots_rs::Webots, spec: &SampledSpec) -> Result<Self> {
        let inertial_unit = webots
            .inertial_unit(spec.reference.to_string())
            .map_err(|error| anyhow!(error))?;
        let accelerometer = webots
            .accelerometer(format!("{}__accel", spec.reference))
            .map_err(|error| anyhow!(error))?;
        let gyro = webots
            .gyro(format!("{}__gyro", spec.reference))
            .map_err(|error| anyhow!(error))?;
        inertial_unit
            .enable(spec.sampling_period_ms)
            .map_err(|error| anyhow!(error))?;
        accelerometer
            .enable(spec.sampling_period_ms)
            .map_err(|error| anyhow!(error))?;
        gyro.enable(spec.sampling_period_ms)
            .map_err(|error| anyhow!(error))?;
        Ok(Self {
            inertial_unit,
            accelerometer,
            gyro,
            spec: spec.clone(),
        })
    }

    fn read_if_due(
        &self,
        step_index: u64,
        time_ns: u64,
    ) -> Result<Option<api::component::imu::Sample>> {
        if !is_due(step_index, self.spec.publish_every_steps) {
            return Ok(None);
        }
        let [roll, pitch, yaw] = self
            .inertial_unit
            .get_roll_pitch_yaw()
            .map_err(|error| anyhow!(error))?;
        let acceleration = self
            .accelerometer
            .values()
            .map_err(|error| anyhow!(error))?
            .map(|value| value as f32);
        let angular_velocity = self
            .gyro
            .values()
            .map_err(|error| anyhow!(error))?
            .map(|value| value as f32);
        Ok(Some(api::component::imu::Sample {
            orientation: Some(quaternion_wxyz_from_rpy(roll, pitch, yaw)),
            angular_velocity_radps: angular_velocity,
            linear_acceleration_mps2: acceleration,
            covariance: None,
            noise_density: None,
            sensor_frame_id: None,
            measured_at_ns: Some(time_ns),
            health: api::component::imu::SensorHealth::Nominal,
            bias: None,
        }))
    }
}

struct NativeAccelerometer {
    sensor: webots_rs::device::accelerometer::Accelerometer,
    spec: SampledSpec,
}

impl NativeAccelerometer {
    fn new(webots: &webots_rs::Webots, spec: &SampledSpec) -> Result<Self> {
        let sensor = webots
            .accelerometer(spec.reference.to_string())
            .map_err(|error| anyhow!(error))?;
        sensor
            .enable(spec.sampling_period_ms)
            .map_err(|error| anyhow!(error))?;
        Ok(Self {
            sensor,
            spec: spec.clone(),
        })
    }

    fn read_if_due(
        &self,
        step_index: u64,
    ) -> Result<Option<api::component::accelerometer::Sample>> {
        if !is_due(step_index, self.spec.publish_every_steps) {
            return Ok(None);
        }
        Ok(Some(api::component::accelerometer::Sample {
            linear_acceleration: self
                .sensor
                .values()
                .map_err(|error| anyhow!(error))?
                .map(|value| value as f32),
        }))
    }
}

struct NativeGyroscope {
    gyro: webots_rs::device::gyro::Gyro,
    spec: SampledSpec,
}

impl NativeGyroscope {
    fn new(webots: &webots_rs::Webots, spec: &SampledSpec) -> Result<Self> {
        let gyro = webots
            .gyro(spec.reference.to_string())
            .map_err(|error| anyhow!(error))?;
        gyro.enable(spec.sampling_period_ms)
            .map_err(|error| anyhow!(error))?;
        Ok(Self {
            gyro,
            spec: spec.clone(),
        })
    }

    fn read_if_due(&self, step_index: u64) -> Result<Option<api::component::gyroscope::Sample>> {
        if !is_due(step_index, self.spec.publish_every_steps) {
            return Ok(None);
        }
        Ok(Some(api::component::gyroscope::Sample {
            angular_velocity: self
                .gyro
                .values()
                .map_err(|error| anyhow!(error))?
                .map(|value| value as f32),
        }))
    }
}

struct NativeRange {
    sensor: webots_rs::device::distance_sensor::DistanceSensor,
    spec: RangeSpec,
}

impl NativeRange {
    fn new(webots: &webots_rs::Webots, spec: &RangeSpec) -> Result<Self> {
        let sensor = webots
            .distance_sensor(spec.sampled.reference.to_string())
            .map_err(|error| anyhow!(error))?;
        sensor
            .enable(spec.sampled.sampling_period_ms)
            .map_err(|error| anyhow!(error))?;
        Ok(Self {
            sensor,
            spec: spec.clone(),
        })
    }

    fn read_if_due(
        &self,
        step_index: u64,
        time_ns: u64,
    ) -> Result<Option<api::component::range::Sample>> {
        if !is_due(step_index, self.spec.sampled.publish_every_steps) {
            return Ok(None);
        }
        Ok(Some(api::component::range::Sample {
            distance_m: self.sensor.value().map_err(|error| anyhow!(error))? as f32,
            limits: Some(api::component::range::Limits {
                min_m: self.spec.min_range_m,
                max_m: self.spec.max_range_m,
            }),
            measured_at_ns: Some(time_ns),
            quality: Some(api::component::range::SampleQuality {
                valid: true,
                confidence: None,
            }),
            health: api::component::range::SensorHealth::Nominal,
        }))
    }
}

struct NativeCamera {
    camera: webots_rs::device::camera::Camera,
    spec: CameraSpec,
}

impl NativeCamera {
    fn new(webots: &webots_rs::Webots, spec: &CameraSpec) -> Result<Self> {
        let camera = webots
            .camera(spec.sampled.reference.to_string())
            .map_err(|error| anyhow!(error))?;
        camera
            .enable(spec.sampled.sampling_period_ms)
            .map_err(|error| anyhow!(error))?;
        Ok(Self {
            camera,
            spec: spec.clone(),
        })
    }

    fn read_if_due(
        &self,
        step_index: u64,
        time_ns: u64,
    ) -> Result<Option<api::component::camera::Frame>> {
        if !is_due(step_index, self.spec.sampled.publish_every_steps) {
            return Ok(None);
        }
        let bgra = self.camera.get_image().map_err(|error| anyhow!(error))?;
        let (encoding, data) = match self.spec.mode {
            CameraMode::Mono => (api::component::camera::Encoding::L8, bgra_to_luma(&bgra)),
            CameraMode::Rgb => (api::component::camera::Encoding::Rgb8, bgra_to_rgb(&bgra)),
        };
        Ok(Some(api::component::camera::Frame {
            width: self.spec.width,
            height: self.spec.height,
            encoding,
            intrinsics: None,
            distortion: None,
            exposure: None,
            measured_at_ns: Some(time_ns),
            calibration: None,
            data,
        }))
    }
}

struct NativeDepth {
    sensor: webots_rs::device::range_finder::RangeFinder,
    spec: DepthSpec,
}

impl NativeDepth {
    fn new(webots: &webots_rs::Webots, spec: &DepthSpec) -> Result<Self> {
        let sensor = webots
            .range_finder(spec.sampled.reference.to_string())
            .map_err(|error| anyhow!(error))?;
        sensor
            .enable(spec.sampled.sampling_period_ms)
            .map_err(|error| anyhow!(error))?;
        Ok(Self {
            sensor,
            spec: spec.clone(),
        })
    }

    fn read_if_due(
        &self,
        step_index: u64,
        time_ns: u64,
    ) -> Result<Option<api::component::depth::Frame>> {
        if !is_due(step_index, self.spec.sampled.publish_every_steps) {
            return Ok(None);
        }
        let samples_mm = self
            .sensor
            .get_range_image()
            .map_err(|error| anyhow!(error))?
            .into_iter()
            .map(meters_to_u16_mm)
            .collect();
        Ok(Some(api::component::depth::Frame {
            samples_mm,
            encoding: api::component::depth::Encoding::U16Millimeters,
            invalid_sample_policy: api::component::depth::InvalidSamplePolicy::ZeroIsInvalid,
            width: Some(self.spec.width),
            height: Some(self.spec.height),
            intrinsics: None,
            distortion: None,
            exposure: None,
            measured_at_ns: Some(time_ns),
            calibration: None,
        }))
    }
}

struct NativeGnss {
    gps: webots_rs::device::gps::Gps,
    spec: GnssSpec,
}

impl NativeGnss {
    fn new(webots: &webots_rs::Webots, spec: &GnssSpec) -> Result<Self> {
        let gps = webots
            .gps(spec.sampled.reference.to_string())
            .map_err(|error| anyhow!(error))?;
        gps.enable(spec.sampled.sampling_period_ms)
            .map_err(|error| anyhow!(error))?;
        Ok(Self {
            gps,
            spec: spec.clone(),
        })
    }

    fn read_if_due(&self, step_index: u64) -> Result<Option<api::component::gnss::Sample>> {
        if !is_due(step_index, self.spec.sampled.publish_every_steps) {
            return Ok(None);
        }
        let reading = self.gps.reading().map_err(|error| anyhow!(error))?;
        let _coordinate_system = self.spec.coordinate_system;
        Ok(Some(api::component::gnss::Sample {
            latitude: reading.position[0],
            longitude: reading.position[1],
            altitude: reading.position[2],
            position_covariance: [0.0; 9],
        }))
    }
}

fn actuator_to_joint_value(actuator_value: f64, gear_ratio: f64) -> f64 {
    actuator_value / gear_ratio
}

fn joint_to_actuator_position(joint_position_rad: f64, gear_ratio: f64) -> f64 {
    joint_position_rad * gear_ratio
}

fn velocity_radps(
    position: f64,
    previous_position: f64,
    time_ns: u64,
    previous_time_ns: u64,
) -> f64 {
    let dt_ns = time_ns.saturating_sub(previous_time_ns);
    if dt_ns == 0 {
        0.0
    } else {
        (position - previous_position) * 1_000_000_000.0 / dt_ns as f64
    }
}

fn quaternion_wxyz_from_rpy(roll: f64, pitch: f64, yaw: f64) -> [f32; 4] {
    let half_roll = roll * 0.5;
    let half_pitch = pitch * 0.5;
    let half_yaw = yaw * 0.5;
    let (sr, cr) = half_roll.sin_cos();
    let (sp, cp) = half_pitch.sin_cos();
    let (sy, cy) = half_yaw.sin_cos();

    [
        (cr * cp * cy + sr * sp * sy) as f32,
        (sr * cp * cy - cr * sp * sy) as f32,
        (cr * sp * cy + sr * cp * sy) as f32,
        (cr * cp * sy - sr * sp * cy) as f32,
    ]
}

fn bgra_to_rgb(bgra: &[u8]) -> Vec<u8> {
    bgra.chunks_exact(4)
        .flat_map(|pixel| [pixel[2], pixel[1], pixel[0]])
        .collect()
}

fn bgra_to_luma(bgra: &[u8]) -> Vec<u8> {
    bgra.chunks_exact(4)
        .map(|pixel| {
            let red = u32::from(pixel[2]);
            let green = u32::from(pixel[1]);
            let blue = u32::from(pixel[0]);
            ((299 * red + 587 * green + 114 * blue) / 1000) as u8
        })
        .collect()
}

fn meters_to_u16_mm(meters: f32) -> u16 {
    if !meters.is_finite() || meters <= 0.0 {
        0
    } else {
        (meters * 1000.0).round().clamp(1.0, f32::from(u16::MAX)) as u16
    }
}

fn yaw_from_axis_angle(rotation: [f64; 4]) -> f64 {
    let [x, y, z, angle] = rotation;
    let norm = (x.mul_add(x, y.mul_add(y, z * z))).sqrt();
    if norm <= f64::EPSILON {
        return 0.0;
    }
    let axis_z = z / norm;
    angle * axis_z
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ContractMapping {
    old_capability: &'static str,
    family: &'static str,
    topic: &'static str,
    direction: phoxal::participant::Direction,
    schema_id: &'static str,
}

#[cfg(test)]
fn contract_mappings() -> Vec<ContractMapping> {
    vec![
        mapping::<api::component::motor::Command>(
            "motor",
            phoxal::participant::Direction::Subscribe,
        ),
        mapping::<api::component::encoder::Sample>(
            "encoder",
            phoxal::participant::Direction::Publish,
        ),
        mapping::<api::component::imu::Sample>("imu", phoxal::participant::Direction::Publish),
        mapping::<api::component::accelerometer::Sample>(
            "accelerometer",
            phoxal::participant::Direction::Publish,
        ),
        mapping::<api::component::gyroscope::Sample>(
            "gyroscope",
            phoxal::participant::Direction::Publish,
        ),
        mapping::<api::component::range::Sample>("range", phoxal::participant::Direction::Publish),
        mapping::<api::component::camera::Frame>("camera", phoxal::participant::Direction::Publish),
        mapping::<api::component::depth::Frame>("depth", phoxal::participant::Direction::Publish),
        mapping::<api::component::gnss::Sample>("gnss", phoxal::participant::Direction::Publish),
    ]
}

#[cfg(test)]
fn mapping<B: ContractBody>(
    old_capability: &'static str,
    direction: phoxal::participant::Direction,
) -> ContractMapping {
    ContractMapping {
        old_capability,
        family: B::FAMILY,
        topic: B::TOPIC,
        direction,
        schema_id: B::SCHEMA_ID,
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<WebotsSimulator>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_old_capabilities_to_current_contract_families() {
        let mappings = contract_mappings();
        for mapping in &mappings {
            assert!(!mapping.schema_id.is_empty());
            assert_eq!(mapping.schema_id.len(), 16);
        }
        assert!(mappings.iter().any(|mapping| {
            mapping.old_capability == "motor"
                && mapping.family == <api::component::motor::Command as ContractBody>::FAMILY
                && mapping.topic == <api::component::motor::Command as ContractBody>::TOPIC
                && mapping.direction == phoxal::participant::Direction::Subscribe
        }));
        assert!(mappings.iter().any(|mapping| {
            mapping.old_capability == "depth"
                && mapping.family == <api::component::depth::Frame as ContractBody>::FAMILY
                && mapping.topic == <api::component::depth::Frame as ContractBody>::TOPIC
                && mapping.direction == phoxal::participant::Direction::Publish
        }));
        assert!(mappings.iter().any(|mapping| {
            mapping.old_capability == "gnss"
                && mapping.family == <api::component::gnss::Sample as ContractBody>::FAMILY
                && mapping.direction == phoxal::participant::Direction::Publish
        }));
    }

    #[test]
    fn emit_apis_reports_simulator_provider_contracts() {
        let json = phoxal::participant::emit_apis_json::<WebotsSimulator>();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["artifact"]["id"], "webots");
        assert_eq!(value["artifact"]["kind"], "simulator");
        assert_eq!(value["participant_class"], "checked");
        let contracts = value["required_contracts"].as_array().unwrap();
        for mapping in contract_mappings() {
            assert!(
                contracts.iter().any(|contract| {
                    contract["family"] == mapping.family
                        && contract["topic"] == mapping.topic
                        && contract["direction"]
                            == serde_json::Value::String(
                                format!("{:?}", mapping.direction).to_lowercase(),
                            )
                        && contract["schema_id"] == mapping.schema_id
                }),
                "missing mapping for {}",
                mapping.old_capability
            );
        }
        assert!(contracts.iter().any(|contract| {
            contract["family"] == <api::simulation::Clock as ContractBody>::FAMILY
                && contract["direction"] == "publish"
        }));
        assert!(contracts.iter().any(|contract| {
            contract["family"] == <api::simulation::Control as ContractBody>::FAMILY
                && contract["direction"] == "subscribe"
        }));
    }

    #[test]
    fn conversion_helpers_match_old_bridge_behavior() {
        assert_eq!(actuator_to_joint_value(4.0, 2.0), 2.0);
        assert_eq!(joint_to_actuator_position(2.0, 2.0), 4.0);
        assert_eq!(velocity_radps(2.0, 1.0, 2_000_000_000, 1_000_000_000), 1.0);
        assert_eq!(bgra_to_rgb(&[10, 20, 30, 255]), vec![30, 20, 10]);
        assert_eq!(bgra_to_luma(&[10, 20, 30, 255]), vec![21]);
        assert_eq!(meters_to_u16_mm(1.25), 1250);
        assert_eq!(meters_to_u16_mm(f32::NAN), 0);
    }

    #[test]
    fn quaternion_from_yaw_is_wxyz() {
        let quaternion = quaternion_wxyz_from_rpy(0.0, 0.0, std::f64::consts::FRAC_PI_2);
        let half = (std::f64::consts::FRAC_PI_2 * 0.5).sin_cos();
        assert!((f64::from(quaternion[0]) - half.1).abs() < 1e-6);
        assert!(f64::from(quaternion[1]).abs() < 1e-6);
        assert!(f64::from(quaternion[2]).abs() < 1e-6);
        assert!((f64::from(quaternion[3]) - half.0).abs() < 1e-6);
    }
}
