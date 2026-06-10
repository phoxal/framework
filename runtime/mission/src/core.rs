use std::f64::consts::PI;

use phoxal::api::v1::explore::GoalCandidates;
use phoxal::api::v1::localize::{LocalizationMode, PoseEstimate};
use phoxal::api::v1::mission::{
    Goal, GoalPose, GoalSource, MissionCommand, MissionFailure, MissionMode, State,
};

#[derive(Debug, Clone, PartialEq)]
pub struct MissionState {
    pub mode: MissionMode,
    pub active_goal: Option<Goal>,
    pub active_goal_accepted_ns: Option<u64>,
    pub failure: Option<MissionFailure>,
    pub exploration_active: bool,
}

impl MissionState {
    pub fn idle() -> Self {
        Self {
            mode: MissionMode::Idle,
            active_goal: None,
            active_goal_accepted_ns: None,
            failure: None,
            exploration_active: false,
        }
    }

    /// Applies one explicit command under the latest localization mode.
    ///
    /// MVP limitations: `Explore` is open-ended, `DeadReckoning` continuation
    /// budgets are not modeled, and directed `NavigateTo` goals always use
    /// `GoalSource::Operator`.
    pub fn apply(
        &mut self,
        command: &MissionCommand,
        localize_mode: LocalizationMode,
        now_ns: u64,
    ) -> GoalPublish {
        match command {
            MissionCommand::NavigateTo {
                goal,
                tolerance,
                max_duration_ns,
            } => {
                self.exploration_active = false;
                if localize_mode == LocalizationMode::Tracking {
                    let goal = Goal {
                        pose: goal.clone(),
                        tolerance: *tolerance,
                        max_duration_ns: *max_duration_ns,
                        source: GoalSource::Operator,
                    };
                    self.mode = MissionMode::Navigating;
                    self.active_goal = Some(goal.clone());
                    self.active_goal_accepted_ns = Some(now_ns);
                    self.failure = None;
                    GoalPublish::Publish(goal)
                } else {
                    self.refuse_command(format!(
                        "NavigateTo requires Tracking localization, got {localize_mode:?}"
                    ));
                    GoalPublish::None
                }
            }
            MissionCommand::Cancel => {
                self.mode = MissionMode::Idle;
                self.active_goal = None;
                self.active_goal_accepted_ns = None;
                self.failure = None;
                self.exploration_active = false;
                GoalPublish::None
            }
            MissionCommand::Pause => {
                self.mode = MissionMode::Paused;
                GoalPublish::None
            }
            MissionCommand::Resume => {
                if let Some(goal) = &self.active_goal {
                    if localize_mode == LocalizationMode::Tracking {
                        self.mode = MissionMode::Navigating;
                        self.active_goal_accepted_ns = Some(now_ns);
                        self.failure = None;
                        GoalPublish::Publish(goal.clone())
                    } else {
                        self.mode = MissionMode::Paused;
                        self.failure = Some(command_refused(format!(
                            "Resume requires Tracking localization, got {localize_mode:?}"
                        )));
                        GoalPublish::None
                    }
                } else {
                    self.mode = MissionMode::Idle;
                    self.active_goal_accepted_ns = None;
                    self.failure = Some(command_refused(
                        "Resume requires an active goal".to_string(),
                    ));
                    GoalPublish::None
                }
            }
            MissionCommand::ManualHandover => {
                self.mode = MissionMode::ManualHandover;
                self.active_goal = None;
                self.active_goal_accepted_ns = None;
                self.failure = None;
                GoalPublish::None
            }
            MissionCommand::Explore { .. } => {
                if localize_mode == LocalizationMode::Tracking {
                    self.mode = MissionMode::Exploring;
                    self.active_goal = None;
                    self.active_goal_accepted_ns = None;
                    self.failure = None;
                    self.exploration_active = true;
                } else {
                    self.refuse_command(format!(
                        "Explore requires Tracking localization, got {localize_mode:?}"
                    ));
                }
                GoalPublish::None
            }
        }
    }

