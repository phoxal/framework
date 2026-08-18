//! The Zenoh session wrapper: the execution-scoped key root, the non-blocking
//! outbound queue, and health counters.
//!
//! This module also owns the two Zenoh <-> Phoxal identity conversions, because
//! the session is where that equivalence is realized: an execution pins the
//! run's router session, and a producer is read back from the session that
//! publishes. Both cross the *value*, never the storage bytes - `uhlc::ID` keeps
//! its bytes little-endian, so hexing the array would produce a byte-reversed
//! string that no longer matches Zenoh's own rendering of the same identity.
//!
//! They are free functions rather than `From` impls or inherent methods because
//! `ZenohId` is foreign to this crate and the Phoxal identities are foreign to
//! Zenoh's, so neither side can carry the conversion.

use std::future::{Future, IntoFuture};
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use crate::identity::{ExecutionId, ParticipantId, ProducerId};
use tokio::sync::{Notify, watch};
use tokio::task::{AbortHandle, JoinHandle};
use zenoh::bytes::{Encoding, ZBytes};
use zenoh::config::ZenohId;
use zenoh::key_expr::OwnedKeyExpr;
use zenoh::qos::CongestionControl;

use crate::bus::abi::truncate_utf8;
use crate::bus::contract::DeliveryFamily;
use crate::bus::error::{BusError, KeyProblem, OutboundBound, Result, SessionIdRole};
use crate::bus::lock::lock;
use crate::bus::metadata::{
    BusMetadata, MAX_SOURCE_LABEL_BYTES, SourceAttribution, SourceLabel, StreamPosition,
};
use crate::bus::outbound::{Outbound, OutboundScheduler};
use crate::bus::runtime_metrics::{RuntimeMetricHandle, RuntimeMetricSnapshot, RuntimeMetrics};
use crate::bus::time::TimeWindow;

/// First chunk of every Phoxal bus key. It exists so a Phoxal execution is
/// recognisable in a trace and cannot collide with a non-Phoxal key tree
/// sharing the same Zenoh fabric.
///
/// This spelling belongs to the frozen bootstrap-reachable subset: a client
/// composes it before it can address the attachment bootstrap at all, so it is
/// preserved across framework majors. Moving it is a bootstrap-breaking event -
/// see `xtask/README.md` "When a gate fails", rule 3 "A frozen bootstrap fact
/// drifted".
pub(crate) const BUS_KEY_PREFIX: &str = "phoxal";

/// The Zenoh wire protocol version the linked transport speaks.
///
/// It is the deepest fact the attachment bootstrap stands on: two peers that
/// disagree here never establish a session, so no Phoxal key, encoding or
/// document is ever exchanged and the frozen bootstrap cannot report the
/// disagreement. It is therefore part of the frozen bootstrap-reachable subset,
/// and a Zenoh upgrade that moves it is a bootstrap-breaking event needing
/// deliberate design rather than a routine dependency bump - see
/// `xtask/README.md` "When a gate fails", rule 3 "A frozen bootstrap fact
/// drifted".
///
/// The value is stated here rather than read from Zenoh: `zenoh-protocol` is an
/// internal crate of the transport and states its own constant for its own use.
/// `tests/frozen_wire_protocol.rs` holds the two together, so this declaration
/// cannot go stale behind a Zenoh upgrade.
pub(crate) const ZENOH_WIRE_PROTOCOL_VERSION: u8 = 9;

/// Capacity (in samples) of each ordered outbound lane. Coalesced state and
/// setpoint lanes retain one pending slot per concrete topic instead.
pub(crate) const OUTBOUND_CAPACITY: usize = 1024;

/// Byte bound of the outbound queue. The queue is bounded in samples AND bytes,
/// because either alone lets a conforming publisher exhaust the other. A publish
/// A sample/stream admission that would exceed it is refused or, for samples,
/// evicts older sample values until the newest item fits; no caller blocks.
pub(crate) const OUTBOUND_MAX_BYTES: usize = 16 * 1024 * 1024;

/// Connection inputs for opening a bus session.
///
/// The execution is the *only* routing input. `RobotId` is model data and never
/// contributes to a key: two executions never share a key, even when they run
/// the same logical robot.
#[derive(Clone, Debug)]
pub(crate) struct BusConfig {
    /// The supervised run this session joins. It is the key root, so traffic
    /// from a previous execution - an ad hoc publisher, an attached tool, a
    /// replayed recording, a second checkout of the same project - physically
    /// cannot be observed as current.
    execution: ExecutionId,
    /// The compiled participant identity, when this is a participant session.
    /// External/operator sessions leave this absent and may provide only a
    /// diagnostic label.
    participant: Option<ParticipantId>,
    /// A bounded diagnostic label for an external producer. It never affects
    /// routing, authority, or Ready admission.
    diagnostic_label: Option<SourceLabel>,
    /// Zenoh connect endpoints. Empty = in-process (local sim / tests).
    connect_endpoints: Vec<String>,
}

#[allow(
    dead_code,
    reason = "compiled in every profile because a domain module never asks which profile it is in; its only consumer is a module one profile declares"
)]
impl BusConfig {
    /// Build a participant bus configuration.
    pub fn for_participant(
        execution: ExecutionId,
        participant: ParticipantId,
        connect_endpoints: Vec<String>,
    ) -> Self {
        BusConfig {
            execution,
            participant: Some(participant),
            diagnostic_label: None,
            connect_endpoints,
        }
    }

    /// Build an external bus configuration with an optional diagnostic label.
    pub fn for_external(
        execution: ExecutionId,
        label: Option<SourceLabel>,
        connect_endpoints: Vec<String>,
    ) -> Self {
        BusConfig {
            execution,
            participant: None,
            diagnostic_label: label,
            connect_endpoints,
        }
    }

    /// The execution scope this session joins.
    #[must_use]
    pub fn execution(&self) -> ExecutionId {
        self.execution
    }

    pub(crate) fn attribution(&self, producer: ProducerId) -> SourceAttribution {
        match (&self.participant, &self.diagnostic_label) {
            (Some(participant), _) => SourceAttribution::Participant(
                crate::bus::metadata::ParticipantSourceIdentity::new(participant.clone(), producer),
            ),
            (None, label) => SourceAttribution::External {
                producer,
                label: label.clone(),
            },
        }
    }
}

/// Live health counters for one session.
#[derive(Debug, Default)]
pub struct BusHealth {
    /// Samples or ordered chunks refused/evicted because an outbound lane was
    /// bounded. Coalesced state/setpoint replacement is not a drop.
    pub outbound_drops: AtomicU64,
    /// Asynchronous Zenoh publication failures observed by the drain task.
    /// These are live evidence; the bounded close report retains the detailed
    /// text for terminal diagnostics.
    pub transport_failures: AtomicU64,
    /// Inbound samples dropped because the ring was full (slow consumer).
    pub inbound_drops: AtomicU64,
    /// Inbound samples that failed to decode. Contract identity lives in the
    /// Zenoh key, so a receiver's per-key subscription is the whole
    /// fast-reject and a decode failure is the only remaining rejection.
    pub decode_errors: AtomicU64,
}

/// Why an owner-owned transport worker made the bus unusable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BusFault {
    /// A concrete subscription could no longer receive from Zenoh.
    SubscriptionReceive { topic: String, error: String },
    /// An owner-owned worker returned without an expected cancellation.
    WorkerExited { worker: String },
    /// An owner-owned worker panicked or was otherwise cancelled unexpectedly.
    WorkerJoin { worker: String, error: String },
}

impl std::fmt::Display for BusFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SubscriptionReceive { topic, error } => {
                write!(formatter, "subscription '{topic}' failed: {error}")
            }
            Self::WorkerExited { worker } => {
                write!(formatter, "bus worker '{worker}' exited unexpectedly")
            }
            Self::WorkerJoin { worker, error } => {
                write!(
                    formatter,
                    "bus worker '{worker}' terminated unexpectedly: {error}"
                )
            }
        }
    }
}

impl std::error::Error for BusFault {}

/// The owner-shared transport terminal state. Handles may observe it, but only
/// the unique [`BusOwner`] changes normal lifecycle state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BusTerminal {
    Open,
    Closing,
    Closed,
    Fatal(BusFault),
}

struct BusInner {
    session: zenoh::Session,
    identity: Arc<BusIdentity>,
    outbound: std::sync::Mutex<OutboundScheduler>,
    outbound_notify: Notify,
    admission: std::sync::Mutex<()>,
    closing: AtomicBool,
    shutdown: Notify,
    #[cfg(test)]
    drain_paused: AtomicBool,
    #[cfg(test)]
    drain_pause_ack: Notify,
    #[cfg(test)]
    drain_resume: Notify,
    drain: std::sync::Mutex<Option<SupervisedWorker>>,
    workers: Arc<BusWorkerGroup>,
    transport_errors: std::sync::Mutex<TransportErrors>,
    terminal: watch::Sender<BusTerminal>,
    worker_failures: std::sync::Mutex<Vec<BusFault>>,
    in_flight: AtomicUsize,
    in_flight_notify: Notify,
}

struct BusIdentity {
    root: String,
    execution: ExecutionId,
    attribution: SourceAttribution,
    producer: ProducerId,
    seq: AtomicU64,
    health: BusHealth,
    runtime_metrics: Arc<RuntimeMetrics>,
}

#[derive(Default)]
struct TransportErrors {
    entries: Vec<String>,
    count: usize,
    truncated: usize,
}

