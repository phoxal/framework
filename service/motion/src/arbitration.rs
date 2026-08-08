//! Body-twist arbitration for manual and navigation candidates.

use std::time::Duration;

use phoxal::api;
use phoxal::bus::{LocalInstant, RobotInstant, Timed};
use phoxal::model::robot::MotionLimits;

/// Manual teleoperation is a **leased** command: the operator must keep proving
/// presence in human time, so silence is measured on the host clock and an
/// accelerated simulation cannot stretch it.
pub(crate) const MANUAL_SILENCE: Duration = Duration::from_millis(150);

/// ...and travel on one manual command is bounded in robot time, so a
/// decelerated simulation cannot shrink it.
pub(crate) const MANUAL_HOLD: Duration = Duration::from_millis(500);

pub(crate) const AUTONOMOUS_STALE: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Arbitration {
    pub(crate) decision: api::motion::Decision,
}

impl Arbitration {
    /// Arbitrate one cycle. `manual` is whatever the receiver-owned lease
    /// reports live; the lease has already applied both expiry conditions, so
    /// there is no separate manual freshness rule here.
    pub(crate) fn decide(
        manual: Option<&api::motion::ManualCommand>,
        autonomous: Option<&Timed<api::navigation::Candidate>>,
        emergency_stop_engaged: bool,
        safety: Option<&Timed<api::safety::MotionConstraints>>,
        limits: MotionLimits,
        now: RobotInstant,
    ) -> Self {
        if emergency_stop_engaged {
            return Self::zero(api::motion::ZeroReason::EmergencyStopEngaged);
        }

        if let Some(manual) = manual {
            // Manual teleoperation is the recovery/commissioning path and must
            // not depend on the world-safety provider being ready. A valid
            // protective stop still wins, and every manual command remains
            // bounded by e-stop, the lease, finite-value, and robot limits.
            let limits = match safety.filter(|constraints| safety_is_usable(constraints, now)) {
                Some(safety) if safety.body.stop => {
                    return Self::zero(api::motion::ZeroReason::SafetyProtectiveStop);
                }
                Some(safety) => constrained_limits(limits, &safety.body),
                None => limits,
            };
            return Self::select(
                api::motion::Source::Manual,
                manual.linear_x_mps,
                manual.angular_z_radps,
                limits,
            );
        }

        if let Some(autonomous) =
            autonomous.filter(|candidate| candidate.fresh_within(now, AUTONOMOUS_STALE))
        {
            let Some(safety) = safety.filter(|constraints| safety_is_usable(constraints, now))
            else {
                return Self::zero(api::motion::ZeroReason::SafetyConstraintsUnavailable);
            };
            if safety.body.stop {
                return Self::zero(api::motion::ZeroReason::SafetyProtectiveStop);
            }
            let limits = constrained_limits(limits, &safety.body);
            return Self::select(
                api::motion::Source::Navigation,
                f64::from(autonomous.body.linear_x_mps),
                f64::from(autonomous.body.angular_z_radps),
                limits,
            );
        }

        let reason = if autonomous.is_some() {
            api::motion::ZeroReason::NavigationCandidateStale
        } else {
            api::motion::ZeroReason::NoCandidate
        };
        Self::zero(reason)
    }

