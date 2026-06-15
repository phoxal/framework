use std::time::Duration;

use crate::core::PlanDecision;
use anyhow::Result;
use phoxal::api::localize::{self as localize_contract, v1::LocalizationState};
use phoxal::api::map::{self as map_contract, v1::MapRevision};
use phoxal::api::mission::{self as mission_contract, v1::Goal};
use phoxal::api::plan::{
    self as plan_contract,
    v1::{PlanReason, PlanStatus},
};
use phoxal::api::topic;
use phoxal::bus::typed::Received;
use phoxal::runtime::clock::Step;
use phoxal::runtime::decision_log::DecisionLog;
use phoxal::runtime::{EmptyArgs, RobotRuntimeArgs};
use phoxal::runtime::{InputPolicy, Io, Runtime, RuntimeInputs, TopicPublisher};

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
    Goal(Received<Goal>),
    LocalizationState(Received<LocalizationState>),
    MapRevision(Received<MapRevision>),
}

pub struct PlanRuntime {
    latest_goal: Option<Received<Goal>>,
    latest_localize: Option<Received<LocalizationState>>,
    latest_map_revision: Option<Received<MapRevision>>,
    decision_log: DecisionLog<PlanLogKey>,
    path_publisher: TopicPublisher<plan_contract::Path>,
    state_publisher: TopicPublisher<plan_contract::State>,
}

type PlanLogKey = (PlanStatus, Option<PlanReason>);

#[async_trait::async_trait]
impl Runtime for PlanRuntime {
    const RUNTIME_ID: &'static str = "plan";

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
        io.subscribe_topic_with(
            topic::new().mission().goal(),
            InputPolicy::latest(),
            |sample| {
                let Received { at_ns, value } = sample;
                let mission_contract::Goal::V1(value) = value;
                Input::Goal(Received { at_ns, value })
            },
        )
        .await?;
        io.subscribe_topic(topic::new().localize().state(), |sample| {
            let Received { at_ns, value } = sample;
            let localize_contract::LocalizationState::V1(value) = value;
            Input::LocalizationState(Received { at_ns, value })
        })
        .await?;
        io.subscribe_topic(topic::new().map().revision(), |sample| {
            let Received { at_ns, value } = sample;
            let map_contract::MapRevision::V1(value) = value;
            Input::MapRevision(Received { at_ns, value })
        })
        .await?;

        let state_topic = topic::new().plan().state();
        let path_publisher = io.publisher_topic(topic::new().plan().path()).await?;
        let state_publisher = io.publisher_topic(state_topic.clone()).await?;

        Ok(Self {
            latest_goal: None,
            latest_localize: None,
            latest_map_revision: None,
            decision_log: DecisionLog::from_topic(Self::RUNTIME_ID, &state_topic),
            path_publisher,
            state_publisher,
        })
    }

    async fn step(&mut self, step: Step, inputs: RuntimeInputs<Self::Input>) -> Result<()> {
        for input in inputs {
            match input {
                Input::Goal(sample) => self.latest_goal = Some(sample),
                Input::LocalizationState(sample) => self.latest_localize = Some(sample),
                Input::MapRevision(sample) => self.latest_map_revision = Some(sample),
            }
        }

        // Simple receding horizon: every step republishes a fresh path from the
        // latest pose to the latest goal instead of caching a long-lived plan.
        let decision = PlanDecision::decide(
            self.latest_goal.as_ref().map(|sample| &sample.value),
            self.latest_localize.as_ref().map(|sample| &sample.value),
            self.latest_map_revision
                .as_ref()
                .map(|sample| &sample.value),
        );
        let timestamp_ns = step.tick.time_ns();
        let (state, path) = decision.outputs(self.latest_goal.as_ref().map(|sample| &sample.value));

        if let Some(path) = path {
            self.path_publisher
                .put(timestamp_ns, &plan_contract::Path::V1(path))
                .await?;
        }
        self.decision_log
            .observe(timestamp_ns, (state.status, state.reason));
        self.state_publisher
            .put(timestamp_ns, &plan_contract::State::V1(state))
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