/// The unique lifetime owner for one execution-scoped Zenoh session.
///
/// `BusOwner` is deliberately not `Clone`: it is the only value that can
/// initiate terminal close, owns the outbound and subscription workers, and
/// contains the producer identity pinned to this session incarnation.
///
/// Crate-private. Owning the transport is what `phoxal::session`,
/// `phoxal::simulator`, `phoxal::supervisor::host` and the participant runner
/// each do on their consumer's behalf; no consumer profile receives it.
pub(crate) struct BusOwner {
    inner: Arc<BusInner>,
    liveness: Arc<AtomicBool>,
}

/// A cloneable use handle for one [`BusOwner`]. Handles share the owner's
/// producer and sequence allocator but cannot close the transport or declare a
/// participant Ready lease.
#[derive(Clone)]
pub struct BusHandle {
    identity: Arc<BusIdentity>,
    owner: Weak<BusInner>,
    liveness: Weak<AtomicBool>,
    terminal: watch::Receiver<BusTerminal>,
}

struct RegisteredWorker {
    name: String,
    expected: Arc<AtomicBool>,
    handle: JoinHandle<()>,
    raw_abort: AbortHandle,
}

struct SupervisedWorker {
    monitor: JoinHandle<()>,
    raw_abort: AbortHandle,
}

/// Private owner-side worker reaper. It deliberately is not a general task
/// framework: these are only transport tasks whose lifetime is the bus owner.
struct BusWorkerGroup {
    workers: std::sync::Mutex<Vec<RegisteredWorker>>,
    changed: Notify,
    closing: AtomicBool,
    reaper: std::sync::Mutex<Option<SupervisedWorker>>,
}

impl BusWorkerGroup {
    fn new() -> Self {
        Self {
            workers: std::sync::Mutex::new(Vec::new()),
            changed: Notify::new(),
            closing: AtomicBool::new(false),
            reaper: std::sync::Mutex::new(None),
        }
    }

    fn register(self: &Arc<Self>, name: String, expected: Arc<AtomicBool>, worker: JoinHandle<()>) {
        let completion = Arc::clone(self);
        let raw_abort = worker.abort_handle();
        let handle = tokio::spawn(async move {
            match worker.await {
                Ok(()) => completion.changed.notify_one(),
                Err(error) if error.is_panic() => {
                    completion.changed.notify_one();
                    std::panic::resume_unwind(error.into_panic());
                }
                Err(error) => {
                    completion.changed.notify_one();
                    panic!("bus worker cancelled unexpectedly: {error}");
                }
            }
        });
        lock(&self.workers).push(RegisteredWorker {
            name,
            expected,
            handle,
            raw_abort,
        });
        self.changed.notify_one();
    }

    fn take_finished(&self) -> Vec<RegisteredWorker> {
        let mut workers = lock(&self.workers);
        let mut finished = Vec::new();
        let mut index = 0;
        while index < workers.len() {
            if workers[index].handle.is_finished() {
                finished.push(workers.swap_remove(index));
            } else {
                index += 1;
            }
        }
        finished
    }

    fn begin_close(&self) {
        self.closing.store(true, Ordering::Release);
        self.changed.notify_waiters();
        self.changed.notify_one();
    }

    fn take_remaining(&self) -> Vec<RegisteredWorker> {
        std::mem::take(&mut *lock(&self.workers))
    }
}

pub(crate) struct SessionLease {
    session: zenoh::Session,
    _operation: OperationLease,
}

pub(crate) struct RuntimeMetricsLease {
    metrics: Arc<RuntimeMetrics>,
    _operation: OperationLease,
}

impl Deref for RuntimeMetricsLease {
    type Target = RuntimeMetrics;

    fn deref(&self) -> &Self::Target {
        &self.metrics
    }
}

impl Deref for SessionLease {
    type Target = zenoh::Session;

    fn deref(&self) -> &Self::Target {
        &self.session
    }
}

struct OperationLease {
    inner: Arc<BusInner>,
}

impl Drop for OperationLease {
    fn drop(&mut self) {
        self.inner.in_flight.fetch_sub(1, Ordering::AcqRel);
        self.inner.in_flight_notify.notify_waiters();
    }
}

impl std::fmt::Debug for BusOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BusOwner")
            .field("root", &self.inner.identity.root)
            .field("producer", &self.inner.identity.producer)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for BusHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BusHandle")
            .field("root", &self.identity.root)
            .field("producer", &self.identity.producer)
            .finish_non_exhaustive()
    }
}

impl Drop for BusOwner {
    fn drop(&mut self) {
        // Drop is the synchronous safety net. No cloneable handle owns the
        // inner Arc, so aborting every owned task here releases the Zenoh
        // session as soon as the unique owner goes away.
        self.liveness.store(false, Ordering::Release);
        let _admission = lock(&self.inner.admission);
        self.inner.closing.store(true, Ordering::Release);
        drop(_admission);
        begin_terminal_close(&self.inner);
        self.inner.shutdown.notify_waiters();
        self.inner.shutdown.notify_one();
        if let Some(drain) = lock(&self.inner.drain).take() {
            drain.raw_abort.abort();
            drain.monitor.abort();
        }
        self.inner.workers.begin_close();
        if let Some(reaper) = lock(&self.inner.workers.reaper).take() {
            reaper.raw_abort.abort();
            reaper.monitor.abort();
        }
        for worker in self.inner.workers.take_remaining() {
            worker.handle.abort();
            worker.raw_abort.abort();
        }
    }
}

impl BusOwner {
    /// Open a session, compose its execution-scoped key root, pin the
    /// bus-owned producer identity into Zenoh, and start the outbound drain
    /// task.
    pub async fn open(config: BusConfig) -> Result<(Self, BusHandle)> {
        if let Some(source) = config
            .participant
            .as_ref()
            .map(ParticipantId::as_str)
            .or_else(|| config.diagnostic_label.as_ref().map(SourceLabel::as_str))
            && source.len() > MAX_SOURCE_LABEL_BYTES
        {
            return Err(BusError::invalid_key(
                source,
                KeyProblem::TooLong {
                    limit: MAX_SOURCE_LABEL_BYTES,
                },
            ));
        }

        // Execution scoping lives in the *root*, not in any contract name: a
        // previous run's traffic lands on a different key and cannot be
        // observed as current.
        let root = format!("{BUS_KEY_PREFIX}/{}", config.execution);
        // Validate the composed root resolves to a legal Zenoh key.
        OwnedKeyExpr::new(root.clone()).map_err(|e| BusError::not_a_key_expression(&root, e))?;

        let producer = mint_producer_id()?;
        let session = zenoh::open(zenoh_config(&config.connect_endpoints, producer)?)
            .await
            .map_err(|e| BusError::Transport(e.to_string()))?;
        let observed = match producer_from_zid(session.zid()) {
            Ok(observed) => observed,
            Err(error) => {
                let _ = session.close().await;
                return Err(error);
            }
        };
        if observed != producer {
            let _ = session.close().await;
            return Err(BusError::SessionIdentityMismatch {
                expected: producer,
                observed,
            });
        }

        let drain_session = session.clone();
        let workers = Arc::new(BusWorkerGroup::new());
        let (terminal, terminal_rx) = watch::channel(BusTerminal::Open);
        let inner = Arc::new(BusInner {
            session,
            identity: Arc::new(BusIdentity {
                root,
                execution: config.execution,
                attribution: config.attribution(producer),
                producer,
                seq: AtomicU64::new(0),
                health: BusHealth::default(),
                runtime_metrics: Arc::new(RuntimeMetrics::default()),
            }),
            outbound: std::sync::Mutex::new(OutboundScheduler::new(
                OUTBOUND_CAPACITY,
                OUTBOUND_MAX_BYTES,
            )),
            outbound_notify: Notify::new(),
            admission: std::sync::Mutex::new(()),
            closing: AtomicBool::new(false),
            shutdown: Notify::new(),
            #[cfg(test)]
            drain_paused: AtomicBool::new(false),
            #[cfg(test)]
            drain_pause_ack: Notify::new(),
            #[cfg(test)]
            drain_resume: Notify::new(),
            drain: std::sync::Mutex::new(None),
            workers: Arc::clone(&workers),
            transport_errors: std::sync::Mutex::new(TransportErrors::default()),
            terminal,
            worker_failures: std::sync::Mutex::new(Vec::new()),
            in_flight: AtomicUsize::new(0),
            in_flight_notify: Notify::new(),
        });

        let drain = spawn_supervised_worker(
            "outbound-drain",
            drain_loop(drain_session, Arc::downgrade(&inner)),
            Arc::downgrade(&inner),
        );
        *lock(&inner.drain) = Some(drain);
        let reaper = spawn_supervised_worker(
            "bus-worker-reaper",
            worker_reaper(Arc::clone(&workers), Arc::downgrade(&inner)),
            Arc::downgrade(&inner),
        );
        *lock(&workers.reaper) = Some(reaper);

        let liveness = Arc::new(AtomicBool::new(true));
        let owner = BusOwner {
            inner: Arc::clone(&inner),
            liveness: Arc::clone(&liveness),
        };
        let handle = BusHandle {
            identity: Arc::clone(&inner.identity),
            owner: Arc::downgrade(&inner),
            liveness: Arc::downgrade(&liveness),
            terminal: terminal_rx,
        };
        Ok((owner, handle))
    }

