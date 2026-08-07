//! `power` - systemd-native host lifecycle control.
//!
//! The participant invokes the host's `systemctl reboot` or `systemctl poweroff`
//! command. A host that has no systemd stays idle and publishes why; a command
//! the host refuses or never answers is published as a fault.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use anyhow::Result;
use phoxal::api;
use phoxal::prelude::*;
use tokio::process::Command;
use tokio::time::timeout;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) struct Api {
    commands: Subscriber<api::power::Command>,
    state: StatePublisher<api::power::State>,
}

pub(crate) struct PowerState {
    latched: api::power::State,
    /// Absent on a host with no systemd. That is the whole of the "cannot act"
    /// case: every command is then answered `Idle` with the reason, never
    /// silently dropped.
    executor: Option<Box<dyn PowerExecutor>>,
}

#[phoxal::service(state = PowerState, api = Api)]
pub(crate) struct Power;

impl Participant for Power {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let executor =
            SystemdExecutor::detect().map(|executor| Box::new(executor) as Box<dyn PowerExecutor>);
        Ok((
            PowerState::new(executor),
            Api {
                commands: ctx
                    .subscriber(api::topic::owner().power().command(), 32)
                    .await?,
                state: ctx
                    .state_publisher(api::topic::owner().power().state())
                    .await?,
            },
        ))
    }

    #[phoxal::step(hz = 1)]
    async fn step(
        &self,
        api: &Self::Api,
        step: StepContext,
        state: &mut Self::State,
    ) -> Result<()> {
        while let Some(received) = api.commands.try_recv() {
            state.apply(received.body).await;
        }
        api.state.publish(&step.token, state.latched.clone())?;
        Ok(())
    }
}

impl PowerState {
    fn new(executor: Option<Box<dyn PowerExecutor>>) -> Self {
        Self {
            latched: idle_state(None),
            executor,
        }
    }

    /// Run `command` on the host and latch the state the next step publishes.
    async fn apply(&mut self, command: api::power::Command) {
        let Some(executor) = self.executor.as_deref() else {
            self.latched = idle_state(Some("systemd host integration is unavailable".to_string()));
            return;
        };
        let facts = CommandFacts::of(command);
        self.latched = match executor.submit(command).await {
            ExecutorOutcome::Accepted => api::power::State {
                status: facts.accepted,
                detail: Some(format!("systemd accepted {}", facts.label)),
            },
            ExecutorOutcome::Failed(detail) => api::power::State {
                status: api::power::Status::Failed,
                detail: Some(detail),
            },
        };
    }
}

/// The host mechanism that carries out a power command.
///
/// [`SystemdExecutor`] is the only implementation the participant ships. The
/// trait exists so `tests::StaticExecutor` can stand in for a host that is not
/// running systemd, which is the only way the accepted and failed paths are
/// reachable off a real machine.
///
/// The returned future is boxed by hand rather than written as `async fn`: an
/// `async fn` in a trait is not dyn-compatible, and the executor is held as
/// `Box<dyn PowerExecutor>` precisely so the test double can replace it.
trait PowerExecutor: Send + Sync {
    fn submit(
        &self,
        command: api::power::Command,
    ) -> Pin<Box<dyn Future<Output = ExecutorOutcome> + Send + '_>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExecutorOutcome {
    Accepted,
    Failed(String),
}

/// What one command is called at each boundary it crosses, resolved by a single
/// match so the spellings cannot drift apart.
#[derive(Clone, Copy)]
struct CommandFacts {
    /// The `systemctl` verb. Fixed by the host rather than by this service:
    /// systemd names the shutdown action `poweroff`, so that is what crosses
    /// the process boundary.
    verb: &'static str,
    /// The contract's own name for the command, as the published `detail` text
    /// spells it.
    label: &'static str,
    /// The status latched once the host accepts the command.
    accepted: api::power::Status,
}

impl CommandFacts {
    const fn of(command: api::power::Command) -> Self {
        match command {
            api::power::Command::Reboot => Self {
                verb: "reboot",
                label: "reboot",
                accepted: api::power::Status::Rebooting,
            },
            api::power::Command::Shutdown => Self {
                verb: "poweroff",
                label: "shutdown",
                accepted: api::power::Status::ShuttingDown,
            },
        }
    }
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
    fn submit(
        &self,
        command: api::power::Command,
    ) -> Pin<Box<dyn Future<Output = ExecutorOutcome> + Send + '_>> {
        Box::pin(async move {
            let verb = CommandFacts::of(command).verb;
            match timeout(
                COMMAND_TIMEOUT,
                Command::new("/usr/bin/systemctl").arg(verb).output(),
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

fn idle_state(detail: Option<String>) -> api::power::State {
    api::power::State {
        status: api::power::Status::Idle,
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An executor that reports a fixed outcome, standing in for a systemd host
    /// the test machine does not have.
    struct StaticExecutor(ExecutorOutcome);

    impl PowerExecutor for StaticExecutor {
        fn submit(
            &self,
            _command: api::power::Command,
        ) -> Pin<Box<dyn Future<Output = ExecutorOutcome> + Send + '_>> {
            Box::pin(async move { self.0.clone() })
        }
    }

    fn state_with(outcome: Option<ExecutorOutcome>) -> PowerState {
        PowerState::new(
            outcome.map(|outcome| Box::new(StaticExecutor(outcome)) as Box<dyn PowerExecutor>),
        )
    }

    #[tokio::test]
    async fn unavailable_host_stays_idle() {
        let mut state = state_with(None);
        state.apply(api::power::Command::Reboot).await;
        assert_eq!(state.latched.status, api::power::Status::Idle);
    }

    #[tokio::test]
    async fn accepted_command_transitions_to_requested_state() {
        let mut state = state_with(Some(ExecutorOutcome::Accepted));
        state.apply(api::power::Command::Shutdown).await;
        assert_eq!(state.latched.status, api::power::Status::ShuttingDown);
        assert_eq!(
            state.latched.detail.as_deref(),
            Some("systemd accepted shutdown")
        );
    }

    #[tokio::test]
    async fn backend_failure_is_typed_as_failed_state() {
        let mut state = state_with(Some(ExecutorOutcome::Failed("denied".to_string())));
        state.apply(api::power::Command::Reboot).await;
        assert_eq!(state.latched.status, api::power::Status::Failed);
        assert_eq!(state.latched.detail.as_deref(), Some("denied"));
    }

    /// The verb crosses a process boundary and the label crosses the wire, so
    /// they are pinned separately even though one command names both.
    #[test]
    fn the_shutdown_command_keeps_both_of_its_spellings() {
        let shutdown = CommandFacts::of(api::power::Command::Shutdown);
        assert_eq!(shutdown.verb, "poweroff");
        assert_eq!(shutdown.label, "shutdown");

        let reboot = CommandFacts::of(api::power::Command::Reboot);
        assert_eq!(reboot.verb, "reboot");
        assert_eq!(reboot.label, "reboot");
    }
}
