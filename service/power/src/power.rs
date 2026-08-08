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
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::timeout;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const COMMAND_REAP_TIMEOUT: Duration = Duration::from_millis(100);
const POWER_SHUTDOWN_ACK_TIMEOUT: Duration = Duration::from_millis(250);

pub(crate) struct Api {
    commands: Subscriber<api::power::Command>,
    command_tx: mpsc::Sender<api::power::Command>,
    state: StatePublisher<api::power::State>,
    shutdown_tx: watch::Sender<bool>,
}

pub(crate) struct PowerState {
    latched: api::power::State,
    results: mpsc::Receiver<(api::power::Command, ExecutorOutcome)>,
    shutdown_ack: Option<oneshot::Receiver<()>>,
    #[cfg(test)]
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
        let (command_tx, mut command_rx) = mpsc::channel(32);
        let (result_tx, result_rx) = mpsc::channel(32);
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let (shutdown_ack_tx, shutdown_ack_rx) = oneshot::channel();
        ctx.spawn_managed("power-command-executor", async move {
            loop {
                let command = tokio::select! {
                    _ = cancellation_requested(&mut shutdown_rx) => break,
                    command = command_rx.recv() => match command {
                        Some(command) => command,
                        None => break,
                    },
                };
                let outcome = match executor.as_deref() {
                    Some(executor) => executor.submit(command, shutdown_rx.clone()).await,
                    None => ExecutorOutcome::Unavailable,
                };
                if outcome == ExecutorOutcome::Cancelled {
                    break;
                }
                if result_tx.send((command, outcome)).await.is_err() {
                    break;
                }
            }
            let _ = shutdown_ack_tx.send(());
            Ok::<(), anyhow::Error>(())
        });
        Ok((
            PowerState::runtime(result_rx, shutdown_ack_rx),
            Api {
                commands: ctx
                    .subscriber(api::topic::owner().power().command())
                    .await?,
                command_tx,
                state: ctx
                    .state_publisher(api::topic::owner().power().state())
                    .await?,
                shutdown_tx,
            },
        ))
    }

    async fn shutdown(&self, api: &Self::Api, state: &mut Self::State) -> Result<()> {
        api.shutdown_tx
            .send(true)
            .map_err(|_| anyhow::anyhow!("power command executor is no longer running"))?;
        let Some(ack) = state.shutdown_ack.take() else {
            return Ok(());
        };
        match timeout(POWER_SHUTDOWN_ACK_TIMEOUT, ack).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err(anyhow::anyhow!(
                "power command executor dropped before acknowledging shutdown"
            )),
            Err(_) => Err(anyhow::anyhow!(
                "power command executor did not acknowledge shutdown within {:?}",
                POWER_SHUTDOWN_ACK_TIMEOUT
            )),
        }
    }

    #[phoxal::step(hz = 1)]
    fn step(&self, api: &Self::Api, step: StepContext, state: &mut Self::State) -> Result<()> {
        while let Some(received) = api.commands.try_recv() {
            api.command_tx
                .try_send(received.body)
                .map_err(|error| anyhow::anyhow!("power command executor unavailable: {error}"))?;
        }
        while let Ok((command, outcome)) = state.results.try_recv() {
            state.latch(command, outcome);
        }
        api.state.publish(&step.token, state.latched.clone())?;
        Ok(())
    }
}

impl PowerState {
    fn runtime(
        results: mpsc::Receiver<(api::power::Command, ExecutorOutcome)>,
        shutdown_ack: oneshot::Receiver<()>,
    ) -> Self {
        Self {
            latched: idle_state(None),
            results,
            shutdown_ack: Some(shutdown_ack),
            #[cfg(test)]
            executor: None,
        }
    }

    #[cfg(test)]
    fn new(executor: Option<Box<dyn PowerExecutor>>) -> Self {
        let (_sender, results) = mpsc::channel(1);
        Self {
            latched: idle_state(None),
            results,
            shutdown_ack: None,
            executor,
        }
    }

    /// Run `command` on the host and latch the state the next step publishes.
    #[cfg(test)]
    async fn apply(&mut self, command: api::power::Command) {
        let Some(executor) = self.executor.as_deref() else {
            self.latched = idle_state(Some("systemd host integration is unavailable".to_string()));
            return;
        };
        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        self.latch(command, executor.submit(command, shutdown_rx).await);
    }

