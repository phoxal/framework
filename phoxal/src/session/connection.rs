//! The session itself: how one is established, what it hands out, and how it
//! ends.

use std::future::Future;
use std::sync::Arc;

use tokio::sync::{oneshot, watch};
use tokio::task::{JoinHandle, JoinSet};

use crate::bus::session::{BusConfig, BusOwner};
use crate::bus::{
    AskQuery, BusFault, BusHandle, DEFAULT_QUERY_TIMEOUT, Endpoint, Event, EventReceiver,
    KeyLivelinessObserver, LivelinessStatus, Publish, Querier, QueryEndpoint, Sample,
    SampleReceiver, Setpoint, SetpointPublisher, SourceLabel, State, StateView, Stream,
    StreamDelivered, StreamPublisher, StreamReceiver, Subscribe, Topic,
};
use crate::identity::{ExecutionId, RobotId};
use crate::supervisor::api;
use crate::supervisor::api::command::{Command, CommandOutcome};
use crate::supervisor::api::connect::PRESENCE_KEY;
use crate::supervisor::api::execution::{Snapshot, SnapshotDocument};
use crate::version::FrameworkVersion;

use crate::session::error::{CloseError, ConnectError, DisconnectReason, SessionError};

/// Inputs for one direct session against one execution.
#[derive(Clone, Debug)]
pub struct ConnectOptions {
    /// The router endpoint to resolve. It must identify exactly one execution.
    pub endpoint: String,
    /// A bounded diagnostic label this session's own traffic carries. It never
    /// affects routing, authority, or admission.
    pub label: String,
}

impl ConnectOptions {
    #[must_use]
    pub fn new(endpoint: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            label: label.into(),
        }
    }
}

/// Immutable facts established while connecting.
///
/// The clock is deliberately absent. The supervisor owns a dynamic time-domain
/// endpoint for participant lifecycle, while a session only establishes the
/// immutable execution identity, model, and compatible framework train.
#[derive(Clone, Debug)]
pub struct ConnectedExecution {
    pub execution: ExecutionId,
    /// Read once at connect from the manifest `supervisor/info` answers with.
    /// The supervisor is handed one bundle root for the life of an execution,
    /// so this value never changes and no caller has to ask twice.
    pub robot: RobotId,
    pub framework: FrameworkVersion,
}

/// Cloneable operations borrowed from one uniquely-owned [`Session`].
///
/// A handle cannot create, replace, or close the underlying session. Once the
/// supervisor identity, snapshot stream, or bus worker is lost, or the owner
/// begins close, existing typed handles become terminal and new operations
/// return [`SessionError::Disconnected`].
#[derive(Clone)]
pub struct SessionHandle {
    connected: Arc<ConnectedExecution>,
    bus: BusHandle,
    snapshots: watch::Receiver<Option<Snapshot>>,
    terminal: watch::Receiver<Option<DisconnectReason>>,
    info: Querier<api::info::InfoRequest>,
    command: Querier<api::command::Request>,
    logs: Querier<api::logs::SnapshotRequest>,
    telemetry: Querier<api::telemetry::SnapshotRequest>,
}

impl std::fmt::Debug for SessionHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionHandle")
            .field("execution", &self.connected.execution)
            .field("terminal", &self.disconnect_reason())
            .finish_non_exhaustive()
    }
}

impl SessionHandle {
    /// The facts established while connecting.
    #[must_use]
    pub fn connected(&self) -> &ConnectedExecution {
        &self.connected
    }

    /// The execution this session is attached to.
    #[must_use]
    pub fn execution(&self) -> ExecutionId {
        self.connected.execution
    }

    /// A receiver for the newest owner-published state.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the session is already terminal or the
    /// subscription cannot be declared.
    pub async fn state_view<E>(
        &self,
        topic: Topic<Subscribe<E>>,
    ) -> Result<StateView<E>, SessionError>
    where
        E: Endpoint<Semantics = State>,
    {
        self.ensure_connected()?;
        Ok(StateView::new(&self.bus, &topic).await?)
    }

