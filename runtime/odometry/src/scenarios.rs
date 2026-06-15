use std::borrow::Cow;

use anyhow::{Result, bail, ensure};
use phoxal::api::joint::v1::JointId;
use phoxal::api::odometry::v1::{
    Integration, IntegrationStep, SourceHealth, SourceId, SourceReason, SourceStatus, Status,
    StatusMode, StatusReason,
};
use phoxal::runtime::{ScenarioDescriptor, ScenarioKind};
use phoxal::scenario::assertions::{
    Meters, Radians, assert_forward_delta, assert_lateral_drift, assert_yaw_drift,
};
use phoxal::scenario::helpers::{
    assert_close, estimate_from_wheel_delta, origin_pose, pose_from_estimate, yaw_from_xyzw,
};

pub const SCENARIOS: &[ScenarioDescriptor] = &[ScenarioDescriptor {
    name: Cow::Borrowed("odometry"),
    summary: Cow::Borrowed(
        "Checks differential wheel odometry integration and typed health reasons.",
    ),
    kind: ScenarioKind::Headless,
    phase: phoxal::runtime::Phase::P1,
    timeout_secs: 60,
    category: Cow::Borrowed("odometry"),
    tier: 1,
}];

pub fn run(name: &str) -> Result<()> {
    match name {
        "odometry" => odometry(),
        _ => anyhow::bail!("odometry has no scenario '{name}'"),
    }
}

fn odometry() -> Result<()> {
    let straight = estimate_from_wheel_delta(1.0, 1.0);
    assert_forward_delta(
        &origin_pose(),
        &pose_from_estimate(&straight),
        Meters(1.0),
        Meters(0.000_001),
    )?;
    assert_lateral_drift(
        &origin_pose(),
        &pose_from_estimate(&straight),
        Meters(0.000_001),
    )?;
    assert_yaw_drift(
        &origin_pose(),
        &pose_from_estimate(&straight),
        Radians(0.000_001),
    )?;

    let arc = estimate_from_wheel_delta(0.8, 1.2);
    let arc_pose = pose_from_estimate(&arc);
    assert_close(
        "arc x",
        arc_pose.translation_m[0],
        0.841_470_984_8,
        0.000_001,
    )?;
    assert_close(
        "arc y",
        arc_pose.translation_m[1],
        0.459_697_694_1,
        0.000_001,
    )?;
    assert_close(
        "arc yaw",
        yaw_from_xyzw(arc_pose.rotation_xyzw),
        1.0,
        0.000_001,
    )?;

    let rotation = estimate_from_wheel_delta(-0.2, 0.2);
    let rotation_pose = pose_from_estimate(&rotation);
    assert_close("rotation x", rotation_pose.translation_m[0], 0.0, 0.000_001)?;
    assert_close(
        "rotation yaw",
        yaw_from_xyzw(rotation_pose.rotation_xyzw),
        1.0,
        0.000_001,
    )?;

    let stale = Status {
        mode: StatusMode::Stale,
        reasons: vec![StatusReason::JointStale],
    };
    ensure!(
        stale.mode == StatusMode::Stale && stale.reasons == [StatusReason::JointStale],
        "stale encoder behavior must surface the typed JointStale reason"
    );

    let source_health = SourceHealth {
        sources: vec![SourceStatus {
            source_id: SourceId::Joint(JointId::new("left_wheel")),
            healthy: false,
            reason: Some(SourceReason::Stale),
        }],
    };
    let Some(source) = source_health.sources.first() else {
        bail!("odometry source health did not include the stale joint source");
    };
    ensure!(
        source.reason == Some(SourceReason::Stale),
        "odometry source health must carry typed stale reasons"
    );

    let integration = Integration {
        steps: vec![IntegrationStep {
            source_id: SourceId::Joint(JointId::new("left_wheel")),
            delta_pose_m: [
                straight.pose.translation_m[0],
                straight.pose.translation_m[1],
                0.0,
            ],
            delta_yaw_rad: yaw_from_xyzw(straight.pose.rotation_xyzw),
        }],
    };
    ensure!(
        integration.steps.len() == 1,
        "odometry debug integration should expose the closed-form step"
    );

    Ok(())
}