    /// The executions whose routers a session at `endpoint` is *directly*
    /// connected to.
    ///
    /// This opens and closes its own short-lived session rather than taking a
    /// `&self`, because it answers the question that has to be settled *before*
    /// a [`BusOwner`] can exist: a bus is execution-scoped, and the execution is
    /// what this reports. The session is independent of any other in the
    /// process - opening and closing it disturbs nothing.
    ///
    /// It shares the Phoxal transport policy, so multicast scouting is off and
    /// only routers reachable through `endpoint` are ever reported. Connect
    /// retry is deliberately *not* shared: the answer is "what is connected
    /// now", so an endpoint with nothing behind it fails immediately instead of
    /// spending the shared connect-retry budget hoping a router appears.
    ///
    /// Cardinality is the caller's rule. This reports what is connected -
    /// none, one, or several - and errors only when a connected session id is
    /// not a Phoxal execution at all.
    ///
    /// Discovery belongs to the frozen bootstrap-reachable subset: it is the
    /// first step of an attachment, and it reads the transport's own
    /// directly-connected router set rather than any Phoxal key, so an
    /// attaching client learns an execution before it can address one. The
    /// mechanics that carry it - a client-mode session on the given endpoint
    /// with multicast scouting off, and a router session id that *is* the
    /// execution - are preserved across framework majors.
    pub async fn probe_routers(endpoint: &str) -> Result<Vec<ExecutionId>> {
        let session = zenoh::open(client_config(endpoint)?)
            .await
            .map_err(|error| BusError::Transport(error.to_string()))?;
        let zids: Vec<_> = session.info().routers_zid().await.collect();
        session
            .close()
            .await
            .map_err(|error| BusError::Transport(error.to_string()))?;
        zids.into_iter().map(execution_from_zid).collect()
    }
}

impl BusHandle {
    fn live_inner(&self) -> Result<Arc<BusInner>> {
        let liveness = self.liveness.upgrade().ok_or(BusError::Closed)?;
        if !liveness.load(Ordering::Acquire) {
            return Err(BusError::Closed);
        }
        let inner = self.owner.upgrade().ok_or(BusError::Closed)?;
        if inner.closing.load(Ordering::Acquire) {
            return Err(BusError::Closed);
        }
        Ok(inner)
    }

    fn admit_operation(&self) -> Result<OperationLease> {
        let inner = self.live_inner()?;
        let _admission = lock(&inner.admission);
        if inner.closing.load(Ordering::Acquire) {
            return Err(BusError::Closed);
        }
        inner.in_flight.fetch_add(1, Ordering::AcqRel);
        drop(_admission);
        Ok(OperationLease { inner })
    }

    /// The composed key root (`phoxal/<execution-id>`).
    pub fn root(&self) -> &str {
        &self.identity.root
    }

    /// The supervised run this session belongs to.
    pub fn execution(&self) -> ExecutionId {
        self.identity.execution
    }

    /// The compiled participant attribution, if this session belongs to one.
    pub fn participant(&self) -> Option<&ParticipantId> {
        self.identity.attribution.participant()
    }

    /// The complete source attribution for this session.
    pub fn attribution(&self) -> &SourceAttribution {
        &self.identity.attribution
    }

    /// This session's producer identity - the id minted by the bus owner and
    /// verified against the opened Zenoh session.
    ///
    /// The guarantee is per *session incarnation*, not per process: a process
    /// that closes its bus and opens another is a different producer, which is
    /// exactly the intended reading, because the second session's sequence
    /// starts at zero again.
    pub fn producer(&self) -> ProducerId {
        self.identity.producer
    }

    /// Build the provenance for one outbound sample: this producer, its next
    /// sequence, and the production instant the caller's temporal role permits.
    pub(crate) fn metadata(&self, produced_at: Option<TimeWindow>) -> Result<BusMetadata> {
        self.live_inner()?;
        Ok(BusMetadata {
            codec: crate::bus::abi::CodecId::MessagePack.as_u8(),
            sequence: self.next_sequence()?,
            stream_position: None,
            produced_at,
            source: self.identity.attribution.clone(),
        })
    }

    /// Live health counters.
    pub fn health(&self) -> &BusHealth {
        &self.identity.health
    }

    /// Drain this session's queue-pressure counters for one rollup window.
    ///
    /// Interval counters reset; declared rows and depth gauges persist. See
    /// [`crate::bus::runtime_metrics`] for what a row means and what it does not.
    pub fn take_runtime_metrics(&self) -> Result<Vec<RuntimeMetricSnapshot>> {
        let operation = self.admit_operation()?;
        let inner = Arc::clone(&operation.inner);
        let snapshots = inner.identity.runtime_metrics.take();
        drop(operation);
        Ok(snapshots)
    }

    pub(crate) fn runtime_metrics(&self) -> Result<RuntimeMetricsLease> {
        let operation = self.admit_operation()?;
        Ok(RuntimeMetricsLease {
            metrics: Arc::clone(&self.identity.runtime_metrics),
            _operation: operation,
        })
    }

    #[cfg(test)]
    pub(crate) fn register_worker(
        &self,
        worker: JoinHandle<()>,
    ) -> std::result::Result<(), JoinHandle<()>> {
        self.register_named_worker("bus-worker", Arc::new(AtomicBool::new(false)), worker)
    }

    pub(crate) fn register_named_worker(
        &self,
        name: impl Into<String>,
        expected: Arc<AtomicBool>,
        worker: JoinHandle<()>,
    ) -> std::result::Result<(), JoinHandle<()>> {
        // Worker registration participates in the same admission gate as
        // publishing and close. If close won first, the caller gets the join
        // handle back and must await it; no task is detached merely because
        // setup raced terminal shutdown.
        let inner = match self.live_inner() {
            Ok(inner) => inner,
            Err(_) => return Err(worker),
        };
        let _admission = lock(&inner.admission);
        if inner.closing.load(Ordering::Acquire) {
            return Err(worker);
        }
        inner.workers.register(name.into(), expected, worker);
        Ok(())
    }

    /// Test-only failure injection for proving that the participant lifecycle
    /// observes an actual owner-owned outbound worker termination.
    #[cfg(any(test, feature = "test-harness"))]
    #[doc(hidden)]
    pub fn __test_abort_outbound_drain(&self) -> Result<()> {
        let inner = self.live_inner()?;
        let drain = lock(&inner.drain);
        let drain = drain.as_ref().ok_or(BusError::Closed)?;
        drain.raw_abort.abort();
        Ok(())
    }

    /// Test-only failure injection for proving that the owner-side worker
    /// reaper is itself supervised as mandatory transport infrastructure.
    #[cfg(any(test, feature = "test-harness"))]
    #[doc(hidden)]
    pub fn __test_abort_worker_reaper(&self) -> Result<()> {
        let inner = self.live_inner()?;
        let reaper = lock(&inner.workers.reaper);
        let reaper = reaper.as_ref().ok_or(BusError::Closed)?;
        reaper.raw_abort.abort();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) async fn test_pause_outbound_drain(&self) -> Result<TestDrainPause> {
        let inner = self.live_inner()?;
        inner.drain_paused.store(true, Ordering::Release);
        inner.outbound_notify.notify_one();
        inner.drain_pause_ack.notified().await;
        Ok(TestDrainPause {
            inner: Arc::downgrade(&inner),
        })
    }

