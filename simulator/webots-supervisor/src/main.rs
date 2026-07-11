//! Webots supervisor simulator artifact.
//!
//! The world/session authority: owns the Webots Supervisor API and publishes
//! the simulator-owned `simulation::*` topics (clock, robot pose, contact),
//! applying `simulation::Control` (pause/resume/reset). This binary never
//! binds component devices (motor, encoder, IMU, ...) - that is
//! `phoxal-simulator-webots-controller`'s job (see `simulator/webots-controller`).
//!
//! Robot-spawning (instantiating robot nodes into the world at startup, and
//! re-spawning them on `simulation::Control::Reset`) is this artifact's
//! spawn authority - see [`SpawnRobot`] for the config shape and
//! `spawn_robots` (the SPAWN SEAM) for where it attaches on [`NativeBackend`].

use anyhow::{Result, anyhow, bail};
use phoxal::prelude::*;
use phoxal_api::y2026_1 as api;

const DEFAULT_DT_NS: u64 = 10_000_000;

#[derive(Clone, Debug, serde::Deserialize, phoxal::schemars::JsonSchema, phoxal::Config)]
#[serde(deny_unknown_fields)]
struct WebotsSupervisorConfig {
    #[serde(default = "default_require_native")]
    require_native: bool,
    /// Robot nodes the supervisor imports into the world at startup (and
    /// re-imports on `simulation::Control::Reset`). Each entry is a complete
    /// Webots PROTO instance node string, already carrying its own
    /// `controller` + `controllerArgs` fields, rendered by the CLI's world
    /// staging. Defaults to empty: a supervisor with no spawn list behaves
    /// exactly as a world pre-staged entirely by Webots.
    #[serde(default)]
    spawn: Vec<SpawnRobot>,
}

impl Default for WebotsSupervisorConfig {
    fn default() -> Self {
        Self {
            require_native: default_require_native(),
            spawn: Vec::new(),
        }
    }
}

fn default_require_native() -> bool {
    true
}

/// One robot node for the supervisor to import into the world at startup
/// (and re-import after a `simulation::Control::Reset`).
///
/// The `node_string` is a complete Webots VRML PROTO instance - the CLI's
/// world-staging step renders it (including that robot's `controller` and
/// `controllerArgs` fields) before handing it to the supervisor; this crate
/// treats it as an opaque string to import verbatim.
#[derive(Clone, Debug, serde::Deserialize, phoxal::schemars::JsonSchema, phoxal::Config)]
#[serde(deny_unknown_fields)]
struct SpawnRobot {
    /// Robot id, used only for logging/diagnostics.
    name: String,
    /// Complete Webots VRML node text (a PROTO instance) to import.
    node_string: String,
}

#[derive(phoxal::Api)]
struct Api {
    clock: Publisher<api::simulation::Clock>,
    control: Subscriber<api::simulation::Control>,
    robot_pose: Publisher<api::simulation::RobotPose>,
    contact: Publisher<api::simulation::Contact>,
}

#[phoxal::simulator(id = "webots-supervisor", config = Option<WebotsSupervisorConfig>)]
struct WebotsSupervisorSimulator {
    running: bool,
    epoch: u64,
    step_index: u64,
    time_ns: u64,
    dt_ns: u64,
    backend: Backend,
}

#[phoxal::behavior]
impl WebotsSupervisorSimulator {
    #[setup]
    async fn setup(
        ctx: &mut SetupContext<Self>,
        config: Option<WebotsSupervisorConfig>,
    ) -> Result<(Self, Self::Api)> {
        let config = config.unwrap_or_default();
        let cap = ctx.owner_capability();

        let clock = ctx
            .publisher(api::topic::internal::new(cap).simulation().clock())
            .await?;
        let control = ctx
            .subscriber(api::topic::internal::new(cap).simulation().control(), 32)
            .await?;
        let robot_pose = ctx
            .publisher(api::topic::internal::new(cap).simulation().robot_pose())
            .await?;
        let contact = ctx
            .publisher(api::topic::internal::new(cap).simulation().contact())
            .await?;

        let backend = Backend::open(&config)?;
        let dt_ns = backend.dt_ns();

        tracing::info!(
            target: "simulator_webots_supervisor",
            webots_runtime_linked = webots_rs::WEBOTS_RUNTIME_LINKED,
            "webots supervisor simulator ready"
        );

        Ok((
            Self {
                running: true,
                epoch: 0,
                step_index: 0,
                time_ns: 0,
                dt_ns,
                backend,
            },
            Self::Api {
                clock,
                control,
                robot_pose,
                contact,
            },
        ))
    }

