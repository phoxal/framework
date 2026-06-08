use std::time::Duration;

use anyhow::Result;
use phoxal_api_explore::v1::{
    ExploreStatus, Frontiers, GoalCandidates, State, frontiers, goal_candidates, state,
};
use phoxal_api_frame::v1::FrameId;
use phoxal_api_localize::v1::{LocalizationRevisionId, LocalizationState};
use phoxal_api_map::v1::{MapRevision, Traversability, revision, traversability};
use phoxal_core_engine::clock::Step;
use phoxal_core_engine::decision_log::DecisionLog;
use phoxal_core_engine::step::{Io, Publisher, Runtime, RuntimeInputs};
use phoxal_core_engine::{EmptyArgs, RobotRuntimeArgs};
use phoxal_infra_bus::pubsub::Stamped;
use phoxal_infra_bus::zenoh_typed::TypedSchema;

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
    Traversability(Stamped<Traversability>),
    MapRevision(Stamped<MapRevision>),
    LocalizationState(Stamped<LocalizationState>),
}

pub struct ExploreRuntime {
    planar_frame_id: FrameId,
    latest_traversability: Option<Stamped<Traversability>>,
    latest_map_revision: Option<Stamped<MapRevision>>,
    latest_pose_xy_m: Option<[f64; 2]>,
    latest_localize_revision: Option<LocalizationRevisionId>,
    last_centroids: Vec<[f64; 2]>,
    decision_log: DecisionLog<ExploreLogKey>,
    frontiers_publisher: Publisher<Stamped<Frontiers>>,
    goal_candidates_publisher: Publisher<Stamped<GoalCandidates>>,
    state_publisher: Publisher<Stamped<State>>,
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
        io.subscribe::<Stamped<Traversability>, _>(traversability::TOPIC, Input::Traversability)
            .await?;
        io.subscribe::<Stamped<MapRevision>, _>(revision::TOPIC, Input::MapRevision)
            .await?;
        io.subscribe::<Stamped<LocalizationState>, _>(
            phoxal_api_localize::v1::state::TOPIC,
            Input::LocalizationState,
        )
        .await?;

        Ok(Self {
            planar_frame_id: config.planar_frame_id,
            latest_traversability: None,
            latest_map_revision: None,
            latest_pose_xy_m: None,
            latest_localize_revision: None,
            last_centroids: Vec::new(),
            decision_log: DecisionLog::new(
                Self::RUNTIME_ID,
                state::TOPIC,
                <State as TypedSchema>::SCHEMA_NAME,
                <State as TypedSchema>::SCHEMA_VERSION,
            ),
            frontiers_publisher: io.publisher::<Stamped<Frontiers>>(frontiers::TOPIC).await?,
            goal_candidates_publisher: io
                .publisher::<Stamped<GoalCandidates>>(goal_candidates::TOPIC)
                .await?,
            state_publisher: io.publisher::<Stamped<State>>(state::TOPIC).await?,
        })
    }

    async fn step(&mut self, step: Step, inputs: RuntimeInputs<Self::Input>) -> Result<()> {
        for input in inputs {
            match input {
                Input::Traversability(sample) => self.latest_traversability = Some(sample),
                Input::MapRevision(sample) => self.latest_map_revision = Some(sample),
                Input::LocalizationState(sample) => {
                    self.latest_localize_revision = sample.data.revision;
                    self.latest_pose_xy_m = sample
                        .data
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
            .map(|sample| sample.data.map_revision_id)
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
            detect_frontiers_in_frame(&traversability.data.cells, &self.planar_frame_id.0);
        let candidates = score_candidates(
            &frontiers,
            &traversability.data.cells,
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
            .put(&Stamped::new(
                timestamp_ns,
                Frontiers {
                    map_revision,
                    built_from_localize_revision: localize_revision,
                    frontiers,
                },
            ))
            .await?;
        self.goal_candidates_publisher
            .put(&Stamped::new(
                timestamp_ns,
                GoalCandidates {
                    map_revision,
                    built_from_localize_revision: localize_revision,
                    candidates,
                },
            ))
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

    fn scenarios() -> &'static [phoxal_core_engine::step::ScenarioDescriptor] {
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
        self.state_publisher
            .put(&Stamped::new(timestamp_ns, state))
            .await
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