    /// A bounded ordered receiver for owner-published samples.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the session is already terminal or the
    /// subscription cannot be declared.
    pub async fn sample_receiver<E>(
        &self,
        topic: Topic<Subscribe<E>>,
    ) -> Result<SampleReceiver<E>, SessionError>
    where
        E: Endpoint<Semantics = Sample>,
    {
        self.ensure_connected()?;
        Ok(SampleReceiver::new(&self.bus, &topic).await?)
    }

    /// An ordered receiver for owner-published events.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the session is already terminal or the
    /// subscription cannot be declared.
    pub async fn event_receiver<E>(
        &self,
        topic: Topic<Subscribe<E>>,
    ) -> Result<EventReceiver<E>, SessionError>
    where
        E: Endpoint<Semantics = Event>,
    {
        self.ensure_connected()?;
        Ok(EventReceiver::new(&self.bus, &topic).await?)
    }

    /// An ordered receiver for the chunks of an owner-published stream.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the session is already terminal or the
    /// subscription cannot be declared.
    pub async fn stream_receiver<E>(
        &self,
        topic: Topic<Subscribe<E>>,
    ) -> Result<StreamReceiver<E>, SessionError>
    where
        E: Endpoint,
        E::Semantics: StreamDelivered,
    {
        self.ensure_connected()?;
        Ok(StreamReceiver::new(&self.bus, &topic).await?)
    }

    /// A newest-actionable publisher for an owner-consumed setpoint.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the session is already terminal or the
    /// publisher cannot be attached.
    pub fn setpoint_publisher<E>(
        &self,
        topic: Topic<Publish<E>>,
    ) -> Result<SetpointPublisher<E>, SessionError>
    where
        E: Endpoint<Semantics = Setpoint>,
    {
        self.ensure_connected()?;
        Ok(SetpointPublisher::new(self.bus.clone(), &topic)?)
    }

    /// An ordered publisher for the chunks of an owner-consumed stream.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the session is already terminal or the
    /// publisher cannot be attached.
    pub fn stream_publisher<E>(
        &self,
        topic: Topic<Publish<E>>,
    ) -> Result<StreamPublisher<E>, SessionError>
    where
        E: Endpoint<Semantics = Stream<crate::bus::In>>,
    {
        self.ensure_connected()?;
        Ok(StreamPublisher::new(self.bus.clone(), &topic)?)
    }

    /// A bounded request/reply handle using the framework's current default
    /// query timeout.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the session is already terminal or the
    /// querier cannot be attached.
    pub fn querier<E>(&self, topic: Topic<AskQuery<E>>) -> Result<Querier<E>, SessionError>
    where
        E: QueryEndpoint,
    {
        self.ensure_connected()?;
        Ok(Querier::new(
            self.bus.clone(),
            &topic,
            DEFAULT_QUERY_TIMEOUT,
        )?)
    }

    /// A watch over the supervisor's authoritative execution snapshot.
    #[must_use]
    pub fn snapshots(&self) -> watch::Receiver<Option<Snapshot>> {
        self.snapshots.clone()
    }

    /// The newest execution snapshot this session has installed.
    #[must_use]
    pub fn snapshot(&self) -> Option<Snapshot> {
        self.snapshots.borrow().clone()
    }

    /// The first terminal reason observed for this session.
    #[must_use]
    pub fn disconnect_reason(&self) -> Option<DisconnectReason> {
        terminal_reason(&self.terminal)
    }

    /// Resolve when this session becomes terminal.
    ///
    /// The returned value is the same first cause retained by
    /// [`Self::disconnect_reason`].
    pub async fn disconnected(&self) -> DisconnectReason {
        let mut terminal = self.terminal.clone();
        wait_for_terminal(&mut terminal).await
    }

    /// Ask supervisor authority to perform one acknowledged host operation.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the session is terminal or the query
    /// fails.
    pub async fn command(&self, command: Command) -> Result<CommandOutcome, SessionError> {
        self.ensure_connected()?;
        Ok(self
            .command
            .query(api::command::Request::V0 { command })
            .await
            .map(|reply| match reply {
                api::command::Reply::V0 { outcome } => outcome,
            })?)
    }