    #[step(hz = 100)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        self.apply_control_commands(api)?;

        if self.running {
            let outputs = self.backend.advance()?;
            self.time_ns = self.time_ns.saturating_add(self.dt_ns);
            self.step_index = self.step_index.saturating_add(1);
            let at = LogicalTime::new(self.epoch, self.time_ns);
            self.publish_outputs(api, at, outputs).await?;
        } else {
            self.dt_ns = self.dt_ns.max(step.dt_ns());
        }

        api.clock
            .publish_at(
                LogicalTime::new(self.epoch, self.time_ns),
                api::simulation::Clock {
                    now_ns: self.time_ns,
                    running: self.running,
                },
            )
            .await?;

        if !self.running {
            tracing::trace!(target: "simulator_webots_supervisor", time_ns = self.time_ns, "simulation paused");
        }

        Ok(())
    }

    #[shutdown]
    async fn shutdown(&mut self, api: &mut Self::Api, ctx: ShutdownContext) -> Result<()> {
        let _ = (api, ctx);
        Ok(())
    }
}

impl WebotsSupervisorSimulator {
    fn apply_control_commands(&mut self, api: &mut Api) -> Result<()> {
        while let Some(received) = api.control.try_recv() {
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

    async fn publish_outputs(
        &self,
        api: &Api,
        at: LogicalTime,
        outputs: BackendOutput,
    ) -> Result<()> {
        if let Some(pose) = outputs.robot_pose {
            api.robot_pose.publish_at(at, pose).await?;
        }
        if let Some(contact) = outputs.contact {
            api.contact.publish_at(at, contact).await?;
        }
        Ok(())
    }
}

/// Webots backend selection: native `webots-rs` linkage when the runtime is
/// present, or a metadata-only stub otherwise (headless `cargo test`/CI).
enum Backend {
    Native(Box<NativeBackend>),
    Stub(StubBackend),
}

impl Backend {
    fn open(config: &WebotsSupervisorConfig) -> Result<Self> {
        if webots_rs::WEBOTS_RUNTIME_LINKED {
            return Ok(Self::Native(Box::new(NativeBackend::new(&config.spawn)?)));
        }

        if config.require_native {
            bail!(
                "Webots runtime is not linked into this build. Rebuild with WEBOTS_HOME pointing at a Webots installation, or set PHOXAL_CONFIG require_native=false for metadata-only contract tests."
            );
        }

        Ok(Self::Stub(StubBackend::new(&config.spawn)))
    }

    fn dt_ns(&self) -> u64 {
        match self {
            Self::Native(backend) => backend.dt_ns,
            Self::Stub(backend) => backend.dt_ns,
        }
    }

    fn advance(&mut self) -> Result<BackendOutput> {
        match self {
            Self::Native(backend) => backend.advance(),
            Self::Stub(backend) => Ok(backend.advance()),
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
}

struct StubBackend {
    dt_ns: u64,
}

impl StubBackend {
    /// Builds the headless (no Webots linked) stand-in. There is no real
    /// Supervisor to import nodes into, so the spawn list is only logged -
    /// this lets tests assert the configured list was read without ever
    /// attempting a live import.
    fn new(spawn: &[SpawnRobot]) -> Self {
        Self::log_spawn(spawn);
        Self {
            dt_ns: DEFAULT_DT_NS,
        }
    }

    fn log_spawn(spawn: &[SpawnRobot]) {
        for entry in spawn {
            tracing::info!(
                target: "simulator_webots_supervisor",
                robot = %entry.name,
                node_string_len = entry.node_string.len(),
                "stub backend: would spawn robot (no Webots runtime linked)"
            );
        }
    }

    fn advance(&mut self) -> BackendOutput {
        BackendOutput {
            robot_pose: Some(api::simulation::RobotPose {
                x_m: 0.0,
                y_m: 0.0,
                yaw_rad: 0.0,
            }),
            contact: Some(api::simulation::Contact {
                in_contact: false,
                detail: None,
            }),
        }
    }

    fn reset(&mut self) {}
}

/// Owns the Webots supervisor-process handle: base handle open, per-step
/// `webots.step()`, and the Supervisor API (world reset, self pose/contact,
/// robot spawn/re-spawn).
struct NativeBackend {
    webots: webots_rs::Webots,
    supervisor: webots_rs::Supervisor,
    dt_ns: u64,
    step_ms: i32,
    /// Retained so `reset` can re-import the same robots: a world reset
    /// restores the *saved* world file, which does not include nodes that
    /// were imported at runtime, so they must be re-spawned every reset.
    spawn: Vec<SpawnRobot>,
}

impl NativeBackend {
    fn new(spawn: &[SpawnRobot]) -> Result<Self> {
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

        // SPAWN SEAM: import every configured robot node before the first
        // `webots.step()` call in `advance` - this is what makes Webots
        // launch that robot's controller process.
        spawn_robots(&supervisor, spawn)?;

        Ok(Self {
            webots,
            supervisor,
            dt_ns,
            step_ms,
            spawn: spawn.to_vec(),
        })
    }

    fn advance(&mut self) -> Result<BackendOutput> {
        if !self
            .webots
            .step(self.step_ms)
            .map_err(|error| anyhow!(error))?
        {
            bail!("Webots requested supervisor shutdown");
        }

        Ok(BackendOutput {
            robot_pose: self.read_robot_pose().ok(),
            contact: self.read_contact().ok(),
        })
    }

    /// Resets the world, then re-imports the spawn list.
    ///
    /// ASSUMPTION FLAGGED FOR THE LIVE GATE: `simulation_reset` restores the
    /// world file Webots loaded at launch, which predates any runtime import,
    /// so the re-imported nodes should not already be present after reset -
    /// this straightforward re-import (no remove-then-reimport bookkeeping)
    /// assumes Webots does not itself retain runtime-imported nodes across a
    /// reset. If live testing on real Webots shows otherwise (nodes
    /// duplicating post-reset), this needs to track spawned node handles and
    /// either skip re-importing or remove-then-reimport instead.
    fn reset(&mut self) -> Result<()> {
        self.supervisor
            .simulation_reset()
            .map_err(|error| anyhow!(error))?;
        spawn_robots(&self.supervisor, &self.spawn)
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

/// Imports every entry in `spawn` into the world root's `children` field, in
/// list order, appending each (position `-1`) so import order matches
/// configuration order. Each `node_string` is a complete Webots PROTO
/// instance node (already carrying its own `controller`/`controllerArgs`),
/// so a successful import is what makes Webots launch that robot's
/// controller process. A malformed node string fails loudly - naming the
/// robot - rather than silently skipping it.
fn spawn_robots(supervisor: &webots_rs::Supervisor, spawn: &[SpawnRobot]) -> Result<()> {
    if spawn.is_empty() {
        return Ok(());
    }

    let root = supervisor.get_root().map_err(|error| anyhow!(error))?;
    let children = root.field("children").map_err(|error| anyhow!(error))?;

    for entry in spawn {
        children
            .import_mf_node_from_string(-1, &entry.node_string)
            .map_err(|error| anyhow!("failed to spawn robot '{}': {error}", entry.name))?;
        tracing::info!(
            target: "simulator_webots_supervisor",
            robot = %entry.name,
            "spawned robot node into world"
        );
    }

    Ok(())
}

struct BackendOutput {
    robot_pose: Option<api::simulation::RobotPose>,
    contact: Option<api::simulation::Contact>,
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

fn main() -> phoxal::Result<()> {
    phoxal::run::<WebotsSupervisorSimulator>()
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
        mapping::<api::simulation::Control>(ContractRole::Subscribe),
        mapping::<api::simulation::RobotPose>(ContractRole::Publish),
        mapping::<api::simulation::Contact>(ContractRole::Publish),
    ]
}

#[cfg(test)]
fn mapping<B: phoxal::bus::ContractBody>(
    role: phoxal::participant::ContractRole,
) -> ContractMapping {
    ContractMapping {
        topic: B::TOPIC,
        role,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoxal::participant::{Participant, ParticipantApi};

    #[test]
    fn api_declares_exactly_the_four_simulation_contracts() {
        assert_eq!(
            <WebotsSupervisorSimulator as Participant>::ID,
            "webots-supervisor"
        );
        assert_eq!(
            <WebotsSupervisorSimulator as Participant>::KIND,
            "simulator"
        );
        assert_eq!(
            <WebotsSupervisorSimulator as Participant>::PARTICIPANT_CLASS,
            "checked"
        );

        let contracts =
            <<WebotsSupervisorSimulator as Participant>::Api as ParticipantApi>::CONTRACTS;

        let expected = contract_mappings();
        assert_eq!(
            contracts.len(),
            expected.len(),
            "supervisor must emit exactly the 4 simulation contracts, got {contracts:?}"
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

        // Never leaks the controller's component::* contracts.
        assert!(
            !contracts.iter().any(|c| c.topic.contains("/component/")),
            "supervisor must not report component::* contracts: {contracts:?}"
        );
    }

    #[test]
    fn yaw_from_axis_angle_extracts_z_component() {
        assert_eq!(
            yaw_from_axis_angle([0.0, 0.0, 1.0, std::f64::consts::FRAC_PI_2]),
            std::f64::consts::FRAC_PI_2
        );
        assert_eq!(yaw_from_axis_angle([0.0, 0.0, 0.0, 1.0]), 0.0);
    }

    #[test]
    fn config_deserializes_spawn_list_from_phoxal_config_json() {
        let json = serde_json::json!({
            "require_native": false,
            "spawn": [
                {"name": "robot-a", "node_string": "Robot { name \"robot-a\" }"},
                {"name": "robot-b", "node_string": "Robot { name \"robot-b\" }"},
            ],
        });

        let config: WebotsSupervisorConfig = serde_json::from_value(json).unwrap();

        assert!(!config.require_native);
        assert_eq!(config.spawn.len(), 2);
        assert_eq!(config.spawn[0].name, "robot-a");
        assert_eq!(config.spawn[0].node_string, "Robot { name \"robot-a\" }");
        assert_eq!(config.spawn[1].name, "robot-b");
    }

    #[test]
    fn config_spawn_defaults_to_empty() {
        let json = serde_json::json!({});
        let config: WebotsSupervisorConfig = serde_json::from_value(json).unwrap();
        assert!(config.spawn.is_empty());
    }

    // The runner deserializes `Self::Config` from JSON `null` when
    // `PHOXAL_CONFIG` is unset (`ParticipantLaunch::config: Option<Value>` ->
    // `None` -> `serde_json::from_value(Value::Null)`); `Option<T>: ParticipantConfig`
    // (the blanket impl in `phoxal::participant::api`) is what makes that a
    // clean `None` -> `unwrap_or_default()` instead of a deserialize error.
    #[test]
    fn config_treats_null_as_absent_and_defaults() {
        let config: Option<WebotsSupervisorConfig> =
            serde_json::from_value(serde_json::Value::Null).unwrap();
        assert!(config.is_none());
        assert!(config.unwrap_or_default().spawn.is_empty());
    }

    #[test]
    fn config_deserializes_an_object_as_present() {
        let config: Option<WebotsSupervisorConfig> =
            serde_json::from_value(serde_json::json!({"require_native": false})).unwrap();
        assert!(!config.unwrap().require_native);
    }

    // `ParticipantConfig::SCHEMA_JSON` is a fixed `"{}"` placeholder (real
    // materialization is a deferred host-side `build.rs` slice - see
    // `phoxal::participant::api`'s module docs), so this asserts directly
    // against `schemars`'s own output for `WebotsSupervisorConfig`, which
    // still derives `phoxal::schemars::JsonSchema` unchanged.
    #[test]
    fn config_schema_includes_spawn_field() {
        let schema = phoxal::schemars::schema_for!(WebotsSupervisorConfig);
        let schema = serde_json::to_string(&schema).unwrap();

        assert!(
            schema.contains("spawn"),
            "config schema should describe the spawn field, got {schema}"
        );
        assert!(
            schema.contains("node_string") && schema.contains("name"),
            "config schema should describe SpawnRobot's fields, got {schema}"
        );
    }

    #[test]
    fn stub_backend_logs_intended_spawns_without_panicking() {
        let spawn = vec![
            SpawnRobot {
                name: "robot-a".to_string(),
                node_string: "Robot { name \"robot-a\" }".to_string(),
            },
            SpawnRobot {
                name: "robot-b".to_string(),
                node_string: "Robot { name \"robot-b\" }".to_string(),
            },
        ];

        // Must not attempt a real Supervisor import - only log the intent.
        let backend = StubBackend::new(&spawn);
        assert_eq!(backend.dt_ns, DEFAULT_DT_NS);
    }

    #[test]
    fn stub_backend_handles_empty_spawn_list() {
        let backend = StubBackend::new(&[]);
        assert_eq!(backend.dt_ns, DEFAULT_DT_NS);
    }
}
