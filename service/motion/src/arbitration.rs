//! Body-twist arbitration: selects between the manual and follow candidates
//! under the current safety envelope, and clamps the winner to the
//! authorized min/max per axis.

use phoxal_api::v1 as api;

const MANUAL_STALE_NS: u64 = 500_000_000;
pub(crate) const FOLLOW_STALE_NS: u64 = 500_000_000;
pub(crate) const SAFETY_STALE_NS: u64 = 500_000_000;

#[derive(Clone)]
pub(crate) struct Timed<T> {
    pub(crate) body: T,
    pub(crate) produced_at_ns: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct BodyTwist {
    linear_x_mps: f64,
    angular_z_radps: f64,
    curvature_limit_radpm: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Candidate {
    source: api::motion::MotionSource,
    twist: BodyTwist,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Arbitration {
    pub(crate) active_source: Option<api::motion::MotionSource>,
    pub(crate) selected: api::motion::Target,
    pub(crate) reason: Option<api::motion::MotionReason>,
}

pub(crate) fn arbitrate(
    manual: Option<&Timed<api::motion::ManualCommand>>,
    follow: Option<&Timed<api::follow::Target>>,
    safety: Option<&Timed<api::safety::SafetyAuthorization>>,
    now_ns: u64,
) -> Arbitration {
    let Some(safety) = safety.filter(|safety| safety_is_fresh(safety, now_ns)) else {
        return zero_arbitration(
            Some(api::motion::MotionSource::MissionStop),
            Some(api::motion::MotionReason::SafetyAuthorizationUnavailable),
        );
    };

    if safety.body.decision == api::safety::SafetyDecision::EmergencyStop {
        return zero_arbitration(
            Some(api::motion::MotionSource::EmergencyStop),
            Some(api::motion::MotionReason::SafetyEmergencyStop),
        );
    }

    if let Some(manual) = fresh_manual(manual, now_ns) {
        let selected = clamp_to_authorization(manual.twist, &safety.body.approved_motion);
        let reason = if safety.body.decision == api::safety::SafetyDecision::Stop {
            Some(api::motion::MotionReason::ManualEscapeUnderStop)
        } else {
            constrained_reason(safety.body.decision)
        };
        return Arbitration {
            active_source: Some(api::motion::MotionSource::Manual),
            selected,
            reason,
        };
    }

    if safety.body.decision == api::safety::SafetyDecision::Stop {
        return zero_arbitration(
            Some(api::motion::MotionSource::MissionStop),
            Some(api::motion::MotionReason::SafetyConstrained(
                motion_safety_decision(safety.body.decision),
            )),
        );
    }

    let Some(follow_candidate) = fresh_follow(follow, now_ns) else {
        return zero_arbitration(None, follow_missing_reason(follow, now_ns));
    };

    Arbitration {
        active_source: Some(api::motion::MotionSource::Follow),
        selected: clamp_to_authorization(follow_candidate.twist, &safety.body.approved_motion),
        reason: constrained_reason(safety.body.decision),
    }
}

fn fresh_manual(
    manual: Option<&Timed<api::motion::ManualCommand>>,
    now_ns: u64,
) -> Option<Candidate> {
    let manual =
        manual.filter(|manual| is_fresh(manual.produced_at_ns, now_ns, MANUAL_STALE_NS))?;
    Some(Candidate {
        source: api::motion::MotionSource::Manual,
        twist: BodyTwist {
            linear_x_mps: manual.body.linear_x_mps,
            angular_z_radps: manual.body.angular_z_radps,
            curvature_limit_radpm: None,
        },
    })
}

fn fresh_follow(follow: Option<&Timed<api::follow::Target>>, now_ns: u64) -> Option<Candidate> {
    let follow =
        follow.filter(|follow| is_fresh(follow.produced_at_ns, now_ns, FOLLOW_STALE_NS))?;
    Some(Candidate {
        source: api::motion::MotionSource::Follow,
        twist: BodyTwist {
            linear_x_mps: follow.body.linear_x_mps,
            angular_z_radps: follow.body.angular_z_radps,
            curvature_limit_radpm: None,
        },
    })
}

fn follow_missing_reason(
    follow: Option<&Timed<api::follow::Target>>,
    now_ns: u64,
) -> Option<api::motion::MotionReason> {
    match follow {
        Some(follow) if !is_fresh(follow.produced_at_ns, now_ns, FOLLOW_STALE_NS) => {
            Some(api::motion::MotionReason::FollowTargetStale)
        }
        _ => Some(api::motion::MotionReason::NoFollowTarget),
    }
}

fn safety_is_fresh(safety: &Timed<api::safety::SafetyAuthorization>, now_ns: u64) -> bool {
    if !is_fresh(safety.produced_at_ns, now_ns, SAFETY_STALE_NS) {
        return false;
    }
    safety
        .body
        .expires_at_ns
        .is_none_or(|expires_at_ns| now_ns <= expires_at_ns)
}

fn is_fresh(produced_at_ns: u64, now_ns: u64, stale_ns: u64) -> bool {
    now_ns.saturating_sub(produced_at_ns) <= stale_ns
}

fn clamp_to_authorization(
    twist: BodyTwist,
    approved_motion: &api::safety::MotionConstraint,
) -> api::motion::Target {
    api::motion::Target {
        linear_x_mps: clamp_to_constraint(twist.linear_x_mps, &approved_motion.linear_x_mps) as f32,
        angular_z_radps: clamp_to_constraint(
            twist.angular_z_radps,
            &approved_motion.angular_z_radps,
        ) as f32,
        curvature_limit_radpm: twist.curvature_limit_radpm,
    }
}

pub(crate) fn clamp_to_constraint(value: f64, constraint: &api::safety::Constraint) -> f64 {
    if !(value.is_finite() && constraint.min.is_finite() && constraint.max.is_finite())
        || constraint.min > constraint.max
    {
        return 0.0;
    }
    value.clamp(constraint.min, constraint.max)
}

fn constrained_reason(decision: api::safety::SafetyDecision) -> Option<api::motion::MotionReason> {
    if decision == api::safety::SafetyDecision::Allow {
        None
    } else {
        Some(api::motion::MotionReason::SafetyConstrained(
            motion_safety_decision(decision),
        ))
    }
}

fn motion_safety_decision(decision: api::safety::SafetyDecision) -> api::motion::SafetyDecision {
    match decision {
        api::safety::SafetyDecision::Allow => api::motion::SafetyDecision::Allow,
        api::safety::SafetyDecision::Slow => api::motion::SafetyDecision::Slow,
        api::safety::SafetyDecision::Stop => api::motion::SafetyDecision::Stop,
        api::safety::SafetyDecision::EmergencyStop => api::motion::SafetyDecision::EmergencyStop,
        api::safety::SafetyDecision::UnknownConservative => {
            api::motion::SafetyDecision::UnknownConservative
        }
    }
}

fn zero_arbitration(
    active_source: Option<api::motion::MotionSource>,
    reason: Option<api::motion::MotionReason>,
) -> Arbitration {
    Arbitration {
        active_source,
        selected: zero_motion_target(),
        reason,
    }
}

pub(crate) fn zero_motion_target() -> api::motion::Target {
    api::motion::Target {
        linear_x_mps: 0.0,
        angular_z_radps: 0.0,
        curvature_limit_radpm: None,
    }
}