    #[cfg(test)]
    pub(crate) fn test_queued_stream_metadata(&self, key: &str) -> Vec<BusMetadata> {
        self.owner
            .upgrade()
            .map(|inner| {
                lock(&inner.outbound)
                    .stream_attachments(key)
                    .into_iter()
                    .map(|attachment| {
                        BusMetadata::decode(&attachment).expect("queued metadata must decode")
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The current owner-shared terminal state.
    pub fn terminal(&self) -> BusTerminal {
        self.terminal.borrow().clone()
    }

    /// Wait for an unexpected owner-owned transport worker failure.
    ///
    /// Normal close is intentionally not returned: this is the lifecycle
    /// runner's critical-fault signal, not a second close capability.
    pub async fn wait_for_fatal(&self) -> BusFault {
        let mut terminal = self.terminal.clone();
        loop {
            if let BusTerminal::Fatal(fault) = terminal.borrow().clone() {
                return fault;
            }
            if terminal.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }

    pub(crate) fn signal_fatal(&self, fault: BusFault) {
        if let Some(inner) = self.owner.upgrade() {
            signal_fatal(&inner, fault);
        }
    }

    /// Wait until the owner has closed admission. The double check around the
    /// notification registration makes this reliable even when close wins
    /// before a worker reaches its first poll.
    pub(crate) async fn wait_for_shutdown(&self) {
        loop {
            if self.owner.upgrade().is_none() {
                return;
            }
            if let Some(inner) = self.owner.upgrade()
                && inner.closing.load(Ordering::Acquire)
            {
                return;
            }
            let inner = match self.owner.upgrade() {
                Some(inner) => inner,
                None => return,
            };
            let notified = inner.shutdown.notified();
            if inner.closing.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }

    /// The underlying Zenoh session (for subscriber/queryable declaration).
    pub(crate) fn session(&self) -> Result<SessionLease> {
        let operation = self.admit_operation()?;
        Ok(SessionLease {
            session: operation.inner.session.clone(),
            _operation: operation,
        })
    }

    /// Compose a full bus key from a family-rooted topic key.
    ///
    /// The composition `phoxal/{execution}/{topic}` belongs to the frozen
    /// bootstrap-reachable subset: it is the grammar an attaching client spells
    /// to reach the attachment bootstrap, so it is preserved across framework
    /// majors. There is no version segment; compatibility is the framework
    /// train both peers were built from, and the bootstrap is what establishes
    /// it.
    pub fn full_key(&self, topic_key: &str) -> String {
        format!("{}/{}", self.identity.root, topic_key)
    }

    /// Allocate the next sequence number for this session's producer.
    ///
    /// One session has exactly one producer and exactly one allocator, so every
    /// sample published under this identity - from any clone of this handle, on
    /// any topic - draws from the same strictly increasing stream. Two counters
    /// behind one identity would each start at zero and the second would be
    /// rejected downstream as a replay.
    pub fn next_sequence(&self) -> Result<u64> {
        self.live_inner()?;
        allocate_sequence(&self.identity.seq)
    }

    /// Non-blocking semantic admission onto the outbound scheduler. State and
    /// setpoint values replace one unsent value per topic; samples evict the
    /// oldest bounded values with evidence; streams refuse admission rather
    /// than evicting. No path blocks the step loop.
    pub(crate) fn enqueue(
        &self,
        key: String,
        encoding: String,
        payload: Vec<u8>,
        mut metadata: BusMetadata,
        family: DeliveryFamily,
        metric: RuntimeMetricHandle,
    ) -> Result<()> {
        // Admission and close share one short critical section. This is the
        // linearization point: a sender either reserves/enqueues before close
        // takes the lock, or observes `Closed` after it.
        let inner = self.live_inner()?;
        let _admission = lock(&inner.admission);
        if inner.closing.load(Ordering::Acquire) {
            return Err(BusError::Closed);
        }
        let mut scheduler = lock(&inner.outbound);
        if family == DeliveryFamily::Stream {
            metadata.stream_position = Some(StreamPosition {
                sequence: scheduler.next_stream_position(&key),
            });
        }
        let attachment = metadata.encode().map_err(|error| {
            BusError::metadata(&key, crate::bus::error::MetadataProblem::Encode(error))
        })?;
        let outbound = Outbound::new(
            key.clone(),
            encoding,
            attachment,
            payload,
            metric.clone(),
            family,
        )
        .ok_or_else(|| {
            metric.record_drop();
            self.dropped(&key, OutboundBound::Byte)
        })?;

        let admission = match scheduler.admit(outbound) {
            Ok(admission) => admission,
            Err(bound) => {
                metric.record_drop();
                return Err(self.dropped(&key, bound));
            }
        };

        if let Some(replaced) = admission.replaced {
            replaced.metric.enqueue_finished();
            replaced.metric.record_latest_overwrite();
        }
        if !admission.evicted.is_empty() {
            let evicted = u64::try_from(admission.evicted.len()).unwrap_or(u64::MAX);
            for old in admission.evicted {
                old.metric.enqueue_finished();
                old.metric.record_bounded_eviction();
            }
            self.identity
                .health
                .outbound_drops
                .fetch_add(evicted, Ordering::Relaxed);
            tracing::warn!(
                target: "phoxal.bus",
                participant = ?self.identity.attribution.participant(),
                key = %key,
                count = evicted,
                "sample outbound lane evicted oldest values"
            );
        }
        // Replacement/eviction and new admission are one scheduler mutation.
        // Retire old depth first so high-water never reports capacity + 1 for
        // a queue state that could not actually exist.
        metric.enqueue_started();
        metric.record_message();
        if family == DeliveryFamily::Stream {
            scheduler.commit_stream_position(&key);
        }
        drop(scheduler);
        inner.outbound_notify.notify_one();
        Ok(())
    }

    fn dropped(&self, key: &str, bound: OutboundBound) -> BusError {
        self.identity
            .health
            .outbound_drops
            .fetch_add(1, Ordering::Relaxed);
        tracing::warn!(
            target: "phoxal.bus",
            participant = ?self.identity.attribution.participant(),
            key,
            bound = %bound,
            "outbound queue saturated; dropped sample (publish never blocks the step loop)"
        );
        BusError::Saturated {
            topic: key.to_string(),
            bound,
        }
    }
}

/// The evidence returned by a deterministic owner close.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BusCloseReport {
    /// Transport failures observed while draining asynchronous publications.
    pub transport_errors: Vec<String>,
    /// Total transport failures, including entries omitted from the bounded evidence.
    pub transport_error_count: usize,
    /// Number of failures omitted or byte-truncated from retained evidence.
    pub transport_errors_truncated: usize,
    /// Error from the terminal Zenoh close, retained beside prior evidence.
    pub session_close_error: Option<String>,
    /// Unexpected owner-owned worker exits observed by the bus worker group.
    pub worker_failures: Vec<BusFault>,
    /// Number of worker joins that could not be completed.
    pub unjoined_workers: usize,
    /// Close stages that exceeded their explicit deadline.
    pub timed_out: Vec<BusCloseTimeout>,
}

impl BusCloseReport {
    /// Whether every close stage completed without transport or worker
    /// evidence. The report remains available even when this is false.
    pub fn is_clean(&self) -> bool {
        self.transport_error_count == 0
            && self.session_close_error.is_none()
            && self.worker_failures.is_empty()
            && self.unjoined_workers == 0
            && self.timed_out.is_empty()
    }
}

impl std::fmt::Display for BusCloseReport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} transport failures ({} retained, {} truncated), session close error: {:?}, worker failures: {:?}, {} unjoined workers, timed out stages: {:?}",
            self.transport_error_count,
            self.transport_errors.len(),
            self.transport_errors_truncated,
            self.session_close_error,
            self.worker_failures,
            self.unjoined_workers,
            self.timed_out,
        )
    }
}

impl std::error::Error for BusCloseReport {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BusCloseTimeout {
    Drain,
    Session,
    Operations(usize),
    Workers(usize),
}

const DEFAULT_BUS_CLOSE_GRACE: Duration = Duration::from_secs(1);

impl BusOwner {
    /// Flush accepted work, join owned workers, and close the session.
    pub async fn close(self) -> BusCloseReport {
        self.close_until(tokio::time::Instant::now() + DEFAULT_BUS_CLOSE_GRACE)
            .await
    }

    /// Close the owner without exceeding `deadline`.
    ///
    /// Every stage consumes the same absolute budget. Once it is exhausted we
    /// still perform the state transitions (stop admission, take handles and
    /// abort workers), but do not wait for more asynchronous progress. The
    /// returned report records each stage that could not complete in time.
    pub async fn close_until(self, deadline: tokio::time::Instant) -> BusCloseReport {
        // Stop accepting new samples first so the drain set is finite, then signal
        // the drain task. `notify_one` stores a permit even if the drain task has
        // not yet registered as a waiter, so the shutdown signal is never lost (a
        // `notify_waiters` here would be dropped if the drain task had not been
        // polled yet - e.g. on a single-worker runtime).
        let _admission = lock(&self.inner.admission);
        self.inner.closing.store(true, Ordering::Release);
        drop(_admission);
        begin_terminal_close(&self.inner);
        self.inner.shutdown.notify_waiters();
        self.inner.shutdown.notify_one();
        let operations = self.inner.in_flight.load(Ordering::Acquire);
        let operations_timed_out = if operations > 0 {
            let timeout = deadline.saturating_duration_since(tokio::time::Instant::now());
            tokio::time::timeout(timeout, async {
                loop {
                    // Register before checking the counter: an admitted
                    // operation may finish between the check and await, and
                    // `notify_waiters` does not retain a permit for a waiter
                    // registered too late.
                    let notified = self.inner.in_flight_notify.notified();
                    if self.inner.in_flight.load(Ordering::Acquire) == 0 {
                        break;
                    }
                    notified.await;
                }
            })
            .await
            .is_err()
        } else {
            false
        };
        let handle = lock(&self.inner.drain).take();
        let mut timed_out = Vec::new();
        if let Some(mut drain) = handle {
            let timeout = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(timeout, &mut drain.monitor).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    lock(&self.inner.worker_failures).push(BusFault::WorkerJoin {
                        worker: "outbound-drain-monitor".to_string(),
                        error: error.to_string(),
                    });
                }
                Err(_) => {
                    drain.raw_abort.abort();
                    drain.monitor.abort();
                    timed_out.push(BusCloseTimeout::Drain);
                }
            }
        }
        let mut report = initial_close_report(&self.inner, timed_out);
        if operations_timed_out {
            report.timed_out.push(BusCloseTimeout::Operations(
                self.inner.in_flight.load(Ordering::Acquire),
            ));
        }
        record_session_close(&mut report, deadline, self.inner.session.close()).await;
        self.inner.workers.begin_close();
        let reaper = { lock(&self.inner.workers.reaper).take() };
        if let Some(mut reaper) = reaper {
            match tokio::time::timeout(
                deadline.saturating_duration_since(tokio::time::Instant::now()),
                &mut reaper.monitor,
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    lock(&self.inner.worker_failures).push(BusFault::WorkerJoin {
                        worker: "bus-worker-reaper".to_string(),
                        error: error.to_string(),
                    });
                }
                Err(_) => {
                    reaper.raw_abort.abort();
                    reaper.monitor.abort();
                    let remaining = self.inner.workers.take_remaining();
                    report.unjoined_workers = remaining.len();
                    for worker in remaining {
                        worker.handle.abort();
                        worker.raw_abort.abort();
                    }
                    report
                        .timed_out
                        .push(BusCloseTimeout::Workers(report.unjoined_workers));
                }
            }
        }
        report.worker_failures = lock(&self.inner.worker_failures).clone();
        mark_terminal_closed(&self.inner);
        report
    }

    pub(crate) fn handle(&self) -> BusHandle {
        BusHandle {
            identity: Arc::clone(&self.inner.identity),
            owner: Arc::downgrade(&self.inner),
            liveness: Arc::downgrade(&self.liveness),
            terminal: self.inner.terminal.subscribe(),
        }
    }
}

/// The Zenoh session identity a run's router opens with.
///
/// Compiled in every profile, because a domain module never asks which profile
/// it is in. Its one non-test caller is the embedded router, which only the
/// `supervisor` profile compiles, so every other profile builds it and uses it
/// nowhere - which is a dead-code report worth allowing rather than a second
/// shape of this module.
#[allow(dead_code, reason = "the embedded router is its only non-test caller")]
pub(crate) fn zenoh_id_for(execution: ExecutionId) -> Result<ZenohId> {
    ZenohId::try_from(&u128::from(execution).to_le_bytes()[..])
        .map_err(|error| BusError::Transport(format!("execution {execution}: {error}")))
}

