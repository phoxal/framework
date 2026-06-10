use phoxal::api::v1::drive::Target as DriveTarget;
use phoxal::api::v1::follow::Target as FollowTarget;
use phoxal::api::v1::motion::{ManualCommand, MotionReason, MotionSource};
use phoxal::api::v1::safety::{SafetyAuthorization, SafetyDecision};
use phoxal::bus::typed::Received;

const MANUAL_COMMAND_STALE_TIMEOUT_NS: u64 = 500_000_000; // 500 ms
const FOLLOW_TARGET_STALE_TIMEOUT_NS: u64 = 500_000_000;
const SAFETY_AUTHORIZATION_STALE_TIMEOUT_NS: u64 = 500_000_000; // 500 ms

#[derive(Debug, Clone, PartialEq)]
pub struct Arbitration {
    pub drive_target: DriveTarget,
    pub active_source: Option<MotionSource>,
    pub reason: Option<MotionReason>,
}

impl Arbitration {
    pub fn decide(
        manual_command: Option<&Received<ManualCommand>>,
        follow_target: Option<&Received<FollowTarget>>,
        safety_authorization: Option<&Received<SafetyAuthorization>>,
        now_ns: u64,
    ) -> Self {
        let Some(safety_authorization) = safety_authorization else {
            return stop_for_invalid_safety_authorization();
        };
        if safety_authorization_is_invalid(safety_authorization, now_ns) {
            return stop_for_invalid_safety_authorization();
        }

        let decision = safety_authorization.value.decision;

        // EmergencyStop is an unconditional hard stop: nothing, not even a manual command,
        // overrides it.
        if decision == SafetyDecision::EmergencyStop {
            return Self {
                drive_target: zero_target(),
                active_source: Some(MotionSource::EmergencyStop),
                reason: Some(MotionReason::SafetyEmergencyStop),
            };
        }

        if let Some(manual) = manual_command.filter(|manual| {
            manual.at_ns.is_some_and(|at_ns| {
                now_ns.saturating_sub(at_ns) <= MANUAL_COMMAND_STALE_TIMEOUT_NS
            })
        }) {
            // The operator drives by sight, so a manual command is not slowed by
            // localization uncertainty (UnknownConservative is bypassed). Every other
            // decision -- including a protective Stop -- still bounds the command to the
            // safety-approved envelope. A frontal-hazard Stop carries an *escape* envelope
            // (forward blocked, reverse + rotation allowed), so the operator can always
            // back the robot away instead of being wedged permanently.
            let approved = &safety_authorization.value.approved_motion;
            let drive_target = if decision == SafetyDecision::UnknownConservative {
                DriveTarget {
                    linear_x_mps: manual.value.linear_x_mps,
                    angular_z_radps: manual.value.angular_z_radps,
                }
            } else {
                DriveTarget {
                    linear_x_mps: clamp_to_constraint(
                        manual.value.linear_x_mps,
                        &approved.linear_x_mps,
                    ),
                    angular_z_radps: clamp_to_constraint(
                        manual.value.angular_z_radps,
                        &approved.angular_z_radps,
                    ),
                }
            };
            let reason =
                (decision == SafetyDecision::Stop).then_some(MotionReason::ManualEscapeUnderStop);
            return Self {
                drive_target,
                active_source: Some(MotionSource::Manual),
                reason,
            };
        }

        Self::arbitrate(follow_target, Some(safety_authorization), now_ns)
    }

