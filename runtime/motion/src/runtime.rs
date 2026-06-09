use std::time::Duration;

use crate::core::Arbitration;
use anyhow::Result;
use phoxal::api::drive::v1::{Target as DriveTarget, target as drive_target};
use phoxal::api::follow::v1::{Target as FollowTarget, target as follow_target};
use phoxal::api::motion::v1::{ManualCommand, MotionReason, MotionSource, State, manual, state};
use phoxal::api::safety::v1::{SafetyAuthorization, authorization as safety_authorization};
use phoxal::bus::pubsub::Stamped;
use phoxal::runtime::clock::Step;
use phoxal::runtime::decision_log::DecisionLog;
use phoxal::runtime::runtime::{InputPolicy, Io, Publisher, Runtime, RuntimeInputs};
use phoxal::runtime::{EmptyArgs, RobotRuntimeArgs};

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
    ManualCommand(Stamped<ManualCommand>),
    FollowTarget(Stamped<FollowTarget>),
    SafetyAuthorization(Stamped<SafetyAuthorization>),
}

pub struct MotionRuntime {
    latest_manual_command: Option<Stamped<ManualCommand>>,
    latest_follow_target: Option<Stamped<FollowTarget>>,
    latest_safety_authorization: Option<Stamped<SafetyAuthorization>>,
    decision_log: DecisionLog<MotionLogKey>,
    drive_target_publisher: Publisher<Stamped<DriveTarget>>,
    state_publisher: Publisher<Stamped<State>>,
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
        io.subscribe_with::<Stamped<ManualCommand>, _>(
            manual::TOPIC,
            InputPolicy::latest(),
            Input::ManualCommand,
        )
        .await?;
        io.subscribe_with::<Stamped<FollowTarget>, _>(
            follow_target::TOPIC,
            InputPolicy::latest(),
            Input::FollowTarget,
        )
        .await?;
        io.subscribe::<Stamped<SafetyAuthorization>, _>(
            safety_authorization::TOPIC,
            Input::SafetyAuthorization,
        )
        .await?;

        let drive_target_publisher = io
            .publisher::<Stamped<DriveTarget>>(drive_target::TOPIC)
            .await?;
        let state_path = state::path();
        let state_publisher = io.publisher::<Stamped<State>>(&state_path).await?;

        Ok(Self {
            latest_manual_command: None,
            latest_follow_target: None,
            latest_safety_authorization: None,
            decision_log: DecisionLog::new(
                Self::RUNTIME_ID,
                state_path,
                state::schema_name(),
                state::schema_version(),
            ),
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
            .put(&Stamped::new(now_ns, drive_target))
            .await?;
        let state = State {
            active_source: arbitration.active_source,
            selected: Some(drive_target),
            reason: arbitration.reason,
        };
        self.decision_log
            .observe(now_ns, (state.active_source, state.reason));
        self.state_publisher
            .put(&Stamped::new(now_ns, state))
            .await?;
        Ok(())
    }

    fn scenarios() -> &'static [phoxal::runtime::runtime::ScenarioDescriptor] {
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
