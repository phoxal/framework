//! Body-twist arbitration for manual and navigation candidates.

use std::time::{Duration, Instant};

use phoxal::api;
use phoxal::bus::LogicalTime;
use phoxal::model::robot::v0::MotionLimits;

pub(crate) const MANUAL_STALE: Duration = Duration::from_millis(150);
pub(crate) const AUTONOMOUS_STALE_NS: u64 = 500_000_000;

#[derive(Clone)]
pub(crate) struct ManualCandidate {
    pub(crate) body: api::motion::ManualCommand,
    pub(crate) received_at: Instant,
}

#[derive(Clone)]
pub(crate) struct Timed<T> {
    pub(crate) body: T,
    pub(crate) at: LogicalTime,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Arbitration {
    pub(crate) source: Option<api::motion::Source>,
    pub(crate) selected: api::drive::Target,
    pub(crate) zero_reason: Option<api::motion::ZeroReason>,
}

pub(crate) fn arbitrate(
    manual: Option<&ManualCandidate>,
    autonomous: Option<&Timed<api::navigation::Candidate>>,
    emergency_stop_engaged: bool,
    safety: Option<&Timed<api::safety::MotionConstraints>>,
    limits: MotionLimits,
    now: LogicalTime,
    host_now: Instant,
) -> Arbitration {
    if emergency_stop_engaged {
        return zero(
            Some(api::motion::Source::EmergencyStop),
            api::motion::ZeroReason::EmergencyStopEngaged,
        );
    }

    if manual.is_some_and(|candidate| candidate.received_at > host_now) {
        return zero(
            Some(api::motion::Source::Manual),
            api::motion::ZeroReason::ManualCandidateFromFuture,
        );
    }

    if let Some(manual) =
        manual.filter(|candidate| host_now.duration_since(candidate.received_at) <= MANUAL_STALE)
    {
        // Manual teleoperation is the recovery/commissioning path and must not
        // depend on the still-experimental world-safety provider being ready.
        // A valid protective stop still wins, and every manual command remains
        // bounded by e-stop, freshness, finite-value, and robot limits.
        let limits = match safety.filter(|constraints| safety_is_usable(constraints, now)) {
            Some(safety) if safety.body.stop => {
                return zero(None, api::motion::ZeroReason::SafetyProtectiveStop);
            }
            Some(safety) => constrained_limits(limits, &safety.body),
            None => limits,
        };
        return select(
            api::motion::Source::Manual,
            manual.body.linear_x_mps,
            manual.body.angular_z_radps,
            limits,
        );
    }

    if autonomous.is_some_and(|candidate| {
        candidate.at.epoch() == now.epoch() && candidate.at.time_ns() > now.time_ns()
    }) {
        return zero(
            Some(api::motion::Source::Navigation),
            api::motion::ZeroReason::NavigationCandidateFromFuture,
        );
    }

    if let Some(autonomous) =
        autonomous.filter(|candidate| is_fresh(candidate.at, now, AUTONOMOUS_STALE_NS))
    {
        let Some(safety) = safety.filter(|constraints| safety_is_usable(constraints, now)) else {
            return zero(None, api::motion::ZeroReason::SafetyConstraintsUnavailable);
        };
        if safety.body.stop {
            return zero(None, api::motion::ZeroReason::SafetyProtectiveStop);
        }
        let limits = constrained_limits(limits, &safety.body);
        return select(
            api::motion::Source::Navigation,
            f64::from(autonomous.body.linear_x_mps),
            f64::from(autonomous.body.angular_z_radps),
            limits,
        );
    }

    let reason = if manual.is_some() {
        api::motion::ZeroReason::ManualCandidateStale
    } else if autonomous.is_some() {
        api::motion::ZeroReason::NavigationCandidateStale
    } else {
        api::motion::ZeroReason::NoCandidate
    };
    zero(None, reason)
}

pub(crate) fn safety_is_usable(
    constraints: &Timed<api::safety::MotionConstraints>,
    now: LogicalTime,
) -> bool {
    let body = &constraints.body;
    let entry_stop = body.constraints.iter().any(|constraint| constraint.stop);
    let entry_linear = body
        .constraints
        .iter()
        .filter_map(|constraint| constraint.max_linear_speed_mps)
        .reduce(f32::min);
    let entry_angular = body
        .constraints
        .iter()
        .filter_map(|constraint| constraint.max_angular_speed_radps)
        .reduce(f32::min);
    constraints.at.epoch() == now.epoch()
        && constraints.at.time_ns() <= now.time_ns()
        && body.expires_at_ns >= now.time_ns()
        && body.expires_at_ns >= constraints.at.time_ns()
        && body.stop == entry_stop
        && body.max_linear_speed_mps == entry_linear
        && body.max_angular_speed_radps == entry_angular
        && body
            .max_linear_speed_mps
            .is_none_or(|limit| limit.is_finite() && limit >= 0.0)
        && body
            .max_angular_speed_radps
            .is_none_or(|limit| limit.is_finite() && limit >= 0.0)
        && body.constraints.iter().all(|constraint| {
            constraint.valid_from_ns <= now.time_ns()
                && constraint.expires_at_ns >= now.time_ns()
                && constraint.expires_at_ns <= body.expires_at_ns
                && constraint.expires_at_ns >= constraint.valid_from_ns
                && constraint
                    .max_linear_speed_mps
                    .is_none_or(|limit| limit.is_finite() && limit >= 0.0)
                && constraint
                    .max_angular_speed_radps
                    .is_none_or(|limit| limit.is_finite() && limit >= 0.0)
                && constraint.observed_value.is_none_or(f32::is_finite)
                && !constraint.source.participant_id.is_empty()
        })
}

fn constrained_limits(
    mut limits: MotionLimits,
    constraints: &api::safety::MotionConstraints,
) -> MotionLimits {
    if let Some(max_linear) = constraints.max_linear_speed_mps {
        limits.max_linear_speed_mps = limits.max_linear_speed_mps.min(f64::from(max_linear));
    }
    if let Some(max_angular) = constraints.max_angular_speed_radps {
        limits.max_angular_speed_radps = limits.max_angular_speed_radps.min(f64::from(max_angular));
    }
    limits
}

pub(crate) fn candidate_age_ns<T>(candidate: Option<&Timed<T>>, now: LogicalTime) -> Option<u64> {
    candidate.and_then(|candidate| {
        (candidate.at.epoch() == now.epoch())
            .then(|| now.time_ns().saturating_sub(candidate.at.time_ns()))
    })
}

pub(crate) fn manual_candidate_age_ns(
    candidate: Option<&ManualCandidate>,
    now: Instant,
) -> Option<u64> {
    candidate.and_then(|candidate| {
        now.checked_duration_since(candidate.received_at)
            .map(|age| u64::try_from(age.as_nanos()).unwrap_or(u64::MAX))
    })
}

fn select(
    source: api::motion::Source,
    linear_x_mps: f64,
    angular_z_radps: f64,
    limits: MotionLimits,
) -> Arbitration {
    if !(linear_x_mps.is_finite() && angular_z_radps.is_finite()) {
        let reason = match source {
            api::motion::Source::Manual => api::motion::ZeroReason::ManualCandidateNotFinite,
            api::motion::Source::Navigation => {
                api::motion::ZeroReason::NavigationCandidateNotFinite
            }
            api::motion::Source::EmergencyStop => api::motion::ZeroReason::EmergencyStopEngaged,
        };
        return zero(Some(source), reason);
    }

    Arbitration {
        source: Some(source),
        selected: api::drive::Target {
            linear_x_mps: linear_x_mps
                .clamp(-limits.max_linear_speed_mps, limits.max_linear_speed_mps)
                as f32,
            angular_z_radps: angular_z_radps.clamp(
                -limits.max_angular_speed_radps,
                limits.max_angular_speed_radps,
            ) as f32,
            curvature_limit_radpm: None,
        },
        zero_reason: None,
    }
}

fn is_fresh(at: LogicalTime, now: LogicalTime, stale_ns: u64) -> bool {
    at.epoch() == now.epoch()
        && at.time_ns() <= now.time_ns()
        && now.time_ns().saturating_sub(at.time_ns()) <= stale_ns
}

fn zero(source: Option<api::motion::Source>, reason: api::motion::ZeroReason) -> Arbitration {
    Arbitration {
        source,
        selected: zero_target(),
        zero_reason: Some(reason),
    }
}

pub(crate) fn zero_target() -> api::drive::Target {
    api::drive::Target {
        linear_x_mps: 0.0,
        angular_z_radps: 0.0,
        curvature_limit_radpm: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW_NS: u64 = 1_000_000_000;
    const LIMITS: MotionLimits = MotionLimits {
        max_linear_speed_mps: 0.6,
        max_angular_speed_radps: 2.0,
    };

    fn now() -> LogicalTime {
        LogicalTime::new(0, NOW_NS)
    }

    fn host_now() -> Instant {
        Instant::now()
    }

    fn manual(linear_x_mps: f64, angular_z_radps: f64, received_at: Instant) -> ManualCandidate {
        ManualCandidate {
            body: api::motion::ManualCommand {
                linear_x_mps,
                angular_z_radps,
            },
            received_at,
        }
    }

    fn safety() -> Timed<api::safety::MotionConstraints> {
        Timed {
            body: api::safety::MotionConstraints {
                sequence: 1,
                stop: false,
                max_linear_speed_mps: None,
                max_angular_speed_radps: None,
                constraints: Vec::new(),
                expires_at_ns: NOW_NS + 1,
            },
            at: now(),
        }
    }

    fn safety_constraint(stop: bool) -> api::safety::Constraint {
        api::safety::Constraint {
            reason: api::safety::ConstraintReason::ObstacleProximity,
            source: api::safety::ConstraintSource {
                kind: api::safety::ConstraintSourceKind::Range,
                participant_id: "safety".to_string(),
                component_id: Some("front_range".to_string()),
                capability_id: Some("range".to_string()),
            },
            stop,
            max_linear_speed_mps: None,
            max_angular_speed_radps: None,
            observed_value: Some(0.1),
            valid_from_ns: NOW_NS,
            expires_at_ns: NOW_NS + 1,
        }
    }

    fn autonomous() -> Timed<api::navigation::Candidate> {
        Timed {
            body: api::navigation::Candidate {
                request_id: api::navigation::RequestId {
                    value: "test-navigation".to_string(),
                },
                linear_x_mps: 0.2,
                angular_z_radps: 0.1,
            },
            at: now(),
        }
    }

    #[test]
    fn contradictory_nested_safety_stop_fails_closed() {
        let mut safety = safety();
        safety.body.constraints.push(safety_constraint(true));
        let autonomous = autonomous();
        let result = arbitrate(
            None,
            Some(&autonomous),
            false,
            Some(&safety),
            LIMITS,
            now(),
            host_now(),
        );
        assert_eq!(
            result.zero_reason,
            Some(api::motion::ZeroReason::SafetyConstraintsUnavailable)
        );
    }

    #[test]
    fn future_expired_and_non_finite_nested_constraints_fail_closed() {
        for mutate in [
            |constraint: &mut api::safety::Constraint| {
                constraint.valid_from_ns = NOW_NS + 1;
            },
            |constraint: &mut api::safety::Constraint| {
                constraint.expires_at_ns = NOW_NS - 1;
            },
            |constraint: &mut api::safety::Constraint| {
                constraint.observed_value = Some(f32::NAN);
            },
        ] {
            let mut safety = safety();
            let mut constraint = safety_constraint(false);
            mutate(&mut constraint);
            safety.body.constraints.push(constraint);
            let autonomous = autonomous();
            let result = arbitrate(
                None,
                Some(&autonomous),
                false,
                Some(&safety),
                LIMITS,
                now(),
                host_now(),
            );
            assert_eq!(
                result.zero_reason,
                Some(api::motion::ZeroReason::SafetyConstraintsUnavailable)
            );
        }
    }

    #[test]
    fn missing_candidates_are_explained() {
        let result = arbitrate(
            None,
            None,
            false,
            Some(&safety()),
            LIMITS,
            now(),
            host_now(),
        );
        assert_eq!(result.source, None);
        assert_eq!(
            result.zero_reason,
            Some(api::motion::ZeroReason::NoCandidate)
        );
    }

    #[test]
    fn stale_manual_is_distinct_from_missing_manual() {
        let host_now = host_now();
        let manual = manual(0.2, 0.1, host_now - MANUAL_STALE - Duration::from_nanos(1));
        let result = arbitrate(
            Some(&manual),
            None,
            false,
            Some(&safety()),
            LIMITS,
            now(),
            host_now,
        );
        assert_eq!(
            result.zero_reason,
            Some(api::motion::ZeroReason::ManualCandidateStale)
        );
    }

    #[test]
    fn emergency_stop_overrides_manual_and_navigation() {
        let manual = manual(0.2, 0.1, host_now());
        let result = arbitrate(Some(&manual), None, true, None, LIMITS, now(), host_now());
        assert_eq!(result.selected, zero_target());
        assert_eq!(
            result.zero_reason,
            Some(api::motion::ZeroReason::EmergencyStopEngaged)
        );
    }

    #[test]
    fn manual_wins_and_is_clamped_to_robot_limits() {
        let manual = manual(9.0, -9.0, host_now());
        let result = arbitrate(
            Some(&manual),
            None,
            false,
            Some(&safety()),
            LIMITS,
            now(),
            host_now(),
        );
        assert_eq!(result.source, Some(api::motion::Source::Manual));
        assert_eq!(result.selected.linear_x_mps, 0.6);
        assert_eq!(result.selected.angular_z_radps, -2.0);
    }

    #[test]
    fn non_finite_candidate_stops() {
        let manual = manual(f64::NAN, 0.0, host_now());
        let result = arbitrate(
            Some(&manual),
            None,
            false,
            Some(&safety()),
            LIMITS,
            now(),
            host_now(),
        );
        assert_eq!(result.selected, zero_target());
        assert_eq!(
            result.zero_reason,
            Some(api::motion::ZeroReason::ManualCandidateNotFinite)
        );
    }

    #[test]
    fn future_dated_manual_candidate_stops() {
        let host_now = host_now();
        let manual = manual(0.2, 0.0, host_now + Duration::from_nanos(1));
        let result = arbitrate(
            Some(&manual),
            None,
            false,
            Some(&safety()),
            LIMITS,
            now(),
            host_now,
        );
        assert_eq!(
            result.zero_reason,
            Some(api::motion::ZeroReason::ManualCandidateFromFuture)
        );
    }

    #[test]
    fn large_finite_candidate_stays_finite_after_f32_conversion() {
        let manual = manual(1.0e300, -1.0e300, host_now());
        let limits = MotionLimits {
            max_linear_speed_mps: f64::from(f32::MAX),
            max_angular_speed_radps: f64::from(f32::MAX),
        };
        let result = arbitrate(
            Some(&manual),
            None,
            false,
            Some(&safety()),
            limits,
            now(),
            host_now(),
        );
        assert!(result.selected.linear_x_mps.is_finite());
        assert!(result.selected.angular_z_radps.is_finite());
    }

    #[test]
    fn logical_epoch_reset_does_not_expire_fresh_manual_input() {
        let limits = MotionLimits {
            max_linear_speed_mps: 0.6,
            max_angular_speed_radps: 2.0,
        };
        let manual = manual(0.4, 0.2, host_now());
        let result = arbitrate(
            Some(&manual),
            None,
            false,
            Some(&Timed {
                at: LogicalTime::new(2, NOW_NS),
                ..safety()
            }),
            limits,
            LogicalTime::new(2, NOW_NS),
            host_now(),
        );
        assert_eq!(result.source, Some(api::motion::Source::Manual));
        assert_eq!(result.selected.linear_x_mps, 0.4);
    }

    #[test]
    fn manual_works_without_safety_and_uses_valid_constraints_when_available() {
        let manual = manual(0.5, 0.4, host_now());
        let missing = arbitrate(Some(&manual), None, false, None, LIMITS, now(), host_now());
        assert_eq!(missing.source, Some(api::motion::Source::Manual));
        assert_eq!(missing.selected.linear_x_mps, 0.5);

        let mut constrained = safety();
        constrained.body.max_linear_speed_mps = Some(0.1);
        let mut entry = safety_constraint(false);
        entry.max_linear_speed_mps = Some(0.1);
        constrained.body.constraints.push(entry);
        let result = arbitrate(
            Some(&manual),
            None,
            false,
            Some(&constrained),
            LIMITS,
            now(),
            host_now(),
        );
        assert_eq!(result.selected.linear_x_mps, 0.1);
        assert_eq!(result.selected.angular_z_radps, 0.4);
    }

    #[test]
    fn autonomous_still_requires_safety_constraints() {
        let autonomous = autonomous();
        let missing = arbitrate(
            None,
            Some(&autonomous),
            false,
            None,
            LIMITS,
            now(),
            host_now(),
        );
        assert_eq!(
            missing.zero_reason,
            Some(api::motion::ZeroReason::SafetyConstraintsUnavailable)
        );
    }

    #[test]
    fn valid_safety_protective_stop_still_blocks_manual() {
        let manual = manual(0.5, 0.4, host_now());
        let mut stopped = safety();
        stopped.body.stop = true;
        stopped.body.constraints.push(safety_constraint(true));
        let result = arbitrate(
            Some(&manual),
            None,
            false,
            Some(&stopped),
            LIMITS,
            now(),
            host_now(),
        );
        assert_eq!(result.selected, zero_target());
        assert_eq!(
            result.zero_reason,
            Some(api::motion::ZeroReason::SafetyProtectiveStop)
        );
    }
}
