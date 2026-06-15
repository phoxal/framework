use std::collections::BTreeSet;

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use phoxal::api::simulation::clock::{self as clock_contract, v1::Clock};
use phoxal::api::simulation::pose::{self as pose_contract, v1::Pose};
use phoxal::api::simulation::reset::{
    self as reset_contract,
    v1::{Request as ResetRequest, Response as ResetResponse},
};
use phoxal::api::simulation::status::{self as status_contract, v1::Status};
use phoxal::api::topic;
use phoxal::bus::builder::Builder;
use phoxal::bus::topic::{PubSub, Topic};
use phoxal::bus::typed::TypedTopicQuery;
use phoxal::util::init_tracing;
use tracing::{info, warn};
use webots_rs::Supervisor;
use webots_rs::Webots;
use webots_rs::supervisor::Node;

const ROBOT_DEF_PREFIX: &str = "RF_ROBOT_";
const WORLD_FRAME_ID: &str = "world";

#[derive(Clone, Debug, Parser)]
struct Cli {
    #[arg(long = "robot-id", env = "ROBOT_ID")]
    robot_id: String,

    #[arg(long = "robot-namespace", env = "ROBOT_NAMESPACE")]
    robot_namespace: String,

    #[arg(
        long = "robot-router-endpoint",
        env = "ROBOT_ROUTER_ENDPOINT",
        required = true
    )]
    robot_router_endpoint: String,

    #[arg(
        long = "robot-connect-timeout-ms",
        env = "ROBOT_CONNECT_TIMEOUT_MS",
        default_value_t = 5_000_u64
    )]
    robot_connect_timeout_ms: u64,

    #[arg(
        long = "robot-connect-retries",
        env = "ROBOT_CONNECT_RETRIES",
        default_value_t = 60_u32
    )]
    robot_connect_retries: u32,
}

struct RobotPoseTarget {
    robot_id: String,
    def_name: String,
    topic: Topic<PubSub<pose_contract::Pose>>,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing()?;

    let cli = Cli::parse();
    info!(
        version = env!("CARGO_PKG_VERSION"),
        robot_id = %cli.robot_id,
        robot_router_endpoint = %cli.robot_router_endpoint,
        robot_namespace = %cli.robot_namespace,
        "webots supervisor startup context resolved"
    );

    let builder = Builder::new(cli.robot_router_endpoint.clone())
        .with_connect_timeout(std::time::Duration::from_millis(
            cli.robot_connect_timeout_ms,
        ))
        .with_connect_retries(cli.robot_connect_retries)
        .with_prefix(cli.robot_namespace.clone());