    /// Accept one candidate, clamped to `limits`. A non-finite candidate is a
    /// stop: it names no reachable velocity, so there is nothing to clamp.
    fn select(
        source: api::motion::Source,
        linear_x_mps: f64,
        angular_z_radps: f64,
        limits: MotionLimits,
    ) -> Self {
        if !(linear_x_mps.is_finite() && angular_z_radps.is_finite()) {
            let reason = match source {
                api::motion::Source::Manual => api::motion::ZeroReason::ManualCandidateNotFinite,
                api::motion::Source::Navigation => {
                    api::motion::ZeroReason::NavigationCandidateNotFinite
                }
                api::motion::Source::EmergencyStop => api::motion::ZeroReason::EmergencyStopEngaged,
            };
            return Self::zero(reason);
        }

        let linear_x_mps =
            linear_x_mps.clamp(-limits.max_linear_speed_mps, limits.max_linear_speed_mps) as f32;
        let angular_z_radps = angular_z_radps.clamp(
            -limits.max_angular_speed_radps,
            limits.max_angular_speed_radps,
        ) as f32;
        let Ok(target) = api::drive::Target::try_new(linear_x_mps, angular_z_radps) else {
            let reason = match source {
                api::motion::Source::Manual => api::motion::ZeroReason::ManualCandidateNotFinite,
                api::motion::Source::Navigation => {
                    api::motion::ZeroReason::NavigationCandidateNotFinite
                }
                api::motion::Source::EmergencyStop => api::motion::ZeroReason::EmergencyStopEngaged,
            };
            return Self::zero(reason);
        };
        Self {
            decision: api::motion::Decision::Active { source, target },
        }
    }

    /// Command no motion, recording why.
    fn zero(reason: api::motion::ZeroReason) -> Self {
        Self {
            decision: api::motion::Decision::Stopped { reason },
        }
    }
}

/// `later` is at or after `earlier` on the same timeline.
///
/// A cross-timeline pair is not "later" and not "earlier"; it is
/// incomparable, and every caller here reads that as unusable, so a deadline
/// carried from a replaced timeline fails closed instead of silently passing.
fn at_or_after(later: RobotInstant, earlier: RobotInstant) -> bool {
    later
        .checked_cmp(earlier)
        .is_ok_and(|order| order != std::cmp::Ordering::Less)
}

/// `deadline` is still in the future at `now`, on the same timeline.
///
/// Expiry is exclusive: a constraint whose `expires_at` is exactly this step's
/// instant has expired, matching how the command lease and the actuator permit
/// treat their own deadlines. The alternative authorizes motion for one more
/// step on a product that was supposed to be finished.
fn not_yet_expired(deadline: RobotInstant, now: RobotInstant) -> bool {
    deadline
        .checked_cmp(now)
        .is_ok_and(|order| order == std::cmp::Ordering::Greater)
}

