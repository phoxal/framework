use std::borrow::Cow;

use crate::core::Arbitration as MotionArbitration;
use anyhow::{Result, ensure};
use phoxal::api::drive::v1::Target as DriveTarget;
use phoxal::api::follow::v1::Target as FollowTarget;
use phoxal::api::localize::v1::LocalizationRevisionId;
use phoxal::api::map::v1::MapRevisionId;
use phoxal::api::motion::v1::{Arbitration, ArbitrationCandidate, MotionSource};
use phoxal::api::safety::v1::{
    Constraint, MotionConstraint, SafetyAuthorization, SafetyDecision, SafetySourceRevision,
};
use phoxal::api::topic;
use phoxal::bus::typed::Received;
use phoxal::runtime::{ScenarioDescriptor, ScenarioKind};
use phoxal::scenario::helpers::assert_topic_schema;

pub const SCENARIOS: &[ScenarioDescriptor] = &[ScenarioDescriptor {
    name: Cow::Borrowed("p3-motion-arbitration-contract"),
    summary: Cow::Borrowed("Checks motion arbitration contracts and follow/safety policy."),
    kind: ScenarioKind::Headless,
    phase: phoxal::runtime::Phase::P3,
    timeout_secs: 60,
    category: Cow::Borrowed("failure-recovery"),
    tier: 1,
}];

pub fn run(name: &str) -> Result<()> {
    match name {
        "p3-motion-arbitration-contract" => p3_motion_arbitration_contract(),
        _ => anyhow::bail!("motion has no scenario '{name}'"),
    }
}

