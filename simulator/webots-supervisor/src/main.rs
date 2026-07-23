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
//! spawn authority. The spawn set is obtained reliably through the
//! `simulation::spawn` query before the first simulation step.

use anyhow::{Result, anyhow, bail};
use phoxal::api;
use phoxal::prelude::*;

const DEFAULT_DT_NS: u64 = 10_000_000;

#[derive(Clone, Debug, serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
struct WebotsSupervisorConfig {
    #[serde(default = "default_require_native")]
    require_native: bool,
}

impl Default for WebotsSupervisorConfig {
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
struct Api {
    clock: Publisher<api::simulation::Clock>,
    control: Subscriber<api::simulation::Control>,
    robot_pose: Publisher<api::simulation::RobotPose>,
    contact: Publisher<api::simulation::Contact>,
    // Served by privileged phoxal-cli orchestration outside the checked
    // participant graph.
    #[phoxal(external)]
    spawn: Querier<api::simulation::SpawnRequest, api::simulation::SpawnSet>,
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
        let spawn = ctx.querier(api::topic::new().simulation().spawn()).await?;

        let mut backend = Backend::open(&config)?;
        let spawn_set = query_spawn_set(&spawn).await;
        backend.spawn(&spawn_set.robots)?;
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
                spawn,
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

