use std::sync::Arc;

use anyhow::Result;
use phoxal::api::v1::power::{Command, FailedReason, RejectedReason, State, Status};
use phoxal::api::v1::topic;
use phoxal::bus::typed::Received;
use phoxal::runtime::RobotRuntimeArgs;
use phoxal::runtime::clock::Step;
use phoxal::runtime::{Io, Runtime, RuntimeInputs, TopicPublisher};

#[derive(Debug)]
pub(crate) struct Config {
    pub(crate) supervisor_address: Option<String>,
    pub(crate) supervisor_api_key: Option<String>,
}

#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    #[arg(long = "balena-supervisor-address", env = "BALENA_SUPERVISOR_ADDRESS")]
    supervisor_address: Option<String>,

    #[arg(
        long = "balena-supervisor-api-key",
        env = "BALENA_SUPERVISOR_API_KEY",
        hide = true
    )]
    supervisor_api_key: Option<String>,
}

pub(crate) enum Input {
    Command(Received<Command>),
}

pub(crate) struct PowerRuntime {
    latched: LatchedState,
    executor: Option<Arc<dyn PowerExecutor>>,
    state_pub: TopicPublisher<State>,
}

#[async_trait::async_trait]
impl Runtime for PowerRuntime {
    const RUNTIME_ID: &'static str = "power";

    type Args = Args;
    type Config = Config;
    type Input = Input;

    fn config(args: &Self::Args, _common: &RobotRuntimeArgs) -> Result<Self::Config> {
        Ok(Config {
            supervisor_address: args.supervisor_address.clone(),
            supervisor_api_key: args.supervisor_api_key.clone(),
        })
    }

    fn clock_period(_config: &Self::Config) -> std::time::Duration {
        std::time::Duration::from_secs(1)
    }

    async fn new(io: &mut Io<Self::Input>, config: Self::Config) -> Result<Self> {
        io.subscribe_topic(topic::new().v1().power().command(), Input::Command)
            .await?;
        let state_pub = io
            .publisher_topic(topic::new().v1().power().state())
            .await?;
        let executor = match config.supervisor_address {
            Some(address) => Some(Arc::new(ReqwestExecutor::new(
                address,
                config.supervisor_api_key,
            )?) as Arc<dyn PowerExecutor>),
            None => None,
        };

        Ok(Self {
            latched: LatchedState::default(),
            executor,
            state_pub,
        })
    }

    async fn step(&mut self, step: Step, inputs: RuntimeInputs<Self::Input>) -> Result<()> {
        let executor = self.executor.clone();
        for input in inputs {
            match input {
                Input::Command(received) => {
                    self.latched
                        .apply(received.value, executor.as_deref())
                        .await;
                }
            }
        }

        let state = self.latched.snapshot();
        self.state_pub.put(step.tick.time_ns(), &state).await?;

        Ok(())
    }
}

#[derive(Debug)]
struct LatchedState {
    state: State,
}

impl LatchedState {
    async fn apply(&mut self, command: Command, executor: Option<&dyn PowerExecutor>) {
        self.state = state_for_command(command, executor).await;
    }

    fn snapshot(&self) -> State {
        self.state.clone()
    }
}

impl Default for LatchedState {
    fn default() -> Self {
        Self {
            state: idle_state(),
        }
    }
}

#[async_trait::async_trait]
trait PowerExecutor: Send + Sync {
    async fn submit(&self, command: Command) -> ExecutorOutcome;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecutorOutcome {
    Accepted,
    Rejected(RejectedReason),
    Failed(FailedReason),
}

struct ReqwestExecutor {
    client: reqwest::Client,
    supervisor_address: String,
    supervisor_api_key: Option<String>,
}

impl ReqwestExecutor {
    fn new(supervisor_address: String, supervisor_api_key: Option<String>) -> Result<Self> {
        Ok(Self {
            client: build_client()?,
            supervisor_address,
            supervisor_api_key,
        })
    }

