use phoxal::api::localize::v1::{LocalizationMode, LocalizationRevisionId, LocalizationState};
use phoxal::api::map::v1::{MapRevision, MapRevisionId};
use phoxal::api::mission::v1::{Goal, GoalPose};
use phoxal::api::plan::v1::{Path, PathPose, PlanReason, PlanStatus, State};

const WAYPOINT_SPACING_M: f64 = 0.25;

#[derive(Debug, Clone, PartialEq)]
pub enum PlanDecision {
    Idle,
    Refused {
        reason: PlanReason,
    },
    Waiting {
        reason: PlanReason,
    },
    Ready {
        poses: Vec<PathPose>,
        map_revision: MapRevisionId,
        built_from_localize_revision: LocalizationRevisionId,
        frame_id: String,
    },
}

impl PlanDecision {
    /// Builds the current planning decision from the latest goal, localization,
    /// and map revision without touching transport or runtime state.
    pub fn decide(
        goal: Option<&Goal>,
        localize: Option<&LocalizationState>,
        map_revision: Option<&MapRevision>,
    ) -> Self {
        let Some(goal) = goal else {
            return Self::Idle;
        };

        let GoalPose::Pose2 {
            xy_m,
            yaw_rad,
            frame_id,
            map_revision: goal_map_revision,
        } = &goal.pose
        else {
            return Self::Refused {
                reason: PlanReason::NonPlanarGoalUnsupported,
            };
        };

        let Some(localize) = localize else {
            return Self::Waiting {
                reason: PlanReason::NoLocalizationState,
            };
        };
        match localize.mode {
            LocalizationMode::Initializing => {
                return Self::Refused {
                    reason: PlanReason::LocalizationInitializing,
                };
            }
            LocalizationMode::Lost => {
                return Self::Refused {
                    reason: PlanReason::LocalizationLost,
                };
            }
            LocalizationMode::Relocalizing => {
                return Self::Refused {
                    reason: PlanReason::LocalizationRelocalizing,
                };
            }
            LocalizationMode::Tracking | LocalizationMode::DeadReckoning => {}
            _ => {
                return Self::Refused {
                    reason: PlanReason::UnsupportedLocalizationMode,
                };
            }
        }
        let Some(pose) = &localize.pose else {
            return Self::Waiting {
                reason: PlanReason::NoLocalizationPose,
            };
        };
        let Some(localize_revision) = localize.revision else {
            return Self::Waiting {
                reason: PlanReason::NoLocalizationRevision,
            };
        };
        let Some(map_revision) = map_revision else {
            return Self::Waiting {
                reason: PlanReason::NoMapRevision,
            };
        };

        if goal_map_revision
            .is_some_and(|goal_revision| goal_revision != map_revision.map_revision_id)
        {
            return Self::Refused {
                reason: PlanReason::GoalMapRevisionMismatch,
            };
        }
        if localize_revision != map_revision.built_from_localize_revision {
            return Self::Refused {
                reason: PlanReason::MapLocalizeRevisionMismatch,
            };
        }

        // MVP limitations: this is straight-line only, with no traversability cost
        // search or obstacle avoidance. Until the map-to-odom transform lands, the
        // current localization pose is treated as already being in the goal frame.
        let _start_yaw_rad = Self::yaw_from_xyzw(pose.rotation_xyzw);
        let start_xy = [pose.translation_m[0], pose.translation_m[1]];
        let poses = Self::straight_line_path(start_xy, *xy_m, *yaw_rad);
        Self::Ready {
            poses,
            map_revision: map_revision.map_revision_id,
            built_from_localize_revision: map_revision.built_from_localize_revision,
            frame_id: frame_id.clone(),
        }
    }