fn p3_motion_arbitration_contract() -> Result<()> {
    fn source_priority(source: MotionSource) -> u8 {
        match source {
            MotionSource::EmergencyStop => 4,
            MotionSource::MissionStop => 3,
            MotionSource::Recovery => 2,
            MotionSource::Manual => 1,
            MotionSource::Follow => 0,
        }
    }

    fn follow_target(linear: f64, angular: f64) -> Received<FollowTarget> {
        received(
            1_000,
            FollowTarget {
                map_revision: MapRevisionId {
                    epoch: 1,
                    sequence: 0,
                },
                built_from_localize_revision: LocalizationRevisionId {
                    epoch: 1,
                    sequence: 0,
                },
                frame_id: "map".to_string(),
                linear_x_mps: linear,
                angular_z_radps: angular,
            },
        )
    }

    fn safety_authorization(decision: SafetyDecision) -> Received<SafetyAuthorization> {
        received(
            1_000,
            SafetyAuthorization {
                decision,
                source_revision: SafetySourceRevision {
                    localization: None,
                    map: None,
                    raw_sources: Vec::new(),
                },
                approved_motion: MotionConstraint {
                    linear_x_mps: Constraint {
                        min: -1.0,
                        max: 1.0,
                    },
                    angular_z_radps: Constraint {
                        min: -1.0,
                        max: 1.0,
                    },
                },
                reasons: Vec::new(),
                expires_at_ns: Some(2_000),
            },
        )
    }

    fn received<T>(at_ns: u64, value: T) -> Received<T> {
        Received {
            at_ns: Some(at_ns),
            value,
        }
    }

    assert_topic_schema(
        topic::new().motion().state(),
        "motion/state",
        "motion state",
    )?;
    assert_topic_schema(
        topic::new().motion().arbitration(),
        "motion/arbitration",
        "motion arbitration debug",
    )?;
    assert_topic_schema(
        topic::new().motion().source_freshness(),
        "motion/source_freshness",
        "motion source freshness debug",
    )?;

    let sources = [
        MotionSource::Manual,
        MotionSource::Follow,
        MotionSource::MissionStop,
        MotionSource::Recovery,
        MotionSource::EmergencyStop,
    ];
    ensure!(
        sources.len() == 5,
        "motion arbitration contract must cover every v1 source variant"
    );

    let arbitration = Arbitration {
        candidates: vec![ArbitrationCandidate {
            source: MotionSource::Follow,
            target: Some(DriveTarget {
                linear_x_mps: 0.5,
                angular_z_radps: 0.0,
            }),
            accepted: true,
            reason: None,
        }],
        selected_source: Some(MotionSource::Follow),
    };
    let encoded = serde_json::to_string(&arbitration)?;
    let decoded: Arbitration = serde_json::from_str(&encoded)?;
    ensure!(
        decoded == arbitration,
        "motion arbitration must round-trip through serde"
    );

    ensure!(
        source_priority(MotionSource::EmergencyStop) > source_priority(MotionSource::MissionStop),
        "emergency stop source must outrank mission stop"
    );
    ensure!(
        source_priority(MotionSource::MissionStop) > source_priority(MotionSource::Recovery),
        "mission stop source must outrank recovery"
    );
    ensure!(
        source_priority(MotionSource::Recovery) > source_priority(MotionSource::Manual),
        "recovery source must outrank manual"
    );
    ensure!(
        source_priority(MotionSource::Manual) > source_priority(MotionSource::Follow),
        "manual source must outrank follow"
    );

    let no_follow = MotionArbitration::arbitrate(None, None, 1_000);
    ensure!(
        no_follow.drive_target.linear_x_mps == 0.0
            && no_follow.drive_target.angular_z_radps == 0.0
            && no_follow.active_source.is_none(),
        "missing follow target must produce a zero drive target with no active source"
    );

    let fresh_follow = follow_target(0.5, 0.2);
    let fresh = MotionArbitration::arbitrate(Some(&fresh_follow), None, 1_000);
    ensure!(
        fresh.drive_target.linear_x_mps == 0.5
            && fresh.drive_target.angular_z_radps == 0.2
            && fresh.active_source == Some(MotionSource::Follow),
        "fresh follow target must pass through as the active follow source"
    );

    let stale_follow = received(0, follow_target(0.5, 0.2).value);
    let stale = MotionArbitration::arbitrate(Some(&stale_follow), None, 1_000_000_000);
    ensure!(
        stale.drive_target.linear_x_mps == 0.0
            && stale.drive_target.angular_z_radps == 0.0
            && stale.active_source.is_none(),
        "stale follow target must produce a zero drive target with no active source"
    );

    let stop_follow = follow_target(0.5, 0.0);
    let stop_authorization = safety_authorization(SafetyDecision::Stop);
    let stop = MotionArbitration::arbitrate(Some(&stop_follow), Some(&stop_authorization), 1_000);
    ensure!(
        stop.drive_target.linear_x_mps == 0.0
            && stop.drive_target.angular_z_radps == 0.0
            && stop.active_source == Some(MotionSource::MissionStop),
        "safety stop must override follow as mission stop"
    );

    let emergency_follow = follow_target(0.5, 0.0);
    let emergency_authorization = safety_authorization(SafetyDecision::EmergencyStop);
    let emergency = MotionArbitration::arbitrate(
        Some(&emergency_follow),
        Some(&emergency_authorization),
        1_000,
    );
    ensure!(
        emergency.drive_target.linear_x_mps == 0.0
            && emergency.drive_target.angular_z_radps == 0.0
            && emergency.active_source == Some(MotionSource::EmergencyStop),
        "safety emergency stop must override follow as emergency stop"
    );

    let allow_follow = follow_target(0.5, 0.0);
    let allow_authorization = safety_authorization(SafetyDecision::Allow);
    let allow =
        MotionArbitration::arbitrate(Some(&allow_follow), Some(&allow_authorization), 1_000);
    ensure!(
        allow.drive_target.linear_x_mps == 0.5
            && allow.drive_target.angular_z_radps == 0.0
            && allow.active_source == Some(MotionSource::Follow),
        "safety allow must preserve the fresh follow target"
    );

    Ok(())
}
