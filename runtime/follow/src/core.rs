use std::f64::consts::{FRAC_PI_2, PI, TAU};

use phoxal_api_follow::v1::{FollowReason, FollowStatus, State, Target};
use phoxal_api_localize::v1::{LocalizationMode, LocalizationState};
use phoxal_api_map::v1::MapRevisionId;
use phoxal_api_plan::v1::{Path, PathPose};

const DEFAULT_FRAME_ID: &str = "map";

/// Tunables. Conservative caps; motion + drive enforce the resolved robot
/// limits downstream.
pub const GOAL_TOLERANCE_M: f64 = 0.15;
pub const LOOKAHEAD_M: f64 = 0.40;
pub const K_LINEAR: f64 = 0.8;
pub const K_ANGULAR: f64 = 1.5;
pub const MAX_LINEAR_MPS: f64 = 0.5;
pub const MAX_ANGULAR_RADPS: f64 = 1.5;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ControlOutput {
    pub linear_x_mps: f64,
    pub angular_z_radps: f64,
    pub arrived: bool,
}

impl ControlOutput {
    /// Pure proportional pure-pursuit toward the path. No I/O.
    /// - `poses` is the path in the planar frame; `robot_xy_m` / `robot_yaw_rad`
    ///   are the robot's planar pose in the same frame.
    /// - Returns zero output with arrived=true once within GOAL_TOLERANCE_M of the
    ///   final pose.
    pub fn pursue(poses: &[PathPose], robot_xy_m: [f64; 2], robot_yaw_rad: f64) -> Self {
        let Some(final_pose) = poses.last() else {
            return zero_control(true);
        };
        let dist_to_goal = distance(robot_xy_m, final_pose.xy_m);
        if dist_to_goal <= GOAL_TOLERANCE_M {
            return zero_control(true);
        }

        let target = poses
            .iter()
            .find(|pose| distance(robot_xy_m, pose.xy_m) > LOOKAHEAD_M)
            .unwrap_or(final_pose);

        let dx = target.xy_m[0] - robot_xy_m[0];
        let dy = target.xy_m[1] - robot_xy_m[1];
        let target_heading = dy.atan2(dx);

        // Planar reverse-pursuit MVP: choose reverse purely by target-behind
        // geometry. Final goal-yaw alignment and forward/reverse hysteresis are
        // later refinements.
        let forward_error = normalize_angle(target_heading - robot_yaw_rad);
        let reverse = forward_error.abs() > FRAC_PI_2;
        let (steer_error, direction) = if reverse {
            (
                normalize_angle(target_heading + PI - robot_yaw_rad),
                -1.0_f64,
            )
        } else {
            (forward_error, 1.0_f64)
        };

        let angular = (K_ANGULAR * steer_error).clamp(-MAX_ANGULAR_RADPS, MAX_ANGULAR_RADPS);
        let alignment = steer_error.cos().max(0.0);
        let linear = (direction * K_LINEAR * dist_to_goal * alignment)
            .clamp(-MAX_LINEAR_MPS, MAX_LINEAR_MPS);

        Self {
            linear_x_mps: linear,
            angular_z_radps: angular,
            arrived: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum FollowDecision {
    Idle,
    Refused { reason: FollowReason },
    Paused { reason: FollowReason },
    Track(ControlOutput),
}

impl FollowDecision {
    /// Decide whether the active path can produce a follow target.
    ///
    /// MVP limitations: DeadReckoning is treated the same as Tracking; no timeout
    /// or distance budget is enforced yet. Follow also does not consume safety
    /// authorization or traversability here: motion applies the safety override,
    /// and traversability re-checking belongs to a later slice.
    pub fn decide(path: Option<&Path>, localize: Option<&LocalizationState>) -> Self {
        let Some(path) = path else {
            return Self::Idle;
        };
        let Some(localize) = localize else {
            return Self::Refused {
                reason: FollowReason::NoLocalizationState,
            };
        };

        match localize.mode {
            LocalizationMode::Initializing => {
                return Self::Refused {
                    reason: FollowReason::LocalizationInitializing,
                };
            }
            LocalizationMode::Lost => {
                return Self::Refused {
                    reason: FollowReason::LocalizationLost,
                };
            }
            LocalizationMode::Relocalizing => {
                return Self::Paused {
                    reason: FollowReason::LocalizationRelocalizing,
                };
            }
            LocalizationMode::Tracking | LocalizationMode::DeadReckoning => {}
            _ => {
                return Self::Refused {
                    reason: FollowReason::UnsupportedLocalizationMode,
                };
            }
        }

        match localize.revision {
            Some(rev) if rev.epoch == path.built_from_localize_revision.epoch => {}
            Some(_) => {
                return Self::Paused {
                    reason: FollowReason::PathLocalizeRevisionMismatch,
                };
            }
            None => {
                return Self::Paused {
                    reason: FollowReason::LocalizationRevisionUnknown,
                };
            }
        }

        let Some(pose) = &localize.pose else {
            return Self::Refused {
                reason: FollowReason::NoLocalizationPose,
            };
        };
        let robot_xy = [pose.translation_m[0], pose.translation_m[1]];
        let robot_yaw = yaw_from_xyzw(pose.rotation_xyzw);
        Self::Track(ControlOutput::pursue(&path.poses, robot_xy, robot_yaw))
    }

    pub fn outputs(&self, latest_path: Option<&Path>) -> (State, Target) {
        match self {
            Self::Idle => (
                State {
                    status: FollowStatus::Idle,
                    reason: None,
                },
                zero_target(latest_path),
            ),
            Self::Refused { reason } => (
                State {
                    status: FollowStatus::Refused,
                    reason: Some(*reason),
                },
                zero_target(latest_path),
            ),
            Self::Paused { reason } => (
                State {
                    status: FollowStatus::Paused,
                    reason: Some(*reason),
                },
                zero_target(latest_path),
            ),
            Self::Track(output) => {
                let reason = output.arrived.then_some(FollowReason::Arrived);
                let target = latest_path
                    .map(|path| target_from_path(path, *output))
                    .unwrap_or_else(|| zero_target(None));
                (
                    State {
                        status: FollowStatus::Tracking,
                        reason,
                    },
                    target,
                )
            }
        }
    }
}

fn target_from_path(path: &Path, output: ControlOutput) -> Target {
    Target {
        map_revision: path.map_revision,
        built_from_localize_revision: path.built_from_localize_revision,
        frame_id: path.frame_id.clone(),
        linear_x_mps: output.linear_x_mps,
        angular_z_radps: output.angular_z_radps,
    }
}

fn zero_target(latest_path: Option<&Path>) -> Target {
    latest_path
        .map(|path| Target {
            map_revision: path.map_revision,
            built_from_localize_revision: path.built_from_localize_revision,
            frame_id: path.frame_id.clone(),
            linear_x_mps: 0.0,
            angular_z_radps: 0.0,
        })
        .unwrap_or_else(|| Target {
            map_revision: MapRevisionId {
                epoch: 0,
                sequence: 0,
            },
            built_from_localize_revision: phoxal_api_localize::v1::LocalizationRevisionId {
                epoch: 0,
                sequence: 0,
            },
            frame_id: DEFAULT_FRAME_ID.into(),
            linear_x_mps: 0.0,
            angular_z_radps: 0.0,
        })
}

fn zero_control(arrived: bool) -> ControlOutput {
    ControlOutput {
        linear_x_mps: 0.0,
        angular_z_radps: 0.0,
        arrived,
    }
}

fn distance(a: [f64; 2], b: [f64; 2]) -> f64 {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    (dx * dx + dy * dy).sqrt()
}

fn normalize_angle(a: f64) -> f64 {
    (a + PI).rem_euclid(TAU) - PI
}

fn yaw_from_xyzw(rotation_xyzw: [f64; 4]) -> f64 {
    let [x, y, z, w] = rotation_xyzw;
    let siny_cosp = 2.0 * (w * z + x * y);
    let cosy_cosp = 1.0 - 2.0 * (y * y + z * z);
    siny_cosp.atan2(cosy_cosp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoxal_api_frame::v1::FrameId;
    use phoxal_api_localize::v1::{
        LocalizationRevisionId, LocalizationSource, LocalizationStatus, PoseEstimate,
    };
    use phoxal_api_mission::v1::{Goal, GoalPose, GoalSource, GoalTolerance};

    #[test]
    fn control_arrived_within_tolerance() {
        let output = ControlOutput::pursue(&[path_pose([0.0, 0.0])], [0.0, 0.0], 0.0);

        assert!(output.arrived);
        assert_eq!(output.linear_x_mps, 0.0);
        assert_eq!(output.angular_z_radps, 0.0);
    }

    #[test]
    fn control_drives_forward_to_goal_ahead() {
        let output = ControlOutput::pursue(&[path_pose([1.0, 0.0])], [0.0, 0.0], 0.0);

        assert!(!output.arrived);
        assert!(output.linear_x_mps > 0.0);
        assert!(output.angular_z_radps.abs() < 0.05);
    }

    #[test]
    fn control_turns_toward_lateral_goal() {
        let output = ControlOutput::pursue(&[path_pose([0.0, 2.0])], [0.0, 0.0], 0.0);

        assert!(output.angular_z_radps > 0.0);
        assert!(output.linear_x_mps < MAX_LINEAR_MPS);
    }

    #[test]
    fn control_reverses_to_goal_behind() {
        let output = ControlOutput::pursue(&[path_pose([-1.0, 0.0])], [0.0, 0.0], 0.0);

        assert!(!output.arrived);
        assert!(output.linear_x_mps < 0.0);
        assert!(output.angular_z_radps.abs() < 0.1);
    }

    #[test]
    fn control_empty_path_is_arrived() {
        let output = ControlOutput::pursue(&[], [0.0, 0.0], 0.0);

        assert!(output.arrived);
        assert_eq!(output.linear_x_mps, 0.0);
        assert_eq!(output.angular_z_radps, 0.0);
    }

    #[test]
    fn control_caps_linear_and_angular() {
        let output = ControlOutput::pursue(&[path_pose([100.0, 100.0])], [0.0, 0.0], 0.0);

        assert!(output.linear_x_mps <= MAX_LINEAR_MPS);
        assert!(output.angular_z_radps.abs() <= MAX_ANGULAR_RADPS);
    }

    #[test]
    fn decide_idle_without_path() {
        assert_eq!(FollowDecision::decide(None, None), FollowDecision::Idle);
    }

    #[test]
    fn decide_refuses_without_localization_state() {
        let path = path(2);

        assert!(matches!(
            FollowDecision::decide(Some(&path), None),
            FollowDecision::Refused {
                reason: FollowReason::NoLocalizationState
            }
        ));
    }

    #[test]
    fn decide_refuses_initializing() {
        let path = path(2);
        let localize = localization_state(LocalizationMode::Initializing, Some(revision(2)), true);

        assert!(matches!(
            FollowDecision::decide(Some(&path), Some(&localize)),
            FollowDecision::Refused {
                reason: FollowReason::LocalizationInitializing
            }
        ));
    }

    #[test]
    fn decide_refuses_lost() {
        let path = path(2);
        let localize = localization_state(LocalizationMode::Lost, Some(revision(2)), true);

        assert!(matches!(
            FollowDecision::decide(Some(&path), Some(&localize)),
            FollowDecision::Refused {
                reason: FollowReason::LocalizationLost
            }
        ));
    }

    #[test]
    fn decide_pauses_relocalizing() {
        let path = path(2);
        let localize = localization_state(LocalizationMode::Relocalizing, Some(revision(2)), true);

        assert!(matches!(
            FollowDecision::decide(Some(&path), Some(&localize)),
            FollowDecision::Paused {
                reason: FollowReason::LocalizationRelocalizing
            }
        ));
    }

    #[test]
    fn decide_pauses_on_revision_epoch_mismatch() {
        let path = path(2);
        let localize = localization_state(LocalizationMode::Tracking, Some(revision(1)), true);

        assert!(matches!(
            FollowDecision::decide(Some(&path), Some(&localize)),
            FollowDecision::Paused {
                reason: FollowReason::PathLocalizeRevisionMismatch
            }
        ));
    }

    #[test]
    fn decide_pauses_without_localization_revision() {
        let path = path(2);
        let localize = localization_state(LocalizationMode::Tracking, None, true);

        assert!(matches!(
            FollowDecision::decide(Some(&path), Some(&localize)),
            FollowDecision::Paused {
                reason: FollowReason::LocalizationRevisionUnknown
            }
        ));
    }

    #[test]
    fn decide_refuses_without_pose() {
        let path = path(2);
        let localize = localization_state(LocalizationMode::Tracking, Some(revision(2)), false);

        assert!(matches!(
            FollowDecision::decide(Some(&path), Some(&localize)),
            FollowDecision::Refused {
                reason: FollowReason::NoLocalizationPose
            }
        ));
    }

    #[test]
    fn decide_tracks_with_matching_revision_and_pose() {
        let path = path(2);
        let localize = localization_state(LocalizationMode::Tracking, Some(revision(2)), true);

        let decision = FollowDecision::decide(Some(&path), Some(&localize));

        assert!(matches!(
            decision,
            FollowDecision::Track(ControlOutput {
                linear_x_mps,
                ..
            }) if linear_x_mps > 0.0
        ));
    }

    #[test]
    fn outputs_marks_arrival_with_typed_reason() {
        let (state, target) = FollowDecision::Track(ControlOutput::pursue(
            &[path_pose([0.0, 0.0])],
            [0.0, 0.0],
            0.0,
        ))
        .outputs(Some(&path(2)));

        assert_eq!(state.status, FollowStatus::Tracking);
        assert_eq!(state.reason, Some(FollowReason::Arrived));
        assert_eq!(target.linear_x_mps, 0.0);
        assert_eq!(target.angular_z_radps, 0.0);
    }

    fn path_pose(xy_m: [f64; 2]) -> PathPose {
        PathPose { xy_m, yaw_rad: 0.0 }
    }

    fn path(localize_epoch: u64) -> Path {
        Path {
            goal: goal(),
            map_revision: MapRevisionId {
                epoch: 7,
                sequence: 11,
            },
            built_from_localize_revision: revision(localize_epoch),
            frame_id: DEFAULT_FRAME_ID.into(),
            poses: vec![path_pose([2.0, 0.0])],
        }
    }

    fn goal() -> Goal {
        Goal {
            pose: GoalPose::Pose2 {
                frame_id: DEFAULT_FRAME_ID.into(),
                map_revision: None,
                xy_m: [2.0, 0.0],
                yaw_rad: 0.0,
            },
            tolerance: GoalTolerance {
                pos_m: GOAL_TOLERANCE_M,
                yaw_rad: None,
            },
            max_duration_ns: None,
            source: GoalSource::Operator,
        }
    }

    fn localization_state(
        mode: LocalizationMode,
        revision: Option<LocalizationRevisionId>,
        include_pose: bool,
    ) -> LocalizationState {
        LocalizationState {
            mode,
            source: LocalizationSource::DeadReckoning,
            revision,
            pose: include_pose.then(|| PoseEstimate {
                frame_id: FrameId::new(DEFAULT_FRAME_ID),
                child_frame_id: FrameId::new("base_link"),
                translation_m: [0.0, 0.0, 0.0],
                rotation_xyzw: [0.0, 0.0, 0.0, 1.0],
            }),
            velocity: None,
            covariance: None,
            imu_bias: None,
            status: LocalizationStatus {
                healthy: true,
                reasons: Vec::new(),
            },
            valid_at_ns: Some(1),
        }
    }

    const fn revision(epoch: u64) -> LocalizationRevisionId {
        LocalizationRevisionId { epoch, sequence: 1 }
    }
}