    fn latch(&mut self, command: api::power::Command, outcome: ExecutorOutcome) {
        let facts = CommandFacts::of(command);
        self.latched = match outcome {
            ExecutorOutcome::Accepted => api::power::State {
                status: facts.accepted,
                detail: Some(format!("systemd accepted {}", facts.label)),
            },
            ExecutorOutcome::Failed(detail) => api::power::State {
                status: api::power::Status::Failed,
                detail: Some(detail),
            },
            ExecutorOutcome::Unavailable => {
                idle_state(Some("systemd host integration is unavailable".to_string()))
            }
            ExecutorOutcome::Cancelled => idle_state(Some(
                "systemd command executor is shutting down".to_string(),
            )),
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
        shutdown: watch::Receiver<bool>,
    ) -> Pin<Box<dyn Future<Output = ExecutorOutcome> + Send + '_>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExecutorOutcome {
    Accepted,
    Failed(String),
    Unavailable,
    Cancelled,
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
        shutdown: watch::Receiver<bool>,
    ) -> Pin<Box<dyn Future<Output = ExecutorOutcome> + Send + '_>> {
        Box::pin(async move {
            let verb = CommandFacts::of(command).verb;
            let mut child = Command::new("/usr/bin/systemctl");
            child.arg(verb);
            run_owned_command(child, COMMAND_TIMEOUT, shutdown).await
        })
    }
}

/// Run one host command with explicit child ownership. A timeout first sends a
/// kill request and then waits for the child to reap inside a second bounded
/// phase; dropping a future is only the final `kill_on_drop` safety net for
/// cancellation of the managed executor task itself.
async fn run_owned_command(
    mut command: Command,
    timeout_duration: Duration,
    mut shutdown: watch::Receiver<bool>,
) -> ExecutorOutcome {
    command.kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return ExecutorOutcome::Failed(format!("systemctl failed: {error}")),
    };

    let mut wait = Box::pin(child.wait());
    let mut timer = Box::pin(tokio::time::sleep(timeout_duration));
    tokio::select! {
        result = &mut wait => match result {
            Ok(status) if status.success() => ExecutorOutcome::Accepted,
            Ok(status) => ExecutorOutcome::Failed(format!("systemctl exited with {status}")),
            Err(error) => ExecutorOutcome::Failed(format!("systemctl failed: {error}")),
        },
        _ = cancellation_requested(&mut shutdown) => {
            drop(wait);
            match kill_and_reap(&mut child).await {
                Ok(()) => ExecutorOutcome::Cancelled,
                Err(error) => ExecutorOutcome::Failed(format!(
                    "power command cancellation cleanup failed: {error}"
                )),
            }
        },
        _ = &mut timer => {
            drop(wait);
            match kill_and_reap(&mut child).await {
                Ok(()) => ExecutorOutcome::Failed("systemctl timed out".to_string()),
                Err(error) => ExecutorOutcome::Failed(format!(
                    "systemctl timed out; {error}"
                )),
            }
        },
    }
}

async fn cancellation_requested(shutdown: &mut watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow() {
            return;
        }
        if shutdown.changed().await.is_err() {
            return;
        }
    }
}

async fn kill_and_reap(child: &mut Child) -> std::result::Result<(), String> {
    let kill_error = child.start_kill().err();
    match timeout(COMMAND_REAP_TIMEOUT, child.wait()).await {
        Ok(Ok(_)) => match kill_error {
            None => Ok(()),
            Some(error) => Err(format!("killing child failed: {error}")),
        },
        Ok(Err(error)) => Err(format!(
            "waiting for killed child failed{}: {error}",
            kill_error
                .map(|error| format!("; killing child failed: {error}"))
                .unwrap_or_default()
        )),
        Err(_) => Err(format!(
            "killed child was not reaped{}",
            kill_error
                .map(|error| format!("; killing child failed: {error}"))
                .unwrap_or_default()
        )),
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
            _shutdown: watch::Receiver<bool>,
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

    /// The production executor never reaches this test's shell command: the
    /// seam exercises the same owned-child timeout path without invoking
    /// systemctl. A command still in flight at shutdown is killed and reaped
    /// within the bounded second phase.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_in_flight_command_is_killed_and_reaped_on_shutdown() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 60"]);
        let (_shutdown_tx, shutdown_rx) = watch::channel(false);

        let began = std::time::Instant::now();
        let outcome = run_owned_command(command, Duration::from_millis(25), shutdown_rx).await;

        assert!(began.elapsed() < Duration::from_secs(1));
        assert_eq!(
            outcome,
            ExecutorOutcome::Failed("systemctl timed out".to_string())
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_in_flight_command_is_killed_when_the_executor_is_cancelled() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 60"]);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (ack_tx, ack_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let outcome = run_owned_command(command, Duration::from_secs(60), shutdown_rx).await;
            ack_tx
                .send(())
                .expect("the cancellation acknowledgement receiver remains alive");
            outcome
        });

        tokio::time::sleep(Duration::from_millis(25)).await;
        shutdown_tx
            .send(true)
            .expect("the in-flight executor still owns its cancellation receiver");
        let outcome = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("cancelling an in-flight executor must be bounded")
            .expect("the executor task must return after kill and reap");
        assert_eq!(outcome, ExecutorOutcome::Cancelled);
        tokio::time::timeout(Duration::from_secs(1), ack_rx)
            .await
            .expect("the executor cancellation acknowledgement must be bounded")
            .expect("the executor must acknowledge after reaping the child");
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
