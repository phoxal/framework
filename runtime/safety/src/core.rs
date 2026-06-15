use std::collections::BTreeMap;

use phoxal::api::component::capability::range::v1 as range;
use phoxal::api::localize::v1::{LocalizationMode, LocalizationState};
use phoxal::api::safety::v1::{
    Constraint, MotionConstraint, SafetyDecision, SafetyReason, SafetyReasonCode,
};
use phoxal::bus::typed::Received;

/// Drop-off horizon beyond the expected floor return for a downward range sensor.
const CLIFF_DROP_MARGIN_M: f32 = 0.12;
/// Nearer-than-floor return horizon for a low obstacle under a downward range sensor.
const CLIFF_GROUND_OBSTACLE_MARGIN_M: f32 = 0.10;
/// Stop horizon: any range sample below this triggers Stop.
const STOP_DISTANCE_M: f32 = 0.30;
/// Stale horizon: a range source older than this triggers Stop with StaleSource.
const RANGE_STALE_TIMEOUT_NS: u64 = 500_000_000; // 500 ms
const LOCALIZATION_STALE_TIMEOUT_NS: u64 = 500_000_000; // 500 ms
/// Maximum linear/angular constraint when Allow.
const MAX_LINEAR_MPS: f64 = 0.6;
const MAX_ANGULAR_RADPS: f64 = 2.0;
/// Speed cap factor when Slow (DeadReckoning).
const SLOW_FACTOR: f64 = 0.30;
/// Conservative cap when localization is Initializing/Lost.
const UNKNOWN_CONSERVATIVE_LINEAR_MPS: f64 = 0.10;
const UNKNOWN_CONSERVATIVE_ANGULAR_RADPS: f64 = 0.30;
/// Escape envelope for a protective Stop from a frontal hazard (obstacle/cliff): forward is
/// blocked, but reverse and in-place rotation stay available so the robot can always back away
/// instead of being wedged. A blind Stop (stale range source) keeps the full zero envelope.
const ESCAPE_REVERSE_MPS: f64 = 0.15;
const ESCAPE_ANGULAR_RADPS: f64 = 0.6;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RangeSafetyClass {
    /// Near-horizontal forward obstacle sensor: stop when something is within STOP_DISTANCE_M.
    Obstacle,
    /// Downward ground/cliff sensor: floor at ~expected_floor_m is normal; stop on drop-off or ground obstacle.
    Cliff { expected_floor_m: f32 },
}

