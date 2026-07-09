//! `power` - host lifecycle commands through the Balena supervisor.
//!
//! This participant subscribes to `power/command` (Reboot/Shutdown) and publishes the
//! latched `power/state`. Each command is forwarded to the Balena supervisor over
//! plain HTTP (its `/v1/reboot` and `/v1/shutdown` endpoints), addressed from the
//! `BALENA_SUPERVISOR_ADDRESS` and `BALENA_SUPERVISOR_API_KEY` environment
//! variables.
//!
//! The executor is built only when both env vars are present, so on a host
//! without a supervisor the participant stays in `Idle` and reports that it is
//! unconfigured rather than failing. The HTTP target is plain-HTTP only: an
//! `https://` address is rejected during parsing. A supervisor non-2xx response
//! or transport/timeout error latches `Failed`; an accepted request latches
//! `Rebooting` or `ShuttingDown`. The participant never executes the lifecycle action
//! itself - it only relays the request to the supervisor.

use std::env;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use phoxal::prelude::*;
use phoxal_api::y2026_1 as api;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

const SUPERVISOR_ADDRESS_ENV: &str = "BALENA_SUPERVISOR_ADDRESS";
const SUPERVISOR_API_KEY_ENV: &str = "BALENA_SUPERVISOR_API_KEY";
const SUPERVISOR_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(phoxal::Api)]
struct Api {
    commands: Subscriber<api::power::Command>,
    state: Publisher<api::power::State>,
}

#[phoxal::service(id = "power", config = ())]
struct Power {
    latched: api::power::State,
    executor: Option<Arc<dyn PowerExecutor>>,
}

#[phoxal::behavior]
impl Power {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        // Owner opt-in (plan #00 L2): the runner-minted capability that the
        // owner (`internal`) topic builder requires.
        let cap = ctx.owner_capability();
        Ok((
            Self {
                latched: idle_state(None),
                executor: BalenaExecutor::from_env().map(|executor| Arc::new(executor) as _),
            },
            Self::Api {
                commands: ctx
                    .subscriber(api::topic::internal::new(cap).power().command(), 32)
                    .await?,
                state: ctx
                    .publisher(api::topic::internal::new(cap).power().state())
                    .await?,
            },
        ))
    }

    #[step(hz = 1)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        while let Some(received) = api.commands.try_recv() {
            self.latched = state_for_command(received.body, self.executor.as_deref()).await;
        }

        api.state
            .publish_at(step.time(), self.latched.clone())
            .await?;
        Ok(())
    }
}

trait PowerExecutor: Send + Sync {
    fn submit<'a>(
        &'a self,
        command: api::power::Command,
    ) -> Pin<Box<dyn Future<Output = ExecutorOutcome> + Send + 'a>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExecutorOutcome {
    Accepted,
    Rejected { status_code: u16 },
    Failed { detail: String },
}

#[derive(Debug, Clone)]
struct BalenaExecutor {
    target: HttpTarget,
    api_key: String,
}

impl BalenaExecutor {
    fn from_env() -> Option<Self> {
        let address = env::var(SUPERVISOR_ADDRESS_ENV).ok()?;
        let api_key = env::var(SUPERVISOR_API_KEY_ENV).ok()?;
        let target = HttpTarget::parse(&address)?;
        Some(Self { target, api_key })
    }
}

impl PowerExecutor for BalenaExecutor {
    fn submit<'a>(
        &'a self,
        command: api::power::Command,
    ) -> Pin<Box<dyn Future<Output = ExecutorOutcome> + Send + 'a>> {
        Box::pin(async move { self.target.post(command, &self.api_key).await })
    }
}

#[derive(Debug, Clone)]
struct HttpTarget {
    host: String,
    port: u16,
    path_prefix: String,
}

impl HttpTarget {
    fn parse(address: &str) -> Option<Self> {
        let trimmed = address.trim().trim_end_matches('/');
        let without_scheme = trimmed.strip_prefix("http://").unwrap_or(trimmed);
        if without_scheme.starts_with("https://") {
            return None;
        }

        let (authority, path) = without_scheme
            .split_once('/')
            .unwrap_or((without_scheme, ""));
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (host, port.parse().ok()?),
            None => (authority, 80),
        };
        if host.is_empty() {
            return None;
        }

        let path_prefix = if path.is_empty() {
            String::new()
        } else {
            format!("/{}", path.trim_matches('/'))
        };
        Some(Self {
            host: host.to_string(),
            port,
            path_prefix,
        })
    }

    async fn post(&self, command: api::power::Command, api_key: &str) -> ExecutorOutcome {
        match timeout(SUPERVISOR_TIMEOUT, self.post_inner(command, api_key)).await {
            Ok(outcome) => outcome,
            Err(_) => ExecutorOutcome::Failed {
                detail: "timeout waiting for Balena supervisor".to_string(),
            },
        }
    }

    async fn post_inner(&self, command: api::power::Command, api_key: &str) -> ExecutorOutcome {
        let mut stream = match TcpStream::connect((self.host.as_str(), self.port)).await {
            Ok(stream) => stream,
            Err(error) => {
                return ExecutorOutcome::Failed {
                    detail: format!("transport error: {error}"),
                };
            }
        };

        let request = self.request(command, api_key);
        if let Err(error) = stream.write_all(request.as_bytes()).await {
            return ExecutorOutcome::Failed {
                detail: format!("transport error: {error}"),
            };
        }

        let mut response = Vec::new();
        if let Err(error) = stream.read_to_end(&mut response).await {
            return ExecutorOutcome::Failed {
                detail: format!("transport error: {error}"),
            };
        }

        let status_code = String::from_utf8_lossy(&response)
            .lines()
            .next()
            .and_then(|status| status.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok());

        match status_code {
            Some(200..=299) => ExecutorOutcome::Accepted,
            Some(status_code) => ExecutorOutcome::Rejected { status_code },
            None => ExecutorOutcome::Failed {
                detail: "invalid HTTP response from Balena supervisor".to_string(),
            },
        }
    }

    fn request(&self, command: api::power::Command, api_key: &str) -> String {
        let path = self.path(command, api_key);
        format!(
            "POST {path} HTTP/1.1\r\nHost: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            self.host
        )
    }

    fn path(&self, command: api::power::Command, api_key: &str) -> String {
        let action = match command {
            api::power::Command::Reboot => "reboot",
            api::power::Command::Shutdown => "shutdown",
        };
        format!("{}/v1/{action}?apikey={api_key}", self.path_prefix)
    }
}

