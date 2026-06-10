use std::time::Duration;

use crate::core::{GoalPublish, MissionState};
use anyhow::Result;
use phoxal::api::explore::v1::GoalCandidates;
use phoxal::api::localize::v1::{LocalizationMode, LocalizationState, PoseEstimate};
use phoxal::api::mission::v1::{Goal, GoalSource, MissionCommand, MissionMode, State};
use phoxal::api::v1::topic;
use phoxal::bus::typed::Received;
use phoxal::runtime::clock::Step;
use phoxal::runtime::decision_log::DecisionLog;
use phoxal::runtime::{EmptyArgs, RobotRuntimeArgs};
use phoxal::runtime::{Io, Runtime, RuntimeInputs, TopicPublisher};

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
    Command(Received<MissionCommand>),
    LocalizationState(Received<LocalizationState>),
    GoalCandidates(Received<GoalCandidates>),
}

pub struct MissionRuntime {
    state: MissionState,
    latest_localize_mode: LocalizationMode,
    latest_localize_pose: Option<PoseEstimate>,
    latest_explore_candidates: Option<GoalCandidates>,
    decision_log: DecisionLog<MissionLogKey>,
    goal_publisher: TopicPublisher<Goal>,
    goal_record_publisher: TopicPublisher<Goal>,
    state_publisher: TopicPublisher<State>,
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
        io.subscribe_topic(topic::new().v1().mission().command(), Input::Command)
            .await?;
        io.subscribe_topic(
            topic::new().v1().localize().state(),
            Input::LocalizationState,
        )
        .await?;
        io.subscribe_topic(
            topic::new().v1().explore().goal_candidates(),
            Input::GoalCandidates,
        )
        .await?;

        let goal_publisher = io
            .publisher_topic(topic::new().v1().mission().goal())
            .await?;
        let goal_record_publisher = io
            .publisher_topic(topic::new().v1().mission().debug().goal_record())
            .await?;
        let state_topic = topic::new().v1().mission().state();
        let state_publisher = io.publisher_topic(state_topic.clone()).await?;

        Ok(Self {
            state: MissionState::idle(),
            latest_localize_mode: LocalizationMode::Initializing,
            latest_localize_pose: None,
            latest_explore_candidates: None,
            decision_log: DecisionLog::from_topic(Self::RUNTIME_ID, &state_topic),
            goal_publisher,
            goal_record_publisher,
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
                            .apply(&command.value, self.latest_localize_mode, timestamp_ns)
                    {
                        self.publish_goal(timestamp_ns, &goal).await?;
                        goal_published_this_step = true;
                    }
                }
                Input::LocalizationState(localize) => {
                    self.latest_localize_mode = localize.value.mode;
                    self.latest_localize_pose = localize.value.pose.clone();
                }
                Input::GoalCandidates(candidates) => {
                    self.latest_explore_candidates = Some(candidates.value);
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
            self.publish_goal(timestamp_ns, &goal).await?;
            goal_published_this_step = true;
        }

        // Re-emit the active goal every step while navigating. `plan` derives a
        // fresh path from the latest pose, so this keeps the MVP receding
        // horizon behavior alive without mission reading planner feedback.
        if !goal_published_this_step
            && self.state.mode == MissionMode::Navigating
            && let Some(goal) = &self.state.active_goal
        {
            self.publish_goal(timestamp_ns, goal).await?;
        }

        let state = self.state.to_product();
        self.decision_log
            .observe(timestamp_ns, mission_log_key(&state));
        self.state_publisher.put(timestamp_ns, &state).await?;

        Ok(())
    }
}

impl MissionRuntime {
    async fn publish_goal(&self, timestamp_ns: u64, goal: &Goal) -> Result<()> {
        self.goal_publisher.put(timestamp_ns, goal).await?;
        self.goal_record_publisher.put(timestamp_ns, goal).await
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
                    Input::LocalizationState(received(90, tracking_state(None))),
                    Input::Command(received(95, explore_command())),
                    Input::GoalCandidates(received(96, goal_candidates())),
                ]),
            )
            .await?;

        let expected_goals = vec![explore_goal([2.0, 0.0], 0.7)];
        let goal_topic = topic::new().v1().mission().goal();
        let published_goals = io.recorded_puts::<Goal>(goal_topic.key().as_ref());
        assert_eq!(published_goals, expected_goals);
        let goal_record_topic = topic::new().v1().mission().debug().goal_record();
        let recorded_goals = io.recorded_puts::<Goal>(goal_record_topic.key().as_ref());
        assert_eq!(recorded_goals, expected_goals);
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
                    Input::LocalizationState(received(90, tracking_state(None))),
                    Input::Command(received(95, explore_command())),
                    Input::GoalCandidates(received(96, goal_candidates())),
                ]),
            )
            .await?;

        runtime
            .step(
                step_at(200),
                RuntimeInputs::from(vec![Input::LocalizationState(received(
                    190,
                    tracking_state(Some(pose_estimate([2.0, 0.0, 0.0]))),
                ))]),
            )
            .await?;

        assert_eq!(runtime.state.mode, MissionMode::Exploring);
        assert_eq!(runtime.state.active_goal, None);
        assert!(runtime.state.exploration_active);

        let state_topic = topic::new().v1().mission().state();
        let published_states = io.recorded_puts::<State>(state_topic.key().as_ref());
        assert_eq!(
            published_states.last().map(|state| state.mode),
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

    fn received<T>(timestamp_ns: u64, value: T) -> Received<T> {
        Received {
            at_ns: Some(timestamp_ns),
            value,
        }
    }
}