    pub fn to_product(&self) -> State {
        State {
            mode: self.mode,
            active_goal: self.active_goal.clone(),
            failure: self.failure.clone(),
        }
    }

    pub fn promote_explore_goal(
        &mut self,
        candidates: &GoalCandidates,
        now_ns: u64,
    ) -> GoalPublish {
        if self.mode != MissionMode::Exploring || !self.exploration_active {
            return GoalPublish::None;
        }

        let Some(candidate) = candidates
            .candidates
            .iter()
            .max_by(|left, right| left.score.total_cmp(&right.score))
        else {
            return GoalPublish::None;
        };

        let goal = Goal {
            pose: candidate.goal.clone(),
            tolerance: candidate.tolerance,
            max_duration_ns: None,
            source: GoalSource::Explore,
        };
        self.mode = MissionMode::Navigating;
        self.active_goal = Some(goal.clone());
        self.active_goal_accepted_ns = Some(now_ns);
        self.failure = None;
        GoalPublish::Publish(goal)
    }

    pub fn complete_active_goal_if_reached(&mut self, pose: Option<&PoseEstimate>) {
        if self.mode == MissionMode::Navigating
            && let (Some(goal), Some(pose)) = (&self.active_goal, pose)
            && reached_goal(pose, goal)
        {
            let resume_exploration = goal.source == GoalSource::Explore && self.exploration_active;
            self.mode = if resume_exploration {
                MissionMode::Exploring
            } else {
                MissionMode::Idle
            };
            self.active_goal = None;
            self.active_goal_accepted_ns = None;
        }
    }

    pub fn fail_active_goal_if_budget_exceeded(&mut self, now_ns: u64) {
        if self.mode == MissionMode::Navigating
            && let Some(goal) = &self.active_goal
            && let Some(budget_ns) = goal.max_duration_ns
            && let Some(accepted_ns) = self.active_goal_accepted_ns
        {
            let elapsed_ns = now_ns.saturating_sub(accepted_ns);
            if elapsed_ns > budget_ns {
                self.mode = MissionMode::Failed;
                self.failure = Some(MissionFailure {
                    code: "navigate_budget_exceeded".into(),
                    detail: Some(format!(
                        "NavigateTo exceeded execution budget: elapsed {elapsed_ns} ns > budget {budget_ns} ns"
                    )),
                });
                self.active_goal = None;
                self.active_goal_accepted_ns = None;
            }
        }
    }

