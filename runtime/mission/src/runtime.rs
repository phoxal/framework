use std::time::Duration;

use crate::core::{GoalPublish, MissionState};
use anyhow::Result;
use phoxal::api::explore::v1::{GoalCandidates, goal_candidates};
use phoxal::api::localize::v1::{LocalizationMode, LocalizationState, PoseEstimate};
use phoxal::api::mission::v1::{
    Goal, GoalSource, MissionCommand, MissionMode, State, command, goal, state,
};
use phoxal::bus::pubsub::Stamped;
use phoxal::bus::zenoh::TypedSchema;
use phoxal::runtime::clock::Step;
use phoxal::runtime::decision_log::DecisionLog;
use phoxal::runtime::runtime::{Io, Publisher, Runtime, RuntimeInputs};
use phoxal::runtime::{EmptyArgs, RobotRuntimeArgs};

const CLOCK_PERIOD: Duration = Duration::from_millis(100);

#[derive(Clone, Default)]
pub struct Config {
    clock_period: Option<Duration>,
}

impl Config {
    pub fn clock_period(&self) -> Duration {
        self.clock_period.unwrap_or(CLOCK_PERIOD)
    }
}

pub enum Input {
    Command(Stamped<MissionCommand>),
    LocalizationState(Stamped<LocalizationState>),
    GoalCandidates(Stamped<GoalCandidates>),
}