    pub fn arbitrate(
        follow_target: Option<&Received<FollowTarget>>,
        safety_authorization: Option<&Received<SafetyAuthorization>>,
        now_ns: u64,
    ) -> Self {
        if let Some(authorization) = safety_authorization {
            let decision = authorization.value.decision;
            if matches!(
                decision,
                SafetyDecision::Stop | SafetyDecision::EmergencyStop
            ) {
                return Self {
                    drive_target: zero_target(),
                    active_source: Some(if matches!(decision, SafetyDecision::EmergencyStop) {
                        MotionSource::EmergencyStop
                    } else {
                        MotionSource::MissionStop
                    }),
                    reason: Some(MotionReason::SafetyConstrained(decision)),
                };
            }
        }

        let Some(follow) = follow_target else {
            return Self {
                drive_target: zero_target(),
                active_source: None,
                reason: Some(MotionReason::NoFollowTarget),
            };
        };

        if follow
            .at_ns
            .is_none_or(|at_ns| now_ns.saturating_sub(at_ns) > FOLLOW_TARGET_STALE_TIMEOUT_NS)
        {
            return Self {
                drive_target: zero_target(),
                active_source: None,
                reason: Some(MotionReason::FollowTargetStale),
            };
        }

        Self {
            drive_target: DriveTarget {
                linear_x_mps: follow.value.linear_x_mps,
                angular_z_radps: follow.value.angular_z_radps,
            },
            active_source: Some(MotionSource::Follow),
            reason: None,
        }
    }
}

fn safety_authorization_is_invalid(
    authorization: &Received<SafetyAuthorization>,
    now_ns: u64,
) -> bool {
    if authorization
        .value
        .expires_at_ns
        .is_some_and(|expires_at_ns| now_ns > expires_at_ns)
    {
        return true;
    }

    authorization
        .at_ns
        .is_none_or(|at_ns| now_ns.saturating_sub(at_ns) > SAFETY_AUTHORIZATION_STALE_TIMEOUT_NS)
}

fn stop_for_invalid_safety_authorization() -> Arbitration {
    Arbitration {
        drive_target: zero_target(),
        active_source: Some(MotionSource::MissionStop),
        reason: Some(MotionReason::SafetyAuthorizationUnavailable),
    }
}

fn clamp_to_constraint(value: f64, constraint: &phoxal::api::v1::safety::Constraint) -> f64 {
    value.clamp(constraint.min, constraint.max)
}

