//! `power` - systemd-native host lifecycle control.
//!
//! The participant invokes the host's `systemctl reboot` or `systemctl poweroff`
//! command. Hosts without systemd remain explicitly inactive; command failures
//! are reported as faults without assuming a Balena supervisor.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use phoxal::api;
use phoxal::prelude::*;
use tokio::process::Command;
use tokio::time::timeout;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(phoxal::Api)]
pub struct Api {
    commands: Subscriber<api::power::Command>,
    state: Publisher<api::power::State>,
}

#[phoxal::service(id = "power", config = ())]
pub struct Power {
    latched: api::power::State,
    executor: Option<Arc<dyn PowerExecutor>>,
}

#[phoxal::behavior]
impl Power {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        let cap = ctx.owner_capability();
        let executor = SystemdExecutor::detect().map(|executor| Arc::new(executor) as _);
        Ok((
            Self {
                latched: idle_state(None),
                executor,
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
    Failed(String),
}

#[derive(Debug, Clone)]
struct SystemdExecutor;

impl SystemdExecutor {
    fn detect() -> Option<Self> {
        cfg!(target_os = "linux")
            .then_some(Self)
            .filter(|_| std::path::Path::new("/usr/bin/systemctl").is_file())
    }
}

impl PowerExecutor for SystemdExecutor {
    fn submit<'a>(
        &'a self,
        command: api::power::Command,
    ) -> Pin<Box<dyn Future<Output = ExecutorOutcome> + Send + 'a>> {
        Box::pin(async move {
            let action = match command {
                api::power::Command::Reboot => "reboot",
                api::power::Command::Shutdown => "poweroff",
            };
            match timeout(
                COMMAND_TIMEOUT,
                Command::new("/usr/bin/systemctl").arg(action).output(),
            )
            .await
            {
                Err(_) => ExecutorOutcome::Failed("systemctl timed out".to_string()),
                Ok(Err(error)) => ExecutorOutcome::Failed(format!("systemctl failed: {error}")),
                Ok(Ok(output)) if output.status.success() => ExecutorOutcome::Accepted,
                Ok(Ok(output)) => ExecutorOutcome::Failed(format!(
                    "systemctl exited with {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                )),
            }
        })
    }
}

async fn state_for_command(
    command: api::power::Command,
    executor: Option<&dyn PowerExecutor>,
) -> api::power::State {
    let Some(executor) = executor else {
        return idle_state(Some("systemd host integration is unavailable".to_string()));
    };
    match executor.submit(command).await {
        ExecutorOutcome::Accepted => api::power::State {
            status: match command {
                api::power::Command::Reboot => api::power::Status::Rebooting,
                api::power::Command::Shutdown => api::power::Status::ShuttingDown,
            },
            detail: Some(format!("systemd accepted {}", command_label(command))),
        },
        ExecutorOutcome::Failed(detail) => api::power::State {
            status: api::power::Status::Failed,
            detail: Some(detail),
        },
    }
}

fn idle_state(detail: Option<String>) -> api::power::State {
    api::power::State {
        status: api::power::Status::Idle,
        detail,
    }
}

fn command_label(command: api::power::Command) -> &'static str {
    match command {
        api::power::Command::Reboot => "reboot",
        api::power::Command::Shutdown => "shutdown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StaticExecutor(ExecutorOutcome);

    impl PowerExecutor for StaticExecutor {
        fn submit<'a>(
            &'a self,
            _command: api::power::Command,
        ) -> Pin<Box<dyn Future<Output = ExecutorOutcome> + Send + 'a>> {
            Box::pin(async move { self.0.clone() })
        }
    }

    #[tokio::test]
    async fn unavailable_host_stays_idle() {
        let state = state_for_command(api::power::Command::Reboot, None).await;
        assert_eq!(state.status, api::power::Status::Idle);
    }

    #[tokio::test]
    async fn accepted_command_transitions_to_requested_state() {
        let executor = StaticExecutor(ExecutorOutcome::Accepted);
        let state = state_for_command(api::power::Command::Shutdown, Some(&executor)).await;
        assert_eq!(state.status, api::power::Status::ShuttingDown);
    }

    #[tokio::test]
    async fn backend_failure_is_typed_as_failed_state() {
        let executor = StaticExecutor(ExecutorOutcome::Failed("denied".to_string()));
        let state = state_for_command(api::power::Command::Reboot, Some(&executor)).await;
        assert_eq!(state.status, api::power::Status::Failed);
        assert_eq!(state.detail.as_deref(), Some("denied"));
    }
}
