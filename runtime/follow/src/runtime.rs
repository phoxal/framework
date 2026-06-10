use std::time::Duration;

use crate::core::FollowDecision;
use anyhow::Result;
use phoxal::api::v1::follow::{FollowReason, FollowStatus, State, Target};
use phoxal::api::v1::localize::LocalizationState;
use phoxal::api::v1::plan::Path;
use phoxal::api::v1::topic;
use phoxal::bus::typed::Received;
use phoxal::runtime::clock::Step;
use phoxal::runtime::decision_log::DecisionLog;
use phoxal::runtime::{EmptyArgs, RobotRuntimeArgs};
use phoxal::runtime::{InputPolicy, Io, Runtime, RuntimeInputs, TopicPublisher};

const CLOCK_PERIOD: Duration = Duration::from_millis(50);

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
    Path(Received<Path>),
    LocalizationState(Received<LocalizationState>),
}

pub struct FollowRuntime {
    latest_path: Option<Received<Path>>,
    latest_localize: Option<Received<LocalizationState>>,
    decision_log: DecisionLog<FollowLogKey>,
    target_publisher: TopicPublisher<Target>,
    state_publisher: TopicPublisher<State>,
}

type FollowLogKey = (FollowStatus, Option<FollowReason>);

#[async_trait::async_trait]
impl Runtime for FollowRuntime {
    const RUNTIME_ID: &'static str = "follow";

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
            topic::new().v1().plan().path(),
            InputPolicy::latest(),
            Input::Path,
        )
        .await?;
        io.subscribe_topic(
            topic::new().v1().localize().state(),
            Input::LocalizationState,
        )
        .await?;

        let state_topic = topic::new().v1().follow().state();
        let target_publisher = io
            .publisher_topic(topic::new().v1().follow().target())
            .await?;
        let state_publisher = io.publisher_topic(state_topic.clone()).await?;

        Ok(Self {
            latest_path: None,
            latest_localize: None,
            decision_log: DecisionLog::from_topic(Self::RUNTIME_ID, &state_topic),
            target_publisher,
            state_publisher,
        })
    }

    async fn step(&mut self, step: Step, inputs: RuntimeInputs<Self::Input>) -> Result<()> {
        for input in inputs {
            match input {
                Input::Path(sample) => self.latest_path = Some(sample),
                Input::LocalizationState(sample) => self.latest_localize = Some(sample),
            }
        }

        let decision = FollowDecision::decide(
            self.latest_path.as_ref().map(|sample| &sample.value),
            self.latest_localize.as_ref().map(|sample| &sample.value),
        );
        let (state, target) =
            decision.outputs(self.latest_path.as_ref().map(|sample| &sample.value));
        let timestamp_ns = step.tick.time_ns();

        self.target_publisher.put(timestamp_ns, &target).await?;
        self.decision_log
            .observe(timestamp_ns, (state.status, state.reason));
        self.state_publisher.put(timestamp_ns, &state).await?;

        Ok(())
    }

    fn scenarios() -> &'static [phoxal::runtime::ScenarioDescriptor] {
        crate::scenarios::SCENARIOS
    }

    async fn run_scenario(name: &str, common: &RobotRuntimeArgs, _args: &Self::Args) -> Result<()> {
        crate::scenarios::run(name, common).await
    }
}
