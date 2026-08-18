use std::f64::consts::{FRAC_PI_2, PI};

use phoxal::api;
use phoxal::geometry::{normalize_angle, planar_distance};

const GOAL_TOLERANCE_M: f64 = 0.15;
const GOAL_YAW_TOLERANCE_RAD: f64 = 0.10;
const LOOKAHEAD_M: f64 = 0.40;
const K_LINEAR: f64 = 0.8;
const K_ANGULAR: f64 = 1.5;
const MAX_LINEAR_MPS: f64 = 0.5;
const MAX_ANGULAR_RADPS: f64 = 1.5;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Output {
    pub(crate) linear_x_mps: f32,
    pub(crate) angular_z_radps: f32,
    pub(crate) target_index: usize,
    pub(crate) distance_remaining_m: f64,
    pub(crate) finished: bool,
}

pub(crate) fn pursue(
    path: &api::navigation::PlannedPath,
    localization: &api::localize::LocalizationState,
) -> Option<Output> {
    let final_pose = path.poses.last()?;
    if !localization.is_usable() {
        return None;
    }

    let robot_xy = (localization.x_m, localization.y_m);
    let final_index = path.poses.len() - 1;
    let distance_remaining_m = planar_distance(robot_xy, pose_xy(final_pose));
    if distance_remaining_m <= GOAL_TOLERANCE_M {
        if let Some(goal_yaw) = final_pose.yaw_rad {
            let yaw_error = normalize_angle(goal_yaw - localization.yaw_rad);
            if yaw_error.abs() > GOAL_YAW_TOLERANCE_RAD {
                return Some(Output {
                    linear_x_mps: 0.0,
                    angular_z_radps: (K_ANGULAR * yaw_error)
                        .clamp(-MAX_ANGULAR_RADPS, MAX_ANGULAR_RADPS)
                        as f32,
                    target_index: final_index,
                    distance_remaining_m,
                    finished: false,
                });
            }
        }
        return Some(Output {
            linear_x_mps: 0.0,
            angular_z_radps: 0.0,
            target_index: final_index,
            distance_remaining_m,
            finished: true,
        });
    }

    let target_index = lookahead_index(&path.poses, robot_xy);
    let target = &path.poses[target_index];
    let target_heading = (target.y_m - localization.y_m).atan2(target.x_m - localization.x_m);
    let forward_error = normalize_angle(target_heading - localization.yaw_rad);
    let reverse = forward_error.abs() > FRAC_PI_2;
    let (steer_error, direction) = if reverse {
        (
            normalize_angle(target_heading + PI - localization.yaw_rad),
            -1.0,
        )
    } else {
        (forward_error, 1.0)
    };

    let angular_z_radps = (K_ANGULAR * steer_error).clamp(-MAX_ANGULAR_RADPS, MAX_ANGULAR_RADPS);
    let alignment = steer_error.cos().max(0.0);
    let linear_x_mps = (direction * K_LINEAR * distance_remaining_m * alignment)
        .clamp(-MAX_LINEAR_MPS, MAX_LINEAR_MPS);

    Some(Output {
        linear_x_mps: linear_x_mps as f32,
        angular_z_radps: angular_z_radps as f32,
        target_index,
        distance_remaining_m,
        finished: false,
    })
}

fn lookahead_index(poses: &[api::navigation::Pose], robot_xy: (f64, f64)) -> usize {
    let nearest = poses
        .iter()
        .enumerate()
        .min_by(|(_, left), (_, right)| {
            squared_distance(robot_xy, pose_xy(left))
                .total_cmp(&squared_distance(robot_xy, pose_xy(right)))
        })
        .map_or(0, |(index, _)| index);
    let mut along = planar_distance(robot_xy, pose_xy(&poses[nearest]));
    if along >= LOOKAHEAD_M {
        return nearest;
    }
    for index in (nearest + 1)..poses.len() {
        along += planar_distance(pose_xy(&poses[index - 1]), pose_xy(&poses[index]));
        if along >= LOOKAHEAD_M {
            return index;
        }
    }
    poses.len() - 1
}

fn pose_xy(pose: &api::navigation::Pose) -> (f64, f64) {
    (pose.x_m, pose.y_m)
}

/// Distance squared, for ranking candidates against each other.
///
/// Picking the nearest pose only needs the order, and comparing squares keeps
/// that comparison exact - a square root would round two genuinely different
/// candidates onto the same value and make the choice depend on iteration
/// order.
fn squared_distance(left: (f64, f64), right: (f64, f64)) -> f64 {
    let dx = right.0 - left.0;
    let dy = right.1 - left.1;
    dx * dx + dy * dy
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reaches_goal_with_zero_candidate() {
        let path = api::navigation::PlannedPath {
            poses: vec![api::navigation::Pose {
                x_m: 0.1,
                y_m: 0.0,
                yaw_rad: None,
            }],
            map_revision: None,
        };
        let output = pursue(&path, &localization(0.0, 0.0, 0.0)).unwrap();
        assert!(output.finished);
        assert_eq!(output.linear_x_mps, 0.0);
    }

    #[test]
    fn aligns_requested_goal_yaw_before_success() {
        let path = api::navigation::PlannedPath {
            poses: vec![api::navigation::Pose {
                x_m: 0.0,
                y_m: 0.0,
                yaw_rad: Some(1.0),
            }],
            map_revision: None,
        };
        let aligning = pursue(&path, &localization(0.0, 0.0, 0.0)).unwrap();
        assert!(!aligning.finished);
        assert_eq!(aligning.linear_x_mps, 0.0);
        assert!(aligning.angular_z_radps > 0.0);

        let finished = pursue(&path, &localization(0.0, 0.0, 0.95)).unwrap();
        assert!(finished.finished);
        assert_eq!(finished.angular_z_radps, 0.0);
    }

    #[test]
    fn target_behind_selects_reverse_motion() {
        let path = api::navigation::PlannedPath {
            poses: vec![api::navigation::Pose {
                x_m: -1.0,
                y_m: 0.0,
                yaw_rad: None,
            }],
            map_revision: None,
        };
        assert!(
            pursue(&path, &localization(0.0, 0.0, 0.0))
                .unwrap()
                .linear_x_mps
                < 0.0
        );
    }

    fn localization(x_m: f64, y_m: f64, yaw_rad: f64) -> api::localize::LocalizationState {
        api::localize::LocalizationState {
            x_m,
            y_m,
            yaw_rad,
            confidence: 1.0,
        }
    }
}