            // Publishing is the advancement signal. When the world is paused
            // there is no new clock sample; consumers simply remain at their
            // last logical time until the next completed Webots step.
            api.clock
                .publish_at(
                    at,
                    api::simulation::Clock {
                        now_ns: self.time_ns,
                        step: self.step_index,
                    },
                )
                .await?;
        } else {
            self.dt_ns = self.dt_ns.max(step.dt_ns());
        }

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

async fn query_spawn_set(
    spawn: &Querier<api::simulation::SpawnRequest, api::simulation::SpawnSet>,
) -> api::simulation::SpawnSet {
    loop {
        match spawn
            .query(api::simulation::SpawnRequest {
                known_revision: None,
            })
            .await
        {
            Ok(spawn_set) => return spawn_set,
            Err(error) => {
                tracing::warn!(
                    target: "simulator_webots_supervisor",
                    %error,
                    "spawn set query failed; retrying"
                );
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
        }
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
            return Ok(Self::Native(Box::new(NativeBackend::new()?)));
        }

        if config.require_native {
            bail!(
                "Webots runtime is not linked into this build. Rebuild with WEBOTS_HOME pointing at a Webots installation, or set PHOXAL_CONFIG require_native=false for metadata-only contract tests."
            );
        }

        Ok(Self::Stub(StubBackend::new()))
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

    fn spawn(&mut self, spawn: &[api::simulation::RobotSpawn]) -> Result<()> {
        match self {
            Self::Native(backend) => backend.spawn(spawn),
            Self::Stub(backend) => {
                backend.spawn(spawn);
                Ok(())
            }
        }
    }
}

struct StubBackend {
    dt_ns: u64,
    spawn: Vec<api::simulation::RobotSpawn>,
}

impl StubBackend {
    fn new() -> Self {
        Self {
            dt_ns: DEFAULT_DT_NS,
            spawn: Vec::new(),
        }
    }

    fn spawn(&mut self, spawn: &[api::simulation::RobotSpawn]) {
        for entry in spawn {
            if self
                .spawn
                .iter()
                .any(|spawned| spawned.robot_id == entry.robot_id)
            {
                continue;
            }
            tracing::info!(
                target: "simulator_webots_supervisor",
                robot = %entry.robot_id,
                node_string_len = entry.node_string.len(),
                "stub backend: would spawn robot (no Webots runtime linked)"
            );
            self.spawn.push(entry.clone());
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

    fn reset(&mut self) {
        for entry in &self.spawn {
            tracing::info!(
                target: "simulator_webots_supervisor",
                robot = %entry.robot_id,
                "stub backend: would re-spawn robot after reset"
            );
        }
    }
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
    spawn: Vec<api::simulation::RobotSpawn>,
}

impl NativeBackend {
    fn new() -> Result<Self> {
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

        Ok(Self {
            webots,
            supervisor,
            dt_ns,
            step_ms,
            spawn: Vec::new(),
        })
    }

    fn spawn(&mut self, spawn: &[api::simulation::RobotSpawn]) -> Result<()> {
        for entry in spawn {
            if self
                .spawn
                .iter()
                .any(|spawned| spawned.robot_id == entry.robot_id)
            {
                continue;
            }
            spawn_robots(&self.supervisor, std::slice::from_ref(entry))?;
            self.spawn.push(entry.clone());
        }
        Ok(())
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
        // Webots deprecated `wb_supervisor_node_get_number_of_contact_points`
        // in favor of the single call that returns the points and their
        // count. `webots-rs::Node::contact_points` wraps that replacement API.
        let count = node
            .contact_points(true)
            .map_err(|error| anyhow!(error))?
            .len();
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
fn spawn_robots(
    supervisor: &webots_rs::Supervisor,
    spawn: &[api::simulation::RobotSpawn],
) -> Result<()> {
    if spawn.is_empty() {
        return Ok(());
    }

    let root = supervisor.get_root().map_err(|error| anyhow!(error))?;
    let children = root.field("children").map_err(|error| anyhow!(error))?;

    for entry in spawn {
        children
            .import_mf_node_from_string(-1, &entry.node_string)
            .map_err(|error| anyhow!("failed to spawn robot '{}': {error}", entry.robot_id))?;
        tracing::info!(
            target: "simulator_webots_supervisor",
            robot = %entry.robot_id,
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
        mapping::<api::simulation::SpawnRequest>(ContractRole::Ask),
        mapping::<api::simulation::SpawnSet>(ContractRole::Ask),
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
    use phoxal::bus::{Codec, ContractBody, DEFAULT_QUERY_TIMEOUT, MessagePack};
    use phoxal::participant::{Participant, ParticipantApi};
    use phoxal::raw::{Bus, BusConfig};

    #[test]
    fn api_declares_simulation_runtime_and_spawn_contracts() {
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
            "supervisor must declare exactly the simulation runtime and spawn contracts, got {contracts:?}"
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
        assert!(
            <Api as ParticipantApi>::__CONTRACTS_JSON.contains(
                r#""version":"v0.2","contract":"simulation::SpawnRequest","external":true"#
            ),
            "spawn ask must be marked external because phoxal-cli serves it outside the checked participant graph; got {}",
            <Api as ParticipantApi>::__CONTRACTS_JSON
        );

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
    fn config_rejects_spawn_field() {
        let error = serde_json::from_value::<WebotsSupervisorConfig>(serde_json::json!({
            "require_native": false,
            "spawn": [{"robot_id": "robot-a", "node_string": "Robot {}"}],
        }))
        .unwrap_err();

        assert!(error.to_string().contains("unknown field `spawn`"));
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
        assert!(config.unwrap_or_default().require_native);
    }

    #[test]
    fn config_deserializes_an_object_as_present() {
        let config: Option<WebotsSupervisorConfig> =
            serde_json::from_value(serde_json::json!({"require_native": false})).unwrap();
        assert!(!config.unwrap().require_native);
    }

    #[test]
    fn config_schema_excludes_spawn_field() {
        let schema =
            <WebotsSupervisorConfig as phoxal::participant::ParticipantConfig>::SCHEMA_JSON;

        assert!(
            !schema.contains("spawn") && !schema.contains("node_string"),
            "config schema must not expose spawn data, got {schema}"
        );
    }

    #[test]
    fn stub_backend_import_is_idempotent_and_reset_keeps_cached_set() {
        let spawn = vec![
            api::simulation::RobotSpawn {
                robot_id: "robot-a".to_string(),
                node_string: "Robot { name \"robot-a\" }".to_string(),
            },
            api::simulation::RobotSpawn {
                robot_id: "robot-b".to_string(),
                node_string: "Robot { name \"robot-b\" }".to_string(),
            },
        ];

        let mut backend = StubBackend::new();
        backend.spawn(&spawn);
        backend.spawn(&spawn);
        backend.reset();

        assert_eq!(backend.dt_ns, DEFAULT_DT_NS);
        assert_eq!(backend.spawn.len(), 2);
        assert_eq!(backend.spawn[0].robot_id, "robot-a");
        assert_eq!(backend.spawn[1].robot_id, "robot-b");
    }

    #[test]
    fn stub_backend_starts_with_empty_spawn_set() {
        let backend = StubBackend::new();
        assert_eq!(backend.dt_ns, DEFAULT_DT_NS);
        assert!(backend.spawn.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_query_retries_until_responder_is_declared() {
        let bus = Bus::open(BusConfig::in_process("test", "spawn-retry"))
            .await
            .unwrap();
        let topic = api::topic::new().simulation().spawn();
        let querier = Querier::<api::simulation::SpawnRequest, api::simulation::SpawnSet>::new(
            bus.clone(),
            &topic,
            DEFAULT_QUERY_TIMEOUT,
        )
        .unwrap();

        let server_bus = bus.clone();
        let server_task = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let server = server_bus
                .declare_server(<api::simulation::SpawnRequest as ContractBody>::TOPIC)
                .await
                .unwrap();
            let incoming = server.recv().await.unwrap();
            let request: api::simulation::SpawnRequest =
                MessagePack::decode(&incoming.request_bytes().unwrap()).unwrap();
            assert_eq!(request.known_revision, None);
            let response = MessagePack::encode(&api::simulation::SpawnSet {
                revision: 1,
                robots: vec![api::simulation::RobotSpawn {
                    robot_id: "robot-a".to_string(),
                    node_string: "Robot { name \"robot-a\" }".to_string(),
                }],
            })
            .unwrap();
            incoming.reply(&server_bus, response).await.unwrap();
        });

        let spawn_set = query_spawn_set(&querier).await;
        assert_eq!(spawn_set.revision, 1);
        assert_eq!(spawn_set.robots.len(), 1);
        assert_eq!(spawn_set.robots[0].robot_id, "robot-a");

        server_task.await.unwrap();
        bus.close().await.unwrap();
    }
}