    /// Ask the host this execution runs on to reboot.
    ///
    /// There is no `stop` and no `restart` beside it. The supervisor starts
    /// nothing, so it can stop nothing: whoever launched a runtime stops that
    /// runtime, and a client that launched none has no business ending a graph
    /// through a process that never started it. What remains here is the one
    /// thing only the machine running the supervisor can do for a remote
    /// operator - cycle its own power.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the session is terminal or the query
    /// fails.
    pub async fn reboot(&self) -> Result<CommandOutcome, SessionError> {
        self.command(Command::Reboot).await
    }

    /// Ask the host this execution runs on to power off.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the session is terminal or the query
    /// fails.
    pub async fn poweroff(&self) -> Result<CommandOutcome, SessionError> {
        self.command(Command::Poweroff).await
    }

    /// The bundle manifest this execution is running, exactly as it is on disk.
    ///
    /// It is the same document every participant of this execution reads, so a
    /// caller that needs the mounted components or a runtime's configuration
    /// gets the robot itself rather than a projection of it that could
    /// disagree. [`ConnectedExecution::robot`] is this document's id, already
    /// read once at connect, so asking again is only worth it for the rest of
    /// the model.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the session is terminal or the query
    /// fails.
    pub async fn manifest(&self) -> Result<crate::model::manifest::ManifestDocument, SessionError> {
        self.ensure_connected()?;
        Ok(self.info.query(api::info::InfoRequest {}).await?.manifest)
    }

    /// One page of the supervisor's retained log view, newest first.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the session is terminal or the query
    /// fails.
    pub async fn logs(
        &self,
        participant_id: Option<String>,
        limit: u32,
        before_sequence: Option<u64>,
    ) -> Result<api::logs::Snapshot, SessionError> {
        self.ensure_connected()?;
        Ok(self
            .logs
            .query(api::logs::SnapshotRequest {
                participant_id,
                limit,
                before_sequence,
            })
            .await?)
    }

    /// The live log stream the supervisor republishes.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the session is terminal or the
    /// subscription cannot be declared.
    pub async fn follow_logs(&self) -> Result<StreamReceiver<api::logs::Follow>, SessionError> {
        self.stream_receiver(api::topics().logs().follow().client())
            .await
    }

    /// One page of the supervisor's retained telemetry view, newest first.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the session is terminal or the query
    /// fails.
    pub async fn telemetry(
        &self,
        participant_id: Option<String>,
        limit: u32,
        before_sequence: Option<u64>,
    ) -> Result<api::telemetry::Snapshot, SessionError> {
        self.ensure_connected()?;
        Ok(self
            .telemetry
            .query(api::telemetry::SnapshotRequest {
                participant_id,
                limit,
                before_sequence,
            })
            .await?)
    }

    /// The live telemetry stream the supervisor republishes.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the session is terminal or the
    /// subscription cannot be declared.
    pub async fn follow_telemetry(
        &self,
    ) -> Result<StreamReceiver<api::telemetry::Follow>, SessionError> {
        self.stream_receiver(api::topics().telemetry().follow().client())
            .await
    }

    fn ensure_connected(&self) -> Result<(), SessionError> {
        ensure_receiver_connected(&self.terminal)
    }
}

/// Unique owner of one session and all work derived from it.
///
/// `Session` is intentionally not `Clone`. Dropping it requests close;
/// [`Session::close`] additionally waits for deterministic close evidence.
pub struct Session {
    endpoint: String,
    handle: SessionHandle,
    close: Option<oneshot::Sender<()>>,
    terminal: watch::Sender<Option<DisconnectReason>>,
    lifecycle: Option<JoinHandle<Result<(), CloseError>>>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Session")
            .field("endpoint", &self.endpoint)
            .field("execution", &self.handle.execution())
            .finish_non_exhaustive()
    }
}