/// The execution a router is routing, read back from its session id.
///
/// The inverse of [`zenoh_id_for`]: a Phoxal router opens with its execution as
/// its Zenoh id, so a router's id *is* the execution. A session id that is not
/// a legal execution therefore belongs to something that is not a Phoxal
/// router, which is a fact worth an error rather than a silent skip.
pub(crate) fn execution_from_zid(zid: ZenohId) -> Result<ExecutionId> {
    ExecutionId::try_from(u128::from_le_bytes(zid.to_le_bytes())).map_err(|source| {
        BusError::ForeignSessionId {
            zid: zid.to_string(),
            role: SessionIdRole::Execution,
            source,
        }
    })
}

/// The producer identity of a session, read back from the id the bus owner
/// pinned into Zenoh.
pub(crate) fn producer_from_zid(zid: ZenohId) -> Result<ProducerId> {
    ProducerId::try_from(u128::from_le_bytes(zid.to_le_bytes())).map_err(|source| {
        BusError::ForeignSessionId {
            zid: zid.to_string(),
            role: SessionIdRole::Producer,
            source,
        }
    })
}

/// Mint a production [`ProducerId`] before opening its Zenoh session.
///
/// The runtime-contract crate intentionally exposes the producer as an opaque
/// value with parsing/conversion only. A producer is a bus-session concern, so
/// its mint belongs here beside the Zenoh configuration and identity check
/// that pin it to the opened session.
fn mint_producer_id() -> Result<ProducerId> {
    let mut bytes = [0_u8; ProducerId::LEN / 2];
    getrandom::fill(&mut bytes)
        .map_err(|error| BusError::Transport(format!("failed to mint producer id: {error}")))?;
    let mut value = u128::from_be_bytes(bytes);
    if value >> 124 == 0 {
        value |= 1 << 124;
    }
    ProducerId::try_from(value)
        .map_err(|error| BusError::Transport(format!("failed to mint producer id: {error}")))
}

/// Hand out the next sequence, failing closed at the end of the range.
///
/// Wrapping would restart the stream at zero under an unchanged producer
/// identity, and every downstream freshness rule reads a non-increasing
/// sequence from a known producer as a replay - so the whole run would go
/// silently deaf to this publisher instead of reporting a bounded failure.
fn allocate_sequence(seq: &AtomicU64) -> Result<u64> {
    seq.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        current.checked_add(1)
    })
    .map_err(|_| BusError::SequenceExhausted)
}

async fn drain_loop(session: zenoh::Session, owner: Weak<BusInner>) {
    loop {
        let Some(inner) = owner.upgrade() else {
            return;
        };
        #[cfg(test)]
        if inner.drain_paused.load(Ordering::Acquire) {
            inner.drain_pause_ack.notify_one();
            while inner.drain_paused.load(Ordering::Acquire)
                && !inner.closing.load(Ordering::Acquire)
            {
                tokio::select! {
                    _ = inner.drain_resume.notified() => {}
                    _ = inner.shutdown.notified() => {}
                }
            }
        }
        let next = { lock(&inner.outbound).pop_next() };
        if let Some(out) = next {
            out.metric.enqueue_finished();
            put(&session, out, &inner).await;
            continue;
        }
        if inner.closing.load(Ordering::Acquire) {
            break;
        }
        tokio::select! {
            // Shutdown wins over waiting so a steady publish stream cannot
            // starve close. Once closing is set, the next loop drains the
            // finite accepted scheduler contents and exits when empty.
            biased;
            _ = inner.shutdown.notified() => {}
            _ = inner.outbound_notify.notified() => {}
        }
    }
}

#[cfg(test)]
pub(crate) struct TestDrainPause {
    inner: Weak<BusInner>,
}

#[cfg(test)]
impl Drop for TestDrainPause {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.upgrade() {
            inner.drain_paused.store(false, Ordering::Release);
            inner.drain_resume.notify_waiters();
            inner.drain_resume.notify_one();
        }
    }
}

fn spawn_supervised_worker<F>(
    name: &'static str,
    future: F,
    owner: Weak<BusInner>,
) -> SupervisedWorker
where
    F: Future<Output = ()> + Send + 'static,
{
    let worker = tokio::spawn(future);
    let raw_abort = worker.abort_handle();
    let monitor = tokio::spawn(async move {
        let result = worker.await;
        let Some(inner) = owner.upgrade() else {
            return;
        };
        if inner.closing.load(Ordering::Acquire) {
            return;
        }
        let fault = match result {
            Ok(()) => BusFault::WorkerExited {
                worker: name.to_string(),
            },
            Err(error) => BusFault::WorkerJoin {
                worker: name.to_string(),
                error: error.to_string(),
            },
        };
        signal_fatal(&inner, fault);
    });
    SupervisedWorker { monitor, raw_abort }
}

async fn record_session_close<F, E>(
    report: &mut BusCloseReport,
    deadline: tokio::time::Instant,
    close: F,
) where
    F: IntoFuture<Output = std::result::Result<(), E>>,
    E: std::fmt::Display,
{
    match tokio::time::timeout(
        deadline.saturating_duration_since(tokio::time::Instant::now()),
        close.into_future(),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(error)) => report.session_close_error = Some(error.to_string()),
        Err(_) => report.timed_out.push(BusCloseTimeout::Session),
    }
}

fn initial_close_report(inner: &BusInner, timed_out: Vec<BusCloseTimeout>) -> BusCloseReport {
    let transport = std::mem::take(&mut *lock(&inner.transport_errors));
    BusCloseReport {
        transport_errors: transport.entries,
        transport_error_count: transport.count,
        transport_errors_truncated: transport.truncated,
        worker_failures: lock(&inner.worker_failures).clone(),
        timed_out,
        ..BusCloseReport::default()
    }
}

async fn worker_reaper(group: Arc<BusWorkerGroup>, owner: Weak<BusInner>) {
    loop {
        group.changed.notified().await;
        for worker in group.take_finished() {
            let expected = worker.expected.load(Ordering::Acquire);
            let result = worker.handle.await;
            let closing = group.closing.load(Ordering::Acquire)
                || owner
                    .upgrade()
                    .is_none_or(|inner| inner.closing.load(Ordering::Acquire));
            if !expected && !closing {
                let fault = match result {
                    Ok(()) => BusFault::WorkerExited {
                        worker: worker.name,
                    },
                    Err(error) => BusFault::WorkerJoin {
                        worker: worker.name,
                        error: error.to_string(),
                    },
                };
                if let Some(inner) = owner.upgrade() {
                    signal_fatal(&inner, fault);
                }
            }
        }
        if group.closing.load(Ordering::Acquire) && lock(&group.workers).is_empty() {
            return;
        }
    }
}

fn begin_terminal_close(inner: &BusInner) {
    inner.terminal.send_if_modified(|terminal| match terminal {
        BusTerminal::Open => {
            *terminal = BusTerminal::Closing;
            true
        }
        BusTerminal::Closing | BusTerminal::Closed | BusTerminal::Fatal(_) => false,
    });
}

fn mark_terminal_closed(inner: &BusInner) {
    inner.terminal.send_if_modified(|terminal| match terminal {
        BusTerminal::Closing => {
            *terminal = BusTerminal::Closed;
            true
        }
        BusTerminal::Open | BusTerminal::Closed | BusTerminal::Fatal(_) => false,
    });
}

fn signal_fatal(inner: &BusInner, fault: BusFault) {
    let fault_for_state = fault.clone();
    if inner.terminal.send_if_modified(|terminal| match terminal {
        BusTerminal::Open => {
            *terminal = BusTerminal::Fatal(fault_for_state);
            true
        }
        BusTerminal::Closing | BusTerminal::Closed | BusTerminal::Fatal(_) => false,
    }) {
        lock(&inner.worker_failures).push(fault);
    }
}

/// The transport-queue policy each delivery family publishes under.
///
/// Zenoh's default is [`CongestionControl::Drop`]: a put whose transmission
/// queue toward the router is full is discarded there, silently and for every
/// subscriber at once. That is the right trade for the lossy families, whose
/// contracts already tolerate it - state and setpoints are latest-wins, and the
/// sample lane evicts its oldest item with evidence - and blocking on their
/// behalf would let one slow link stall every other topic sharing the drain.
///
/// Streams are the exception: their contract is lossless per producer, the
/// outbox refuses admission instead of evicting
/// ([`crate::bus::outbound::OutboundScheduler::admit`]), and a receiver treats a
/// missing position as fatal by design. A transport-level drop therefore
/// manufactures exactly the gap the whole family is built to make impossible,
/// and it lands on every subscriber simultaneously - one dropped world-clock
/// chunk faulted twelve supervised participants at once, eight seconds after
/// the graph reached Ready. [`CongestionControl::Block`] turns that loss into
/// backpressure on the drain loop instead, where the outbox's own bounded
/// refusal reports it to the publisher as a bounded, attributable failure.
const fn congestion_control_for(family: DeliveryFamily) -> CongestionControl {
    match family {
        DeliveryFamily::Stream => CongestionControl::Block,
        // Query traffic never reaches this drain; keeping it with the lossy
        // families makes an accidental future route degrade rather than block.
        DeliveryFamily::State
        | DeliveryFamily::Setpoint
        | DeliveryFamily::Sample
        | DeliveryFamily::Query => CongestionControl::Drop,
    }
}