    fn endpoint(&self, command: Command) -> String {
        let path = match command {
            Command::Poweroff => "shutdown",
            Command::Reboot => "reboot",
        };
        format!(
            "{}/v1/{path}",
            self.supervisor_address.trim_end_matches('/')
        )
    }
}

#[async_trait::async_trait]
impl PowerExecutor for ReqwestExecutor {
    async fn submit(&self, command: Command) -> ExecutorOutcome {
        let mut url = match reqwest::Url::parse(&self.endpoint(command)) {
            Ok(url) => url,
            Err(_) => return ExecutorOutcome::Failed(FailedReason::SupervisorTransport),
        };
        if let Some(api_key) = &self.supervisor_api_key {
            url.query_pairs_mut().append_pair("apikey", api_key);
        }

        match self.client.post(url).send().await {
            Ok(response) => {
                let status = response.status();
                if status.is_success() {
                    ExecutorOutcome::Accepted
                } else {
                    ExecutorOutcome::Rejected(RejectedReason::SupervisorReturnedHttp {
                        code: status.as_u16(),
                    })
                }
            }
            Err(_) => ExecutorOutcome::Failed(FailedReason::SupervisorTransport),
        }
    }
}

fn build_client() -> Result<reqwest::Client> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    Ok(reqwest::Client::builder()
        .use_preconfigured_tls(tls)
        .build()?)
}

async fn state_for_command(command: Command, executor: Option<&dyn PowerExecutor>) -> State {
    let Some(executor) = executor else {
        return State {
            requested: Some(command),
            status: Status::Rejected(RejectedReason::SupervisorUnavailable),
        };
    };

    let status = match executor.submit(command).await {
        ExecutorOutcome::Accepted => Status::Accepted,
        ExecutorOutcome::Rejected(reason) => Status::Rejected(reason),
        ExecutorOutcome::Failed(reason) => Status::Failed(reason),
    };

    State {
        requested: Some(command),
        status,
    }
}

fn idle_state() -> State {
    State {
        requested: None,
        status: Status::Idle,
    }
}

#[cfg(test)]
mod tests {
    use super::{ExecutorOutcome, LatchedState, PowerExecutor, idle_state, state_for_command};
    use phoxal::api::v1::power::{Command, FailedReason, RejectedReason, State, Status};

    struct StaticExecutor {
        outcome: ExecutorOutcome,
    }

    #[async_trait::async_trait]
    impl PowerExecutor for StaticExecutor {
        async fn submit(&self, _command: Command) -> ExecutorOutcome {
            self.outcome
        }
    }

    #[test]
    fn default_state_is_idle() {
        assert_eq!(
            idle_state(),
            State {
                requested: None,
                status: Status::Idle,
            }
        );
    }

    #[tokio::test]
    async fn command_without_supervisor_is_rejected_supervisor_unavailable() {
        let state = state_for_command(Command::Reboot, None).await;

        assert_eq!(
            state,
            State {
                requested: Some(Command::Reboot),
                status: Status::Rejected(RejectedReason::SupervisorUnavailable),
            }
        );
    }

    #[tokio::test]
    async fn accepted_supervisor_response_becomes_accepted_state() {
        let executor = StaticExecutor {
            outcome: ExecutorOutcome::Accepted,
        };

        let state = state_for_command(Command::Poweroff, Some(&executor)).await;

        assert_eq!(
            state,
            State {
                requested: Some(Command::Poweroff),
                status: Status::Accepted,
            }
        );
    }

    #[tokio::test]
    async fn http_error_supervisor_response_becomes_rejected_state() {
        let executor = StaticExecutor {
            outcome: ExecutorOutcome::Rejected(RejectedReason::SupervisorReturnedHttp {
                code: 500,
            }),
        };

        let state = state_for_command(Command::Reboot, Some(&executor)).await;

        assert_eq!(
            state,
            State {
                requested: Some(Command::Reboot),
                status: Status::Rejected(RejectedReason::SupervisorReturnedHttp { code: 500 }),
            }
        );
    }

    #[tokio::test]
    async fn transport_error_becomes_failed_state() {
        let executor = StaticExecutor {
            outcome: ExecutorOutcome::Failed(FailedReason::SupervisorTransport),
        };

        let state = state_for_command(Command::Poweroff, Some(&executor)).await;

        assert_eq!(
            state,
            State {
                requested: Some(Command::Poweroff),
                status: Status::Failed(FailedReason::SupervisorTransport),
            }
        );
    }

    #[tokio::test]
    async fn snapshot_remains_latched_until_next_command() {
        let accepted = StaticExecutor {
            outcome: ExecutorOutcome::Accepted,
        };
        let mut latched = LatchedState::default();

        latched.apply(Command::Reboot, Some(&accepted)).await;

        let expected = State {
            requested: Some(Command::Reboot),
            status: Status::Accepted,
        };
        assert_eq!(latched.snapshot(), expected);
        assert_eq!(latched.snapshot(), expected);
    }
}