impl Session {
    /// Resolve one endpoint to one execution, complete the frozen compatibility
    /// bootstrap, and establish the current typed session.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectError`] when the endpoint does not identify exactly one
    /// execution, when the peers were built from different compatibility lines,
    /// or when the transport or the initial exchange fails.
    pub async fn connect(options: &ConnectOptions) -> Result<Self, ConnectError> {
        let execution = crate::execution::resolve_execution(&options.endpoint)
            .await
            .map_err(ConnectError::from)?;
        let label = SourceLabel::new(options.label.clone())?;
        let (owner, bus) = BusOwner::open(BusConfig::for_external(
            execution,
            Some(label),
            vec![options.endpoint.clone()],
        ))
        .await?;

        let initialized = match initialize(&bus).await {
            Ok(initialized) => initialized,
            Err(error) => {
                let _ = owner.close().await;
                return Err(error);
            }
        };

        let Initialized {
            connected,
            snapshots,
            terminal,
            terminal_tx,
            identity,
            tasks,
            info,
            command,
            logs,
            telemetry,
        } = initialized;
        let handle = SessionHandle {
            connected,
            bus,
            snapshots,
            terminal: terminal.clone(),
            info,
            command,
            logs,
            telemetry,
        };
        let (close_tx, close_rx) = oneshot::channel();
        let lifecycle_bus = handle.bus.clone();
        let lifecycle = tokio::spawn(run_lifecycle(
            owner,
            lifecycle_bus,
            identity,
            tasks,
            terminal,
            terminal_tx.clone(),
            close_rx,
        ));

        Ok(Self {
            endpoint: options.endpoint.clone(),
            handle,
            close: Some(close_tx),
            terminal: terminal_tx,
            lifecycle: Some(lifecycle),
        })
    }

    /// A cloneable operations handle borrowed from this session.
    #[must_use]
    pub fn handle(&self) -> SessionHandle {
        self.handle.clone()
    }

    /// The facts established while connecting.
    #[must_use]
    pub fn connected(&self) -> &ConnectedExecution {
        self.handle.connected()
    }

    /// The execution this session is attached to.
    #[must_use]
    pub fn execution(&self) -> ExecutionId {
        self.handle.execution()
    }

    /// Close the session and wait for its private lifecycle task to flush and
    /// join the transport.
    ///
    /// # Errors
    ///
    /// Returns [`CloseError`] when the transport did not close cleanly or the
    /// lifecycle task failed before returning evidence.
    pub async fn close(mut self) -> Result<(), CloseError> {
        request_close(&self.terminal, &mut self.close);
        let Some(lifecycle) = self.lifecycle.take() else {
            return Ok(());
        };
        lifecycle.await.map_err(|error| CloseError::Lifecycle {
            detail: error.to_string(),
        })?
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        request_close(&self.terminal, &mut self.close);
    }
}

struct Initialized {
    connected: Arc<ConnectedExecution>,
    snapshots: watch::Receiver<Option<Snapshot>>,
    terminal: watch::Receiver<Option<DisconnectReason>>,
    terminal_tx: watch::Sender<Option<DisconnectReason>>,
    identity: KeyLivelinessObserver,
    tasks: JoinSet<SnapshotPumpExit>,
    info: Querier<api::info::InfoRequest>,
    command: Querier<api::command::Request>,
    logs: Querier<api::logs::SnapshotRequest>,
    telemetry: Querier<api::telemetry::SnapshotRequest>,
}