/// Publish one admitted outbound item on the session-owned Zenoh drain.
///
/// The congestion policy is per family (see [`congestion_control_for`]), which
/// is why the drain carries [`Outbound::family`] this far down. Blocking here
/// stalls the single drain loop while the link drains, and that is deliberate:
/// backpressure is bounded by the transport lease and keepalive pinned in
/// [`apply_phoxal_transport_policy`], so a peer that stops reading is detected
/// and dropped rather than parking this loop forever.
async fn put(session: &zenoh::Session, out: Outbound, inner: &Arc<BusInner>) {
    let key = match OwnedKeyExpr::new(out.key.clone()) {
        Ok(key) => key,
        Err(e) => {
            let error = format!("invalid publish key '{}': {e}", out.key);
            record_transport_error(inner, error.clone());
            tracing::error!(target: "phoxal.bus", key = %out.key, error = %error, "invalid publish key");
            return;
        }
    };
    if let Err(e) = session
        .put(key, ZBytes::from(out.payload))
        .encoding(Encoding::from(out.encoding))
        .attachment(out.attachment)
        .congestion_control(congestion_control_for(out.family))
        .await
    {
        let error = format!("publish on '{}' failed: {e}", out.key);
        record_transport_error(inner, error.clone());
        tracing::warn!(target: "phoxal.bus", key = %out.key, error = %error, "publish failed");
    }
}

fn record_transport_error(inner: &BusInner, error: String) {
    inner
        .identity
        .health
        .transport_failures
        .fetch_add(1, Ordering::Relaxed);
    let mut errors = lock(&inner.transport_errors);
    errors.count = errors.count.saturating_add(1);
    if errors.entries.len() >= 32 {
        errors.truncated = errors.truncated.saturating_add(1);
    } else {
        let bounded = truncate_utf8(&error, 1024);
        if bounded.len() < error.len() {
            errors.truncated = errors.truncated.saturating_add(1);
        }
        errors.entries.push(bounded);
    }
}

/// Bound on how long a client-mode `BusOwner::open` retries a failed connect
/// before giving up. Passed to Zenoh as `connect/timeout_ms`, which wraps its
/// own internal connect retry (exponential backoff via `connect/retry/*`,
/// left at Zenoh's shipped defaults: 1s initial / 4s max / x2 increase)
/// rather than one this crate reimplements.
///
/// 20s comfortably survives the startup race this bounds: several
/// participants opening a bus session while a router is still coming up.
/// Zenoh's default backoff yields attempts at roughly t=0, 1s, 3s, 7s, 11s,
/// 15s, 19s - about seven tries - which is both more attempts and a shorter
/// worst case than today's only fallback (the CLI's crash-restart loop:
/// `RESTART_SEC=2s` between attempts, up to `START_LIMIT_BURST=5` in
/// `START_LIMIT_INTERVAL=60s`, each cycle paying full process-spawn cost). It
/// is also small next to the CLI's five-minute overall readiness deadline
/// (`wait_for_required_readiness`), so a genuinely absent router still fails
/// fast and legibly instead of silently eating the whole startup budget.
const CONNECT_TIMEOUT_MS: u64 = 20_000;

/// Apply the transport policy shared by every Phoxal-owned Zenoh session: a
/// nominal three-second link lease, Zenoh's documented four keepalives per
/// lease, and no multicast scouting.
///
/// Both ends of a Phoxal link read these from here - participants through
/// [`zenoh_config`], the supervisor's embedded router through
/// [`crate::bus::router`]. That single source is the point: a router disagreeing
/// with its clients about lease or keepalive produces exactly the kind of
/// intermittent link churn that is hardest to diagnose. They are applied after
/// any authored defaults so the runtime contract is explicit at the final
/// configuration boundary.
pub(crate) fn apply_phoxal_transport_policy(config: &mut zenoh::Config) -> Result<()> {
    config
        .insert_json5("transport/link/tx/lease", "3000")
        .map_err(|error| BusError::Transport(error.to_string()))?;
    config
        .insert_json5("transport/link/tx/keep_alive", "4")
        .map_err(|error| BusError::Transport(error.to_string()))?;
    config
        .insert_json5("scouting/multicast/enabled", "false")
        .map_err(|error| BusError::Transport(error.to_string()))?;
    Ok(())
}

/// A client config for a single endpoint carrying the shared transport policy
/// and no connect retry, for the callers whose question is "what is there right
/// now" rather than "get me attached eventually".
pub(crate) fn client_config(endpoint: &str) -> Result<zenoh::Config> {
    let mut config = zenoh::Config::default();
    apply_phoxal_transport_policy(&mut config)?;
    let endpoints = serde_json::to_string(std::slice::from_ref(&endpoint))
        .map_err(|error| BusError::Transport(error.to_string()))?;
    for (key, value) in [
        ("mode", "\"client\""),
        ("connect/endpoints", endpoints.as_str()),
    ] {
        config
            .insert_json5(key, value)
            .map_err(|error| BusError::Transport(error.to_string()))?;
    }
    Ok(config)
}