    let bus = builder.connect().await?;
    let webots = Webots::new().map_err(|error| anyhow!(error))?;
    let supervisor = webots.get_supervisor();
    let step_ms = webots
        .get_basic_time_step()
        .map_err(|error| anyhow!(error))? as i32;
    let dt_ns = std::time::Duration::from_millis(step_ms as u64).as_nanos() as u64;
    let simulation_clock_topic = topic::new().simulation().clock();
    let simulation_status_topic = topic::new().simulation().status();
    let robot_pose_targets = discover_robot_pose_targets(&supervisor).await?;
    if robot_pose_targets.is_empty() {
        warn!(
            def_prefix = ROBOT_DEF_PREFIX,
            "webots supervisor found no staged robot nodes for pose publication"
        );
    } else {
        let robot_ids = robot_pose_targets
            .iter()
            .map(|target| target.robot_id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        info!(
            robots = robot_ids,
            count = robot_pose_targets.len(),
            "webots supervisor pose publishers ready"
        );
    }

    let (reset_tx, mut reset_rx) = tokio::sync::mpsc::channel::<
        TypedTopicQuery<reset_contract::Request, reset_contract::Response>,
    >(8);
    let reset_bus = bus.clone();
    tokio::spawn(async move {
        let reset_topic = topic::new().simulation().reset();
        let reset_server = match reset_bus.responder(&reset_topic).await {
            Ok(server) => server,
            Err(error) => {
                warn!(error = %error, "failed to serve simulation reset command");
                return;
            }
        };

        loop {
            match reset_server.recv().await {
                Ok(request) => {
                    if reset_tx.send(request).await.is_err() {
                        warn!("reset request channel closed");
                        return;
                    }
                }
                Err(error) => {
                    warn!(error = %error, "simulation reset command responder failed");
                    return;
                }
            }
        }
    });

    let mut shutdown = Box::pin(tokio::signal::ctrl_c());
    let mut epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut step = 0_u64;
    let mut time_ns = 0_u64;

    info!(
        step_ms,
        dt_ns, epoch, "webots supervisor stepping loop started"
    );

    loop {
        tokio::select! {
            _ = &mut shutdown => {
                info!("received Ctrl+C, shutting down webots supervisor");
                return Ok(());
            }
            _ = tokio::task::yield_now() => {}
        }

        while let Ok(request) = reset_rx.try_recv() {
            let reset_contract::Request::V1(ResetRequest {}) = match request.request() {
                Ok(request) => request,
                Err(error) => {
                    warn!(error = %error, "failed to decode simulation reset request");
                    continue;
                }
            };
            match supervisor.simulation_reset() {
                Ok(()) => {
                    epoch = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    step = 0;
                    time_ns = 0;
                    if let Err(error) = request
                        .reply(&reset_contract::Response::V1(ResetResponse { epoch }))
                        .await
                    {
                        warn!(error = %error, "failed to acknowledge simulation reset command");
                    } else {
                        info!(epoch, "accepted simulation reset command");
                    }
                }
                Err(error) => {
                    warn!(error = %error, "failed to reset simulation");
                    // Intentionally do not reply on failure; query caller gets timeout/error.
                }
            }
        }

        let stepped = webots.step(step_ms).map_err(|error| anyhow!(error))?;
        if !stepped {
            info!("webots requested supervisor shutdown");
            return Ok(());
        }

        step = step.saturating_add(1);
        time_ns = time_ns.saturating_add(dt_ns);
        let clock = Clock::new(epoch, step, time_ns, dt_ns);
        bus.publish(
            &simulation_clock_topic,
            time_ns,
            &clock_contract::Clock::V1(clock),
        )
        .await?;
        bus.publish(
            &simulation_status_topic,
            time_ns,
            &status_contract::Status::V1(Status {
                time_ns,
                epoch,
                step,
                dt_ns,
            }),
        )
        .await?;

        for target in &robot_pose_targets {
            let node = Node::from_def(&target.def_name).map_err(|error| anyhow!(error))?;
            let pose = read_pose(&node)
                .with_context(|| format!("failed to read pose for robot '{}'", target.robot_id))?;
            bus.publish(&target.topic, time_ns, &pose_contract::Pose::V1(pose))
                .await?;
        }
    }
}

async fn discover_robot_pose_targets(supervisor: &Supervisor) -> Result<Vec<RobotPoseTarget>> {
    let root = supervisor.get_root().map_err(|error| anyhow!(error))?;
    let children = root
        .field("children")
        .map_err(|error| anyhow!(error))
        .context("failed to read Webots root children")?;
    let mut targets = Vec::new();
    let mut robot_ids = BTreeSet::new();
    let mut def_names = BTreeSet::new();

    for index in 0..children.count().map_err(|error| anyhow!(error))? {
        let child = children.mf_node(index).map_err(|error| anyhow!(error))?;
        let Some(identity) = robot_identity(&child)? else {
            continue;
        };
        if !def_names.insert(identity.def_name.clone()) {
            bail!(
                "staged Webots robot DEF '{}' is duplicated",
                identity.def_name
            );
        }
        if !robot_ids.insert(identity.robot_id.clone()) {
            bail!(
                "staged Webots robot id '{}' is duplicated",
                identity.robot_id
            );
        }
        let topic = topic::new().simulation().robot(&identity.robot_id).pose();
        targets.push(RobotPoseTarget {
            robot_id: identity.robot_id,
            def_name: identity.def_name,
            topic,
        });
    }

    Ok(targets)
}

struct RobotIdentity {
    robot_id: String,
    def_name: String,
}

fn robot_identity(node: &Node) -> Result<Option<RobotIdentity>> {
    let Ok(def_name) = node.def() else {
        return Ok(None);
    };
    if !is_robot_def_name(&def_name) {
        return Ok(None);
    }

    let robot_id = node
        .field("name")
        .map_err(|error| anyhow!(error))?
        .sf_string()
        .map_err(|error| anyhow!(error))
        .with_context(|| format!("failed to read robot id from Webots node '{def_name}'"))?;
    validate_robot_id(&robot_id, &def_name)?;

    Ok(Some(RobotIdentity { robot_id, def_name }))
}

fn is_robot_def_name(def_name: &str) -> bool {
    def_name
        .strip_prefix(ROBOT_DEF_PREFIX)
        .is_some_and(|suffix| !suffix.is_empty())
}

fn validate_robot_id(robot_id: &str, def_name: &str) -> Result<()> {
    if robot_id.trim().is_empty() {
        bail!("staged Webots node '{def_name}' has an empty robot id");
    }
    if robot_id.contains('/') {
        bail!("staged Webots node '{def_name}' has robot id '{robot_id}' containing '/'");
    }
    Ok(())
}

fn read_pose(node: &Node) -> Result<Pose> {
    let translation_m = node
        .field("translation")
        .map_err(|error| anyhow!(error))?
        .sf_vec3f()
        .map_err(|error| anyhow!(error))
        .context("failed to read Webots translation field")?;
    let rotation = node
        .field("rotation")
        .map_err(|error| anyhow!(error))?
        .sf_rotation()
        .map_err(|error| anyhow!(error))
        .context("failed to read Webots rotation field")?;

    Ok(Pose {
        frame_id: WORLD_FRAME_ID.to_string(),
        translation_m,
        rotation_xyzw: axis_angle_to_xyzw(rotation),
    })
}

fn axis_angle_to_xyzw(rotation: [f64; 4]) -> [f64; 4] {
    let [x, y, z, angle] = rotation;
    let axis_norm = (x.mul_add(x, y.mul_add(y, z * z))).sqrt();
    if axis_norm <= f64::EPSILON {
        return [0.0, 0.0, 0.0, 1.0];
    }

    let half_angle = angle * 0.5;
    let scale = half_angle.sin() / axis_norm;
    [x * scale, y * scale, z * scale, half_angle.cos()]
}

#[cfg(test)]
mod tests {
    use super::{axis_angle_to_xyzw, is_robot_def_name, validate_robot_id};

