use std::time::Duration;

use anyhow::Result;
use phoxal::api::explore::v1::{ExploreStatus, Frontiers, GoalCandidates, State};
use phoxal::api::frame::v1::FrameId;
use phoxal::api::localize::v1::{LocalizationRevisionId, LocalizationState};
use phoxal::api::map::v1::{MapRevision, Traversability};
use phoxal::api::v1::topic;
use phoxal::bus::typed::Received;
use phoxal::runtime::clock::Step;
use phoxal::runtime::decision_log::DecisionLog;
use phoxal::runtime::{EmptyArgs, RobotRuntimeArgs};
use phoxal::runtime::{Io, Runtime, RuntimeInputs, TopicPublisher};

use crate::frontiers::detect_frontiers_in_frame;
use crate::scoring::{candidate_centroids, score_candidates};

const CLOCK_PERIOD: Duration = Duration::from_millis(500);
const PLANAR_FRAME_ID: &str = "map";

#[derive(Clone, Debug)]
pub struct Config {
    planar_frame_id: FrameId,
    clock_period: Duration,
}

impl Config {
    pub fn from_args(_args: &RobotRuntimeArgs) -> Result<Self> {
        Ok(Self {
            planar_frame_id: FrameId::new(PLANAR_FRAME_ID),
            clock_period: CLOCK_PERIOD,
        })
    }

    pub const fn clock_period(&self) -> Duration {
        self.clock_period
    }
}

pub enum Input {
    Traversability(Received<Traversability>),
    MapRevision(Received<MapRevision>),
    LocalizationState(Received<LocalizationState>),
}

