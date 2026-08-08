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

use std::ops::Deref;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use phoxal_runtime_contract::identity::{ExecutionId, ParticipantId, ProducerId};
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;
use zenoh::bytes::{Encoding, ZBytes};
use zenoh::config::ZenohId;
use zenoh::key_expr::OwnedKeyExpr;

use crate::abi::truncate_utf8;
use crate::error::{BusError, KeyProblem, OutboundBound, Result, SessionIdRole};
use crate::lock::lock;
use crate::metadata::{BusMetadata, MAX_SOURCE_LABEL_BYTES, SourceAttribution, SourceLabel};
use crate::runtime_metrics::{RuntimeMetricHandle, RuntimeMetricSnapshot, RuntimeMetrics};
use crate::time::TimeWindow;

/// First chunk of every Phoxal bus key. It exists so a Phoxal execution is
/// recognisable in a trace and cannot collide with a non-Phoxal key tree
/// sharing the same Zenoh fabric.
const BUS_KEY_PREFIX: &str = "phoxal";

/// Capacity (in samples) of the runner-owned outbound queue. A publish that would
/// exceed this drops the sample and bumps the drop counter - it never blocks the
/// step loop.
pub(crate) const OUTBOUND_CAPACITY: usize = 1024;

/// Byte bound of the outbound queue. The queue is bounded in samples AND bytes,
/// because either alone lets a conforming publisher exhaust the other. A publish
/// that would exceed it is dropped + counted rather than blocking.
pub(crate) const OUTBOUND_MAX_BYTES: usize = 16 * 1024 * 1024;

/// Connection inputs for opening a bus session.
///
/// The execution is the *only* routing input. `RobotId` is model data and never
/// contributes to a key: two executions never share a key, even when they run
/// the same logical robot.
#[derive(Clone, Debug)]
pub struct BusConfig {
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
                crate::metadata::ParticipantSourceIdentity::new(participant.clone(), producer),
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
    /// Samples dropped because the outbound queue was full.
    pub outbound_drops: AtomicU64,
    /// Inbound samples dropped because the ring was full (slow consumer).
    pub inbound_drops: AtomicU64,
    /// Inbound samples that failed to decode. Contract identity lives in the
    /// Zenoh key, so a receiver's per-key subscription is the whole
    /// fast-reject and a decode failure is the only remaining rejection.
    pub decode_errors: AtomicU64,
}

struct Outbound {
    key: String,
    encoding: String,
    attachment: Vec<u8>,
    payload: Vec<u8>,
    bytes: usize,
    metric: RuntimeMetricHandle,
}

struct BusInner {
    session: zenoh::Session,
    identity: Arc<BusIdentity>,
    outbound: mpsc::Sender<Outbound>,
    queued_bytes: AtomicUsize,
    admission: std::sync::Mutex<()>,
    closing: AtomicBool,
    shutdown: Notify,
    drain: std::sync::Mutex<Option<JoinHandle<()>>>,
    workers: std::sync::Mutex<Vec<JoinHandle<()>>>,
    transport_errors: std::sync::Mutex<TransportErrors>,
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
pub struct BusOwner {
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
        self.inner.shutdown.notify_waiters();
        self.inner.shutdown.notify_one();
        if let Some(drain) = lock(&self.inner.drain).take() {
            drain.abort();
        }
        for worker in std::mem::take(&mut *lock(&self.inner.workers)) {
            worker.abort();
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

        let (tx, rx) = mpsc::channel::<Outbound>(OUTBOUND_CAPACITY);
        let drain_session = session.clone();
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
            outbound: tx,
            queued_bytes: AtomicUsize::new(0),
            admission: std::sync::Mutex::new(()),
            closing: AtomicBool::new(false),
            shutdown: Notify::new(),
            drain: std::sync::Mutex::new(None),
            workers: std::sync::Mutex::new(Vec::new()),
            transport_errors: std::sync::Mutex::new(TransportErrors::default()),
            in_flight: AtomicUsize::new(0),
            in_flight_notify: Notify::new(),
        });

        let drain = tokio::spawn(drain_loop(drain_session, rx, Arc::downgrade(&inner)));
        *lock(&inner.drain) = Some(drain);