pub struct MissionRuntime {
    state: MissionState,
    latest_localize_mode: LocalizationMode,
    latest_localize_pose: Option<PoseEstimate>,
    latest_explore_candidates: Option<GoalCandidates>,
    decision_log: DecisionLog<MissionLogKey>,
    goal_publisher: Publisher<Stamped<Goal>>,
    state_publisher: Publisher<Stamped<State>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MissionLogKey {
    mode: MissionMode,
    active_goal_source: Option<MissionGoalSource>,
    has_failure: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MissionGoalSource {
    Operator,
    Explore,
    Recovery,
}

#[async_trait::async_trait]
impl Runtime for MissionRuntime {
    const RUNTIME_ID: &'static str = "mission";

    type Args = EmptyArgs;
    type Config = Config;
    type Input = Input;

    fn config(_args: &Self::Args, _common: &RobotRuntimeArgs) -> Result<Self::Config> {
        Ok(Config::default())
    }

    fn clock_period(config: &Self::Config) -> Duration {
        config.clock_period()
    }

    async fn new(io: &mut Io<Self::Input>, _config: Self::Config) -> Result<Self> {
        io.subscribe::<Stamped<MissionCommand>, _>(&command::path(), Input::Command)
            .await?;
        io.subscribe::<Stamped<LocalizationState>, _>(
            &phoxal::api::localize::v1::state::path(),
            Input::LocalizationState,
        )
        .await?;
        io.subscribe::<Stamped<GoalCandidates>, _>(&goal_candidates::path(), Input::GoalCandidates)
            .await?;

        let goal_publisher = io.publisher::<Stamped<Goal>>(&goal::path()).await?;
        let state_publisher = io.publisher::<Stamped<State>>(&state::path()).await?;

        Ok(Self {
            state: MissionState::idle(),
            latest_localize_mode: LocalizationMode::Initializing,
            latest_localize_pose: None,
            latest_explore_candidates: None,
            decision_log: DecisionLog::new(
                Self::RUNTIME_ID,
                state::path(),
                <State as TypedSchema>::SCHEMA_NAME,
                <State as TypedSchema>::SCHEMA_VERSION,
            ),
            goal_publisher,
            state_publisher,
        })
    }

    async fn step(&mut self, step: Step, inputs: RuntimeInputs<Self::Input>) -> Result<()> {
        let timestamp_ns = step.tick.time_ns();
        let mut goal_published_this_step = false;

        for input in inputs {
            match input {
                Input::Command(command) => {
                    if let GoalPublish::Publish(goal) =
                        self.state
                            .apply(&command.data, self.latest_localize_mode, timestamp_ns)
                    {
                        self.goal_publisher
                            .put(&Stamped::new(timestamp_ns, goal))
                            .await?;
                        goal_published_this_step = true;
                    }
                }
                Input::LocalizationState(localize) => {
                    self.latest_localize_mode = localize.data.mode;
                    self.latest_localize_pose = localize.data.pose.clone();
                }
                Input::GoalCandidates(candidates) => {
                    self.latest_explore_candidates = Some(candidates.data);
                }
            }
        }

        self.state
            .complete_active_goal_if_reached(self.latest_localize_pose.as_ref());
        self.state.fail_active_goal_if_budget_exceeded(timestamp_ns);

        if let Some(candidates) = self.latest_explore_candidates.as_ref()
            && let GoalPublish::Publish(goal) =
                self.state.promote_explore_goal(candidates, timestamp_ns)
        {
            self.latest_explore_candidates = None;
            self.goal_publisher
                .put(&Stamped::new(timestamp_ns, goal))
                .await?;
            goal_published_this_step = true;
        }

        // Re-emit the active goal every step while navigating. `plan` derives a
        // fresh path from the latest pose, so this keeps the MVP receding
        // horizon behavior alive without mission reading planner feedback.
        if !goal_published_this_step
            && self.state.mode == MissionMode::Navigating
            && let Some(goal) = &self.state.active_goal
        {
            self.goal_publisher
                .put(&Stamped::new(timestamp_ns, goal.clone()))
                .await?;
        }

        let state = self.state.to_product();
        self.decision_log
            .observe(timestamp_ns, mission_log_key(&state));
        self.state_publisher
            .put(&Stamped::new(timestamp_ns, state))
            .await?;

        Ok(())
    }
}

fn mission_log_key(state: &State) -> MissionLogKey {
    MissionLogKey {
        mode: state.mode,
        active_goal_source: state
            .active_goal
            .as_ref()
            .map(|goal| mission_goal_source(&goal.source)),
        has_failure: state.failure.is_some(),
    }
}

const fn mission_goal_source(source: &GoalSource) -> MissionGoalSource {
    match source {
        GoalSource::Operator => MissionGoalSource::Operator,
        GoalSource::Explore => MissionGoalSource::Explore,
        GoalSource::Recovery => MissionGoalSource::Recovery,
    }
}

#[cfg(test)]
mod tests {
    use phoxal::api::explore::v1::{GoalCandidate, GoalCandidates};
    use phoxal::api::frame::v1::FrameId;
    use phoxal::api::localize::v1::{LocalizationSource, LocalizationStatus};
    use phoxal::api::map::v1::MapRevisionId;
    use phoxal::api::mission::v1::{
        ExplorationCompletion, ExplorationCompletionMode, GoalPose, GoalSource, GoalTolerance,
    };
    use phoxal::api::simulation::v1::clock::Clock;

    use super::*;

    #[tokio::test]
    async fn step_promotes_latest_top_scored_explore_candidate() -> Result<()> {
        let mut io = Io::<Input>::recording();
        let mut runtime = <MissionRuntime as Runtime>::new(&mut io, Config::default()).await?;

        runtime
            .step(
                step_at(100),
                RuntimeInputs::from(vec![
                    Input::LocalizationState(Stamped::new(90, tracking_state(None))),
                    Input::Command(Stamped::new(95, explore_command())),
                    Input::GoalCandidates(Stamped::new(96, goal_candidates())),
                ]),
            )
            .await?;

        let published_goals = io.recorded_puts::<Stamped<Goal>>(&goal::path());
        assert_eq!(
            published_goals,
            vec![Stamped::new(100, explore_goal([2.0, 0.0], 0.7))]
        );
        assert_eq!(runtime.state.mode, MissionMode::Navigating);
        assert_eq!(
            runtime.state.active_goal,
            Some(explore_goal([2.0, 0.0], 0.7))
        );
        assert!(runtime.state.exploration_active);
        assert_eq!(runtime.latest_explore_candidates, None);

        Ok(())
    }

    #[tokio::test]
    async fn step_returns_to_exploring_when_explore_goal_is_reached() -> Result<()> {
        let mut io = Io::<Input>::recording();
        let mut runtime = <MissionRuntime as Runtime>::new(&mut io, Config::default()).await?;
        runtime
            .step(
                step_at(100),
                RuntimeInputs::from(vec![
                    Input::LocalizationState(Stamped::new(90, tracking_state(None))),
                    Input::Command(Stamped::new(95, explore_command())),
                    Input::GoalCandidates(Stamped::new(96, goal_candidates())),
                ]),
            )
            .await?;

        runtime
            .step(
                step_at(200),
                RuntimeInputs::from(vec![Input::LocalizationState(Stamped::new(
                    190,
                    tracking_state(Some(pose_estimate([2.0, 0.0, 0.0]))),
                ))]),
            )
            .await?;

        assert_eq!(runtime.state.mode, MissionMode::Exploring);
        assert_eq!(runtime.state.active_goal, None);
        assert!(runtime.state.exploration_active);

        let published_states = io.recorded_puts::<Stamped<State>>(&state::path());
        assert_eq!(
            published_states.last().map(|stamped| stamped.data.mode),
            Some(MissionMode::Exploring)
        );

        Ok(())
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

    fn explore_goal(xy_m: [f64; 2], pos_tolerance_m: f64) -> Goal {
        Goal {
            pose: GoalPose::Pose2 {
                frame_id: "map".into(),
                map_revision: None,
                xy_m,
                yaw_rad: 0.0,
            },
            tolerance: goal_tolerance(pos_tolerance_m),
            max_duration_ns: None,
            source: GoalSource::Explore,
        }
    }

    fn goal_tolerance(pos_m: f64) -> GoalTolerance {
        GoalTolerance {
            pos_m,
            yaw_rad: Some(0.14),
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

    fn tracking_state(pose: Option<PoseEstimate>) -> LocalizationState {
        LocalizationState {
            mode: LocalizationMode::Tracking,
            source: LocalizationSource::SimulatorTruth,
            revision: None,
            pose,
            velocity: None,
            covariance: None,
            imu_bias: None,
            status: LocalizationStatus {
                healthy: true,
                reasons: Vec::new(),
            },
            valid_at_ns: Some(90),
        }
    }

    fn step_at(time_ns: u64) -> Step {
        Step::new(Clock::new(1, time_ns / 100, time_ns, 100))
    }
}