const fn zero_target() -> DriveTarget {
    DriveTarget {
        linear_x_mps: 0.0,
        angular_z_radps: 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoxal::api::v1::localize::LocalizationRevisionId;
    use phoxal::api::v1::map::MapRevisionId;
    use phoxal::api::v1::safety::{
        Constraint, MotionConstraint, RawSourceRevision, SafetyReason, SafetySourceRevision,
    };

    const NOW_NS: u64 = 1_000_000_000;

    #[test]
    fn zero_target_is_zero() {
        let target = zero_target();

        assert_eq!(target.linear_x_mps, 0.0);
        assert_eq!(target.angular_z_radps, 0.0);
    }

    #[test]
    fn arbitration_returns_zero_when_no_follow_target() {
        let arbitration = Arbitration::arbitrate(None, None, NOW_NS);

        assert_eq!(arbitration.drive_target, zero_target());
        assert_eq!(arbitration.active_source, None);
        assert_eq!(arbitration.reason, Some(MotionReason::NoFollowTarget));
    }

    #[test]
    fn arbitration_passes_through_fresh_follow_target() {
        let follow = received(NOW_NS, follow_target(0.5, 0.2));

        let arbitration = Arbitration::arbitrate(Some(&follow), None, NOW_NS);

        assert_eq!(arbitration.drive_target.linear_x_mps, 0.5);
        assert_eq!(arbitration.drive_target.angular_z_radps, 0.2);
        assert_eq!(arbitration.active_source, Some(MotionSource::Follow));
    }

    #[test]
    fn arbitration_zeroes_stale_follow_target() {
        let follow = received(NOW_NS - 1_000_000_000, follow_target(0.5, 0.2));

        let arbitration = Arbitration::arbitrate(Some(&follow), None, NOW_NS);

        assert_eq!(arbitration.drive_target, zero_target());
        assert_eq!(arbitration.active_source, None);
        assert_eq!(arbitration.reason, Some(MotionReason::FollowTargetStale));
    }

    #[test]
    fn arbitration_stops_on_safety_stop() {
        let follow = received(NOW_NS, follow_target(0.5, 0.2));
        let safety = received(NOW_NS, safety_authorization(SafetyDecision::Stop));

        let arbitration = Arbitration::arbitrate(Some(&follow), Some(&safety), NOW_NS);

        assert_eq!(arbitration.drive_target, zero_target());
        assert_eq!(arbitration.active_source, Some(MotionSource::MissionStop));
        assert_eq!(
            arbitration.reason,
            Some(MotionReason::SafetyConstrained(SafetyDecision::Stop))
        );
    }

    #[test]
    fn arbitration_emergency_stops_overrides() {
        let follow = received(NOW_NS, follow_target(0.5, 0.2));
        let safety = received(NOW_NS, safety_authorization(SafetyDecision::EmergencyStop));

        let arbitration = Arbitration::arbitrate(Some(&follow), Some(&safety), NOW_NS);

        assert_eq!(arbitration.drive_target, zero_target());
        assert_eq!(arbitration.active_source, Some(MotionSource::EmergencyStop));
        assert_eq!(
            arbitration.reason,
            Some(MotionReason::SafetyConstrained(
                SafetyDecision::EmergencyStop
            ))
        );
    }

    #[test]
    fn arbitration_passes_through_when_safety_allows() {
        let follow = received(NOW_NS, follow_target(0.5, 0.2));
        let safety = received(NOW_NS, safety_authorization(SafetyDecision::Allow));

        let arbitration = Arbitration::arbitrate(Some(&follow), Some(&safety), NOW_NS);

        assert_eq!(arbitration.drive_target.linear_x_mps, 0.5);
        assert_eq!(arbitration.drive_target.angular_z_radps, 0.2);
        assert_eq!(arbitration.active_source, Some(MotionSource::Follow));
    }

    #[test]
    fn decide_motion_stops_on_missing_authorization() {
        let arbitration = Arbitration::decide(None, None, None, NOW_NS);

        assert_eq!(arbitration.drive_target, zero_target());
        assert_eq!(arbitration.active_source, Some(MotionSource::MissionStop));
        assert_eq!(
            arbitration.reason,
            Some(MotionReason::SafetyAuthorizationUnavailable)
        );
    }

    #[test]
    fn decide_motion_stops_on_expired_authorization() {
        let follow = received(NOW_NS, follow_target(0.5, 0.2));
        let safety = received(
            NOW_NS,
            safety_authorization_with_expiry(SafetyDecision::Allow, Some(NOW_NS - 1)),
        );

        let arbitration = Arbitration::decide(None, Some(&follow), Some(&safety), NOW_NS);

        assert_eq!(arbitration.drive_target, zero_target());
        assert_eq!(arbitration.active_source, Some(MotionSource::MissionStop));
        assert_eq!(
            arbitration.reason,
            Some(MotionReason::SafetyAuthorizationUnavailable)
        );
    }

    #[test]
    fn decide_motion_stops_on_stale_authorization() {
        let follow = received(NOW_NS, follow_target(0.5, 0.2));
        let safety = received(
            NOW_NS - SAFETY_AUTHORIZATION_STALE_TIMEOUT_NS - 1,
            safety_authorization_with_expiry(SafetyDecision::Allow, Some(NOW_NS + 1_000)),
        );

        let arbitration = Arbitration::decide(None, Some(&follow), Some(&safety), NOW_NS);

        assert_eq!(arbitration.drive_target, zero_target());
        assert_eq!(arbitration.active_source, Some(MotionSource::MissionStop));
        assert_eq!(
            arbitration.reason,
            Some(MotionReason::SafetyAuthorizationUnavailable)
        );
    }

    #[test]
    fn decide_motion_drives_with_fresh_authorization() {
        let follow = received(NOW_NS, follow_target(0.5, 0.2));
        let safety = received(NOW_NS, safety_authorization(SafetyDecision::Allow));

        let arbitration = Arbitration::decide(None, Some(&follow), Some(&safety), NOW_NS);

        assert_eq!(arbitration.drive_target.linear_x_mps, 0.5);
        assert_eq!(arbitration.drive_target.angular_z_radps, 0.2);
        assert_eq!(arbitration.active_source, Some(MotionSource::Follow));
    }

    #[test]
    fn manual_command_overrides_follow() {
        let safety = received(
            NOW_NS,
            safety_authorization_with_limits(SafetyDecision::UnknownConservative, 0.10, 0.30),
        );
        let manual = received(
            NOW_NS,
            ManualCommand {
                linear_x_mps: 0.08,
                angular_z_radps: -0.2,
            },
        );
        let follow = received(NOW_NS, follow_target(0.5, 0.2));

        let arbitration = Arbitration::decide(Some(&manual), Some(&follow), Some(&safety), NOW_NS);

        assert_eq!(arbitration.active_source, Some(MotionSource::Manual));
        assert_eq!(arbitration.drive_target.linear_x_mps, 0.08);
        assert_eq!(arbitration.drive_target.angular_z_radps, -0.2);
        assert_eq!(arbitration.reason, None);
    }

    #[test]
    fn manual_command_bypasses_conservative_envelope() {
        let safety = received(
            NOW_NS,
            safety_authorization_with_limits(SafetyDecision::UnknownConservative, 0.10, 0.30),
        );
        let manual = received(
            NOW_NS,
            ManualCommand {
                linear_x_mps: 5.0,
                angular_z_radps: -5.0,
            },
        );

        let arbitration = Arbitration::decide(Some(&manual), None, Some(&safety), NOW_NS);

        assert_eq!(arbitration.active_source, Some(MotionSource::Manual));
        assert_eq!(arbitration.drive_target.linear_x_mps, 5.0);
        assert_eq!(arbitration.drive_target.angular_z_radps, -5.0);
    }

    #[test]
    fn manual_command_clamped_under_slow() {
        let safety = received(
            NOW_NS,
            safety_authorization_with_limits(SafetyDecision::Slow, 0.10, 0.30),
        );
        let manual = received(
            NOW_NS,
            ManualCommand {
                linear_x_mps: 5.0,
                angular_z_radps: -5.0,
            },
        );

        let arbitration = Arbitration::decide(Some(&manual), None, Some(&safety), NOW_NS);

        assert_eq!(arbitration.active_source, Some(MotionSource::Manual));
        assert_eq!(arbitration.drive_target.linear_x_mps, 0.10);
        assert_eq!(arbitration.drive_target.angular_z_radps, -0.30);
    }

    #[test]
    fn stale_manual_command_is_ignored() {
        let safety = received(
            NOW_NS,
            safety_authorization_with_limits(SafetyDecision::UnknownConservative, 0.10, 0.30),
        );
        let manual = received(
            NOW_NS - 1_000_000_000,
            ManualCommand {
                linear_x_mps: 0.08,
                angular_z_radps: 0.0,
            },
        );

        let arbitration = Arbitration::decide(Some(&manual), None, Some(&safety), NOW_NS);

        assert_eq!(arbitration.drive_target, zero_target());
        assert_eq!(arbitration.active_source, None);
    }

    #[test]
    fn emergency_stop_overrides_manual_command() {
        let safety = received(
            NOW_NS,
            safety_authorization_with_limits(SafetyDecision::EmergencyStop, 0.10, 0.30),
        );
        let manual = received(
            NOW_NS,
            ManualCommand {
                linear_x_mps: -0.10,
                angular_z_radps: 0.5,
            },
        );

        let arbitration = Arbitration::decide(Some(&manual), None, Some(&safety), NOW_NS);

        assert_eq!(arbitration.drive_target, zero_target());
        assert_eq!(arbitration.active_source, Some(MotionSource::EmergencyStop));
        assert_eq!(arbitration.reason, Some(MotionReason::SafetyEmergencyStop));
    }

    #[test]
    fn manual_forward_blocked_under_protective_stop() {
        let safety = received(
            NOW_NS,
            safety_authorization_with_escape(SafetyDecision::Stop, 0.15, 0.60),
        );
        let manual = received(
            NOW_NS,
            ManualCommand {
                linear_x_mps: 0.30,
                angular_z_radps: 0.0,
            },
        );

        let arbitration = Arbitration::decide(Some(&manual), None, Some(&safety), NOW_NS);

        assert_eq!(arbitration.active_source, Some(MotionSource::Manual));
        assert_eq!(arbitration.drive_target.linear_x_mps, 0.0);
        assert_eq!(arbitration.drive_target.angular_z_radps, 0.0);
        assert_eq!(
            arbitration.reason,
            Some(MotionReason::ManualEscapeUnderStop)
        );
    }

    #[test]
    fn manual_reverse_allowed_under_protective_stop() {
        let safety = received(
            NOW_NS,
            safety_authorization_with_escape(SafetyDecision::Stop, 0.15, 0.60),
        );
        let manual = received(
            NOW_NS,
            ManualCommand {
                linear_x_mps: -0.30,
                angular_z_radps: 0.0,
            },
        );

        let arbitration = Arbitration::decide(Some(&manual), None, Some(&safety), NOW_NS);

        assert_eq!(arbitration.active_source, Some(MotionSource::Manual));
        assert_eq!(arbitration.drive_target.linear_x_mps, -0.15);
        assert_eq!(arbitration.drive_target.angular_z_radps, 0.0);
    }

    #[test]
    fn manual_rotation_allowed_under_protective_stop() {
        let safety = received(
            NOW_NS,
            safety_authorization_with_escape(SafetyDecision::Stop, 0.15, 0.60),
        );
        let manual = received(
            NOW_NS,
            ManualCommand {
                linear_x_mps: 0.0,
                angular_z_radps: 1.5,
            },
        );

        let arbitration = Arbitration::decide(Some(&manual), None, Some(&safety), NOW_NS);

        assert_eq!(arbitration.active_source, Some(MotionSource::Manual));
        assert_eq!(arbitration.drive_target.linear_x_mps, 0.0);
        assert_eq!(arbitration.drive_target.angular_z_radps, 0.60);
    }

    fn received<T>(at_ns: u64, value: T) -> Received<T> {
        Received {
            at_ns: Some(at_ns),
            value,
        }
    }

    fn follow_target(linear_x_mps: f64, angular_z_radps: f64) -> FollowTarget {
        FollowTarget {
            map_revision: MapRevisionId {
                epoch: 1,
                sequence: 2,
            },
            built_from_localize_revision: LocalizationRevisionId {
                epoch: 1,
                sequence: 1,
            },
            frame_id: "base_footprint".into(),
            linear_x_mps,
            angular_z_radps,
        }
    }

    fn safety_authorization(decision: SafetyDecision) -> SafetyAuthorization {
        safety_authorization_with_limits(decision, 1.0, 1.0)
    }

    fn safety_authorization_with_expiry(
        decision: SafetyDecision,
        expires_at_ns: Option<u64>,
    ) -> SafetyAuthorization {
        let mut authorization = safety_authorization_with_limits(decision, 1.0, 1.0);
        authorization.expires_at_ns = expires_at_ns;
        authorization
    }

    fn safety_authorization_with_escape(
        decision: SafetyDecision,
        reverse_mps: f64,
        angular_radps: f64,
    ) -> SafetyAuthorization {
        let mut authorization = safety_authorization_with_limits(decision, 1.0, 1.0);
        authorization.approved_motion = MotionConstraint {
            linear_x_mps: Constraint {
                min: -reverse_mps,
                max: 0.0,
            },
            angular_z_radps: Constraint {
                min: -angular_radps,
                max: angular_radps,
            },
        };
        authorization
    }

    fn safety_authorization_with_limits(
        decision: SafetyDecision,
        linear_x_mps: f64,
        angular_z_radps: f64,
    ) -> SafetyAuthorization {
        SafetyAuthorization {
            decision,
            source_revision: SafetySourceRevision {
                localization: None,
                map: None,
                raw_sources: Vec::<RawSourceRevision>::new(),
            },
            approved_motion: motion_constraint(linear_x_mps, angular_z_radps),
            reasons: Vec::<SafetyReason>::new(),
            expires_at_ns: Some(NOW_NS + 1_000_000_000),
        }
    }

    fn motion_constraint(linear_x_mps: f64, angular_z_radps: f64) -> MotionConstraint {
        MotionConstraint {
            linear_x_mps: Constraint {
                min: -linear_x_mps,
                max: linear_x_mps,
            },
            angular_z_radps: Constraint {
                min: -angular_z_radps,
                max: angular_z_radps,
            },
        }
    }
}
