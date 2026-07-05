//! Webots supervisor simulator artifact.
//!
//! The world/session authority: owns the Webots Supervisor API and publishes
//! the simulator-owned `simulation::*` topics (clock, robot pose, contact),
//! applying `simulation::Control` (pause/resume/reset). This binary never
//! binds component devices (motor, encoder, IMU, ...) - that is
//! `phoxal-simulator-webots-controller`'s job (see `simulator/webots-controller`).
//!
//! Robot-spawning (instantiating robot nodes into the world) is NOT
//! implemented here - see the `SPAWN SEAM` comment on [`NativeBackend`] below
//! for where that authority attaches in P6-2 (sim-launch).

use anyhow::{Result, anyhow, bail};
use phoxal::prelude::*;
use phoxal_api::y2026_1 as api;

const DEFAULT_DT_NS: u64 = 10_000_000;

#[derive(Clone, Debug, serde::Deserialize, phoxal::schemars::JsonSchema)]
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

#[derive(phoxal::Simulator)]
#[phoxal(id = "webots-supervisor", api = y2026_1, config = Option<WebotsSupervisorConfig>)]
struct WebotsSupervisorSimulator {
    running: bool,
    epoch: u64,
    step_index: u64,
    time_ns: u64,
    dt_ns: u64,
    backend: Backend,
    clock: Publisher<api::simulation::Clock>,
    control: Subscriber<api::simulation::Control>,
    robot_pose: Publisher<api::simulation::RobotPose>,
    contact: Publisher<api::simulation::Contact>,
}

#[phoxal::behavior]
impl WebotsSupervisorSimulator {
    #[setup]
    async fn setup(
        ctx: &mut SetupContext<Self>,
        config: Option<WebotsSupervisorConfig>,
    ) -> Result<Self> {
        let config = config.unwrap_or_default();
        let cap = ctx.owner_capability();

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

        let backend = Backend::open(&config)?;
        let dt_ns = backend.dt_ns();

        tracing::info!(
            target: "simulator_webots_supervisor",
            webots_runtime_linked = webots_rs::WEBOTS_RUNTIME_LINKED,
            "webots supervisor simulator ready"
        );

        Ok(Self {
            running: true,
            epoch: 0,
            step_index: 0,
            time_ns: 0,
            dt_ns,
            backend,
            clock,
            control,
            robot_pose,
            contact,
        })
    }

    #[step(hz = 100)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        self.apply_control_commands()?;

        if self.running {
            let outputs = self.backend.advance()?;
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
            tracing::trace!(target: "simulator_webots_supervisor", time_ns = self.time_ns, "simulation paused");
        }

        Ok(())
    }

    #[shutdown]
    async fn shutdown(&mut self, _ctx: ShutdownContext) -> Result<()> {
        Ok(())
    }
}

impl WebotsSupervisorSimulator {
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

    async fn publish_outputs(&self, at: LogicalTime, outputs: BackendOutput) -> Result<()> {
        if let Some(pose) = outputs.robot_pose {
            self.robot_pose.publish_at(at, pose).await?;
        }
        if let Some(contact) = outputs.contact {
            self.contact.publish_at(at, contact).await?;
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
}

struct StubBackend {
    dt_ns: u64,
}

impl StubBackend {
    fn new() -> Self {
        Self {
            dt_ns: DEFAULT_DT_NS,
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
/// `webots.step()`, and the Supervisor API (world reset, self pose/contact).
///
/// SPAWN SEAM (P6-2 / sim-launch): robot-spawning - instantiating robot nodes
/// into the world from a launch plan - is not implemented in this slice. This
/// backend only reads/controls a world that Webots has already loaded with
/// its robot nodes staged (matching today's monolith behavior). When P6-2
/// lands spawn authority, it attaches here: a `spawn_robots(&self, plan: &..)`
/// step during `NativeBackend::new` (or a dedicated setup phase before the
/// step loop starts), using `self.supervisor` to import/insert robot nodes
/// before the first `webots.step()` call below.
struct NativeBackend {
    webots: webots_rs::Webots,
    supervisor: webots_rs::Supervisor,
    dt_ns: u64,
    step_ms: i32,
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

        // SPAWN SEAM: robot-spawning attaches here in P6-2, before the first
        // `webots.step()` call in `advance`.

        Ok(Self {
            webots,
            supervisor,
            dt_ns,
            step_ms,
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

    fn reset(&mut self) -> Result<()> {
        self.supervisor
            .simulation_reset()
            .map_err(|error| anyhow!(error))
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
    family: &'static str,
    topic: &'static str,
    direction: phoxal::participant::Direction,
    schema_id: &'static str,
}

#[cfg(test)]
fn contract_mappings() -> Vec<ContractMapping> {
    vec![
        mapping::<api::simulation::Clock>(phoxal::participant::Direction::Publish),
        mapping::<api::simulation::Control>(phoxal::participant::Direction::Subscribe),
        mapping::<api::simulation::RobotPose>(phoxal::participant::Direction::Publish),
        mapping::<api::simulation::Contact>(phoxal::participant::Direction::Publish),
    ]
}

#[cfg(test)]
fn mapping<B: phoxal::bus::ContractBody>(
    direction: phoxal::participant::Direction,
) -> ContractMapping {
    ContractMapping {
        family: B::FAMILY,
        topic: B::TOPIC,
        direction,
        schema_id: B::SCHEMA_ID,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emit_apis_reports_exactly_the_four_simulation_contracts() {
        let json = phoxal::participant::emit_apis_json::<WebotsSupervisorSimulator>();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["artifact"]["id"], "webots-supervisor");
        assert_eq!(value["artifact"]["kind"], "simulator");
        assert_eq!(value["participant_class"], "checked");
        let contracts = value["required_contracts"].as_array().unwrap();

        let expected = contract_mappings();
        assert_eq!(
            contracts.len(),
            expected.len(),
            "supervisor must emit exactly the 4 simulation contracts, got {contracts:?}"
        );
        for mapping in &expected {
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
                mapping.family
            );
        }

        // Never leaks the controller's component::* contracts.
        assert!(
            !contracts.iter().any(|contract| contract["family"]
                .as_str()
                .unwrap_or_default()
                .starts_with("component::")),
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
}
