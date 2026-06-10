use std::time::Duration;

use crate::core::Arbitration;
use anyhow::Result;
use phoxal::api::v1::drive::Target as DriveTarget;
use phoxal::api::v1::follow::Target as FollowTarget;
use phoxal::api::v1::motion::{ManualCommand, MotionReason, MotionSource, State};
use phoxal::api::v1::safety::SafetyAuthorization;
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
    ManualCommand(Received<ManualCommand>),
    FollowTarget(Received<FollowTarget>),
    SafetyAuthorization(Received<SafetyAuthorization>),
}

pub struct MotionRuntime {
    latest_manual_command: Option<Received<ManualCommand>>,
    latest_follow_target: Option<Received<FollowTarget>>,
    latest_safety_authorization: Option<Received<SafetyAuthorization>>,
    decision_log: DecisionLog<MotionLogKey>,
    drive_target_publisher: TopicPublisher<DriveTarget>,
    state_publisher: TopicPublisher<State>,
}

type MotionLogKey = (Option<MotionSource>, Option<MotionReason>);

#[async_trait::async_trait]
impl Runtime for MotionRuntime {
    const RUNTIME_ID: &'static str = "motion";

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
            topic::new().v1().motion().manual(),
            InputPolicy::latest(),
            Input::ManualCommand,
        )
        .await?;
        io.subscribe_topic_with(
            topic::new().v1().follow().target(),
            InputPolicy::latest(),
            Input::FollowTarget,
        )
        .await?;
        io.subscribe_topic(
            topic::new().v1().safety().authorization(),
            Input::SafetyAuthorization,
        )
        .await?;

        let drive_target_publisher = io
            .publisher_topic(topic::new().v1().drive().target())
            .await?;
        let state_topic = topic::new().v1().motion().state();
        let state_publisher = io.publisher_topic(state_topic.clone()).await?;

        Ok(Self {
            latest_manual_command: None,
            latest_follow_target: None,
            latest_safety_authorization: None,
            decision_log: DecisionLog::from_topic(Self::RUNTIME_ID, &state_topic),
            drive_target_publisher,
            state_publisher,
        })
    }

    async fn step(&mut self, step: Step, inputs: RuntimeInputs<Self::Input>) -> Result<()> {
        for input in inputs {
            match input {
                Input::ManualCommand(sample) => self.latest_manual_command = Some(sample),
                Input::FollowTarget(sample) => self.latest_follow_target = Some(sample),
                Input::SafetyAuthorization(sample) => {
                    self.latest_safety_authorization = Some(sample);
                }
            }
        }

        let now_ns = step.tick.time_ns();
        let arbitration = Arbitration::decide(
            self.latest_manual_command.as_ref(),
            self.latest_follow_target.as_ref(),
            self.latest_safety_authorization.as_ref(),
            now_ns,
        );
        let drive_target = arbitration.drive_target;

        self.drive_target_publisher
            .put(now_ns, &drive_target)
            .await?;
        let state = State {
            active_source: arbitration.active_source,
            selected: Some(drive_target),
            reason: arbitration.reason,
        };
        self.decision_log
            .observe(now_ns, (state.active_source, state.reason));
        self.state_publisher.put(now_ns, &state).await?;
        Ok(())
    }

    fn scenarios() -> &'static [phoxal::runtime::ScenarioDescriptor] {
        crate::scenarios::SCENARIOS
    }

    async fn run_scenario(
        name: &str,
        _common: &RobotRuntimeArgs,
        _args: &Self::Args,
    ) -> Result<()> {
        crate::scenarios::run(name)
    }
}