pub struct EvaluationOutcome {
    pub decision: SafetyDecision,
    pub motion_constraint: MotionConstraint,
    pub reasons: Vec<SafetyReason>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EmergencyStopInputs {
    pub hardware_engaged: bool,
    pub operator_engaged: bool,
}

impl EmergencyStopInputs {
    pub const fn engaged(self) -> bool {
        self.hardware_engaged || self.operator_engaged
    }
}

impl EvaluationOutcome {
    pub fn evaluate(
        range_samples: &BTreeMap<String, Received<range::Sample>>,
        range_classes: &BTreeMap<String, RangeSafetyClass>,
        localize_state: Option<&Received<LocalizationState>>,
        emergency_stop: EmergencyStopInputs,
        now_ns: u64,
    ) -> Self {
        if emergency_stop.engaged() {
            return Self {
                decision: SafetyDecision::EmergencyStop,
                motion_constraint: zero_motion(),
                reasons: vec![SafetyReason {
                    code: SafetyReasonCode::EmergencyStop,
                    detail: emergency_stop_detail(emergency_stop),
                }],
            };
        }

        // 1) Stale source -> Stop.
        for (source_id, sample) in range_samples {
            let age_ns = now_ns.saturating_sub(received_at_ns(sample));
            if age_ns > RANGE_STALE_TIMEOUT_NS {
                return Self {
                    decision: SafetyDecision::Stop,
                    motion_constraint: zero_motion(),
                    reasons: vec![SafetyReason {
                        code: SafetyReasonCode::StaleSource,
                        detail: Some(format!("{source_id} stale ({age_ns} ns)")),
                    }],
                };
            }
        }

        // 2) Obstacle, drop-off, or ground obstacle in stop horizon -> Stop.
        for (source_id, sample) in range_samples {
            let distance_m = sample.value.distance_m();
            let range_class = range_classes
                .get(source_id)
                .copied()
                .unwrap_or(RangeSafetyClass::Obstacle);
            match range_class {
                RangeSafetyClass::Obstacle if distance_m <= STOP_DISTANCE_M => {
                    return stop_for_obstacle(format!("{source_id} at {distance_m:.2} m"));
                }
                RangeSafetyClass::Obstacle => {}
                RangeSafetyClass::Cliff { expected_floor_m } => {
                    if distance_m > expected_floor_m + CLIFF_DROP_MARGIN_M {
                        return stop_for_obstacle(format!(
                            "{source_id} cliff/drop: {distance_m:.2} m > floor {expected_floor_m:.2} m"
                        ));
                    }
                    if distance_m > 0.0
                        && distance_m < expected_floor_m - CLIFF_GROUND_OBSTACLE_MARGIN_M
                    {
                        return stop_for_obstacle(format!(
                            "{source_id} ground obstacle: {distance_m:.2} m"
                        ));
                    }
                }
            }
        }

        // 3) Localization mode policy.
        let mode_outcome = match localize_state {
            Some(state) => {
                let age_ns = now_ns.saturating_sub(received_at_ns(state));
                if age_ns > LOCALIZATION_STALE_TIMEOUT_NS {
                    return Self {
                        decision: SafetyDecision::UnknownConservative,
                        motion_constraint: conservative_motion(),
                        reasons: vec![SafetyReason {
                            code: SafetyReasonCode::StaleSource,
                            detail: Some(format!("localization stale ({age_ns} ns)")),
                        }],
                    };
                }
                state.value.mode
            }
            None => {
                return Self {
                    decision: SafetyDecision::UnknownConservative,
                    motion_constraint: conservative_motion(),
                    reasons: vec![SafetyReason {
                        code: SafetyReasonCode::LocalizationMode,
                        detail: Some("no localization state".into()),
                    }],
                };
            }
        };
        if range_samples.is_empty() {
            return Self {
                decision: SafetyDecision::UnknownConservative,
                motion_constraint: conservative_motion(),
                reasons: vec![SafetyReason {
                    code: SafetyReasonCode::StaleSource,
                    detail: Some("no range evidence".into()),
                }],
            };
        }

        match mode_outcome {
            LocalizationMode::Initializing | LocalizationMode::Lost => Self {
                decision: SafetyDecision::UnknownConservative,
                motion_constraint: conservative_motion(),
                reasons: vec![SafetyReason {
                    code: SafetyReasonCode::LocalizationMode,
                    detail: Some(format!("localization {mode_outcome:?}")),
                }],
            },
            LocalizationMode::DeadReckoning | LocalizationMode::Relocalizing => Self {
                decision: SafetyDecision::Slow,
                motion_constraint: slow_motion(),
                reasons: vec![SafetyReason {
                    code: SafetyReasonCode::LocalizationMode,
                    detail: Some(format!("localization {mode_outcome:?}")),
                }],
            },
            LocalizationMode::Tracking => Self {
                decision: SafetyDecision::Allow,
                motion_constraint: full_motion(),
                reasons: vec![SafetyReason {
                    code: SafetyReasonCode::Clear,
                    detail: None,
                }],
            },
            _ => Self {
                decision: SafetyDecision::UnknownConservative,
                motion_constraint: conservative_motion(),
                reasons: vec![SafetyReason {
                    code: SafetyReasonCode::LocalizationMode,
                    detail: Some("unknown localization mode".into()),
                }],
            },
        }
    }
}

fn received_at_ns<T>(sample: &Received<T>) -> u64 {
    sample.at_ns.unwrap_or(0)
}

fn stop_for_obstacle(detail: String) -> EvaluationOutcome {
    EvaluationOutcome {
        decision: SafetyDecision::Stop,
        motion_constraint: obstacle_escape_motion(),
        reasons: vec![SafetyReason {
            code: SafetyReasonCode::Obstacle,
            detail: Some(detail),
        }],
    }
}

fn emergency_stop_detail(emergency_stop: EmergencyStopInputs) -> Option<String> {
    match (
        emergency_stop.hardware_engaged,
        emergency_stop.operator_engaged,
    ) {
        (true, true) => Some("hardware and operator emergency stop engaged".into()),
        (true, false) => Some("hardware emergency stop engaged".into()),
        (false, true) => Some("operator emergency stop engaged".into()),
        (false, false) => None,
    }
}

fn zero_motion() -> MotionConstraint {
    MotionConstraint {
        linear_x_mps: Constraint { min: 0.0, max: 0.0 },
        angular_z_radps: Constraint { min: 0.0, max: 0.0 },
    }
}

fn slow_motion() -> MotionConstraint {
    MotionConstraint {
        linear_x_mps: Constraint {
            min: -MAX_LINEAR_MPS * SLOW_FACTOR,
            max: MAX_LINEAR_MPS * SLOW_FACTOR,
        },
        angular_z_radps: Constraint {
            min: -MAX_ANGULAR_RADPS * SLOW_FACTOR,
            max: MAX_ANGULAR_RADPS * SLOW_FACTOR,
        },
    }
}

/// Protective-Stop envelope for a frontal hazard: forward blocked, reverse + rotation allowed.
fn obstacle_escape_motion() -> MotionConstraint {
    MotionConstraint {
        linear_x_mps: Constraint {
            min: -ESCAPE_REVERSE_MPS,
            max: 0.0,
        },
        angular_z_radps: Constraint {
            min: -ESCAPE_ANGULAR_RADPS,
            max: ESCAPE_ANGULAR_RADPS,
        },
    }
}

fn conservative_motion() -> MotionConstraint {
    MotionConstraint {
        linear_x_mps: Constraint {
            min: -UNKNOWN_CONSERVATIVE_LINEAR_MPS,
            max: UNKNOWN_CONSERVATIVE_LINEAR_MPS,
        },
        angular_z_radps: Constraint {
            min: -UNKNOWN_CONSERVATIVE_ANGULAR_RADPS,
            max: UNKNOWN_CONSERVATIVE_ANGULAR_RADPS,
        },
    }
}

fn full_motion() -> MotionConstraint {
    MotionConstraint {
        linear_x_mps: Constraint {
            min: -MAX_LINEAR_MPS,
            max: MAX_LINEAR_MPS,
        },
        angular_z_radps: Constraint {
            min: -MAX_ANGULAR_RADPS,
            max: MAX_ANGULAR_RADPS,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoxal::api::localize::v1::{
        LocalizationSource, LocalizationStatus, LocalizationStatusReason,
    };

    const NOW_NS: u64 = 2_000_000_000;
    const FRESH_SAMPLE_NS: u64 = NOW_NS - 10_000_000;

    #[test]
    fn evaluate_returns_unknown_conservative_when_no_inputs() {
        let outcome = EvaluationOutcome::evaluate(
            &BTreeMap::new(),
            &BTreeMap::new(),
            None,
            no_emergency_stop(),
            NOW_NS,
        );

        assert_eq!(outcome.decision, SafetyDecision::UnknownConservative);
        assert_eq!(outcome.motion_constraint, conservative_motion());
        assert_eq!(
            reason_codes(&outcome),
            vec![SafetyReasonCode::LocalizationMode]
        );
    }

    #[test]
    fn evaluate_returns_stop_on_obstacle_in_stop_horizon() {
        let outcome = EvaluationOutcome::evaluate(
            &range_samples([("front.range", FRESH_SAMPLE_NS, 0.20)]),
            &BTreeMap::new(),
            None,
            no_emergency_stop(),
            NOW_NS,
        );

        assert_eq!(outcome.decision, SafetyDecision::Stop);
        assert_eq!(outcome.motion_constraint, obstacle_escape_motion());
        assert_eq!(reason_codes(&outcome), vec![SafetyReasonCode::Obstacle]);
    }

    #[test]
    fn evaluate_returns_stop_on_stale_range_source() {
        let outcome = EvaluationOutcome::evaluate(
            &range_samples([("front.range", NOW_NS - 1_000_000_000, 1.0)]),
            &BTreeMap::new(),
            Some(&localize_state(LocalizationMode::Tracking)),
            no_emergency_stop(),
            NOW_NS,
        );

        assert_eq!(outcome.decision, SafetyDecision::Stop);
        assert_eq!(outcome.motion_constraint, zero_motion());
        assert_eq!(reason_codes(&outcome), vec![SafetyReasonCode::StaleSource]);
    }

    #[test]
    fn stale_localization_forces_conservative() {
        let stale_tracking = localize_state_at(
            LocalizationMode::Tracking,
            NOW_NS - LOCALIZATION_STALE_TIMEOUT_NS - 1,
        );

        let outcome = EvaluationOutcome::evaluate(
            &range_samples([("front.range", FRESH_SAMPLE_NS, 5.0)]),
            &BTreeMap::new(),
            Some(&stale_tracking),
            no_emergency_stop(),
            NOW_NS,
        );

        assert_eq!(outcome.decision, SafetyDecision::UnknownConservative);
        assert_eq!(outcome.motion_constraint, conservative_motion());
        assert_eq!(reason_codes(&outcome), vec![SafetyReasonCode::StaleSource]);
    }

    #[test]
    fn fresh_tracking_still_allows() {
        let outcome = EvaluationOutcome::evaluate(
            &range_samples([("front.range", FRESH_SAMPLE_NS, 5.0)]),
            &BTreeMap::new(),
            Some(&localize_state(LocalizationMode::Tracking)),
            no_emergency_stop(),
            NOW_NS,
        );

        assert_eq!(outcome.decision, SafetyDecision::Allow);
        assert_eq!(outcome.motion_constraint, full_motion());
        assert_eq!(reason_codes(&outcome), vec![SafetyReasonCode::Clear]);
    }

    #[test]
    fn evaluate_returns_slow_on_dead_reckoning_mode() {
        let outcome = EvaluationOutcome::evaluate(
            &range_samples([("front.range", FRESH_SAMPLE_NS, 5.0)]),
            &BTreeMap::new(),
            Some(&localize_state(LocalizationMode::DeadReckoning)),
            no_emergency_stop(),
            NOW_NS,
        );

        assert_eq!(outcome.decision, SafetyDecision::Slow);
        assert_eq!(outcome.motion_constraint, slow_motion());
        assert_eq!(
            reason_codes(&outcome),
            vec![SafetyReasonCode::LocalizationMode]
        );
    }

    #[test]
    fn evaluate_returns_unknown_conservative_on_initializing_mode() {
        let outcome = EvaluationOutcome::evaluate(
            &range_samples([("front.range", FRESH_SAMPLE_NS, 5.0)]),
            &BTreeMap::new(),
            Some(&localize_state(LocalizationMode::Initializing)),
            no_emergency_stop(),
            NOW_NS,
        );

        assert_eq!(outcome.decision, SafetyDecision::UnknownConservative);
        assert_eq!(outcome.motion_constraint, conservative_motion());
        assert_eq!(
            reason_codes(&outcome),
            vec![SafetyReasonCode::LocalizationMode]
        );
    }

    #[test]
    fn evaluate_returns_unknown_conservative_on_lost_mode() {
        let outcome = EvaluationOutcome::evaluate(
            &range_samples([("front.range", FRESH_SAMPLE_NS, 5.0)]),
            &BTreeMap::new(),
            Some(&localize_state(LocalizationMode::Lost)),
            no_emergency_stop(),
            NOW_NS,
        );

        assert_eq!(outcome.decision, SafetyDecision::UnknownConservative);
        assert_eq!(outcome.motion_constraint, conservative_motion());
        assert_eq!(
            reason_codes(&outcome),
            vec![SafetyReasonCode::LocalizationMode]
        );
    }

    #[test]
    fn evaluate_obstacle_overrides_localization_mode() {
        let outcome = EvaluationOutcome::evaluate(
            &range_samples([("front.range", FRESH_SAMPLE_NS, 0.10)]),
            &BTreeMap::new(),
            Some(&localize_state(LocalizationMode::Tracking)),
            no_emergency_stop(),
            NOW_NS,
        );

        assert_eq!(outcome.decision, SafetyDecision::Stop);
        assert_eq!(outcome.motion_constraint, obstacle_escape_motion());
        assert_eq!(reason_codes(&outcome), vec![SafetyReasonCode::Obstacle]);
    }

    #[test]
    fn cliff_sensor_normal_floor_reading_does_not_stop() {
        let outcome = EvaluationOutcome::evaluate(
            &range_samples([("front_left_ground_tof.range", FRESH_SAMPLE_NS, 0.30)]),
            &range_classes([(
                "front_left_ground_tof.range",
                RangeSafetyClass::Cliff {
                    expected_floor_m: 0.30,
                },
            )]),
            Some(&localize_state(LocalizationMode::Tracking)),
            no_emergency_stop(),
            NOW_NS,
        );

        assert_eq!(outcome.decision, SafetyDecision::Allow);
        assert_eq!(outcome.motion_constraint, full_motion());
        assert_eq!(reason_codes(&outcome), vec![SafetyReasonCode::Clear]);
    }

    #[test]
    fn cliff_sensor_dropoff_stops() {
        let outcome = EvaluationOutcome::evaluate(
            &range_samples([(
                "front_left_ground_tof.range",
                FRESH_SAMPLE_NS,
                0.30 + CLIFF_DROP_MARGIN_M + 0.05,
            )]),
            &range_classes([(
                "front_left_ground_tof.range",
                RangeSafetyClass::Cliff {
                    expected_floor_m: 0.30,
                },
            )]),
            Some(&localize_state(LocalizationMode::Tracking)),
            no_emergency_stop(),
            NOW_NS,
        );

        assert_eq!(outcome.decision, SafetyDecision::Stop);
        assert_eq!(outcome.motion_constraint, obstacle_escape_motion());
        assert_eq!(reason_codes(&outcome), vec![SafetyReasonCode::Obstacle]);
    }

    #[test]
    fn cliff_sensor_ground_obstacle_stops() {
        let outcome = EvaluationOutcome::evaluate(
            &range_samples([(
                "front_left_ground_tof.range",
                FRESH_SAMPLE_NS,
                0.30 - CLIFF_GROUND_OBSTACLE_MARGIN_M - 0.05,
            )]),
            &range_classes([(
                "front_left_ground_tof.range",
                RangeSafetyClass::Cliff {
                    expected_floor_m: 0.30,
                },
            )]),
            Some(&localize_state(LocalizationMode::Tracking)),
            no_emergency_stop(),
            NOW_NS,
        );

        assert_eq!(outcome.decision, SafetyDecision::Stop);
        assert_eq!(outcome.motion_constraint, obstacle_escape_motion());
        assert_eq!(reason_codes(&outcome), vec![SafetyReasonCode::Obstacle]);
    }

    #[test]
    fn obstacle_sensor_unchanged() {
        let outcome = EvaluationOutcome::evaluate(
            &range_samples([("front_center_tof.range", FRESH_SAMPLE_NS, 0.20)]),
            &range_classes([("front_center_tof.range", RangeSafetyClass::Obstacle)]),
            Some(&localize_state(LocalizationMode::Tracking)),
            no_emergency_stop(),
            NOW_NS,
        );

        assert_eq!(outcome.decision, SafetyDecision::Stop);
        assert_eq!(outcome.motion_constraint, obstacle_escape_motion());
        assert_eq!(reason_codes(&outcome), vec![SafetyReasonCode::Obstacle]);
    }

    #[test]
    fn obstacle_stop_allows_reverse_and_rotation_but_not_forward() {
        let escape = obstacle_escape_motion();

        // Forward is blocked.
        assert_eq!(escape.linear_x_mps.max, 0.0);
        // Reverse is permitted.
        assert!(escape.linear_x_mps.min < 0.0);
        // Rotation in either direction is permitted.
        assert!(escape.angular_z_radps.min < 0.0);
        assert!(escape.angular_z_radps.max > 0.0);
    }

    #[test]
    fn hardware_emergency_stop_engaged_returns_emergency_stop_with_zero_motion() {
        let outcome = EvaluationOutcome::evaluate(
            &range_samples([("front.range", FRESH_SAMPLE_NS, 5.0)]),
            &BTreeMap::new(),
            Some(&localize_state(LocalizationMode::Tracking)),
            EmergencyStopInputs {
                hardware_engaged: true,
                operator_engaged: false,
            },
            NOW_NS,
        );

        assert_eq!(outcome.decision, SafetyDecision::EmergencyStop);
        assert_eq!(outcome.motion_constraint, zero_motion());
        assert_eq!(
            outcome.motion_constraint.linear_x_mps,
            Constraint { min: 0.0, max: 0.0 }
        );
        assert_eq!(
            outcome.motion_constraint.angular_z_radps,
            Constraint { min: 0.0, max: 0.0 }
        );
        assert_eq!(
            reason_codes(&outcome),
            vec![SafetyReasonCode::EmergencyStop]
        );
    }

    #[test]
    fn operator_emergency_stop_engaged_returns_emergency_stop() {
        let outcome = EvaluationOutcome::evaluate(
            &range_samples([("front.range", FRESH_SAMPLE_NS, 5.0)]),
            &BTreeMap::new(),
            Some(&localize_state(LocalizationMode::Tracking)),
            EmergencyStopInputs {
                hardware_engaged: false,
                operator_engaged: true,
            },
            NOW_NS,
        );

        assert_eq!(outcome.decision, SafetyDecision::EmergencyStop);
        assert_eq!(outcome.motion_constraint, zero_motion());
        assert_eq!(
            reason_codes(&outcome),
            vec![SafetyReasonCode::EmergencyStop]
        );
    }

    #[test]
    fn no_emergency_stop_keeps_existing_allow_behavior() {
        let outcome = EvaluationOutcome::evaluate(
            &range_samples([("front.range", FRESH_SAMPLE_NS, 5.0)]),
            &BTreeMap::new(),
            Some(&localize_state(LocalizationMode::Tracking)),
            no_emergency_stop(),
            NOW_NS,
        );

        assert_eq!(outcome.decision, SafetyDecision::Allow);
        assert_eq!(outcome.motion_constraint, full_motion());
        assert_eq!(reason_codes(&outcome), vec![SafetyReasonCode::Clear]);
    }

    #[test]
    fn emergency_stop_wins_over_obstacle_stale_range_and_missing_localization() {
        let outcome = EvaluationOutcome::evaluate(
            &range_samples([("front.range", NOW_NS - 1_000_000_000, 0.10)]),
            &BTreeMap::new(),
            None,
            EmergencyStopInputs {
                hardware_engaged: true,
                operator_engaged: true,
            },
            NOW_NS,
        );

        assert_eq!(outcome.decision, SafetyDecision::EmergencyStop);
        assert_eq!(outcome.motion_constraint, zero_motion());
        assert_eq!(
            reason_codes(&outcome),
            vec![SafetyReasonCode::EmergencyStop]
        );
    }

    fn range_samples<const N: usize>(
        samples: [(&str, u64, f32); N],
    ) -> BTreeMap<String, Received<range::Sample>> {
        samples
            .into_iter()
            .map(|(source_id, timestamp_ns, distance_m)| {
                (
                    source_id.to_string(),
                    received(timestamp_ns, range::Sample::new(distance_m)),
                )
            })
            .collect()
    }

    fn range_classes<const N: usize>(
        classes: [(&str, RangeSafetyClass); N],
    ) -> BTreeMap<String, RangeSafetyClass> {
        classes
            .into_iter()
            .map(|(source_id, safety_class)| (source_id.to_string(), safety_class))
            .collect()
    }

    fn localize_state(mode: LocalizationMode) -> Received<LocalizationState> {
        localize_state_at(mode, FRESH_SAMPLE_NS)
    }

    const fn no_emergency_stop() -> EmergencyStopInputs {
        EmergencyStopInputs {
            hardware_engaged: false,
            operator_engaged: false,
        }
    }

    fn localize_state_at(mode: LocalizationMode, timestamp_ns: u64) -> Received<LocalizationState> {
        received(
            timestamp_ns,
            LocalizationState {
                mode,
                source: LocalizationSource::DeadReckoning,
                revision: None,
                pose: None,
                velocity: None,
                covariance: None,
                imu_bias: None,
                status: LocalizationStatus {
                    healthy: matches!(
                        mode,
                        LocalizationMode::Tracking
                            | LocalizationMode::DeadReckoning
                            | LocalizationMode::Relocalizing
                    ),
                    reasons: status_reasons(mode),
                },
                valid_at_ns: Some(timestamp_ns),
            },
        )
    }

    fn received<T>(timestamp_ns: u64, value: T) -> Received<T> {
        Received {
            at_ns: Some(timestamp_ns),
            value,
        }
    }

    fn status_reasons(mode: LocalizationMode) -> Vec<LocalizationStatusReason> {
        match mode {
            LocalizationMode::Tracking
            | LocalizationMode::DeadReckoning
            | LocalizationMode::Relocalizing => Vec::new(),
            LocalizationMode::Initializing => vec![LocalizationStatusReason::BackendInitializing],
            LocalizationMode::Lost => vec![LocalizationStatusReason::SensorStale],
            _ => vec![LocalizationStatusReason::BackendError],
        }
    }

    fn reason_codes(outcome: &EvaluationOutcome) -> Vec<SafetyReasonCode> {
        outcome.reasons.iter().map(|reason| reason.code).collect()
    }
}
