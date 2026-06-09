use std::time::Duration;

use crate::core::PlanDecision;
use anyhow::Result;
use phoxal::api::localize::v1::LocalizationState;
use phoxal::api::map::v1::MapRevision;
use phoxal::api::mission::v1::Goal;
use phoxal::api::plan::v1::{Path, PlanReason, PlanStatus, State, path, state};
use phoxal::bus::pubsub::Stamped;
use phoxal::bus::zenoh::TypedSchema;
use phoxal::runtime::clock::Step;
use phoxal::runtime::decision_log::DecisionLog;
use phoxal::runtime::{EmptyArgs, RobotRuntimeArgs};
use phoxal::runtime::{InputPolicy, Io, Publisher, Runtime, RuntimeInputs};

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
    Goal(Stamped<Goal>),
    LocalizationState(Stamped<LocalizationState>),
    MapRevision(Stamped<MapRevision>),
}

pub struct PlanRuntime {
    latest_goal: Option<Stamped<Goal>>,
    latest_localize: Option<Stamped<LocalizationState>>,
    latest_map_revision: Option<Stamped<MapRevision>>,
    decision_log: DecisionLog<PlanLogKey>,
    path_publisher: Publisher<Stamped<Path>>,
    state_publisher: Publisher<Stamped<State>>,
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
        io.subscribe_with::<Stamped<Goal>, _>(
            &phoxal::api::mission::v1::goal::path(),
            InputPolicy::latest(),
            Input::Goal,
        )
        .await?;
        io.subscribe::<Stamped<LocalizationState>, _>(
            &phoxal::api::localize::v1::state::path(),
            Input::LocalizationState,
        )
        .await?;
        io.subscribe::<Stamped<MapRevision>, _>(
            &phoxal::api::map::v1::revision::path(),
            Input::MapRevision,
        )
        .await?;

        let path_publisher = io.publisher::<Stamped<Path>>(&path::path()).await?;
        let state_publisher = io.publisher::<Stamped<State>>(&state::path()).await?;

        Ok(Self {
            latest_goal: None,
            latest_localize: None,
            latest_map_revision: None,
            decision_log: DecisionLog::new(
                Self::RUNTIME_ID,
                state::path(),
                <State as TypedSchema>::SCHEMA_NAME,
                <State as TypedSchema>::SCHEMA_VERSION,
            ),
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
            self.latest_goal.as_ref().map(|sample| &sample.data),
            self.latest_localize.as_ref().map(|sample| &sample.data),
            self.latest_map_revision.as_ref().map(|sample| &sample.data),
        );
        let timestamp_ns = step.tick.time_ns();
        let (state, path) = decision.outputs(self.latest_goal.as_ref().map(|sample| &sample.data));

        if let Some(path) = path {
            self.path_publisher
                .put(&Stamped::new(timestamp_ns, path))
                .await?;
        }
        self.decision_log
            .observe(timestamp_ns, (state.status, state.reason));
        self.state_publisher
            .put(&Stamped::new(timestamp_ns, state))
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