        let liveness = Arc::new(AtomicBool::new(true));
        let owner = BusOwner {
            inner: Arc::clone(&inner),
            liveness: Arc::clone(&liveness),
        };
        let handle = BusHandle {
            identity: Arc::clone(&inner.identity),
            owner: Arc::downgrade(&inner),
            liveness: Arc::downgrade(&liveness),
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
            codec: crate::abi::CodecId::MessagePack.as_u8(),
            sequence: self.next_sequence()?,
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
    /// [`crate::runtime_metrics`] for what a row means and what it does not.
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

    pub(crate) fn register_worker(
        &self,
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
        lock(&inner.workers).push(worker);
        Ok(())
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

    /// Compose a full bus key from a version-qualified topic key.
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

    /// Non-blocking enqueue onto the outbound queue. A full queue (samples or
    /// bytes) drops the sample, bumps the drop counter, and returns
    /// `Saturated` - it never blocks the step loop.
    pub(crate) fn enqueue(
        &self,
        key: String,
        encoding: String,
        attachment: Vec<u8>,
        payload: Vec<u8>,
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
        let bytes = key.len() + encoding.len() + attachment.len() + payload.len();

        // Atomically reserve the bytes *before* making the item visible to the
        // drain. A CAS loop makes the limit a global invariant across cloned
        // Bus publishers; add-then-check would let concurrent callers each
        // observe an individually valid pre-add value and collectively exceed it.
        if !reserve_outbound_bytes(&inner.queued_bytes, bytes) {
            metric.record_drop();
            return Err(self.dropped(&key, OutboundBound::Byte));
        }

        metric.enqueue_started();

        let outbound = Outbound {
            key,
            encoding,
            attachment,
            payload,
            bytes,
            metric: metric.clone(),
        };
        match inner.outbound.try_send(outbound) {
            Ok(()) => {
                metric.record_message();
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(out)) => {
                inner.queued_bytes.fetch_sub(bytes, Ordering::AcqRel);
                out.metric.enqueue_finished();
                out.metric.record_drop();
                Err(self.dropped(&out.key, OutboundBound::Sample))
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                inner.queued_bytes.fetch_sub(bytes, Ordering::AcqRel);
                metric.enqueue_finished();
                Err(BusError::Closed)
            }
        }
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
    /// Number of worker joins that could not be completed.
    pub unjoined_workers: usize,
    /// Close stages that exceeded their explicit deadline.
    pub timed_out: Vec<BusCloseTimeout>,
}

impl BusCloseReport {
    /// Whether every close stage completed without transport or worker
    /// evidence. The report remains available even when this is false.
    pub fn is_clean(&self) -> bool {
        self.transport_error_count == 0 && self.unjoined_workers == 0 && self.timed_out.is_empty()
    }
}

impl std::fmt::Display for BusCloseReport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} transport failures ({} retained, {} truncated), {} unjoined workers, timed out stages: {:?}",
            self.transport_error_count,
            self.transport_errors.len(),
            self.transport_errors_truncated,
            self.unjoined_workers,
            self.timed_out,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BusCloseTimeout {
    Drain,
    Session,
    Operations(usize),
    Workers(usize),
}

const CLOSE_DRAIN_TIMEOUT: Duration = Duration::from_millis(250);
const CLOSE_SESSION_TIMEOUT: Duration = Duration::from_millis(250);
const CLOSE_OPERATIONS_TIMEOUT: Duration = Duration::from_millis(250);
const CLOSE_WORKER_TIMEOUT: Duration = Duration::from_millis(250);

impl BusOwner {
    /// Flush accepted work, join owned workers, and close the session.
    pub async fn close(self) -> Result<BusCloseReport> {
        // The lifecycle runner uses `close_until` with its supervised grace.
        // Keep this standalone convenience compatible with the historical
        // per-stage caps rather than imposing a new one-second process limit.
        self.close_until(tokio::time::Instant::now() + Duration::from_secs(60 * 60))
            .await
    }

    /// Close the owner without exceeding `deadline`.
    ///
    /// Every stage consumes the same absolute budget. Once it is exhausted we
    /// still perform the state transitions (stop admission, take handles and
    /// abort workers), but do not wait for more asynchronous progress. The
    /// returned report records each stage that could not complete in time.
    pub async fn close_until(self, deadline: tokio::time::Instant) -> Result<BusCloseReport> {
        // Stop accepting new samples first so the drain set is finite, then signal
        // the drain task. `notify_one` stores a permit even if the drain task has
        // not yet registered as a waiter, so the shutdown signal is never lost (a
        // `notify_waiters` here would be dropped if the drain task had not been
        // polled yet - e.g. on a single-worker runtime).
        let _admission = lock(&self.inner.admission);
        self.inner.closing.store(true, Ordering::Release);
        drop(_admission);
        self.inner.shutdown.notify_waiters();
        self.inner.shutdown.notify_one();
        let operations = self.inner.in_flight.load(Ordering::Acquire);
        let operations_timed_out = if operations > 0 {
            let timeout = CLOSE_OPERATIONS_TIMEOUT
                .min(deadline.saturating_duration_since(tokio::time::Instant::now()));
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
        if let Some(mut handle) = handle {
            let timeout = CLOSE_DRAIN_TIMEOUT
                .min(deadline.saturating_duration_since(tokio::time::Instant::now()));
            if tokio::time::timeout(timeout, &mut handle).await.is_err() {
                handle.abort();
                timed_out.push(BusCloseTimeout::Drain);
            }
        }
        let transport = std::mem::take(&mut *lock(&self.inner.transport_errors));
        let mut report = BusCloseReport {
            transport_errors: transport.entries,
            transport_error_count: transport.count,
            transport_errors_truncated: transport.truncated,
            timed_out,
            ..BusCloseReport::default()
        };
        if operations_timed_out {
            report.timed_out.push(BusCloseTimeout::Operations(
                self.inner.in_flight.load(Ordering::Acquire),
            ));
        }
        let session_result = match tokio::time::timeout(
            CLOSE_SESSION_TIMEOUT
                .min(deadline.saturating_duration_since(tokio::time::Instant::now())),
            self.inner.session.close(),
        )
        .await
        {
            Ok(result) => result.map_err(|e| BusError::Transport(e.to_string())),
            Err(_) => {
                report.timed_out.push(BusCloseTimeout::Session);
                Ok(())
            }
        };
        let workers = std::mem::take(&mut *lock(&self.inner.workers));
        let mut timed_out_workers = 0;
        for worker in workers {
            let mut worker = worker;
            match tokio::time::timeout(
                CLOSE_WORKER_TIMEOUT
                    .min(deadline.saturating_duration_since(tokio::time::Instant::now())),
                &mut worker,
            )
            .await
            {
                Ok(result) => {
                    if result.is_err() {
                        report.unjoined_workers = report.unjoined_workers.saturating_add(1);
                    }
                }
                Err(_) => {
                    timed_out_workers += 1;
                    report.unjoined_workers = report.unjoined_workers.saturating_add(1);
                    worker.abort();
                }
            }
        }
        if timed_out_workers > 0 {
            report
                .timed_out
                .push(BusCloseTimeout::Workers(timed_out_workers));
        }
        session_result.map(|()| report)
    }

    pub(crate) fn handle(&self) -> BusHandle {
        BusHandle {
            identity: Arc::clone(&self.inner.identity),
            owner: Arc::downgrade(&self.inner),
            liveness: Arc::downgrade(&self.liveness),
        }
    }
}

/// The Zenoh session identity a run's router opens with.
#[cfg(any(test, feature = "router"))]
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

/// Mint the only production [`ProducerId`] in the workspace.
///
/// The runtime-contract crate intentionally exposes the producer as an opaque
/// value with parsing/conversion only. A producer is a bus-session concern, so
/// its random mint belongs here beside the code that pins it into Zenoh.
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

fn reserve_outbound_bytes(queued: &AtomicUsize, bytes: usize) -> bool {
    queued
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current
                .checked_add(bytes)
                .filter(|next| *next <= OUTBOUND_MAX_BYTES)
        })
        .is_ok()
}

async fn drain_loop(
    session: zenoh::Session,
    mut rx: mpsc::Receiver<Outbound>,
    owner: Weak<BusInner>,
) {
    loop {
        let Some(inner) = owner.upgrade() else {
            return;
        };
        tokio::select! {
            // Shutdown wins over draining so a steady publish stream cannot starve
            // close: on shutdown, flush the finite already-queued set, then stop.
            biased;
            _ = inner.shutdown.notified() => {
                while let Ok(out) = rx.try_recv() {
                    inner.queued_bytes.fetch_sub(out.bytes, Ordering::AcqRel);
                    out.metric.enqueue_finished();
                    put(&session, out, &inner).await;
                }
                break;
            }
            msg = rx.recv() => match msg {
                Some(out) => {
                    inner.queued_bytes.fetch_sub(out.bytes, Ordering::AcqRel);
                    out.metric.enqueue_finished();
                    put(&session, out, &inner).await;
                }
                None => break,
            },
        }
    }
}

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
        .await
    {
        let error = format!("publish on '{}' failed: {e}", out.key);
        record_transport_error(inner, error.clone());
        tracing::warn!(target: "phoxal.bus", key = %out.key, error = %error, "publish failed");
    }
}