fn zenoh_config(connect_endpoints: &[String], producer: ProducerId) -> Result<zenoh::Config> {
    let mut config = zenoh::Config::default();
    apply_phoxal_transport_policy(&mut config)?;
    let id = serde_json::to_string(&producer.to_string())
        .map_err(|error| BusError::Transport(error.to_string()))?;
    config
        .insert_json5("id", &id)
        .map_err(|error| BusError::Transport(error.to_string()))?;
    if connect_endpoints.is_empty() {
        // In-process: no listeners, no scouting. A single session still delivers
        // its own publications to its own subscribers.
        config
            .insert_json5("listen/endpoints", "[]")
            .map_err(|error| BusError::Transport(error.to_string()))?;
    } else {
        config
            .insert_json5("mode", "\"client\"")
            .map_err(|error| BusError::Transport(error.to_string()))?;
        let json = serde_json::to_string(connect_endpoints)
            .map_err(|error| BusError::Transport(error.to_string()))?;
        config
            .insert_json5("connect/endpoints", &json)
            .map_err(|error| BusError::Transport(error.to_string()))?;
        // Client mode's shipped default is `connect/timeout_ms: 0` ("no
        // retry" - see zenoh-config's DEFAULT_CONFIG.json5), so a router that
        // is not yet accepting connections - the ordinary case when several
        // participants race a router at startup - fails the very first
        // attempt and there is no second one. Setting a bounded, nonzero
        // timeout here switches `zenoh::open` onto Zenoh's own internal retry
        // path: it retries the connect with exponential backoff
        // (`connect/retry/*`, left at Zenoh's shipped defaults: 1s initial
        // delay, doubling, capped at 4s) until either a link is established
        // or this timeout elapses, at which point it returns a clear error
        // naming the endpoint(s) it could not reach. That is a better lever
        // than an outer retry loop in this crate: Zenoh already owns the
        // backoff math, the per-attempt diagnostics (each failed attempt logs
        // the underlying transport error), and the "clear bounded failure"
        // requirement, all through one `.await` on `zenoh::open`.
        config
            .insert_json5("connect/timeout_ms", &CONNECT_TIMEOUT_MS.to_string())
            .map_err(|error| BusError::Transport(error.to_string()))?;
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use serial_test::serial;

    use crate::bus::handle::publisher::StatePublisher;
    use crate::bus::handle::subscriber::{Latest, Subscriber};
    use crate::bus::test_support::{TARGET_TOPIC, Target, bound, participant_config, step};

    fn test_producer(value: u128) -> ProducerId {
        ProducerId::try_from((1_u128 << 124) | value).expect("canonical test producer")
    }

    #[test]
    fn the_sequence_allocator_fails_closed_instead_of_wrapping() {
        let seq = AtomicU64::new(0);
        assert_eq!(allocate_sequence(&seq).unwrap(), 0);
        assert_eq!(allocate_sequence(&seq).unwrap(), 1);

        // At the end of the range the allocator refuses rather than wrapping to
        // zero, which a receiver would read as a replay from this producer.
        seq.store(u64::MAX, Ordering::Relaxed);
        assert!(matches!(
            allocate_sequence(&seq),
            Err(BusError::SequenceExhausted)
        ));
        assert_eq!(
            seq.load(Ordering::Relaxed),
            u64::MAX,
            "a refused allocation must not advance the counter"
        );
    }

    #[test]
    fn only_streams_publish_under_transport_backpressure() {
        // A dropped stream chunk is an unrecoverable gap for every subscriber
        // at once, so streams are the one family that must never be discarded
        // at a full transmission queue. The lossy families stay on Drop so a
        // single slow link cannot stall the shared drain.
        for (family, expected) in [
            (DeliveryFamily::Stream, CongestionControl::Block),
            (DeliveryFamily::State, CongestionControl::Drop),
            (DeliveryFamily::Setpoint, CongestionControl::Drop),
            (DeliveryFamily::Sample, CongestionControl::Drop),
            (DeliveryFamily::Query, CongestionControl::Drop),
        ] {
            assert_eq!(
                congestion_control_for(family),
                expected,
                "unexpected congestion policy for {family:?}"
            );
        }
    }

    #[test]
    fn phoxal_transport_settings_are_accepted_by_pinned_zenoh_config() {
        zenoh_config(&[], test_producer(1))
            .expect("pinned Zenoh must accept Phoxal lease settings");
        zenoh_config(&["tcp/127.0.0.1:7447".to_string()], test_producer(2))
            .expect("pinned Zenoh must accept Phoxal client settings");
    }

    #[test]
    fn client_mode_bounds_the_connect_retry_but_in_process_does_not() {
        // In-process (no endpoints) never dials anything, so there is nothing
        // to retry - the key must be absent, not merely zero, so it cannot be
        // mistaken for "connect once, no retry" on a config path that never
        // connects at all.
        let in_process = zenoh_config(&[], test_producer(3)).expect("in-process config");
        assert_eq!(
            in_process
                .get_json("connect/timeout_ms")
                .expect("key is always present, unresolved by default"),
            "null",
            "in-process config must leave Zenoh's mode-dependent default \
             untouched - it never dials anything, so there is nothing to \
             bound",
        );

        // Client mode (this is what a real launch uses) must carry the bounded,
        // nonzero timeout that switches Zenoh onto its own retry-with-backoff
        // path - see CONNECT_TIMEOUT_MS's docs for why this exact value.
        let client = zenoh_config(&["tcp/127.0.0.1:7447".to_string()], test_producer(4))
            .expect("client config");
        assert_eq!(
            client
                .get_json("connect/timeout_ms")
                .expect("client config must set a connect timeout"),
            CONNECT_TIMEOUT_MS.to_string(),
        );
    }

    #[test]
    fn an_execution_and_its_session_identity_render_identically() {
        let execution = ExecutionId::mint();
        let zid = zenoh_id_for(execution).expect("an execution is always a session id");
        assert_eq!(zid.to_string(), execution.to_string());

        // And back: the session that opened with it reports a producer whose
        // text is the same string again.
        let producer = producer_from_zid(zid).expect("a session id is always a producer");
        assert_eq!(producer.to_string(), execution.to_string());
    }

    /// The bus key root is written out rather than composed from whatever the
    /// prefix currently is, because a root that follows the code is not frozen.
    ///
    /// This fact is part of the frozen bootstrap-reachable subset and is
    /// preserved across framework majors. A change here is a bootstrap-breaking
    /// event - see `xtask/README.md` "When a gate fails", rule 3 "A frozen
    /// bootstrap fact drifted".
    #[test]
    fn the_bootstrap_key_grammar_is_pinned_to_its_literal() {
        assert_eq!(BUS_KEY_PREFIX, "phoxal");

        let execution = ExecutionId::parse("1c8f3a5b7d9e0f2a4b6c8d0e1f325476")
            .expect("a canonical execution id");
        assert_eq!(
            format!("{BUS_KEY_PREFIX}/{execution}"),
            "phoxal/1c8f3a5b7d9e0f2a4b6c8d0e1f325476"
        );

        // And the composition below it: one root, one family-rooted topic key,
        // no version segment between them.
        let root = format!("{BUS_KEY_PREFIX}/{execution}");
        assert_eq!(
            format!("{root}/supervisor/connect"),
            "phoxal/1c8f3a5b7d9e0f2a4b6c8d0e1f325476/supervisor/connect"
        );
        OwnedKeyExpr::new(format!("{root}/supervisor/connect"))
            .expect("the composed bootstrap key is a legal Zenoh key");
    }

    /// The execution's text form is what the key root carries and what a router
    /// session reports, so a client that reads one and spells the other needs
    /// the two to be the same string.
    ///
    /// This fact is part of the frozen bootstrap-reachable subset and is
    /// preserved across framework majors. A change here is a bootstrap-breaking
    /// event - see `xtask/README.md` "When a gate fails", rule 3 "A frozen
    /// bootstrap fact drifted".
    #[test]
    fn the_bootstrap_execution_text_form_is_pinned_to_the_session_id_spelling() {
        let execution = ExecutionId::parse("1c8f3a5b7d9e0f2a4b6c8d0e1f325476")
            .expect("a canonical execution id");
        let rendered = execution.to_string();
        assert_eq!(rendered.len(), 32);
        assert!(
            rendered
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "an execution renders as lowercase hexadecimal: {rendered}"
        );
        assert_ne!(&rendered[..1], "0", "the leading nibble is never zero");

        // The Zenoh session id a router opens with renders as that same string,
        // and reads back as the same execution.
        let zid = zenoh_id_for(execution).expect("an execution is always a session id");
        assert_eq!(zid.to_string(), rendered);
        assert_eq!(
            execution_from_zid(zid).expect("a router session id is an execution"),
            execution
        );
    }

    /// Discovery is the first step of an attachment, so the session it opens is
    /// pinned: a client on exactly the given endpoint, with multicast scouting
    /// off, so only routers reachable through that endpoint are ever reported.
    ///
    /// This fact is part of the frozen bootstrap-reachable subset and is
    /// preserved across framework majors. A change here is a bootstrap-breaking
    /// event - see `xtask/README.md` "When a gate fails", rule 3 "A frozen
    /// bootstrap fact drifted".
    #[test]
    fn the_bootstrap_discovery_session_is_pinned_to_a_scouting_free_client() {
        let config = client_config("tcp/127.0.0.1:7447").expect("a discovery client config");
        assert_eq!(
            config.get_json("mode").expect("the mode is set"),
            "\"client\""
        );
        assert_eq!(
            config
                .get_json("connect/endpoints")
                .expect("the endpoint is set"),
            "[\"tcp/127.0.0.1:7447\"]"
        );
        assert_eq!(
            config
                .get_json("scouting/multicast/enabled")
                .expect("multicast scouting is set"),
            "false",
            "discovery reports the routers behind the given endpoint and never a multicast peer"
        );
        assert_eq!(
            config
                .get_json("transport/link/tx/lease")
                .expect("the lease is set"),
            "3000"
        );
        assert_eq!(
            config
                .get_json("transport/link/tx/keep_alive")
                .expect("the keepalive is set"),
            "4"
        );
    }

    /// The transport underneath the bootstrap has a wire version of its own,
    /// and two peers that disagree on it never exchange a Phoxal byte.
    ///
    /// This fact is part of the frozen bootstrap-reachable subset and is
    /// preserved across framework majors. A change here is a bootstrap-breaking
    /// event: a Zenoh release that moves its wire protocol version needs
    /// deliberate design, never a routine dependency bump - see
    /// `xtask/README.md` "When a gate fails", rule 3 "A frozen bootstrap fact
    /// drifted".
    #[test]
    fn the_zenoh_wire_protocol_version_is_pinned_to_its_literal() {
        assert_eq!(
            ZENOH_WIRE_PROTOCOL_VERSION, 9,
            "the Zenoh wire protocol version this train speaks is a frozen bootstrap fact; a \
             transport upgrade that moves it breaks every peer built from every other line"
        );
    }

    #[test]
    fn a_producer_identity_requires_the_canonical_width() {
        let short: ZenohId = "abc".parse().expect("a short session id is legal");
        assert!(
            producer_from_zid(short).is_err(),
            "a transport id that is not full-width cannot be a Phoxal producer"
        );
        let zid: ZenohId = format!("1{}", "a".repeat(31))
            .parse()
            .expect("a full-width session id is legal");
        let producer = producer_from_zid(zid).expect("a session id is always a producer");
        assert_eq!(
            producer.to_string(),
            format!("{:032x}", u128::from(producer))
        );
        assert_eq!(ProducerId::parse(&producer.to_string()), Ok(producer));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn an_external_bus_does_not_require_participant_attribution() {
        let config = BusConfig::for_external(ExecutionId::mint(), None, Vec::new());
        let (owner, bus) = BusOwner::open(config).await.unwrap();
        assert!(bus.participant().is_none());
        assert!(matches!(
            owner.declare_participant_ready().await,
            Err(BusError::InvalidKey { .. })
        ));
        owner.close().await;
    }

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn ready_lease_is_owner_only_and_carries_exact_participant_and_producer() {
        let participant = ParticipantId::new("ready-test").unwrap();
        let (owner, bus) = BusOwner::open(BusConfig::for_participant(
            ExecutionId::mint(),
            participant.clone(),
            Vec::new(),
        ))
        .await
        .unwrap();
        let ready = owner
            .declare_participant_ready()
            .await
            .expect("the owner can declare Ready");
        assert_eq!(ready.participant(), &participant);
        assert_eq!(ready.producer(), bus.producer());
        drop(ready);
        owner.close().await;
    }

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reopening_an_execution_mints_a_new_pinned_producer() {
        let config = participant_config("reopen");
        let (first_owner, first) = BusOwner::open(config.clone()).await.unwrap();
        assert_eq!(
            first.producer().to_string(),
            first.session().unwrap().zid().to_string(),
            "the owner pins and reads back the same producer identity"
        );
        let first_producer = first.producer();
        first_owner.close().await;

        let (second_owner, second) = BusOwner::open(config).await.unwrap();
        assert_ne!(
            second.producer(),
            first_producer,
            "a reopened session must not reuse its predecessor's provenance"
        );
        second_owner.close().await;
    }

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn every_clone_uses_one_session_sequence_allocator() {
        let (owner, bus) = BusOwner::open(participant_config("sequence"))
            .await
            .unwrap();
        let clone = bus.clone();
        assert_eq!(bus.next_sequence().unwrap(), 0);
        assert_eq!(clone.next_sequence().unwrap(), 1);
        assert_eq!(bus.next_sequence().unwrap(), 2);
        owner.close().await;
    }

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropping_owner_invalidates_every_transport_operation_and_weakens_handle() {
        let participant = ParticipantId::new("drop-owner").unwrap();
        let (owner, bus) = BusOwner::open(BusConfig::for_participant(
            ExecutionId::mint(),
            participant,
            Vec::new(),
        ))
        .await
        .unwrap();
        let topic = bound::<Target>(TARGET_TOPIC).owner();
        let publisher = StatePublisher::<Target>::new(bus.clone(), &topic).unwrap();
        drop(owner);

        assert!(
            bus.liveness.upgrade().is_none(),
            "a handle must observe the owner liveness token immediately"
        );
        tokio::time::timeout(Duration::from_secs(1), async {
            while bus.owner.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("aborted owner tasks release their final strong reference");
        assert!(bus.session().is_err());
        assert!(bus.next_sequence().is_err());
        assert!(bus.take_runtime_metrics().is_err());
        assert!(
            publisher
                .publish(
                    &step(1, 1),
                    Target {
                        linear_x_mps: 0.0,
                        angular_z_radps: 0.0,
                    },
                )
                .is_err()
        );
        let subscribe = bound::<Target>(TARGET_TOPIC).client();
        assert!(Latest::<Target>::new(&bus, &subscribe).await.is_err());
        assert!(Subscriber::<Target>::new(&bus, &subscribe).await.is_err());
        assert!(bus.declare_server("dead").await.is_err());
        assert!(bus.observe_participant_ready(|_| {}).await.is_err());
        assert!(bus.observe_liveliness_key("dead", |_| {}).await.is_err());
    }

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn close_reports_bounded_stuck_workers() {
        let (owner, bus) = BusOwner::open(participant_config("stuck-worker"))
            .await
            .unwrap();
        let stuck = tokio::spawn(std::future::pending::<()>());
        bus.register_worker(stuck).expect("worker is admitted");
        let report = owner
            .close_until(tokio::time::Instant::now() + Duration::from_millis(25))
            .await;
        assert!(report.timed_out.contains(&BusCloseTimeout::Workers(1)));
    }

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn close_until_expired_deadline_is_fail_closed_with_evidence() {
        let (owner, bus) = BusOwner::open(participant_config("expired-close"))
            .await
            .unwrap();
        let stuck = tokio::spawn(std::future::pending::<()>());
        bus.register_worker(stuck).expect("worker is admitted");

        let began = std::time::Instant::now();
        let report = owner.close_until(tokio::time::Instant::now()).await;

        assert!(began.elapsed() < Duration::from_millis(100));
        assert_eq!(report.unjoined_workers, 1);
        assert!(report.timed_out.contains(&BusCloseTimeout::Workers(1)));
        assert!(!report.is_clean());
    }

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn close_uses_the_supplied_deadline_not_a_hidden_worker_cap() {
        let (owner, bus) = BusOwner::open(participant_config("worker-deadline"))
            .await
            .unwrap();
        bus.register_worker(tokio::spawn(async {
            tokio::time::sleep(Duration::from_millis(300)).await;
        }))
        .expect("worker is admitted");

        let report = owner
            .close_until(tokio::time::Instant::now() + Duration::from_millis(500))
            .await;
        assert!(
            !report
                .timed_out
                .iter()
                .any(|timeout| matches!(timeout, BusCloseTimeout::Workers(_))),
            "a worker completing before the supplied deadline must not hit the old 250 ms cap"
        );
    }

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn close_waits_for_an_admitted_operation_and_rejects_later_operations() {
        let (owner, bus) = BusOwner::open(participant_config("operation-race"))
            .await
            .unwrap();
        let admitted = bus.admit_operation().expect("operation is admitted");
        let close = tokio::spawn(owner.close());
        tokio::task::yield_now().await;
        assert!(
            !close.is_finished(),
            "close must wait for admitted operations"
        );
        drop(admitted);
        let report = close.await.unwrap();
        assert!(
            !report
                .timed_out
                .iter()
                .any(|timeout| matches!(timeout, BusCloseTimeout::Operations(_)))
        );
        assert!(
            bus.admit_operation().is_err(),
            "operations after close reject"
        );
    }

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn a_stalled_admitted_operation_is_typed_close_timeout_evidence() {
        let (owner, bus) = BusOwner::open(participant_config("stalled-operation"))
            .await
            .unwrap();
        let admitted = bus.admit_operation().expect("operation is admitted");
        let report = owner
            .close_until(tokio::time::Instant::now() + Duration::from_millis(25))
            .await;
        assert!(report.timed_out.contains(&BusCloseTimeout::Operations(1)));
        drop(admitted);
    }

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn transport_evidence_is_bounded_at_utf8_boundaries() {
        let (owner, bus) = BusOwner::open(participant_config("transport-evidence"))
            .await
            .unwrap();
        for _ in 0..40 {
            record_transport_error(&owner.inner, "é".repeat(2_000));
        }
        assert_eq!(bus.health().transport_failures.load(Ordering::Relaxed), 40);
        let report = owner.close().await;
        assert_eq!(report.transport_error_count, 40);
        assert!(report.transport_errors_truncated > 0);
        assert!(
            report
                .transport_errors
                .iter()
                .all(|entry| entry.len() <= 1024)
        );
        assert!(
            report
                .transport_errors
                .iter()
                .all(|entry| std::str::from_utf8(entry.as_bytes()).is_ok())
        );
    }

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn unexpected_worker_exit_faults_handles_and_is_reaped() {
        let (owner, bus) = BusOwner::open(participant_config("worker-fatal"))
            .await
            .unwrap();
        let observer = bus.clone();
        bus.register_worker(tokio::spawn(async {}))
            .expect("worker is admitted");

        let fault = tokio::time::timeout(Duration::from_secs(1), observer.wait_for_fatal())
            .await
            .expect("worker completion must signal fatal");
        assert!(matches!(fault, BusFault::WorkerExited { .. }));
        tokio::time::timeout(Duration::from_secs(1), async {
            while !lock(&owner.inner.workers.workers).is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("completed worker must be reaped before owner close");

        let report = owner.close().await;
        assert_eq!(report.worker_failures, vec![fault]);
        assert!(matches!(observer.terminal(), BusTerminal::Fatal(_)));
    }

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn aborting_the_owned_drain_is_a_terminal_bus_fault() {
        let (owner, bus) = BusOwner::open(participant_config("drain-fatal"))
            .await
            .unwrap();
        bus.__test_abort_outbound_drain()
            .expect("the live owner has a drain worker");

        let fault = tokio::time::timeout(Duration::from_secs(1), bus.wait_for_fatal())
            .await
            .expect("the drain monitor must wake lifecycle observers");
        assert!(matches!(
            fault,
            BusFault::WorkerJoin { ref worker, .. } if worker == "outbound-drain"
        ));
        let report = owner.close().await;
        assert!(report.worker_failures.contains(&fault));
    }

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn close_path_keeps_async_and_session_close_evidence_together() {
        let (owner, _bus) = BusOwner::open(participant_config("close-evidence"))
            .await
            .unwrap();
        record_transport_error(&owner.inner, "earlier put failed".to_string());
        let mut report = initial_close_report(&owner.inner, Vec::new());
        record_session_close(
            &mut report,
            tokio::time::Instant::now() + Duration::from_secs(1),
            async { Err::<(), _>("later session close failed") },
        )
        .await;
        assert!(!report.is_clean());
        assert_eq!(report.transport_errors, ["earlier put failed"]);
        assert_eq!(
            report.session_close_error.as_deref(),
            Some("later session close failed")
        );
        owner.close().await;
    }

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn worker_registration_after_close_is_returned_for_joining() {
        let (owner, bus) = BusOwner::open(participant_config("worker-race"))
            .await
            .unwrap();
        owner.close().await;

        let worker = tokio::spawn(async {});
        let worker = bus
            .register_worker(worker)
            .expect_err("a worker cannot be admitted after close");
        worker.await.expect("the rejected worker is still joined");
    }

    /// The root is the execution and nothing else: no namespace, no robot id, and
    /// no prefix character in front of the identity - a canonical session id always
    /// starts with a legal chunk character.
    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn the_key_root_is_the_execution() {
        let config = participant_config("r1");
        let execution = config.execution();
        let (owner, bus) = BusOwner::open(config).await.unwrap();
        let expected_root = format!("phoxal/{execution}");
        assert_eq!(bus.root(), expected_root);
        assert!(!bus.root().contains("r1"), "the robot id is not routing");
        assert_eq!(
            bus.full_key("yTEST/drive/state"),
            format!("{expected_root}/yTEST/drive/state")
        );
        owner.close().await;
    }

    /// A session publishes under the identity Zenoh gave it, so provenance can be
    /// matched against the transport without a side channel.
    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_producer_is_the_publishing_session() {
        let (owner, bus) = BusOwner::open(participant_config("producer"))
            .await
            .unwrap();
        assert_eq!(
            bus.producer().to_string(),
            bus.session().unwrap().zid().to_string()
        );

        let pub_topic = bound::<Target>(TARGET_TOPIC).owner();
        let sub_topic = bound::<Target>(TARGET_TOPIC).client();
        let publisher = StatePublisher::<Target>::new(bus.clone(), &pub_topic).unwrap();
        let latest = Latest::<Target>::new(&bus, &sub_topic).await.unwrap();

        let mut observed = None;
        for tick in 0..100 {
            publisher
                .publish(
                    &step(1, 100 + tick),
                    Target {
                        linear_x_mps: 1.0,
                        angular_z_radps: 0.0,
                    },
                )
                .unwrap();
            if let Some(sample) = latest.observed() {
                observed = Some(sample);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let observed = observed.expect("the sample must arrive");
        assert_eq!(observed.metadata.source.producer(), bus.producer());

        owner.close().await;
    }

    /// Traffic from a previous execution lands on a different key root and cannot
    /// be observed as current - the structural property execution scoping exists
    /// to provide.
    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_previous_execution_cannot_be_observed_as_current() {
        let (previous_owner, previous) =
            BusOwner::open(participant_config("scoped")).await.unwrap();
        let (current_owner, current) = BusOwner::open(participant_config("scoped")).await.unwrap();
        assert_ne!(previous.root(), current.root());

        let pub_topic = bound::<Target>(TARGET_TOPIC).owner();
        let sub_topic = bound::<Target>(TARGET_TOPIC).client();
        let stale = StatePublisher::<Target>::new(previous.clone(), &pub_topic).unwrap();
        let latest = Latest::<Target>::new(&current, &sub_topic).await.unwrap();

        stale
            .publish(
                &step(1, 100),
                Target {
                    linear_x_mps: 6.0,
                    angular_z_radps: 0.0,
                },
            )
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            latest.latest().is_none(),
            "a previous execution's traffic must not reach the current run"
        );

        previous_owner.close().await;
        current_owner.close().await;
    }
}
