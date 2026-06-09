use std::borrow::Cow;
use std::collections::BTreeMap;
use std::time::Instant;

use crate::core::{EmergencyStopInputs, EvaluationOutcome};
use anyhow::{Result, ensure};
use phoxal::api::component::v1::capability::range;
use phoxal::api::localize::v1::{
    LocalizationMode, LocalizationSource, LocalizationState, LocalizationStatus,
};
use phoxal::api::safety::v1::{
    Constraint, MotionConstraint, SafetyAuthorization, SafetyDecision, SafetyReason,
    SafetyReasonCode, SafetySourceRevision, State as SafetyState,
};
use phoxal::bus::pubsub::Stamped;
use phoxal::runtime::RobotRuntimeArgs;
use phoxal::runtime::runtime::{ScenarioDescriptor, ScenarioKind};
use phoxal::scenario::harness::ScenarioContext;
use phoxal::scenario::helpers::assert_schema;
use phoxal::scenario::webots::{
    command_deadline, context_from_args, kill_service, restart_service, wait_for_safety_decision,
    wait_until_tracking,
};

const SAFETY_LOCALIZATION_TARGET: &str = "localize";

pub const SCENARIOS: &[ScenarioDescriptor] = &[
    ScenarioDescriptor {
        name: Cow::Borrowed("p3-safety-decision-policy"),
        summary: Cow::Borrowed(
            "Checks safety decision priority, serde contract, and core evaluation policy.",
        ),
        kind: ScenarioKind::Headless,
        phase: phoxal::runtime::runtime::Phase::P3,
        timeout_secs: 60,
        category: Cow::Borrowed("safety"),
        tier: 1,
    },
    ScenarioDescriptor {
        name: Cow::Borrowed("p3-safety-localization-modes"),
        summary: Cow::Borrowed("Checks safety authority degrades on localization loss."),
        kind: ScenarioKind::Webots {
            world: Cow::Borrowed("ArenaWorld"),
        },
        phase: phoxal::runtime::runtime::Phase::P3,
        timeout_secs: 120,
        category: Cow::Borrowed("safety"),
        tier: 2,
    },
    ScenarioDescriptor {
        name: Cow::Borrowed("p3-safety-range-sensor-staleness"),
        summary: Cow::Borrowed("Checks stale low-rate range evidence stops motion."),
        kind: ScenarioKind::Webots {
            world: Cow::Borrowed("ArenaWorld"),
        },
        phase: phoxal::runtime::runtime::Phase::P3,
        timeout_secs: 120,
        category: Cow::Borrowed("safety"),
        tier: 2,
    },
];

pub async fn run(name: &str, common: &RobotRuntimeArgs) -> Result<()> {
    match name {
        "p3-safety-decision-policy" => p3_safety_decision_policy(),
        "p3-safety-localization-modes" => {
            let ctx = webots_context(common).await?;
            assert_p3_safety_localization_modes(&ctx, common, deadline_for(name)?).await
        }
        "p3-safety-range-sensor-staleness" => {
            let ctx = webots_context(common).await?;
            assert_p3_safety_range_sensor_staleness(&ctx, deadline_for(name)?).await
        }
        _ => anyhow::bail!("safety has no scenario '{name}'"),
    }
}

async fn webots_context(common: &RobotRuntimeArgs) -> Result<ScenarioContext> {
    let ctx = context_from_args(common).await?;
    ctx.reset_simulation().await?;
    Ok(ctx)
}

fn deadline_for(name: &str) -> Result<Instant> {
    let timeout_secs = SCENARIOS
        .iter()
        .find(|scenario| scenario.name.as_ref() == name)
        .map(|scenario| scenario.timeout_secs)
        .unwrap_or(60);
    command_deadline(timeout_secs)
}