async fn initialize(bus: &BusHandle) -> Result<Initialized, ConnectError> {
    let bootstrap = crate::execution::attach_execution(bus)
        .await
        .map_err(ConnectError::from)?;
    let crate::execution::ExecutionBootstrap {
        execution,
        framework,
        info: execution_info,
        time_domain: _,
        time_domains: _,
    } = bootstrap;

    let info = Querier::new(
        bus.clone(),
        &api::topics().info().client(),
        DEFAULT_QUERY_TIMEOUT,
    )?;
    // The identity is one fact for the life of the execution, so it is asked
    // once here and carried on `ConnectedExecution`. Every screen that shows
    // the robot reads it from there rather than repeating this query.
    let robot = execution_info.manifest.into_robot().id().clone();

    let stream = StreamReceiver::new(bus, &api::topics().snapshot().client()).await?;
    let current = Querier::new(
        bus.clone(),
        &api::topics().snapshot().current().client(),
        DEFAULT_QUERY_TIMEOUT,
    )?
    .query(api::snapshot::CurrentRequest {})
    .await?
    .into_snapshot();
    current.validate()?;

    let (snapshots_tx, snapshots) = watch::channel(Some(current.clone()));
    let mut tasks = JoinSet::new();
    tasks.spawn(pump_snapshots(stream, snapshots_tx, current.revision));

    let (terminal_tx, terminal) = watch::channel(None);
    let on_change = terminal_tx.clone();
    let identity = bus
        .observe_liveliness_key(PRESENCE_KEY, move |status| {
            if status == LivelinessStatus::Lost {
                latch_terminal(&on_change, DisconnectReason::SupervisorIdentityLost);
            }
        })
        .await?;
    if identity.initial() == LivelinessStatus::Lost {
        return Err(ConnectError::SupervisorUnavailable);
    }

    Ok(Initialized {
        connected: Arc::new(ConnectedExecution {
            execution,
            robot,
            framework,
        }),
        snapshots,
        terminal,
        terminal_tx,
        identity,
        tasks,
        info,
        command: Querier::new(
            bus.clone(),
            &api::topics().command().client(),
            DEFAULT_QUERY_TIMEOUT,
        )?,
        logs: Querier::new(
            bus.clone(),
            &api::topics().logs().snapshot().client(),
            DEFAULT_QUERY_TIMEOUT,
        )?,
        telemetry: Querier::new(
            bus.clone(),
            &api::topics().telemetry().snapshot().client(),
            DEFAULT_QUERY_TIMEOUT,
        )?,
    })
}

async fn run_lifecycle(
    owner: BusOwner,
    bus: BusHandle,
    identity: KeyLivelinessObserver,
    mut tasks: JoinSet<SnapshotPumpExit>,
    mut terminal: watch::Receiver<Option<DisconnectReason>>,
    terminal_tx: watch::Sender<Option<DisconnectReason>>,
    close: oneshot::Receiver<()>,
) -> Result<(), CloseError> {
    let reason = wait_for_shutdown(&mut terminal, close, &mut tasks, bus.wait_for_fatal()).await;
    latch_terminal(&terminal_tx, reason);
    tasks.abort_all();
    drop(identity);
    let report = owner.close().await;
    tasks.shutdown().await;
    if report.is_clean() {
        Ok(())
    } else {
        Err(CloseError::Transport {
            detail: report.to_string(),
        })
    }
}

fn request_close(
    terminal: &watch::Sender<Option<DisconnectReason>>,
    close: &mut Option<oneshot::Sender<()>>,
) {
    latch_terminal(terminal, DisconnectReason::SessionClosed);
    if let Some(close) = close.take() {
        let _ = close.send(());
    }
}

fn latch_terminal(terminal: &watch::Sender<Option<DisconnectReason>>, reason: DisconnectReason) {
    terminal.send_if_modified(|current| {
        if current.is_some() {
            false
        } else {
            *current = Some(reason);
            true
        }
    });
}

fn terminal_reason(
    terminal: &watch::Receiver<Option<DisconnectReason>>,
) -> Option<DisconnectReason> {
    terminal.borrow().clone().or_else(|| {
        terminal
            .has_changed()
            .is_err()
            .then_some(DisconnectReason::LifecycleEnded)
    })
}

fn ensure_receiver_connected(
    terminal: &watch::Receiver<Option<DisconnectReason>>,
) -> Result<(), SessionError> {
    match terminal_reason(terminal) {
        Some(reason) => Err(SessionError::Disconnected { reason }),
        None => Ok(()),
    }
}

