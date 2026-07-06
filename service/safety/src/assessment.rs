//! Safety decision logic: assessing the aggregate posture from the current
//! inputs (fail-closed on stale/missing required sources, worst-of across
//! battery and drive) and deriving the motion authorization for a decision.

use std::f64::consts::PI;

use phoxal_api::y2026_1 as api;

use crate::robot_config::RequiredSources;

const BATTERY_CRITICAL_RATIO: f32 = 0.10;
const BATTERY_LOW_RATIO: f32 = 0.25;
const AUTHORIZATION_TTL_NS: u64 = 100_000_000;
pub(crate) const SOURCE_FRESH_NS: u64 = 1_000_000_000;

#[derive(Clone)]
pub(crate) struct Timed<T> {
    pub(crate) body: T,
    pub(crate) produced_at_ns: u64,
}

pub(crate) struct SafetyInputs<'a> {
    pub(crate) required: RequiredSources,
    pub(crate) battery: Option<&'a Timed<api::battery::State>>,
    pub(crate) drive: Option<&'a Timed<api::drive::State>>,
    pub(crate) emergency_stop_engaged: bool,
}

pub(crate) fn assess(inputs: &SafetyInputs<'_>, now_ns: u64) -> api::safety::Status {
    let mut active_reasons = Vec::new();
    let mut decision = api::safety::SafetyDecision::Allow;

    if inputs.emergency_stop_engaged {
        return api::safety::Status {
            decision: api::safety::SafetyDecision::EmergencyStop,
            active_reasons: vec![reason(api::safety::SafetyReasonCode::EmergencyStopEngaged)],
        };
    }

    if source_stale(inputs.required.battery, inputs.battery, now_ns) {
        active_reasons.push(source_stale_reason("battery/state"));
        decision = worst_decision(decision, api::safety::SafetyDecision::Stop);
    }
    if source_stale(inputs.required.drive, inputs.drive, now_ns) {
        active_reasons.push(source_stale_reason("drive/state"));
        decision = worst_decision(decision, api::safety::SafetyDecision::Stop);
    }

    if let Some(battery) = fresh_body(inputs.battery, now_ns) {
        if battery.charge_ratio < BATTERY_CRITICAL_RATIO {
            active_reasons.push(reason(api::safety::SafetyReasonCode::BatteryCritical));
            decision = worst_decision(decision, api::safety::SafetyDecision::Stop);
        } else if battery.charge_ratio < BATTERY_LOW_RATIO {
            active_reasons.push(reason(api::safety::SafetyReasonCode::BatteryLow));
            decision = worst_decision(decision, api::safety::SafetyDecision::Slow);
        }
    }

    if let Some(drive) = fresh_body(inputs.drive, now_ns) {
        match drive.stop_reason {
            Some(api::drive::StopReason::Fault) => {
                active_reasons.push(reason(api::safety::SafetyReasonCode::DriveFault));
                decision = worst_decision(decision, api::safety::SafetyDecision::Stop);
            }
            Some(api::drive::StopReason::EmergencyStop) => {
                active_reasons.push(reason(api::safety::SafetyReasonCode::EmergencyStopEngaged));
                decision = worst_decision(decision, api::safety::SafetyDecision::EmergencyStop);
            }
            Some(api::drive::StopReason::NoTarget) | None => {}
        }
    }

    api::safety::Status {
        decision,
        active_reasons,
    }
}

pub(crate) fn emergency_stop_engaged(
    software_estop_engaged: bool,
    component_estops: &[bool],
) -> bool {
    software_estop_engaged || component_estops.iter().any(|engaged| *engaged)
}

pub(crate) fn authorize(
    status: &api::safety::Status,
    _inputs: &SafetyInputs<'_>,
    now_ns: u64,
) -> api::safety::SafetyAuthorization {
    api::safety::SafetyAuthorization {
        decision: status.decision,
        approved_motion: approved_motion(status.decision),
        reasons: status.active_reasons.clone(),
        source_revision: api::safety::SafetySourceRevision {
            localization: None,
            map: None,
        },
        expires_at_ns: Some(now_ns.saturating_add(AUTHORIZATION_TTL_NS)),
    }
}

fn source_stale<T>(required: bool, timed: Option<&Timed<T>>, now_ns: u64) -> bool {
    required && fresh_body(timed, now_ns).is_none()
}

fn fresh_body<T>(timed: Option<&Timed<T>>, now_ns: u64) -> Option<&T> {
    let timed = timed?;
    (now_ns.saturating_sub(timed.produced_at_ns) <= SOURCE_FRESH_NS).then_some(&timed.body)
}

fn approved_motion(decision: api::safety::SafetyDecision) -> api::safety::MotionConstraint {
    match decision {
        api::safety::SafetyDecision::Allow => motion_constraint(-1.0, 1.0, -PI, PI),
        api::safety::SafetyDecision::Slow => motion_constraint(-0.1, 0.1, -0.5, 0.5),
        api::safety::SafetyDecision::Stop
        | api::safety::SafetyDecision::EmergencyStop
        | api::safety::SafetyDecision::UnknownConservative => motion_constraint(0.0, 0.0, 0.0, 0.0),
    }
}

fn motion_constraint(
    linear_min: f64,
    linear_max: f64,
    angular_min: f64,
    angular_max: f64,
) -> api::safety::MotionConstraint {
    api::safety::MotionConstraint {
        linear_x_mps: api::safety::Constraint {
            min: linear_min,
            max: linear_max,
        },
        angular_z_radps: api::safety::Constraint {
            min: angular_min,
            max: angular_max,
        },
    }
}

fn reason(code: api::safety::SafetyReasonCode) -> api::safety::SafetyReason {
    api::safety::SafetyReason { code, detail: None }
}

fn source_stale_reason(source: &str) -> api::safety::SafetyReason {
    api::safety::SafetyReason {
        code: api::safety::SafetyReasonCode::SourceStale,
        detail: Some(format!("{source} missing or stale")),
    }
}

fn worst_decision(
    current: api::safety::SafetyDecision,
    candidate: api::safety::SafetyDecision,
) -> api::safety::SafetyDecision {
    if decision_rank(candidate) > decision_rank(current) {
        candidate
    } else {
        current
    }
}

fn decision_rank(decision: api::safety::SafetyDecision) -> u8 {
    match decision {
        api::safety::SafetyDecision::Allow => 0,
        api::safety::SafetyDecision::Slow => 1,
        api::safety::SafetyDecision::Stop => 2,
        api::safety::SafetyDecision::EmergencyStop => 3,
        api::safety::SafetyDecision::UnknownConservative => 4,
    }
}