fn p3_safety_decision_policy() -> Result<()> {
    fn decision_priority(decision: SafetyDecision) -> u8 {
        match decision {
            SafetyDecision::EmergencyStop => 4,
            SafetyDecision::Stop => 3,
            SafetyDecision::UnknownConservative => 2,
            SafetyDecision::Slow => 1,
            SafetyDecision::Allow => 0,
        }
    }

    fn localize_state(mode: LocalizationMode) -> Stamped<LocalizationState> {
        Stamped::new(
            1_000,
            LocalizationState {
                mode,
                source: LocalizationSource::DeadReckoning,
                revision: None,
                pose: None,
                velocity: None,
                covariance: None,
                imu_bias: None,
                status: LocalizationStatus {
                    healthy: true,
                    reasons: Vec::new(),
                },
                valid_at_ns: None,
            },
        )
    }

    fn range_samples(
        source_id: &str,
        timestamp_ns: u64,
        distance_m: f32,
    ) -> BTreeMap<String, Stamped<range::Sample>> {
        BTreeMap::from([(
            source_id.to_string(),
            Stamped::new(timestamp_ns, range::Sample::new(distance_m)),
        )])
    }

    assert_schema::<SafetyAuthorization>(
        "runtime/safety/authorization",
        1,
        "safety authorization",
    )?;
    assert_schema::<SafetyState>("runtime/safety/state", 1, "safety state")?;

    let decisions = [
        SafetyDecision::Allow,
        SafetyDecision::Slow,
        SafetyDecision::Stop,
        SafetyDecision::EmergencyStop,
        SafetyDecision::UnknownConservative,
    ];
    ensure!(
        decisions.len() == 5,
        "safety decision policy must cover every v1 decision variant"
    );

    let reason_codes = [
        SafetyReasonCode::Clear,
        SafetyReasonCode::Obstacle,
        SafetyReasonCode::MissingSupport,
        SafetyReasonCode::StaleSource,
        SafetyReasonCode::LatencyExceeded,
        SafetyReasonCode::EmergencyStop,
        SafetyReasonCode::LocalizationMode,
        SafetyReasonCode::UnknownSpace,
    ];
    ensure!(
        reason_codes.len() == 8,
        "safety reason policy must cover every v1 reason variant"
    );

    let authorization = SafetyAuthorization {
        decision: SafetyDecision::Stop,
        source_revision: SafetySourceRevision {
            localization: None,
            map: None,
            raw_sources: Vec::new(),
        },
        approved_motion: MotionConstraint {
            linear_x_mps: Constraint { min: 0.0, max: 0.0 },
            angular_z_radps: Constraint { min: 0.0, max: 0.0 },
        },
        reasons: vec![SafetyReason {
            code: SafetyReasonCode::Obstacle,
            detail: None,
        }],
        expires_at_ns: Some(1_000_000_000),
    };
    let encoded = serde_json::to_string(&authorization)?;
    let decoded: SafetyAuthorization = serde_json::from_str(&encoded)?;
    ensure!(
        decoded == authorization,
        "safety authorization must round-trip through serde"
    );

    ensure!(
        decision_priority(SafetyDecision::EmergencyStop) > decision_priority(SafetyDecision::Stop),
        "emergency stop must outrank stop"
    );
    ensure!(
        decision_priority(SafetyDecision::Stop)
            > decision_priority(SafetyDecision::UnknownConservative),
        "stop must outrank unknown-conservative"
    );
    ensure!(
        decision_priority(SafetyDecision::UnknownConservative)
            > decision_priority(SafetyDecision::Slow),
        "unknown-conservative must outrank slow"
    );
    ensure!(
        decision_priority(SafetyDecision::Slow) > decision_priority(SafetyDecision::Allow),
        "slow must outrank allow"
    );

    let now_ns = 1_000;
    let tracking = localize_state(LocalizationMode::Tracking);
    let dead_reckoning = localize_state(LocalizationMode::DeadReckoning);
    let initializing = localize_state(LocalizationMode::Initializing);

    let obstacle = EvaluationOutcome::evaluate(
        &range_samples("front_center_tof.range", 1_000, 0.20),
        &BTreeMap::new(),
        Some(&tracking),
        EmergencyStopInputs::default(),
        now_ns,
    );
    ensure!(
        obstacle.decision == SafetyDecision::Stop,
        "obstacle range sample must stop, got {:?}",
        obstacle.decision
    );

    let clear_tracking = EvaluationOutcome::evaluate(
        &range_samples("front_center_tof.range", 1_000, 5.0),
        &BTreeMap::new(),
        Some(&tracking),
        EmergencyStopInputs::default(),
        now_ns,
    );
    ensure!(
        clear_tracking.decision == SafetyDecision::Allow,
        "clear range with tracking localization must allow, got {:?}",
        clear_tracking.decision
    );

    let clear_dead_reckoning = EvaluationOutcome::evaluate(
        &range_samples("front_center_tof.range", 1_000, 5.0),
        &BTreeMap::new(),
        Some(&dead_reckoning),
        EmergencyStopInputs::default(),
        now_ns,
    );
    ensure!(
        clear_dead_reckoning.decision == SafetyDecision::Slow,
        "clear range with dead-reckoning localization must slow, got {:?}",
        clear_dead_reckoning.decision
    );

    let clear_initializing = EvaluationOutcome::evaluate(
        &range_samples("front_center_tof.range", 1_000, 5.0),
        &BTreeMap::new(),
        Some(&initializing),
        EmergencyStopInputs::default(),
        now_ns,
    );
    ensure!(
        clear_initializing.decision == SafetyDecision::UnknownConservative,
        "clear range with initializing localization must be unknown-conservative, got {:?}",
        clear_initializing.decision
    );

    let no_inputs = EvaluationOutcome::evaluate(
        &BTreeMap::new(),
        &BTreeMap::new(),
        None,
        EmergencyStopInputs::default(),
        now_ns,
    );
    ensure!(
        no_inputs.decision == SafetyDecision::UnknownConservative,
        "missing range and localization inputs must be unknown-conservative, got {:?}",
        no_inputs.decision
    );

    let stale = EvaluationOutcome::evaluate(
        &range_samples("front_center_tof.range", 0, 5.0),
        &BTreeMap::new(),
        Some(&tracking),
        EmergencyStopInputs::default(),
        1_000_000_000,
    );
    ensure!(
        stale.decision == SafetyDecision::Stop,
        "stale range source must stop, got {:?}",
        stale.decision
    );

    Ok(())
}