    fn refuse_command(&mut self, detail: String) {
        self.mode = MissionMode::Idle;
        self.active_goal = None;
        self.active_goal_accepted_ns = None;
        self.failure = Some(command_refused(detail));
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum GoalPublish {
    None,
    Publish(Goal),
}

fn command_refused(detail: String) -> MissionFailure {
    MissionFailure {
        code: "command_refused".into(),
        detail: Some(detail),
    }
}

fn reached_goal(pose: &PoseEstimate, goal: &Goal) -> bool {
    match &goal.pose {
        GoalPose::Pose2 { xy_m, yaw_rad, .. } => {
            let dx = pose.translation_m[0] - xy_m[0];
            let dy = pose.translation_m[1] - xy_m[1];
            let position_reached = (dx * dx + dy * dy).sqrt() <= goal.tolerance.pos_m;
            let heading_reached = goal.tolerance.yaw_rad.is_none_or(|tolerance_rad| {
                let pose_yaw_rad = yaw_from_xyzw(pose.rotation_xyzw);
                normalize_angle_rad(pose_yaw_rad - yaw_rad).abs() <= tolerance_rad
            });
            position_reached && heading_reached
        }
        _ => false,
    }
}

fn yaw_from_xyzw(rotation_xyzw: [f64; 4]) -> f64 {
    let [x, y, z, w] = rotation_xyzw;
    (2.0 * (w * z + x * y)).atan2(1.0 - 2.0 * (y * y + z * z))
}

fn normalize_angle_rad(angle_rad: f64) -> f64 {
    (angle_rad + PI).rem_euclid(2.0 * PI) - PI
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use phoxal::api::v1::explore::{GoalCandidate, GoalCandidates};
    use phoxal::api::v1::frame::FrameId;
    use phoxal::api::v1::localize::LocalizationMode;
    use phoxal::api::v1::map::MapRevisionId;
    use phoxal::api::v1::mission::{
        ExplorationCompletion, ExplorationCompletionMode, GoalTolerance,
    };

    use super::*;

    const ACCEPTED_NS: u64 = 1_000;
    const RESUMED_NS: u64 = 2_000;

    #[test]
    fn navigate_to_accepted_in_tracking_publishes_goal() {
        let command = navigate_to_command();
        let mut state = MissionState::idle();

        let publish = state.apply(&command, LocalizationMode::Tracking, ACCEPTED_NS);

        let expected = goal();
        assert_eq!(publish, GoalPublish::Publish(expected.clone()));
        assert_eq!(state.mode, MissionMode::Navigating);
        assert_eq!(state.active_goal, Some(expected));
        assert_eq!(state.active_goal_accepted_ns, Some(ACCEPTED_NS));
        assert_eq!(state.failure, None);
        assert!(!state.exploration_active);
    }

    #[test]
    fn navigate_to_refused_in_dead_reckoning() {
        assert_navigate_to_refused(LocalizationMode::DeadReckoning);
    }

    #[test]
    fn navigate_to_refused_in_initializing() {
        assert_navigate_to_refused(LocalizationMode::Initializing);
    }

    #[test]
    fn navigate_to_refused_in_lost() {
        assert_navigate_to_refused(LocalizationMode::Lost);
    }

    #[test]
    fn navigate_to_refused_in_relocalizing() {
        assert_navigate_to_refused(LocalizationMode::Relocalizing);
    }

    #[test]
    fn cancel_clears_goal() {
        let mut state = navigating_state();

        let publish = state.apply(
            &MissionCommand::Cancel,
            LocalizationMode::Tracking,
            ACCEPTED_NS,
        );

        assert_eq!(publish, GoalPublish::None);
        assert_eq!(state.mode, MissionMode::Idle);
        assert_eq!(state.active_goal, None);
        assert_eq!(state.active_goal_accepted_ns, None);
        assert_eq!(state.failure, None);
        assert!(!state.exploration_active);
    }

    #[test]
    fn pause_keeps_goal() {
        let expected = goal();
        let mut state = navigating_state();

        let publish = state.apply(
            &MissionCommand::Pause,
            LocalizationMode::Tracking,
            ACCEPTED_NS,
        );

        assert_eq!(publish, GoalPublish::None);
        assert_eq!(state.mode, MissionMode::Paused);
        assert_eq!(state.active_goal, Some(expected));
        assert_eq!(state.active_goal_accepted_ns, Some(ACCEPTED_NS));
    }

    #[test]
    fn resume_republishes_goal_in_tracking() {
        let expected = goal();
        let mut state = paused_state();

        let publish = state.apply(
            &MissionCommand::Resume,
            LocalizationMode::Tracking,
            RESUMED_NS,
        );

        assert_eq!(publish, GoalPublish::Publish(expected.clone()));
        assert_eq!(state.mode, MissionMode::Navigating);
        assert_eq!(state.active_goal, Some(expected));
        assert_eq!(state.active_goal_accepted_ns, Some(RESUMED_NS));
        assert_eq!(state.failure, None);
    }

    #[test]
    fn resume_refused_without_tracking() {
        let mut state = paused_state();

        let publish = state.apply(
            &MissionCommand::Resume,
            LocalizationMode::DeadReckoning,
            RESUMED_NS,
        );

        assert_eq!(publish, GoalPublish::None);
        assert_ne!(state.mode, MissionMode::Navigating);
        assert!(state.failure.is_some());
    }

    #[test]
    fn manual_handover_clears_goal() {
        let mut state = navigating_state();

        let publish = state.apply(
            &MissionCommand::ManualHandover,
            LocalizationMode::Tracking,
            ACCEPTED_NS,
        );

        assert_eq!(publish, GoalPublish::None);
        assert_eq!(state.mode, MissionMode::ManualHandover);
        assert_eq!(state.active_goal, None);
        assert_eq!(state.active_goal_accepted_ns, None);
        assert_eq!(state.failure, None);
    }

    #[test]
    fn explore_in_tracking_starts_exploration_session() {
        let mut state = MissionState::idle();

        let publish = state.apply(&explore_command(), LocalizationMode::Tracking, ACCEPTED_NS);

        assert_eq!(publish, GoalPublish::None);
        assert_eq!(state.mode, MissionMode::Exploring);
        assert_eq!(state.active_goal, None);
        assert_eq!(state.active_goal_accepted_ns, None);
        assert_eq!(state.failure, None);
        assert!(state.exploration_active);
    }

    #[test]
    fn explore_is_refused_without_tracking() {
        let mut state = MissionState::idle();

        let publish = state.apply(&explore_command(), LocalizationMode::Lost, ACCEPTED_NS);

        assert_eq!(publish, GoalPublish::None);
        assert_eq!(state.mode, MissionMode::Idle);
        assert_eq!(state.active_goal, None);
        assert_eq!(state.active_goal_accepted_ns, None);
        assert!(state.failure.is_some());
        assert!(!state.exploration_active);
    }

    #[test]
    fn promote_explore_goal_selects_top_scored_candidate() {
        let mut state = MissionState::idle();
        let publish = state.apply(&explore_command(), LocalizationMode::Tracking, ACCEPTED_NS);
        assert_eq!(publish, GoalPublish::None);

        let promoted = state.promote_explore_goal(&goal_candidates(), RESUMED_NS);

        let expected = explore_goal([2.0, 0.0], 0.7);
        assert_eq!(promoted, GoalPublish::Publish(expected.clone()));
        assert_eq!(state.mode, MissionMode::Navigating);
        assert_eq!(state.active_goal, Some(expected));
        assert_eq!(state.active_goal_accepted_ns, Some(RESUMED_NS));
        assert!(state.exploration_active);
    }

    #[test]
    fn reached_explore_goal_returns_to_exploring() {
        let mut state = MissionState {
            mode: MissionMode::Navigating,
            active_goal: Some(explore_goal([2.0, 0.0], 0.7)),
            active_goal_accepted_ns: Some(ACCEPTED_NS),
            failure: None,
            exploration_active: true,
        };

        state.complete_active_goal_if_reached(Some(&pose_estimate_with_yaw([2.0, 0.0, 0.0], 0.0)));

        assert_eq!(state.mode, MissionMode::Exploring);
        assert_eq!(state.active_goal, None);
        assert_eq!(state.active_goal_accepted_ns, None);
        assert_eq!(state.failure, None);
        assert!(state.exploration_active);
    }

    #[test]
    fn reached_goal_with_position_in_and_no_yaw_tolerance() {
        let pose = pose_estimate_with_yaw([1.1, 0.0, 0.0], 1.5);
        let goal = goal_with_tolerance([1.0, 0.0], 0.0, 0.2, None);

        assert!(reached_goal(&pose, &goal));
    }

    #[test]
    fn reached_goal_with_position_in_and_yaw_in() {
        let pose = pose_estimate_with_yaw([1.1, 0.0, 0.0], 0.55);
        let goal = goal_with_tolerance([1.0, 0.0], 0.5, 0.2, Some(0.1));

        assert!(reached_goal(&pose, &goal));
    }

    #[test]
    fn reached_goal_with_position_in_and_yaw_out_is_false() {
        let pose = pose_estimate_with_yaw([1.1, 0.0, 0.0], 0.8);
        let goal = goal_with_tolerance([1.0, 0.0], 0.5, 0.2, Some(0.1));

        assert!(!reached_goal(&pose, &goal));
    }

    #[test]
    fn reached_goal_with_position_out_and_yaw_in_is_false() {
        let pose = pose_estimate_with_yaw([1.3, 0.0, 0.0], 0.05);
        let goal = goal_with_tolerance([1.0, 0.0], 0.0, 0.2, Some(0.1));

        assert!(!reached_goal(&pose, &goal));
    }

    #[test]
    fn reached_goal_normalizes_yaw_wraparound() {
        let pose = pose_estimate_with_yaw([1.0, 0.0, 0.0], -PI + 0.01);
        let goal = goal_with_tolerance([1.0, 0.0], PI - 0.01, 0.2, Some(0.05));

        assert!(reached_goal(&pose, &goal));
    }

    #[test]
    fn reached_goal_is_false_for_non_pose2_goal() {
        let pose = pose_estimate_with_yaw([1.0, 0.0, 0.0], 0.0);
        let goal = Goal {
            pose: GoalPose::Pose3 {
                frame_id: "map".into(),
                map_revision: None,
                translation_m: [1.0, 0.0, 0.0],
                rotation_wxyz: [1.0, 0.0, 0.0, 0.0],
            },
            tolerance: goal_tolerance(),
            max_duration_ns: None,
            source: GoalSource::Operator,
        };

        assert!(!reached_goal(&pose, &goal));
    }

    #[test]
    fn navigate_budget_fails_only_after_budget_is_exceeded() {
        const BUDGET_NS: u64 = 50;
        let mut state = MissionState::idle();

        let publish = state.apply(
            &navigate_to_command_with_budget(Some(BUDGET_NS)),
            LocalizationMode::Tracking,
            ACCEPTED_NS,
        );
        assert!(matches!(publish, GoalPublish::Publish(_)));

        state.fail_active_goal_if_budget_exceeded(ACCEPTED_NS);
        state.fail_active_goal_if_budget_exceeded(ACCEPTED_NS + BUDGET_NS);
        assert_eq!(state.mode, MissionMode::Navigating);
        assert!(state.failure.is_none());
        assert!(state.active_goal.is_some());

        state.fail_active_goal_if_budget_exceeded(ACCEPTED_NS + BUDGET_NS + 1);

        assert_eq!(state.mode, MissionMode::Failed);
        assert_eq!(
            state.failure.as_ref().map(|failure| failure.code.as_str()),
            Some("navigate_budget_exceeded")
        );
        assert_eq!(state.active_goal, None);
        assert_eq!(state.active_goal_accepted_ns, None);
    }

    #[test]
    fn navigate_goal_without_budget_never_fails_on_budget() {
        let mut state = MissionState::idle();

        let publish = state.apply(
            &navigate_to_command_with_budget(None),
            LocalizationMode::Tracking,
            ACCEPTED_NS,
        );
        assert!(matches!(publish, GoalPublish::Publish(_)));

        state.fail_active_goal_if_budget_exceeded(u64::MAX);

        assert_eq!(state.mode, MissionMode::Navigating);
        assert!(state.failure.is_none());
        assert!(state.active_goal.is_some());
        assert_eq!(state.active_goal_accepted_ns, Some(ACCEPTED_NS));
    }

    fn assert_navigate_to_refused(mode: LocalizationMode) {
        let mut state = MissionState::idle();

        let publish = state.apply(&navigate_to_command(), mode, ACCEPTED_NS);

        assert_eq!(publish, GoalPublish::None);
        assert_eq!(state.mode, MissionMode::Idle);
        assert_eq!(state.active_goal, None);
        assert_eq!(state.active_goal_accepted_ns, None);
        assert!(state.failure.is_some());
    }

    fn navigating_state() -> MissionState {
        MissionState {
            mode: MissionMode::Navigating,
            active_goal: Some(goal()),
            active_goal_accepted_ns: Some(ACCEPTED_NS),
            failure: None,
            exploration_active: false,
        }
    }

    fn paused_state() -> MissionState {
        MissionState {
            mode: MissionMode::Paused,
            active_goal: Some(goal()),
            active_goal_accepted_ns: Some(ACCEPTED_NS),
            failure: None,
            exploration_active: false,
        }
    }

    fn navigate_to_command() -> MissionCommand {
        navigate_to_command_with_budget(None)
    }

    fn navigate_to_command_with_budget(max_duration_ns: Option<u64>) -> MissionCommand {
        MissionCommand::NavigateTo {
            goal: goal_pose(),
            tolerance: goal_tolerance(),
            max_duration_ns,
        }
    }

    fn explore_command() -> MissionCommand {
        MissionCommand::Explore {
            area: None,
            completion: ExplorationCompletion {
                mode: ExplorationCompletionMode::OpenEnded,
                coverage_goal: None,
            },
            max_duration_ns: None,
        }
    }

    fn goal() -> Goal {
        Goal {
            pose: goal_pose(),
            tolerance: goal_tolerance(),
            max_duration_ns: None,
            source: GoalSource::Operator,
        }
    }

    fn goal_with_tolerance(
        xy_m: [f64; 2],
        yaw_rad: f64,
        pos_m: f64,
        yaw_tolerance_rad: Option<f64>,
    ) -> Goal {
        Goal {
            pose: GoalPose::Pose2 {
                frame_id: "map".into(),
                map_revision: None,
                xy_m,
                yaw_rad,
            },
            tolerance: GoalTolerance {
                pos_m,
                yaw_rad: yaw_tolerance_rad,
            },
            max_duration_ns: None,
            source: GoalSource::Operator,
        }
    }

    fn explore_goal(xy_m: [f64; 2], pos_tolerance_m: f64) -> Goal {
        Goal {
            pose: GoalPose::Pose2 {
                frame_id: "map".into(),
                map_revision: None,
                xy_m,
                yaw_rad: 0.0,
            },
            tolerance: GoalTolerance {
                pos_m: pos_tolerance_m,
                yaw_rad: Some(0.14),
            },
            max_duration_ns: None,
            source: GoalSource::Explore,
        }
    }

    fn goal_candidates() -> GoalCandidates {
        GoalCandidates {
            map_revision: MapRevisionId {
                epoch: 1,
                sequence: 2,
            },
            built_from_localize_revision: phoxal::api::v1::localize::LocalizationRevisionId {
                epoch: 1,
                sequence: 3,
            },
            candidates: vec![
                GoalCandidate {
                    id: "lower-score".into(),
                    goal: explore_goal([1.0, 0.0], 0.4).pose,
                    tolerance: GoalTolerance {
                        pos_m: 0.4,
                        yaw_rad: Some(0.14),
                    },
                    score: 0.2,
                },
                GoalCandidate {
                    id: "top-score".into(),
                    goal: explore_goal([2.0, 0.0], 0.7).pose,
                    tolerance: GoalTolerance {
                        pos_m: 0.7,
                        yaw_rad: Some(0.14),
                    },
                    score: 0.9,
                },
            ],
        }
    }

    fn goal_pose() -> GoalPose {
        GoalPose::Pose2 {
            frame_id: "map".into(),
            map_revision: None,
            xy_m: [1.0, 0.0],
            yaw_rad: 0.0,
        }
    }

    fn goal_tolerance() -> GoalTolerance {
        GoalTolerance {
            pos_m: 0.2,
            yaw_rad: Some(0.14),
        }
    }

    fn pose_estimate_with_yaw(translation_m: [f64; 3], yaw_rad: f64) -> PoseEstimate {
        PoseEstimate {
            frame_id: FrameId::new("map"),
            child_frame_id: FrameId::new("base_footprint"),
            translation_m,
            rotation_xyzw: [0.0, 0.0, (yaw_rad / 2.0).sin(), (yaw_rad / 2.0).cos()],
        }
    }
}
