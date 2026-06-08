use phoxal::api::explore::v1::GoalCandidates;
use phoxal::api::localize::v1::{LocalizationMode, PoseEstimate};
use phoxal::api::mission::v1::{
    Goal, GoalPose, GoalSource, MissionCommand, MissionFailure, MissionMode, State,
};

#[derive(Debug, Clone, PartialEq)]
pub struct MissionState {
    pub mode: MissionMode,
    pub active_goal: Option<Goal>,
    pub failure: Option<MissionFailure>,
    pub exploration_active: bool,
}

impl MissionState {
    pub fn idle() -> Self {
        Self {
            mode: MissionMode::Idle,
            active_goal: None,
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
    ) -> GoalPublish {
        match command {
            MissionCommand::NavigateTo { goal, tolerance } => {
                self.exploration_active = false;
                if localize_mode == LocalizationMode::Tracking {
                    let goal = Goal {
                        pose: goal.clone(),
                        tolerance: *tolerance,
                        source: GoalSource::Operator,
                    };
                    self.mode = MissionMode::Navigating;
                    self.active_goal = Some(goal.clone());
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
                    self.failure = Some(command_refused(
                        "Resume requires an active goal".to_string(),
                    ));
                    GoalPublish::None
                }
            }
            MissionCommand::ManualHandover => {
                self.mode = MissionMode::ManualHandover;
                self.active_goal = None;
                self.failure = None;
                GoalPublish::None
            }
            MissionCommand::Explore { .. } => {
                if localize_mode == LocalizationMode::Tracking {
                    self.mode = MissionMode::Exploring;
                    self.active_goal = None;
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

    pub fn promote_explore_goal(&mut self, candidates: &GoalCandidates) -> GoalPublish {
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
            source: GoalSource::Explore,
        };
        self.mode = MissionMode::Navigating;
        self.active_goal = Some(goal.clone());
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
        }
    }

    fn refuse_command(&mut self, detail: String) {
        self.mode = MissionMode::Idle;
        self.active_goal = None;
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
        GoalPose::Pose2 { xy_m, .. } => {
            let dx = pose.translation_m[0] - xy_m[0];
            let dy = pose.translation_m[1] - xy_m[1];
            (dx * dx + dy * dy).sqrt() <= goal.tolerance.pos_m
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use phoxal::api::explore::v1::{GoalCandidate, GoalCandidates};
    use phoxal::api::frame::v1::FrameId;
    use phoxal::api::localize::v1::LocalizationMode;
    use phoxal::api::map::v1::MapRevisionId;
    use phoxal::api::mission::v1::{
        ExplorationCompletion, ExplorationCompletionMode, GoalTolerance,
    };

    use super::*;

    #[test]
    fn navigate_to_accepted_in_tracking_publishes_goal() {
        let command = navigate_to_command();
        let mut state = MissionState::idle();

        let publish = state.apply(&command, LocalizationMode::Tracking);

        let expected = goal();
        assert_eq!(publish, GoalPublish::Publish(expected.clone()));
        assert_eq!(state.mode, MissionMode::Navigating);
        assert_eq!(state.active_goal, Some(expected));
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

        let publish = state.apply(&MissionCommand::Cancel, LocalizationMode::Tracking);

        assert_eq!(publish, GoalPublish::None);
        assert_eq!(state.mode, MissionMode::Idle);
        assert_eq!(state.active_goal, None);
        assert_eq!(state.failure, None);
        assert!(!state.exploration_active);
    }

    #[test]
    fn pause_keeps_goal() {
        let expected = goal();
        let mut state = navigating_state();

        let publish = state.apply(&MissionCommand::Pause, LocalizationMode::Tracking);

        assert_eq!(publish, GoalPublish::None);
        assert_eq!(state.mode, MissionMode::Paused);
        assert_eq!(state.active_goal, Some(expected));
    }

    #[test]
    fn resume_republishes_goal_in_tracking() {
        let expected = goal();
        let mut state = paused_state();

        let publish = state.apply(&MissionCommand::Resume, LocalizationMode::Tracking);

        assert_eq!(publish, GoalPublish::Publish(expected.clone()));
        assert_eq!(state.mode, MissionMode::Navigating);
        assert_eq!(state.active_goal, Some(expected));
        assert_eq!(state.failure, None);
    }

    #[test]
    fn resume_refused_without_tracking() {
        let mut state = paused_state();

        let publish = state.apply(&MissionCommand::Resume, LocalizationMode::DeadReckoning);

        assert_eq!(publish, GoalPublish::None);
        assert_ne!(state.mode, MissionMode::Navigating);
        assert!(state.failure.is_some());
    }

    #[test]
    fn manual_handover_clears_goal() {
        let mut state = navigating_state();

        let publish = state.apply(&MissionCommand::ManualHandover, LocalizationMode::Tracking);

        assert_eq!(publish, GoalPublish::None);
        assert_eq!(state.mode, MissionMode::ManualHandover);
        assert_eq!(state.active_goal, None);
        assert_eq!(state.failure, None);
    }

    #[test]
    fn explore_in_tracking_starts_exploration_session() {
        let mut state = MissionState::idle();

        let publish = state.apply(&explore_command(), LocalizationMode::Tracking);

        assert_eq!(publish, GoalPublish::None);
        assert_eq!(state.mode, MissionMode::Exploring);
        assert_eq!(state.active_goal, None);
        assert_eq!(state.failure, None);
        assert!(state.exploration_active);
    }

    #[test]
    fn explore_is_refused_without_tracking() {
        let mut state = MissionState::idle();

        let publish = state.apply(&explore_command(), LocalizationMode::Lost);

        assert_eq!(publish, GoalPublish::None);
        assert_eq!(state.mode, MissionMode::Idle);
        assert_eq!(state.active_goal, None);
        assert!(state.failure.is_some());
        assert!(!state.exploration_active);
    }

    #[test]
    fn promote_explore_goal_selects_top_scored_candidate() {
        let mut state = MissionState::idle();
        let publish = state.apply(&explore_command(), LocalizationMode::Tracking);
        assert_eq!(publish, GoalPublish::None);

        let promoted = state.promote_explore_goal(&goal_candidates());

        let expected = explore_goal([2.0, 0.0], 0.7);
        assert_eq!(promoted, GoalPublish::Publish(expected.clone()));
        assert_eq!(state.mode, MissionMode::Navigating);
        assert_eq!(state.active_goal, Some(expected));
        assert!(state.exploration_active);
    }

    #[test]
    fn reached_explore_goal_returns_to_exploring() {
        let mut state = MissionState {
            mode: MissionMode::Navigating,
            active_goal: Some(explore_goal([2.0, 0.0], 0.7)),
            failure: None,
            exploration_active: true,
        };

        state.complete_active_goal_if_reached(Some(&pose_estimate([2.0, 0.0, 0.0])));

        assert_eq!(state.mode, MissionMode::Exploring);
        assert_eq!(state.active_goal, None);
        assert_eq!(state.failure, None);
        assert!(state.exploration_active);
    }

    #[test]
    fn reached_goal_is_true_within_pose2_position_tolerance() {
        let pose = pose_estimate([1.1, 0.0, 0.0]);

        assert!(reached_goal(&pose, &goal()));
    }

    #[test]
    fn reached_goal_is_false_outside_pose2_position_tolerance() {
        let pose = pose_estimate([1.3, 0.0, 0.0]);

        assert!(!reached_goal(&pose, &goal()));
    }

    #[test]
    fn reached_goal_is_false_for_non_pose2_goal() {
        let pose = pose_estimate([1.0, 0.0, 0.0]);
        let goal = Goal {
            pose: GoalPose::Pose3 {
                frame_id: "map".into(),
                map_revision: None,
                translation_m: [1.0, 0.0, 0.0],
                rotation_wxyz: [1.0, 0.0, 0.0, 0.0],
            },
            tolerance: goal_tolerance(),
            source: GoalSource::Operator,
        };

        assert!(!reached_goal(&pose, &goal));
    }

    fn assert_navigate_to_refused(mode: LocalizationMode) {
        let mut state = MissionState::idle();

        let publish = state.apply(&navigate_to_command(), mode);

        assert_eq!(publish, GoalPublish::None);
        assert_eq!(state.mode, MissionMode::Idle);
        assert_eq!(state.active_goal, None);
        assert!(state.failure.is_some());
    }

    fn navigating_state() -> MissionState {
        MissionState {
            mode: MissionMode::Navigating,
            active_goal: Some(goal()),
            failure: None,
            exploration_active: false,
        }
    }

    fn paused_state() -> MissionState {
        MissionState {
            mode: MissionMode::Paused,
            active_goal: Some(goal()),
            failure: None,
            exploration_active: false,
        }
    }

    fn navigate_to_command() -> MissionCommand {
        MissionCommand::NavigateTo {
            goal: goal_pose(),
            tolerance: goal_tolerance(),
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
                time_ns: None,
            },
            source: GoalSource::Explore,
        }
    }

    fn goal_candidates() -> GoalCandidates {
        GoalCandidates {
            map_revision: MapRevisionId {
                epoch: 1,
                sequence: 2,
            },
            built_from_localize_revision: phoxal::api::localize::v1::LocalizationRevisionId {
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
                        time_ns: None,
                    },
                    score: 0.2,
                },
                GoalCandidate {
                    id: "top-score".into(),
                    goal: explore_goal([2.0, 0.0], 0.7).pose,
                    tolerance: GoalTolerance {
                        pos_m: 0.7,
                        yaw_rad: Some(0.14),
                        time_ns: None,
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
            time_ns: None,
        }
    }

    fn pose_estimate(translation_m: [f64; 3]) -> PoseEstimate {
        PoseEstimate {
            frame_id: FrameId::new("map"),
            child_frame_id: FrameId::new("base_footprint"),
            translation_m,
            rotation_xyzw: [0.0, 0.0, 0.0, 1.0],
        }
    }
}