async fn assert_p3_safety_localization_modes(
    ctx: &ScenarioContext,
    common: &RobotRuntimeArgs,
    deadline: Instant,
) -> Result<()> {
    wait_until_tracking(ctx, deadline).await?;
    wait_for_safety_decision(
        ctx,
        deadline,
        |decision| decision != SafetyDecision::UnknownConservative,
        "authority under Tracking",
    )
    .await?;

    kill_service(common, SAFETY_LOCALIZATION_TARGET)?;

    wait_for_safety_decision(
        ctx,
        deadline,
        |decision| decision == SafetyDecision::UnknownConservative,
        "conservative on localization loss",
    )
    .await?;

    restart_service(common, SAFETY_LOCALIZATION_TARGET)?;

    wait_until_tracking(ctx, deadline).await?;
    wait_for_safety_decision(
        ctx,
        deadline,
        |decision| decision != SafetyDecision::UnknownConservative,
        "authority after recovery",
    )
    .await
}

async fn assert_p3_safety_range_sensor_staleness(
    ctx: &ScenarioContext,
    deadline: Instant,
) -> Result<()> {
    wait_until_tracking(ctx, deadline).await?;

    loop {
        let safety = ctx.latest_safety_state().await?;
        let stale = safety
            .data
            .active_reasons
            .iter()
            .any(|reason| reason.code == SafetyReasonCode::StaleSource);
        if safety.data.decision == SafetyDecision::Stop && stale {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "safety did not report a stale range source (decision {:?}, reasons {:?})",
            safety.data.decision,
            safety.data.active_reasons
        );
        ctx.advance_for_secs(0.5).await?;
    }
}