pub(crate) fn safety_is_usable(
    constraints: &Timed<api::safety::MotionConstraints>,
    now: RobotInstant,
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
    // The product carries its own expiry, so age is not what bounds it here.
    // `Duration::MAX` asks only the two questions the carrier still owns: that
    // the stamp belongs to this world history, and that it is not in this
    // step's future.
    constraints.fresh_within(now, Duration::MAX)
        && not_yet_expired(body.expires_at, now)
        && at_or_after(body.expires_at, constraints.at)
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
            at_or_after(now, constraint.valid_from)
                && not_yet_expired(constraint.expires_at, now)
                && at_or_after(body.expires_at, constraint.expires_at)
                && at_or_after(constraint.expires_at, constraint.valid_from)
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

/// The age of a robot-time candidate at this step. A candidate from a replaced
/// world has no age at all, which is reported as absent rather than as a wrong
/// number.
pub(crate) fn candidate_age_ns<T>(candidate: Option<&Timed<T>>, now: RobotInstant) -> Option<u64> {
    candidate.and_then(|candidate| {
        now.duration_since(candidate.at)
            .ok()
            .map(|age| u64::try_from(age.as_nanos()).unwrap_or(u64::MAX))
    })
}

/// How long ago this receiver observed the live manual command, on the host
/// clock the lease ages it against.
pub(crate) fn manual_observed_age_ns(
    observed_at: Option<LocalInstant>,
    now: LocalInstant,
) -> Option<u64> {
    observed_at.map(|observed_at| {
        u64::try_from(now.saturating_duration_since(observed_at).as_nanos()).unwrap_or(u64::MAX)
    })
}

#[cfg(test)]
mod tests {
    use phoxal::bus::TimelineId;

    use super::*;

    const NOW_NS: u64 = 1_000_000_000;
    const LIMITS: MotionLimits = MotionLimits {
        max_linear_speed_mps: 0.6,
        max_angular_speed_radps: 2.0,
    };

    fn line() -> TimelineId {
        TimelineId::from_raw(1).expect("test timeline must be nonzero")
    }

    fn now() -> RobotInstant {
        RobotInstant::new(line(), NOW_NS)
    }

    /// A live manual command, i.e. one the receiver-owned lease already
    /// admitted. Manual freshness is the lease's job (see its own tests in
    /// `phoxal-bus` and `service/motion`), not arbitration's.
    fn manual(linear_x_mps: f64, angular_z_radps: f64) -> api::motion::ManualCommand {
        api::motion::ManualCommand {
            linear_x_mps,
            angular_z_radps,
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
                expires_at: RobotInstant::new(line(), NOW_NS + 1),
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
            valid_from: RobotInstant::new(line(), NOW_NS),
            expires_at: RobotInstant::new(line(), NOW_NS + 1),
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

    fn stopped_reason(result: &Arbitration) -> api::motion::ZeroReason {
        let api::motion::Decision::Stopped { reason } = &result.decision else {
            panic!("expected a stopped motion decision");
        };
        *reason
    }

    fn active_target(result: &Arbitration) -> &api::drive::Target {
        let api::motion::Decision::Active { target, .. } = &result.decision else {
            panic!("expected an active motion decision");
        };
        target
    }

    fn active_source(result: &Arbitration) -> api::motion::Source {
        let api::motion::Decision::Active { source, .. } = &result.decision else {
            panic!("expected an active motion decision");
        };
        *source
    }

    #[test]
    fn contradictory_nested_safety_stop_fails_closed() {
        let mut safety = safety();
        safety.body.constraints.push(safety_constraint(true));
        let autonomous = autonomous();
        let result =
            Arbitration::decide(None, Some(&autonomous), false, Some(&safety), LIMITS, now());
        assert_eq!(
            stopped_reason(&result),
            api::motion::ZeroReason::SafetyConstraintsUnavailable
        );
    }

    #[test]
    fn future_expired_and_non_finite_nested_constraints_fail_closed() {
        for mutate in [
            |constraint: &mut api::safety::Constraint| {
                constraint.valid_from = RobotInstant::new(line(), NOW_NS + 1);
            },
            |constraint: &mut api::safety::Constraint| {
                constraint.expires_at = RobotInstant::new(line(), NOW_NS - 1);
            },
            // The exact expiry instant is already expired: a constraint is
            // usable *before* it lapses, never at the moment it does.
            |constraint: &mut api::safety::Constraint| {
                constraint.expires_at = RobotInstant::new(line(), NOW_NS);
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
            let result =
                Arbitration::decide(None, Some(&autonomous), false, Some(&safety), LIMITS, now());
            assert_eq!(
                stopped_reason(&result),
                api::motion::ZeroReason::SafetyConstraintsUnavailable
            );
        }
    }

    #[test]
    fn missing_candidates_are_explained() {
        let result = Arbitration::decide(None, None, false, Some(&safety()), LIMITS, now());
        assert!(matches!(
            result.decision,
            api::motion::Decision::Stopped {
                reason: api::motion::ZeroReason::NoCandidate
            }
        ));
        assert_eq!(
            stopped_reason(&result),
            api::motion::ZeroReason::NoCandidate
        );
    }

    #[test]
    fn emergency_stop_overrides_manual_and_navigation() {
        let manual = manual(0.2, 0.1);
        let result = Arbitration::decide(Some(&manual), None, true, None, LIMITS, now());
        assert!(matches!(
            result.decision,
            api::motion::Decision::Stopped {
                reason: api::motion::ZeroReason::EmergencyStopEngaged
            }
        ));
        assert_eq!(
            stopped_reason(&result),
            api::motion::ZeroReason::EmergencyStopEngaged
        );
    }

    #[test]
    fn manual_wins_and_is_clamped_to_robot_limits() {
        let manual = manual(9.0, -9.0);
        let result =
            Arbitration::decide(Some(&manual), None, false, Some(&safety()), LIMITS, now());
        assert_eq!(active_source(&result), api::motion::Source::Manual);
        assert_eq!(active_target(&result).linear_x_mps(), 0.6);
        assert_eq!(active_target(&result).angular_z_radps(), -2.0);
    }

    #[test]
    fn non_finite_candidate_stops() {
        let manual = manual(f64::NAN, 0.0);
        let result =
            Arbitration::decide(Some(&manual), None, false, Some(&safety()), LIMITS, now());
        assert!(matches!(
            result.decision,
            api::motion::Decision::Stopped {
                reason: api::motion::ZeroReason::ManualCandidateNotFinite
            }
        ));
        assert_eq!(
            stopped_reason(&result),
            api::motion::ZeroReason::ManualCandidateNotFinite
        );
    }

    #[test]
    fn large_finite_candidate_stays_finite_after_f32_conversion() {
        let manual = manual(1.0e300, -1.0e300);
        let limits = MotionLimits {
            max_linear_speed_mps: f64::from(f32::MAX),
            max_angular_speed_radps: f64::from(f32::MAX),
        };
        let result =
            Arbitration::decide(Some(&manual), None, false, Some(&safety()), limits, now());
        assert!(active_target(&result).linear_x_mps().is_finite());
        assert!(active_target(&result).angular_z_radps().is_finite());
    }

    #[test]
    fn a_replacement_timeline_does_not_expire_a_live_manual_command() {
        let limits = MotionLimits {
            max_linear_speed_mps: 0.6,
            max_angular_speed_radps: 2.0,
        };
        let replaced = TimelineId::mint();
        let manual = manual(0.4, 0.2);
        let result = Arbitration::decide(
            Some(&manual),
            None,
            false,
            Some(&Timed {
                at: RobotInstant::new(replaced, NOW_NS),
                ..safety()
            }),
            limits,
            RobotInstant::new(replaced, NOW_NS),
        );
        assert_eq!(active_source(&result), api::motion::Source::Manual);
        assert_eq!(active_target(&result).linear_x_mps(), 0.4);
    }

    #[test]
    fn manual_works_without_safety_and_uses_valid_constraints_when_available() {
        let manual = manual(0.5, 0.4);
        let missing = Arbitration::decide(Some(&manual), None, false, None, LIMITS, now());
        assert_eq!(active_source(&missing), api::motion::Source::Manual);
        assert_eq!(active_target(&missing).linear_x_mps(), 0.5);

        let mut constrained = safety();
        constrained.body.max_linear_speed_mps = Some(0.1);
        let mut entry = safety_constraint(false);
        entry.max_linear_speed_mps = Some(0.1);
        constrained.body.constraints.push(entry);
        let result = Arbitration::decide(
            Some(&manual),
            None,
            false,
            Some(&constrained),
            LIMITS,
            now(),
        );
        assert_eq!(active_target(&result).linear_x_mps(), 0.1);
        assert_eq!(active_target(&result).angular_z_radps(), 0.4);
    }

    #[test]
    fn autonomous_still_requires_safety_constraints() {
        let autonomous = autonomous();
        let missing = Arbitration::decide(None, Some(&autonomous), false, None, LIMITS, now());
        assert_eq!(
            stopped_reason(&missing),
            api::motion::ZeroReason::SafetyConstraintsUnavailable
        );
    }

    #[test]
    fn valid_safety_protective_stop_still_blocks_manual() {
        let manual = manual(0.5, 0.4);
        let mut stopped = safety();
        stopped.body.stop = true;
        stopped.body.constraints.push(safety_constraint(true));
        let result = Arbitration::decide(Some(&manual), None, false, Some(&stopped), LIMITS, now());
        assert!(matches!(
            result.decision,
            api::motion::Decision::Stopped {
                reason: api::motion::ZeroReason::SafetyProtectiveStop
            }
        ));
        assert_eq!(
            stopped_reason(&result),
            api::motion::ZeroReason::SafetyProtectiveStop
        );
    }
}