    #[test]
    fn robot_def_names_require_prefix_and_suffix() {
        assert!(is_robot_def_name("RF_ROBOT_ROBOT_V1"));
        assert!(!is_robot_def_name("ROBOT_V1"));
        assert!(!is_robot_def_name("RF_ROBOT_"));
    }

    #[test]
    fn robot_ids_must_be_valid_topic_segments() {
        assert!(validate_robot_id("robot-v1", "RF_ROBOT_ROBOT_V1").is_ok());
        assert!(validate_robot_id("", "RF_ROBOT_ROBOT_V1").is_err());
        assert!(validate_robot_id("   ", "RF_ROBOT_ROBOT_V1").is_err());
        assert!(validate_robot_id("fleet/robot-v1", "RF_ROBOT_ROBOT_V1").is_err());
    }

    #[test]
    fn axis_angle_to_xyzw_handles_identity_rotation() {
        assert_eq!(
            axis_angle_to_xyzw([0.0, 0.0, 1.0, 0.0]),
            [0.0, 0.0, 0.0, 1.0]
        );
        assert_eq!(
            axis_angle_to_xyzw([0.0, 0.0, 0.0, 1.0]),
            [0.0, 0.0, 0.0, 1.0]
        );
    }

    #[test]
    fn axis_angle_to_xyzw_normalizes_axis() {
        let quaternion = axis_angle_to_xyzw([0.0, 0.0, 2.0, std::f64::consts::FRAC_PI_2]);

        assert_close(quaternion[0], 0.0);
        assert_close(quaternion[1], 0.0);
        assert_close(quaternion[2], std::f64::consts::FRAC_1_SQRT_2);
        assert_close(quaternion[3], std::f64::consts::FRAC_1_SQRT_2);
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= 1.0e-12,
            "actual {actual} != expected {expected}"
        );
    }
}