async fn state_for_command(
    command: api::power::Command,
    executor: Option<&dyn PowerExecutor>,
) -> api::power::State {
    let Some(executor) = executor else {
        return idle_state(Some(format!(
            "Balena supervisor is not configured; set {SUPERVISOR_ADDRESS_ENV} and {SUPERVISOR_API_KEY_ENV}"
        )));
    };

    match executor.submit(command).await {
        ExecutorOutcome::Accepted => api::power::State {
            status: status_for_command(command),
            detail: Some(format!(
                "Balena supervisor accepted {}",
                command_label(command)
            )),
        },
        ExecutorOutcome::Rejected { status_code } => api::power::State {
            status: api::power::Status::Failed,
            detail: Some(format!("Balena supervisor returned HTTP {status_code}")),
        },
        ExecutorOutcome::Failed { detail } => api::power::State {
            status: api::power::Status::Failed,
            detail: Some(format!("Balena supervisor request failed: {detail}")),
        },
    }
}

fn idle_state(detail: Option<String>) -> api::power::State {
    api::power::State {
        status: api::power::Status::Idle,
        detail,
    }
}

fn status_for_command(command: api::power::Command) -> api::power::Status {
    match command {
        api::power::Command::Reboot => api::power::Status::Rebooting,
        api::power::Command::Shutdown => api::power::Status::ShuttingDown,
    }
}

fn command_label(command: api::power::Command) -> &'static str {
    match command {
        api::power::Command::Reboot => "reboot",
        api::power::Command::Shutdown => "shutdown",
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<Power>()
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;

    use phoxal::participant::{ContractRole, Participant, ParticipantApi};
    use phoxal_api::ContractBody;
    use phoxal_api::y2026_1 as api;

    use super::{ExecutorOutcome, HttpTarget, Power, PowerExecutor, idle_state, state_for_command};

    #[derive(Clone)]
    struct StaticExecutor {
        outcome: ExecutorOutcome,
    }

    impl PowerExecutor for StaticExecutor {
        fn submit<'a>(
            &'a self,
            _command: api::power::Command,
        ) -> Pin<Box<dyn Future<Output = ExecutorOutcome> + Send + 'a>> {
            Box::pin(async move { self.outcome.clone() })
        }
    }

    #[test]
    fn idle_state_keeps_status_idle() {
        assert_eq!(idle_state(None).status, api::power::Status::Idle);
        assert!(idle_state(None).detail.is_none());
    }

    #[tokio::test]
    async fn unconfigured_supervisor_stays_idle_with_detail() {
        let state = state_for_command(api::power::Command::Reboot, None).await;

        assert_eq!(state.status, api::power::Status::Idle);
        assert!(
            state
                .detail
                .unwrap()
                .contains("Balena supervisor is not configured")
        );
    }

    #[tokio::test]
    async fn accepted_reboot_latches_rebooting() {
        let executor = StaticExecutor {
            outcome: ExecutorOutcome::Accepted,
        };

        let state = state_for_command(api::power::Command::Reboot, Some(&executor)).await;

        assert_eq!(state.status, api::power::Status::Rebooting);
        assert!(state.detail.unwrap().contains("accepted reboot"));
    }

    #[tokio::test]
    async fn accepted_shutdown_latches_shutting_down() {
        let executor = StaticExecutor {
            outcome: ExecutorOutcome::Accepted,
        };

        let state = state_for_command(api::power::Command::Shutdown, Some(&executor)).await;

        assert_eq!(state.status, api::power::Status::ShuttingDown);
        assert!(state.detail.unwrap().contains("accepted shutdown"));
    }

    #[tokio::test]
    async fn supervisor_rejection_fails_with_detail() {
        let executor = StaticExecutor {
            outcome: ExecutorOutcome::Rejected { status_code: 500 },
        };

        let state = state_for_command(api::power::Command::Reboot, Some(&executor)).await;

        assert_eq!(state.status, api::power::Status::Failed);
        assert!(state.detail.unwrap().contains("HTTP 500"));
    }

    #[test]
    fn http_target_builds_balena_paths() {
        let target = HttpTarget::parse("http://balena-supervisor:48484").unwrap();

        assert_eq!(target.host, "balena-supervisor");
        assert_eq!(target.port, 48484);
        assert_eq!(
            target.path(api::power::Command::Reboot, "secret"),
            "/v1/reboot?apikey=secret"
        );
        assert!(HttpTarget::parse("https://example.com").is_none());
    }

    #[test]
    fn api_reports_contracts() {
        assert_eq!(<Power as Participant>::ID, "power");

        let contracts = <<Power as Participant>::Api as ParticipantApi>::CONTRACTS;
        assert!(contracts.iter().any(|c| {
            c.topic == <api::power::Command as ContractBody>::TOPIC
                && c.role == ContractRole::Subscribe
        }));
        assert!(contracts.iter().any(|c| {
            c.topic == <api::power::State as ContractBody>::TOPIC && c.role == ContractRole::Publish
        }));
    }
}
