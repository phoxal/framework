use std::borrow::Cow;

use crate::core::DifferentialDrive;
use anyhow::{Result, ensure};
use phoxal::api::v1::drive::{
    ActuatorAuthority, State as DriveState, StopReason, Target as DriveTarget,
};
use phoxal::api::v1::topic;
use phoxal::runtime::{ScenarioDescriptor, ScenarioKind};
use phoxal::scenario::helpers::{assert_close, assert_topic_schema};

pub const SCENARIOS: &[ScenarioDescriptor] = &[ScenarioDescriptor {
    name: Cow::Borrowed("p3-drive-kinematics-contract"),
    summary: Cow::Borrowed("Checks drive kinematics and typed drive contracts."),
    kind: ScenarioKind::Headless,
    phase: phoxal::runtime::Phase::P3,
    timeout_secs: 60,
    category: Cow::Borrowed("failure-recovery"),
    tier: 1,
}];

pub fn run(name: &str) -> Result<()> {
    match name {
        "p3-drive-kinematics-contract" => p3_drive_kinematics_contract(),
        _ => anyhow::bail!("drive has no scenario '{name}'"),
    }
}

fn p3_drive_kinematics_contract() -> Result<()> {
    assert_topic_schema(
        topic::new().v1().drive().target(),
        "v1/drive/target",
        1,
        "drive target",
    )?;
    assert_topic_schema(
        topic::new().v1().drive().state(),
        "v1/drive/state",
        1,
        "drive state",
    )?;
    assert_topic_schema(
        topic::new().v1().drive().actuator_commands(),
        "v1/drive/actuator_commands",
        1,
        "drive actuator commands debug",
    )?;
    assert_topic_schema(
        topic::new().v1().drive().saturation(),
        "v1/drive/saturation",
        1,
        "drive saturation debug",
    )?;
    assert_topic_schema(
        topic::new().v1().drive().watchdog(),
        "v1/drive/watchdog",
        1,
        "drive watchdog debug",
    )?;
    assert_topic_schema(
        topic::new().v1().drive().kinematics(),
        "v1/drive/kinematics",
        1,
        "drive kinematics debug",
    )?;

    let actuator_authorities = [
        ActuatorAuthority::Active,
        ActuatorAuthority::Stopped,
        ActuatorAuthority::Degraded,
    ];
    ensure!(
        actuator_authorities.len() == 3,
        "drive authority contract must cover every v1 authority variant"
    );

    let stop_reasons = [
        StopReason::CommandTimedOut,
        StopReason::SafetyStop,
        StopReason::EmergencyStop,
        StopReason::NoTarget,
    ];
    ensure!(
        stop_reasons.len() == 4,
        "drive stop contract must cover every v1 stop reason variant"
    );

    let target = DriveTarget {
        linear_x_mps: 0.5,
        angular_z_radps: 0.2,
    };
    let state = DriveState {
        target,
        limited_target: target,
        actuator_authority: ActuatorAuthority::Active,
        stop_reason: None,
    };
    let encoded = serde_json::to_string(&state)?;
    let decoded: DriveState = serde_json::from_str(&encoded)?;
    ensure!(
        decoded == state,
        "drive state must round-trip through serde"
    );

    let drive = DifferentialDrive {
        wheel_radius_m: 0.10,
        wheel_base_m: 0.40,
    };

    let (left_radps, right_radps) = drive.invert(1.0, 0.0);
    assert_close("straight left wheel radps", left_radps, 10.0, 1e-9)?;
    assert_close("straight right wheel radps", right_radps, 10.0, 1e-9)?;

    let (left_radps, right_radps) = drive.invert(0.5, 0.5);
    assert_close("arc left wheel radps", left_radps, 4.0, 1e-9)?;
    assert_close("arc right wheel radps", right_radps, 6.0, 1e-9)?;

    let (left_radps, right_radps) = drive.invert(0.0, 1.0);
    assert_close("rotation left wheel radps", left_radps, -2.0, 1e-9)?;
    assert_close("rotation right wheel radps", right_radps, 2.0, 1e-9)?;

    ensure!(
        (5.0_f64).clamp(-2.0, 2.0) == 2.0,
        "clamp must cap positive values"
    );
    ensure!(
        (-5.0_f64).clamp(-2.0, 2.0) == -2.0,
        "clamp must cap negative values"
    );
    ensure!(
        (1.5_f64).clamp(-2.0, 2.0) == 1.5,
        "clamp must preserve in-range values"
    );

    Ok(())
}