pub struct ExploreRuntime {
    planar_frame_id: FrameId,
    latest_traversability: Option<Received<Traversability>>,
    latest_map_revision: Option<Received<MapRevision>>,
    latest_pose_xy_m: Option<[f64; 2]>,
    latest_localize_revision: Option<LocalizationRevisionId>,
    last_centroids: Vec<[f64; 2]>,
    decision_log: DecisionLog<ExploreLogKey>,
    frontiers_publisher: TopicPublisher<Frontiers>,
    goal_candidates_publisher: TopicPublisher<GoalCandidates>,
    state_publisher: TopicPublisher<State>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ExploreLogKey {
    status: ExploreStatus,
    reason: Option<ExploreReason>,
    frontier_count: Option<usize>,
    candidate_count: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExploreReason {
    WaitingForMapRevision,
    WaitingForLocalizationRevision,
    WaitingForLocalizationPose,
    Other,
}

#[async_trait::async_trait]
impl Runtime for ExploreRuntime {
    const RUNTIME_ID: &'static str = "explore";

    type Args = EmptyArgs;
    type Config = Config;
    type Input = Input;

    fn config(_args: &Self::Args, common: &RobotRuntimeArgs) -> Result<Self::Config> {
        Config::from_args(common)
    }

    fn clock_period(config: &Self::Config) -> Duration {
        config.clock_period()
    }

    async fn new(io: &mut Io<Self::Input>, config: Self::Config) -> Result<Self> {
        io.subscribe_topic(
            topic::new().v1().map().traversability(),
            Input::Traversability,
        )
        .await?;
        io.subscribe_topic(topic::new().v1().map().revision(), Input::MapRevision)
            .await?;
        io.subscribe_topic(
            topic::new().v1().localize().state(),
            Input::LocalizationState,
        )
        .await?;

        let state_topic = topic::new().v1().explore().state();

        Ok(Self {
            planar_frame_id: config.planar_frame_id,
            latest_traversability: None,
            latest_map_revision: None,
            latest_pose_xy_m: None,
            latest_localize_revision: None,
            last_centroids: Vec::new(),
            decision_log: DecisionLog::from_topic(Self::RUNTIME_ID, &state_topic),
            frontiers_publisher: io
                .publisher_topic(topic::new().v1().explore().frontiers())
                .await?,
            goal_candidates_publisher: io
                .publisher_topic(topic::new().v1().explore().goal_candidates())
                .await?,
            state_publisher: io.publisher_topic(state_topic).await?,
        })
    }

    async fn step(&mut self, step: Step, inputs: RuntimeInputs<Self::Input>) -> Result<()> {
        for input in inputs {
            match input {
                Input::Traversability(sample) => self.latest_traversability = Some(sample),
                Input::MapRevision(sample) => self.latest_map_revision = Some(sample),
                Input::LocalizationState(sample) => {
                    self.latest_localize_revision = sample.value.revision;
                    self.latest_pose_xy_m = sample
                        .value
                        .pose
                        .map(|pose| [pose.translation_m[0], pose.translation_m[1]]);
                }
            }
        }

        let timestamp_ns = step.tick.time_ns();
        let Some(traversability) = &self.latest_traversability else {
            self.publish_state(timestamp_ns, ExploreStatus::Idle, None, None, None)
                .await?;
            return Ok(());
        };
        let Some(map_revision) = self
            .latest_map_revision
            .as_ref()
            .map(|sample| sample.value.map_revision_id)
        else {
            self.publish_state(
                timestamp_ns,
                ExploreStatus::Evaluating,
                Some("waiting for map revision".to_string()),
                None,
                None,
            )
            .await?;
            return Ok(());
        };
        let Some(localize_revision) = self.latest_localize_revision else {
            self.publish_state(
                timestamp_ns,
                ExploreStatus::Evaluating,
                Some("waiting for localization revision".to_string()),
                None,
                None,
            )
            .await?;
            return Ok(());
        };
        let Some(robot_xy_m) = self.latest_pose_xy_m else {
            self.publish_state(
                timestamp_ns,
                ExploreStatus::Evaluating,
                Some("waiting for localization pose".to_string()),
                None,
                None,
            )
            .await?;
            return Ok(());
        };

        let frontiers =
            detect_frontiers_in_frame(&traversability.value.cells, &self.planar_frame_id.0);
        let candidates = score_candidates(
            &frontiers,
            &traversability.value.cells,
            robot_xy_m,
            map_revision,
            &self.last_centroids,
        );
        let frontier_count = frontiers.len();
        let candidate_count = candidates.len();
        self.last_centroids = candidate_centroids(&candidates);
        let status = if candidates.is_empty() {
            ExploreStatus::Blocked
        } else {
            ExploreStatus::Ready
        };

        self.frontiers_publisher
            .put(
                timestamp_ns,
                &Frontiers {
                    map_revision,
                    built_from_localize_revision: localize_revision,
                    frontiers,
                },
            )
            .await?;
        self.goal_candidates_publisher
            .put(
                timestamp_ns,
                &GoalCandidates {
                    map_revision,
                    built_from_localize_revision: localize_revision,
                    candidates,
                },
            )
            .await?;
        self.publish_state(
            timestamp_ns,
            status,
            None,
            Some(frontier_count),
            Some(candidate_count),
        )
        .await?;

        Ok(())
    }

    fn scenarios() -> &'static [phoxal::runtime::ScenarioDescriptor] {
        crate::scenarios::SCENARIOS
    }

    async fn run_scenario(name: &str, common: &RobotRuntimeArgs, _args: &Self::Args) -> Result<()> {
        crate::scenarios::run(name, common).await
    }
}

impl ExploreRuntime {
    async fn publish_state(
        &mut self,
        timestamp_ns: u64,
        status: ExploreStatus,
        reason: Option<String>,
        frontier_count: Option<usize>,
        candidate_count: Option<usize>,
    ) -> Result<()> {
        let reason_key = reason.as_deref().map(explore_reason);
        let state = State { status, reason };
        let logged = ExploreLogKey {
            status: state.status,
            reason: reason_key,
            frontier_count,
            candidate_count,
        };
        self.decision_log.observe(timestamp_ns, logged);
        self.state_publisher.put(timestamp_ns, &state).await
    }
}

fn explore_reason(reason: &str) -> ExploreReason {
    match reason {
        "waiting for map revision" => ExploreReason::WaitingForMapRevision,
        "waiting for localization revision" => ExploreReason::WaitingForLocalizationRevision,
        "waiting for localization pose" => ExploreReason::WaitingForLocalizationPose,
        _ => ExploreReason::Other,
    }
}