async fn wait_for_shutdown<F>(
    terminal: &mut watch::Receiver<Option<DisconnectReason>>,
    close: oneshot::Receiver<()>,
    tasks: &mut JoinSet<SnapshotPumpExit>,
    fatal: F,
) -> DisconnectReason
where
    F: Future<Output = BusFault>,
{
    if let Some(reason) = terminal_reason(terminal) {
        return reason;
    }
    tokio::pin!(fatal);
    tokio::select! {
        biased;
        reason = wait_for_terminal(terminal) => reason,
        fault = &mut fatal => DisconnectReason::TransportFault { fault },
        result = tasks.join_next() => snapshot_task_reason(result),
        _ = close => DisconnectReason::SessionClosed,
    }
}

async fn wait_for_terminal(
    terminal: &mut watch::Receiver<Option<DisconnectReason>>,
) -> DisconnectReason {
    loop {
        if let Some(reason) = terminal_reason(terminal) {
            return reason;
        }
        if terminal.changed().await.is_err() {
            return DisconnectReason::LifecycleEnded;
        }
    }
}

fn snapshot_task_reason(
    result: Option<Result<SnapshotPumpExit, tokio::task::JoinError>>,
) -> DisconnectReason {
    match result {
        Some(Ok(SnapshotPumpExit::StreamFailed { detail })) => {
            DisconnectReason::SnapshotStreamFailed { detail }
        }
        Some(Ok(SnapshotPumpExit::ObserversDropped)) => DisconnectReason::SessionClosed,
        Some(Err(error)) => DisconnectReason::SnapshotStreamFailed {
            detail: format!("snapshot pump task failed: {error}"),
        },
        None => DisconnectReason::SnapshotStreamFailed {
            detail: "snapshot pump task was absent".to_string(),
        },
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SnapshotPumpDecision {
    Continue,
    ObserversDropped,
}

fn pump_snapshot_step(
    sender: &watch::Sender<Option<Snapshot>>,
    revision: &mut u64,
    snapshot: Snapshot,
) -> SnapshotPumpDecision {
    if snapshot.revision <= *revision {
        return SnapshotPumpDecision::Continue;
    }
    let installed_revision = snapshot.revision;
    if sender.send(Some(snapshot)).is_err() {
        return SnapshotPumpDecision::ObserversDropped;
    }
    *revision = installed_revision;
    SnapshotPumpDecision::Continue
}

#[derive(Debug, Eq, PartialEq)]
enum SnapshotPumpExit {
    StreamFailed { detail: String },
    ObserversDropped,
}

async fn pump_snapshots(
    stream: StreamReceiver<api::snapshot::Update>,
    sender: watch::Sender<Option<Snapshot>>,
    mut revision: u64,
) -> SnapshotPumpExit {
    loop {
        let observed = match stream.recv().await {
            Ok(observed) => observed,
            Err(error) => {
                return SnapshotPumpExit::StreamFailed {
                    detail: error.to_string(),
                };
            }
        };
        let SnapshotDocument::V0(snapshot) = observed.body;
        if pump_snapshot_step(&sender, &mut revision, snapshot)
            == SnapshotPumpDecision::ObserversDropped
        {
            return SnapshotPumpExit::ObserversDropped;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::api::execution::Lifecycle;

    fn snapshot(revision: u64, lifecycle: Lifecycle) -> Snapshot {
        Snapshot {
            revision,
            lifecycle,
            processes: Vec::new(),
        }
    }

    #[test]
    fn framework_selection_accepts_both_directions_within_one_line() {
        for (older, newer) in [
            (
                FrameworkVersion::new(0, 60, 1),
                FrameworkVersion::new(0, 60, 9),
            ),
            (
                FrameworkVersion::new(1, 2, 1),
                FrameworkVersion::new(1, 9, 9),
            ),
        ] {
            assert!(crate::execution::ensure_compatible_framework(older, newer).is_ok());
            assert!(crate::execution::ensure_compatible_framework(newer, older).is_ok());
        }
    }

    #[test]
    fn snapshot_pump_installs_only_strictly_newer_revisions() {
        let (sender, receiver) = watch::channel(Some(snapshot(7, Lifecycle::Starting)));
        let mut revision = 7;

        assert_eq!(
            pump_snapshot_step(&sender, &mut revision, snapshot(8, Lifecycle::Ready)),
            SnapshotPumpDecision::Continue
        );
        for observed in [8, 6] {
            assert_eq!(
                pump_snapshot_step(
                    &sender,
                    &mut revision,
                    snapshot(observed, Lifecycle::Starting),
                ),
                SnapshotPumpDecision::Continue
            );
        }
        assert_eq!(revision, 8);
        assert_eq!(
            receiver.borrow().as_ref().map(|value| value.revision),
            Some(8)
        );
    }

    #[tokio::test]
    async fn explicit_close_is_latched_before_the_lifecycle_is_woken() {
        let (terminal_tx, mut terminal) = watch::channel(None);
        let (close_tx, close_rx) = oneshot::channel();
        let mut tasks = JoinSet::new();
        tasks.spawn(std::future::pending::<SnapshotPumpExit>());
        let mut close = Some(close_tx);
        request_close(&terminal_tx, &mut close);

        assert_eq!(
            terminal_reason(&terminal),
            Some(DisconnectReason::SessionClosed)
        );

        assert_eq!(
            wait_for_shutdown(&mut terminal, close_rx, &mut tasks, std::future::pending(),).await,
            DisconnectReason::SessionClosed
        );
    }

    #[tokio::test]
    async fn identity_loss_is_latched_and_not_overwritten_by_close() {
        let (terminal_tx, mut terminal) = watch::channel(None);
        let (_close_tx, close_rx) = oneshot::channel();
        let mut tasks = JoinSet::new();
        tasks.spawn(std::future::pending::<SnapshotPumpExit>());
        latch_terminal(&terminal_tx, DisconnectReason::SupervisorIdentityLost);
        latch_terminal(&terminal_tx, DisconnectReason::SessionClosed);

        assert_eq!(
            wait_for_shutdown(&mut terminal, close_rx, &mut tasks, std::future::pending(),).await,
            DisconnectReason::SupervisorIdentityLost
        );
    }

    #[tokio::test]
    async fn snapshot_stream_failure_preserves_its_detail() {
        let (_terminal_tx, mut terminal) = watch::channel(None);
        let (_close_tx, close_rx) = oneshot::channel();
        let mut tasks = JoinSet::new();
        tasks.spawn(async {
            SnapshotPumpExit::StreamFailed {
                detail: "subscriber closed".to_string(),
            }
        });

        assert_eq!(
            wait_for_shutdown(&mut terminal, close_rx, &mut tasks, std::future::pending(),).await,
            DisconnectReason::SnapshotStreamFailed {
                detail: "subscriber closed".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn transport_fault_is_a_terminal_lifecycle_reason() {
        let (_terminal_tx, mut terminal) = watch::channel(None);
        let (_close_tx, close_rx) = oneshot::channel();
        let mut tasks = JoinSet::new();
        tasks.spawn(std::future::pending::<SnapshotPumpExit>());
        let fault = BusFault::WorkerExited {
            worker: "outbound-drain".to_string(),
        };

        assert_eq!(
            wait_for_shutdown(&mut terminal, close_rx, &mut tasks, {
                let fault = fault.clone();
                async move { fault }
            })
            .await,
            DisconnectReason::TransportFault { fault }
        );
    }

    #[test]
    fn terminal_state_preempts_a_snapshot_observer() {
        let (terminal_tx, terminal) = watch::channel(None);
        latch_terminal(&terminal_tx, DisconnectReason::SupervisorIdentityLost);

        assert!(matches!(
            ensure_receiver_connected(&terminal),
            Err(SessionError::Disconnected {
                reason: DisconnectReason::SupervisorIdentityLost,
            })
        ));
    }
}