fn record_transport_error(inner: &BusInner, error: String) {
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
/// [`crate::router`]. That single source is the point: a router disagreeing
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
    use std::sync::Barrier;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    use serial_test::serial;

    use crate::contract::ContractBody;
    use crate::handle::publisher::StatePublisher;
    use crate::handle::subscriber::{Latest, Subscriber};
    use crate::test_support::{Target, participant_config, step};
    use crate::topic::{Publish, Subscribe, Topic};

    fn test_producer(value: u128) -> ProducerId {
        ProducerId::try_from((1_u128 << 124) | value).expect("canonical test producer")
    }

    #[test]
    fn concurrent_byte_reservations_never_exceed_the_global_limit() {
        const CALLERS: usize = 8;
        let queued = AtomicUsize::new(0);
        let accepted = AtomicUsize::new(0);
        let barrier = Barrier::new(CALLERS);
        let bytes = OUTBOUND_MAX_BYTES / 2 + 1;

        std::thread::scope(|scope| {
            for _ in 0..CALLERS {
                scope.spawn(|| {
                    barrier.wait();
                    if reserve_outbound_bytes(&queued, bytes) {
                        accepted.fetch_add(1, Ordering::Relaxed);
                    }
                });
            }
        });

        assert_eq!(accepted.load(Ordering::Relaxed), 1);
        assert_eq!(queued.load(Ordering::Relaxed), bytes);
        assert!(queued.load(Ordering::Relaxed) <= OUTBOUND_MAX_BYTES);
        assert!(!reserve_outbound_bytes(&queued, usize::MAX));
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
        owner.close().await.unwrap();
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
        owner.close().await.unwrap();
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
        first_owner.close().await.unwrap();

        let (second_owner, second) = BusOwner::open(config).await.unwrap();
        assert_ne!(
            second.producer(),
            first_producer,
            "a reopened session must not reuse its predecessor's provenance"
        );
        second_owner.close().await.unwrap();
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
        owner.close().await.unwrap();
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
        let topic = Topic::<Publish<Target>>::new_static(<Target as ContractBody>::TOPIC);
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
        let subscribe = Topic::<Subscribe<Target>>::new_static(<Target as ContractBody>::TOPIC);
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
        let report = owner.close().await.unwrap();
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
        let report = owner
            .close_until(tokio::time::Instant::now())
            .await
            .unwrap();

        assert!(began.elapsed() < Duration::from_millis(100));
        assert_eq!(report.unjoined_workers, 1);
        assert!(report.timed_out.contains(&BusCloseTimeout::Workers(1)));
        assert!(!report.is_clean());
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
        let report = close.await.unwrap().unwrap();
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
        let report = owner.close().await.unwrap();
        assert!(report.timed_out.contains(&BusCloseTimeout::Operations(1)));
        drop(admitted);
    }

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn transport_evidence_is_bounded_at_utf8_boundaries() {
        let (owner, _bus) = BusOwner::open(participant_config("transport-evidence"))
            .await
            .unwrap();
        for _ in 0..40 {
            record_transport_error(&owner.inner, "é".repeat(2_000));
        }
        let report = owner.close().await.unwrap();
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
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn worker_registration_after_close_is_returned_for_joining() {
        let (owner, bus) = BusOwner::open(participant_config("worker-race"))
            .await
            .unwrap();
        owner.close().await.unwrap();

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
        owner.close().await.unwrap();
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

        let pub_topic = Topic::<Publish<Target>>::new_static(<Target as ContractBody>::TOPIC);
        let sub_topic = Topic::<Subscribe<Target>>::new_static(<Target as ContractBody>::TOPIC);
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

        owner.close().await.unwrap();
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

        let pub_topic = Topic::<Publish<Target>>::new_static(<Target as ContractBody>::TOPIC);
        let sub_topic = Topic::<Subscribe<Target>>::new_static(<Target as ContractBody>::TOPIC);
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

        previous_owner.close().await.unwrap();
        current_owner.close().await.unwrap();
    }

    /// A client attaching to a running robot knows an endpoint and nothing else -
    /// not even which execution is on the other end. The probe is what turns the
    /// endpoint into that execution, and it must report the router's own identity,
    /// not a fresh one.
    #[cfg(feature = "router")]
    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_probe_reports_the_execution_of_the_router_behind_an_endpoint() {
        let (_dir, endpoint) = crate::test_support::socket_endpoint("phoxal-probe-");
        let execution = ExecutionId::mint();
        let router = crate::router::Router::open(execution, std::slice::from_ref(&endpoint), None)
            .await
            .expect("the router binds its endpoint");

        let probed = BusOwner::probe_routers(&endpoint)
            .await
            .expect("a running router must be reachable");
        assert_eq!(
            probed,
            vec![execution],
            "the probe must report exactly the router that is there"
        );

        // The probe owns its whole session: a bus opened afterwards on the same
        // endpoint is unaffected by it having come and gone.
        let (owner, _bus) = BusOwner::open(BusConfig::for_participant(
            execution,
            ParticipantId::new("after-probe").expect("test participant id"),
            vec![endpoint.clone()],
        ))
        .await
        .expect("the probe must leave the endpoint usable");
        assert_eq!(
            BusOwner::probe_routers(&endpoint)
                .await
                .expect("probing again while a bus is open must work"),
            vec![execution],
            "a probe must not disturb - or be disturbed by - an existing session"
        );

        owner.close().await.unwrap();
        router.close().await.unwrap();
    }

    /// Nothing behind the endpoint has to fail promptly and say so: a client
    /// deciding "there is no robot here" cannot wait out a connect-retry budget.
    #[cfg(feature = "router")]
    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_probe_of_a_dead_endpoint_fails_without_waiting_out_a_retry_budget() {
        let (dir, endpoint) = crate::test_support::socket_endpoint("phoxal-probe-dead-");
        drop(dir);

        let started = std::time::Instant::now();
        let probed = BusOwner::probe_routers(&endpoint).await;
        let error = probed.expect_err("an endpoint with nothing behind it is not connectable");
        assert!(
            error.to_string().contains("Unable to connect"),
            "the failure must name the unreachable endpoint: {error}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the probe must not spend the shared connect-retry budget, took {:?}",
            started.elapsed()
        );
    }

    /// A router that is not a Phoxal router answers the probe with a session id
    /// that is not an execution. Reporting that as "no robot here" would be wrong
    /// in the one direction that matters - something *is* listening on that
    /// endpoint - so the probe errors and names the id it could not read.
    ///
    /// The foreign router is spun in-process with a deliberately narrow Zenoh id:
    /// `zenoh::open` accepts short ids, while an execution is pinned to the full
    /// 32-hex width, so `abc` is a legal session id that is not a legal execution.
    #[cfg(feature = "router")]
    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_probe_of_a_router_that_is_not_a_phoxal_execution_fails_naming_the_id() {
        let (_dir, endpoint) = crate::test_support::socket_endpoint("phoxal-probe-foreign-");

        let mut config = zenoh::Config::default();
        let endpoints =
            serde_json::to_string(std::slice::from_ref(&endpoint)).expect("endpoints serialize");
        for (key, value) in [
            // Narrow on purpose: a legal `ZenohId`, never a legal `ExecutionId`.
            ("id", "\"abc\""),
            ("mode", "\"router\""),
            ("listen/endpoints", endpoints.as_str()),
            ("listen/timeout_ms", "0"),
            ("listen/exit_on_failure", "true"),
            ("scouting/delay", "0"),
            ("scouting/multicast/enabled", "false"),
        ] {
            config.insert_json5(key, value).expect("router config key");
        }
        let foreign = zenoh::open(config)
            .await
            .expect("a plain zenoh router binds the endpoint");

        let error = BusOwner::probe_routers(&endpoint)
            .await
            .expect_err("a router whose id is not an execution is not a phoxal robot");
        let message = error.to_string();
        assert!(
            message.contains("abc"),
            "the failure must name the id it could not read: {message}"
        );
        assert!(
            message.contains("phoxal execution"),
            "the failure must say what the id failed to be: {message}"
        );

        foreign.close().await.expect("close the foreign router");
    }
}