    pub fn outputs(&self, latest_goal: Option<&Goal>) -> (State, Option<Path>) {
        match self {
            Self::Idle => (
                State {
                    status: PlanStatus::Idle,
                    reason: None,
                },
                None,
            ),
            Self::Refused { reason } => (
                State {
                    status: PlanStatus::Refused,
                    reason: Some(*reason),
                },
                None,
            ),
            Self::Waiting { reason } => (
                State {
                    status: PlanStatus::Planning,
                    reason: Some(*reason),
                },
                None,
            ),
            Self::Ready {
                poses,
                map_revision,
                built_from_localize_revision,
                frame_id,
            } => {
                let path = latest_goal.map(|goal| Path {
                    goal: goal.clone(),
                    map_revision: *map_revision,
                    built_from_localize_revision: *built_from_localize_revision,
                    frame_id: frame_id.clone(),
                    poses: poses.clone(),
                });
                (
                    State {
                        status: PlanStatus::Ready,
                        reason: None,
                    },
                    path,
                )
            }
        }
    }

    /// Straight-line interpolation from start to goal. The final pose carries the
    /// goal yaw; intermediate poses face along the travel direction. Always returns
    /// at least the goal pose. Pure; no I/O.
    fn straight_line_path(
        start_xy_m: [f64; 2],
        goal_xy_m: [f64; 2],
        goal_yaw_rad: f64,
    ) -> Vec<PathPose> {
        let dx = goal_xy_m[0] - start_xy_m[0];
        let dy = goal_xy_m[1] - start_xy_m[1];
        let distance = (dx * dx + dy * dy).sqrt();
        if distance <= f64::EPSILON {
            return vec![PathPose {
                xy_m: goal_xy_m,
                yaw_rad: goal_yaw_rad,
            }];
        }
        let travel_yaw = dy.atan2(dx);
        let segments = (distance / WAYPOINT_SPACING_M).ceil().max(1.0) as usize;
        let mut poses = Vec::with_capacity(segments);
        for step in 1..=segments {
            let t = step as f64 / segments as f64;
            let xy = [start_xy_m[0] + dx * t, start_xy_m[1] + dy * t];
            let yaw = if step == segments {
                goal_yaw_rad
            } else {
                travel_yaw
            };
            poses.push(PathPose {
                xy_m: xy,
                yaw_rad: yaw,
            });
        }
        poses
    }

