use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail, ensure};
use phoxal_api_localize::v1::LocalizationMode;
use phoxal_api_mission::v1::{GoalPose, GoalTolerance};
use phoxal_api_motion::v1::ManualCommand;
use phoxal_api_safety::v1::SafetyDecision;
use phoxal_core_engine::DEFAULT_ROBOT_NAMESPACE;
use phoxal_core_engine::presence::Readiness;
use phoxal_core_engine::{RobotIdentity, RobotRuntimeArgs};

use crate::harness::{ScenarioContext, ScenarioEnvironment};

const DEFAULT_ROUTER_ENDPOINT: &str = "tcp/router:7447";
const SCENARIO_CONNECT_TIMEOUT_SECS: u64 = 30;
const TRACKING_WAIT_BUDGET_SECS: f64 = 30.0;
pub const P4_GOAL_XY_M: [f64; 2] = [1.0, 0.0];
pub const P4_GOAL_REACHED_TOLERANCE_M: f64 = 0.30;

pub async fn context_from_args(common: &RobotRuntimeArgs) -> Result<ScenarioContext> {
    let identity = common.identity();
    ScenarioEnvironment::new(
        common
            .robot_router_endpoint
            .clone()
            .unwrap_or_else(|| DEFAULT_ROUTER_ENDPOINT.to_string()),
        identity.robot_namespace,
        identity.robot_id,
    )
    .context("scenario context missing router endpoint, robot id, or robot namespace")?
    .connect(Duration::from_secs(SCENARIO_CONNECT_TIMEOUT_SECS))
    .await
}

pub fn command_deadline(timeout_secs: u64) -> Result<Instant> {
    Instant::now()
        .checked_add(Duration::from_secs(timeout_secs))
        .ok_or_else(|| anyhow!("scenario wallclock timeout overflows"))
}

pub async fn wait_until_tracking(ctx: &ScenarioContext, deadline: Instant) -> Result<()> {
    let mut waited_secs = 0.0_f64;
    loop {
        let localize = ctx.latest_localization_state().await?;
        if localize.data.mode == LocalizationMode::Tracking {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline && waited_secs < TRACKING_WAIT_BUDGET_SECS,
            "localize never reached Tracking under simulator-truth (last mode {:?})",
            localize.data.mode
        );
        ctx.advance_for_secs(0.5).await?;
        waited_secs += 0.5;
    }
}

pub async fn publish_and_advance(
    ctx: &ScenarioContext,
    command: ManualCommand,
    step_secs: f64,
) -> Result<()> {
    ctx.publish_manual_command(command).await?;
    ctx.advance_for_secs(step_secs).await?;
    Ok(())
}

pub fn p4_goal() -> (GoalPose, GoalTolerance) {
    (
        GoalPose::Pose2 {
            frame_id: "map".into(),
            map_revision: None,
            xy_m: P4_GOAL_XY_M,
            yaw_rad: 0.0,
        },
        GoalTolerance {
            pos_m: 0.20,
            yaw_rad: Some(0.2),
        },
    )
}

pub async fn wait_until_robot_reaches(
    ctx: &ScenarioContext,
    deadline: Instant,
    goal_xy_m: [f64; 2],
    tolerance_m: f64,
) -> Result<()> {
    loop {
        let pose = ctx.simulation_pose().await?;
        let dx = pose.data.translation_m[0] - goal_xy_m[0];
        let dy = pose.data.translation_m[1] - goal_xy_m[1];
        if (dx * dx + dy * dy).sqrt() <= tolerance_m {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "robot did not reach goal {:?} (last pose [{:.2}, {:.2}])",
            goal_xy_m,
            pose.data.translation_m[0],
            pose.data.translation_m[1]
        );
        ctx.advance_for_secs(0.5).await?;
    }
}

pub async fn wait_for_mission_state(
    ctx: &ScenarioContext,
    deadline: Instant,
    predicate: impl Fn(&phoxal_api_mission::v1::State) -> bool,
    what: &str,
) -> Result<()> {
    loop {
        let state = ctx.latest_mission_state().await?;
        if predicate(&state.data) {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "mission never reached {what} (last mode {:?})",
            state.data.mode
        );
        ctx.advance_for_secs(0.5).await?;
    }
}

pub async fn wait_for_runtime_readiness(
    ctx: &ScenarioContext,
    deadline: Instant,
    runtime_id: &str,
    predicate: impl Fn(Option<Readiness>) -> bool,
    what: &str,
) -> Result<()> {
    loop {
        let summary = ctx.latest_presence_summary().await?;
        let readiness = summary
            .data
            .runtimes
            .iter()
            .find(|runtime| runtime.runtime_id.0 == runtime_id)
            .map(|runtime| runtime.readiness);
        if predicate(readiness) {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "timed out waiting for runtime '{runtime_id}' to be {what} (last readiness {readiness:?})"
        );
        ctx.advance_for_secs(1.0).await?;
    }
}

pub async fn wait_for_safety_decision(
    ctx: &ScenarioContext,
    deadline: Instant,
    predicate: impl Fn(SafetyDecision) -> bool,
    what: &str,
) -> Result<()> {
    loop {
        let state = ctx.latest_safety_state().await?;
        if predicate(state.data.decision) {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "safety never reached {what} (last decision {:?})",
            state.data.decision
        );
        ctx.advance_for_secs(0.5).await?;
    }
}

pub fn kill_service(common: &RobotRuntimeArgs, service: &str) -> Result<()> {
    compose_service(common, service, "kill")
}

pub fn restart_service(common: &RobotRuntimeArgs, service: &str) -> Result<()> {
    compose_service(common, service, "start")
}

fn compose_service(common: &RobotRuntimeArgs, service: &str, action: &str) -> Result<()> {
    let identity = common.identity();
    let compose_path = dev_compose_path(&identity)?;
    let status = Command::new("docker")
        .arg("compose")
        .arg("-f")
        .arg(&compose_path)
        .arg(action)
        .arg(service)
        .status()
        .with_context(|| format!("failed to run docker compose {action} {service}"))?;
    if status.success() {
        return Ok(());
    }
    bail!(
        "docker compose {action} {service} in {} failed with status {status}",
        compose_path.display()
    )
}

fn dev_compose_path(identity: &RobotIdentity) -> Result<PathBuf> {
    Ok(std::env::current_dir()?
        .join("dist")
        .join(DEFAULT_ROBOT_NAMESPACE)
        .join(identity.host_name())
        .join("docker-compose.yml"))
}