    fn yaw_from_xyzw(rotation_xyzw: [f64; 4]) -> f64 {
        let [x, y, z, w] = rotation_xyzw;
        let siny_cosp = 2.0 * (w * z + x * y);
        let cosy_cosp = 1.0 - 2.0 * (y * y + z * z);
        siny_cosp.atan2(cosy_cosp)
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::{FRAC_PI_4, PI};

    use phoxal::api::frame::v1::FrameId;
    use phoxal::api::localize::v1::{LocalizationSource, LocalizationStatus, PoseEstimate};
    use phoxal::api::map::v1::{MapRevisionCause, RegionSummary};
    use phoxal::api::mission::v1::{GoalSource, GoalTolerance};

    use super::*;

    const EPS: f64 = 1e-9;

    #[test]
    fn straight_line_path_reaches_goal() {
        let poses = PlanDecision::straight_line_path([0.0, 0.0], [1.0, 0.0], PI);

        assert_close(poses[poses.len() - 1].xy_m[0], 1.0);
        assert_close(poses[poses.len() - 1].xy_m[1], 0.0);
        assert_close(poses[poses.len() - 1].yaw_rad, PI);
    }

    #[test]
    fn straight_line_path_spacing() {
        let poses = PlanDecision::straight_line_path([0.0, 0.0], [1.0, 0.0], 0.0);

        assert_eq!(poses.len(), 4);
        let mut previous = [0.0, 0.0];
        for pose in poses {
            assert_close(distance(previous, pose.xy_m), WAYPOINT_SPACING_M);
            previous = pose.xy_m;
        }
    }

    #[test]
    fn straight_line_path_zero_distance() {
        let poses = PlanDecision::straight_line_path([1.0, 2.0], [1.0, 2.0], PI);

        assert_eq!(
            poses,
            vec![PathPose {
                xy_m: [1.0, 2.0],
                yaw_rad: PI,
            }]
        );
    }

    #[test]
    fn straight_line_path_intermediate_faces_travel() {
        let poses = PlanDecision::straight_line_path([0.0, 0.0], [1.0, 1.0], PI);

        for pose in &poses[..poses.len() - 1] {
            assert_close(pose.yaw_rad, FRAC_PI_4);
        }
        assert_close(poses[poses.len() - 1].yaw_rad, PI);
    }

    #[test]
    fn decide_idle_without_goal() {
        assert_eq!(PlanDecision::decide(None, None, None), PlanDecision::Idle);
    }

    #[test]
    fn decide_refuses_pose3_goal() {
        let goal = goal_pose3();

        assert!(matches!(
            PlanDecision::decide(
                Some(&goal),
                Some(&localize_tracking_with_pose()),
                Some(&map_revision())
            ),
            PlanDecision::Refused {
                reason: PlanReason::NonPlanarGoalUnsupported
            }
        ));
    }

    #[test]
    fn decide_waits_without_localization_state() {
        assert!(matches!(
            PlanDecision::decide(Some(&goal_pose2(None)), None, Some(&map_revision())),
            PlanDecision::Waiting {
                reason: PlanReason::NoLocalizationState
            }
        ));
    }

    #[test]
    fn decide_refuses_initializing() {
        assert_refused_for_mode(
            LocalizationMode::Initializing,
            PlanReason::LocalizationInitializing,
        );
    }

    #[test]
    fn decide_refuses_lost() {
        assert_refused_for_mode(LocalizationMode::Lost, PlanReason::LocalizationLost);
    }

    #[test]
    fn decide_refuses_relocalizing() {
        assert_refused_for_mode(
            LocalizationMode::Relocalizing,
            PlanReason::LocalizationRelocalizing,
        );
    }

    #[test]
    fn decide_waits_without_pose() {
        let localize = LocalizationState {
            pose: None,
            ..localize_tracking_with_pose()
        };

        assert!(matches!(
            PlanDecision::decide(
                Some(&goal_pose2(None)),
                Some(&localize),
                Some(&map_revision())
            ),
            PlanDecision::Waiting {
                reason: PlanReason::NoLocalizationPose
            }
        ));
    }

    #[test]
    fn decide_waits_without_localization_revision() {
        let localize = LocalizationState {
            revision: None,
            ..localize_tracking_with_pose()
        };

        assert!(matches!(
            PlanDecision::decide(
                Some(&goal_pose2(None)),
                Some(&localize),
                Some(&map_revision())
            ),
            PlanDecision::Waiting {
                reason: PlanReason::NoLocalizationRevision
            }
        ));
    }

    #[test]
    fn decide_waits_without_map_revision() {
        let localize = localize_tracking_with_pose();

        assert!(matches!(
            PlanDecision::decide(Some(&goal_pose2(None)), Some(&localize), None),
            PlanDecision::Waiting {
                reason: PlanReason::NoMapRevision
            }
        ));
    }

    #[test]
    fn decide_refuses_goal_map_revision_mismatch() {
        let goal = goal_pose2(Some(MapRevisionId {
            epoch: 1,
            sequence: 4,
        }));

        assert!(matches!(
            PlanDecision::decide(
                Some(&goal),
                Some(&localize_tracking_with_pose()),
                Some(&map_revision())
            ),
            PlanDecision::Refused {
                reason: PlanReason::GoalMapRevisionMismatch
            }
        ));
    }

    #[test]
    fn decide_refuses_map_localize_revision_mismatch() {
        let localize = LocalizationState {
            revision: Some(LocalizationRevisionId {
                epoch: 1,
                sequence: 8,
            }),
            ..localize_tracking_with_pose()
        };

        assert!(matches!(
            PlanDecision::decide(
                Some(&goal_pose2(None)),
                Some(&localize),
                Some(&map_revision())
            ),
            PlanDecision::Refused {
                reason: PlanReason::MapLocalizeRevisionMismatch
            }
        ));
    }

    #[test]
    fn decide_ready_links_map_and_localize_revision() {
        let decision = PlanDecision::decide(
            Some(&goal_pose2(Some(MapRevisionId {
                epoch: 1,
                sequence: 3,
            }))),
            Some(&localize_tracking_with_pose()),
            Some(&map_revision()),
        );

        let PlanDecision::Ready {
            poses,
            map_revision,
            built_from_localize_revision,
            frame_id,
        } = decision
        else {
            panic!("expected ready decision");
        };
        assert_eq!(
            map_revision,
            MapRevisionId {
                epoch: 1,
                sequence: 3,
            }
        );
        assert_eq!(
            built_from_localize_revision,
            LocalizationRevisionId {
                epoch: 1,
                sequence: 7,
            }
        );
        assert_eq!(frame_id, "map");
        assert!(!poses.is_empty());
        assert_close(poses[poses.len() - 1].xy_m[0], 1.0);
        assert_close(poses[poses.len() - 1].xy_m[1], 0.0);
    }

    fn assert_refused_for_mode(mode: LocalizationMode, expected_reason: PlanReason) {
        let localize = LocalizationState {
            mode,
            ..localize_tracking_with_pose()
        };

        assert!(matches!(
            PlanDecision::decide(
                Some(&goal_pose2(None)),
                Some(&localize),
                Some(&map_revision())
            ),
            PlanDecision::Refused { reason } if reason == expected_reason
        ));
    }

    fn goal_pose2(map_revision: Option<MapRevisionId>) -> Goal {
        Goal {
            pose: GoalPose::Pose2 {
                frame_id: "map".into(),
                map_revision,
                xy_m: [1.0, 0.0],
                yaw_rad: PI,
            },
            tolerance: GoalTolerance {
                pos_m: 0.1,
                yaw_rad: Some(0.1),
            },
            max_duration_ns: None,
            source: GoalSource::Operator,
        }
    }

    fn goal_pose3() -> Goal {
        Goal {
            pose: GoalPose::Pose3 {
                frame_id: "map".into(),
                map_revision: None,
                translation_m: [1.0, 0.0, 0.0],
                rotation_wxyz: [1.0, 0.0, 0.0, 0.0],
            },
            tolerance: GoalTolerance {
                pos_m: 0.1,
                yaw_rad: Some(0.1),
            },
            max_duration_ns: None,
            source: GoalSource::Operator,
        }
    }

    fn localize_tracking_with_pose() -> LocalizationState {
        LocalizationState {
            mode: LocalizationMode::Tracking,
            source: LocalizationSource::DeadReckoning,
            revision: Some(LocalizationRevisionId {
                epoch: 1,
                sequence: 7,
            }),
            pose: Some(PoseEstimate {
                frame_id: FrameId("map".into()),
                child_frame_id: FrameId("base_footprint".into()),
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

    fn map_revision() -> MapRevision {
        MapRevision {
            map_revision_id: MapRevisionId {
                epoch: 1,
                sequence: 3,
            },
            previous_map_revision_id: None,
            built_from_localize_revision: LocalizationRevisionId {
                epoch: 1,
                sequence: 7,
            },
            cause: MapRevisionCause::SensorIntegration,
            affected_region: Some(RegionSummary {
                frame_id: FrameId("map".into()),
                min_xyz_m: [-1.0, -1.0, -0.1],
                max_xyz_m: [1.0, 1.0, 0.1],
            }),
        }
    }

    fn distance(a: [f64; 2], b: [f64; 2]) -> f64 {
        let dx = b[0] - a[0];
        let dy = b[1] - a[1];
        (dx * dx + dy * dy).sqrt()
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= EPS,
            "{actual} differs from {expected}"
        );
    }
}
