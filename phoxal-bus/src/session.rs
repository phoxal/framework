//! The Zenoh session wrapper: the execution-scoped key root, the non-blocking
//! outbound queue (D43e), and health counters.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;
use zenoh::bytes::{Encoding, ZBytes};
use zenoh::key_expr::OwnedKeyExpr;

use crate::error::{BusError, Result};
use crate::identity::{ExecutionId, ProducerId, execution_from_zid, producer_from_zid};
use crate::metadata::{BusMetadata, MAX_SOURCE_PARTICIPANT_BYTES};
use crate::runtime_metrics::{RuntimeMetricHandle, RuntimeMetricSnapshot, RuntimeMetrics};
use crate::time::TimeWindow;

/// First chunk of every Phoxal bus key. It exists so a Phoxal execution is
/// recognisable in a trace and cannot collide with a non-Phoxal key tree
/// sharing the same Zenoh fabric.
const BUS_KEY_PREFIX: &str = "phoxal";

/// Capacity (in samples) of the runner-owned outbound queue. A publish that would
/// exceed this drops the sample and bumps the drop counter - it never blocks the
/// step loop (D35/D43e).
pub(crate) const OUTBOUND_CAPACITY: usize = 1024;

/// Byte bound of the outbound queue (D43e: limits in samples AND bytes). A
/// publish that would exceed it is dropped + counted rather than blocking.
const OUTBOUND_MAX_BYTES: usize = 16 * 1024 * 1024;

/// Connection inputs for opening a bus session.
///
/// The execution is the *only* routing input. A robot's namespace and id are
/// model data: two robots in one execution are two participants under one root,
/// and two executions never share a key however they are named.
#[derive(Clone, Debug)]
pub struct BusConfig {
    /// The supervised run this session joins (#952 section B). It is the key
    /// root, so traffic from a previous execution - an ad hoc publisher, an
    /// attached tool, a replayed recording, a second checkout of the same
    /// project - physically cannot be observed as current.
    pub execution: ExecutionId,
    /// The participant id (`ParticipantLaunch.participant_id`, never the static
    /// participant/artifact id - D53). A diagnostic label, never identity.
    pub participant: String,
    /// Zenoh connect endpoints. Empty = in-process (local sim / tests).
    pub connect_endpoints: Vec<String>,
}

impl BusConfig {
    /// An in-process config (no endpoints, multicast off) for local sim + tests,
    /// on a freshly minted execution.
    pub fn in_process(participant: impl Into<String>) -> Self {
        BusConfig {
            execution: ExecutionId::mint(),
            participant: participant.into(),
            connect_endpoints: Vec::new(),
        }
    }

    /// Join `execution` instead of a freshly minted one.
    #[must_use]
    pub fn in_execution(mut self, execution: ExecutionId) -> Self {
        self.execution = execution;
        self
    }
}

/// Live health counters for one session (D32/D35/D37).
#[derive(Debug, Default)]
pub struct BusHealth {
    /// Samples dropped because the outbound queue was full.
    pub outbound_drops: AtomicU64,
    /// Inbound samples dropped because the ring was full (slow consumer).
    pub inbound_drops: AtomicU64,
    /// Inbound samples that failed to decode (D1: identity now lives in the key,
    /// so decode failures are the only remaining rejection - there is no
    /// separate schema/family mismatch counter).
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
    root: String,
    execution: ExecutionId,
    participant: String,
    producer: ProducerId,
    seq: AtomicU64,
    outbound: mpsc::Sender<Outbound>,
    queued_bytes: AtomicUsize,
    closing: AtomicBool,
    shutdown: Notify,
    drain: std::sync::Mutex<Option<JoinHandle<()>>>,
    health: BusHealth,
    runtime_metrics: RuntimeMetrics,
}

/// A Zenoh session bound to one execution's key root, with a non-blocking
/// publish path and health counters. Cloning shares the underlying session,
/// queue, producer identity, and sequence allocator.
#[derive(Clone)]
pub struct Bus {
    inner: Arc<BusInner>,
}

impl std::fmt::Debug for Bus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bus")
            .field("root", &self.inner.root)
            .field("participant", &self.inner.participant)
            .finish_non_exhaustive()
    }
}

impl Bus {
    /// Open a session, compose its execution-scoped key root, adopt the
    /// producer identity Zenoh assigned the session, and start the outbound
    /// drain task.
    pub async fn open(config: BusConfig) -> Result<Self> {
        if config.participant.is_empty() {
            return Err(BusError::Namespace(
                "participant id must not be empty".to_string(),
            ));
        }
        if config.participant.len() > MAX_SOURCE_PARTICIPANT_BYTES {
            return Err(BusError::Namespace(format!(
                "participant id exceeds the {MAX_SOURCE_PARTICIPANT_BYTES}-byte limit"
            )));
        }

        // Execution scoping lives in the *root*, not in any contract name: a
        // previous run's traffic lands on a different key and cannot be
        // observed as current (#952 section B).
        let root = format!("{BUS_KEY_PREFIX}/{}", config.execution);
        // Validate the composed root resolves to a legal Zenoh key.
        OwnedKeyExpr::new(root.clone())
            .map_err(|e| BusError::Namespace(format!("invalid key root '{root}': {e}")))?;

        let session = zenoh::open(zenoh_config(&config.connect_endpoints)?)
            .await
            .map_err(|e| BusError::Transport(e.to_string()))?;
        // The producer is the session, so it exists only once the session does.
        // Everything that carries provenance is built below this line.
        let producer = producer_from_zid(session.zid())?;

        let (tx, rx) = mpsc::channel::<Outbound>(OUTBOUND_CAPACITY);
        let drain_session = session.clone();
        let inner = Arc::new(BusInner {
            session,
            root,
            execution: config.execution,
            participant: config.participant,
            producer,
            seq: AtomicU64::new(0),
            outbound: tx,
            queued_bytes: AtomicUsize::new(0),
            closing: AtomicBool::new(false),
            shutdown: Notify::new(),
            drain: std::sync::Mutex::new(None),
            health: BusHealth::default(),
            runtime_metrics: RuntimeMetrics::default(),
        });

        let drain = tokio::spawn(drain_loop(drain_session, rx, Arc::clone(&inner)));
        *inner.drain.lock().expect("drain mutex poisoned") = Some(drain);

        Ok(Bus { inner })
    }

    /// The executions whose routers a session at `endpoint` is *directly*
    /// connected to.
    ///
    /// This opens and closes its own short-lived session rather than taking a
    /// `&self`, because it answers the question that has to be settled *before*
    /// a [`Bus`] can exist: a bus is execution-scoped, and the execution is
    /// what this reports. The session is independent of any other in the
    /// process - opening and closing it disturbs nothing.
    ///
    /// It shares the Phoxal transport policy, so multicast scouting is off and
    /// only routers reachable through `endpoint` are ever reported. Connect
    /// retry is deliberately *not* shared: the answer is "what is connected
    /// now", so an endpoint with nothing behind it fails immediately instead of
    /// spending [`CONNECT_TIMEOUT_MS`] hoping a router appears.
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

    /// The composed key root (`phoxal/<execution-id>`).
    pub fn root(&self) -> &str {
        &self.inner.root
    }

    /// The supervised run this session belongs to.
    pub fn execution(&self) -> ExecutionId {
        self.inner.execution
    }

    /// The participant id carried as a diagnostic label in sample metadata.
    pub fn participant(&self) -> &str {
        &self.inner.participant
    }

    /// This session's producer identity - the id Zenoh assigned the session.
    ///
    /// The guarantee is per *session incarnation*, not per process: a process
    /// that closes its bus and opens another is a different producer, which is
    /// exactly the intended reading, because the second session's sequence
    /// starts at zero again.
    pub fn producer(&self) -> ProducerId {
        self.inner.producer
    }

    /// Build the provenance for one outbound sample: this producer, its next
    /// sequence, and the production instant the caller's temporal role permits.
    pub(crate) fn metadata(&self, produced_at: Option<TimeWindow>) -> Result<BusMetadata> {
        Ok(BusMetadata {
            codec: crate::abi::CodecId::MessagePack.as_u8(),
            producer: self.inner.producer,
            sequence: self.next_sequence()?,
            produced_at,
            participant: self.inner.participant.clone(),
        })
    }

    /// Live health counters.
    pub fn health(&self) -> &BusHealth {
        &self.inner.health
    }

    #[doc(hidden)]
    pub fn take_runtime_metrics(&self) -> Vec<RuntimeMetricSnapshot> {
        self.inner.runtime_metrics.take()
    }

    pub(crate) fn runtime_metrics(&self) -> &RuntimeMetrics {
        &self.inner.runtime_metrics
    }

    /// The underlying Zenoh session (for subscriber/queryable declaration).
    pub fn session(&self) -> &zenoh::Session {
        &self.inner.session
    }

    /// Compose a full bus key from a version-qualified topic key.
    pub fn full_key(&self, topic_key: &str) -> String {
        format!("{}/{}", self.inner.root, topic_key)
    }

    /// Allocate the next sequence number for this session's producer.
    ///
    /// One session has exactly one producer and exactly one allocator, so every
    /// sample published under this identity - from any clone of this `Bus`, on
    /// any topic - draws from the same strictly increasing stream. Two counters
    /// behind one identity would each start at zero and the second would be
    /// rejected downstream as a replay.
    pub fn next_sequence(&self) -> Result<u64> {
        allocate_sequence(&self.inner.seq)
    }

    /// Non-blocking enqueue onto the outbound queue. A full queue (samples or
    /// bytes) drops the sample, bumps the drop counter, and returns
    /// `Saturated` - it never blocks the step loop (D35/D43e).
    pub(crate) fn enqueue(
        &self,
        key: String,
        encoding: String,
        attachment: Vec<u8>,
        payload: Vec<u8>,
        metric: RuntimeMetricHandle,
    ) -> Result<()> {
        if self.inner.closing.load(Ordering::Acquire) {
            return Err(BusError::Closed);
        }
        let bytes = key.len() + encoding.len() + attachment.len() + payload.len();

        // Atomically reserve the bytes *before* making the item visible to the
        // drain. A CAS loop makes the limit a global invariant across cloned
        // Bus publishers; add-then-check would let concurrent callers each
        // observe an individually valid pre-add value and collectively exceed it.
        if !reserve_outbound_bytes(&self.inner.queued_bytes, bytes) {
            metric.record_drop();
            return Err(self.dropped(&key, "byte bound"));
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
        match self.inner.outbound.try_send(outbound) {
            Ok(()) => {
                metric.record_message();
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(out)) => {
                self.inner.queued_bytes.fetch_sub(bytes, Ordering::AcqRel);
                out.metric.enqueue_finished();
                out.metric.record_drop();
                Err(self.dropped(&out.key, "sample bound"))
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.inner.queued_bytes.fetch_sub(bytes, Ordering::AcqRel);
                metric.enqueue_finished();
                Err(BusError::Closed)
            }
        }
    }

    fn dropped(&self, key: &str, detail: &str) -> BusError {
        self.inner
            .health
            .outbound_drops
            .fetch_add(1, Ordering::Relaxed);
        tracing::warn!(
            target: "phoxal.bus",
            participant = %self.inner.participant,
            key,
            detail,
            "outbound queue saturated; dropped sample (publish never blocks the step loop)"
        );
        BusError::Saturated {
            topic: key.to_string(),
            detail: detail.to_string(),
        }
    }

    /// Flush the outbound queue and close the session.
    pub async fn close(&self) -> Result<()> {
        // Stop accepting new samples first so the drain set is finite, then signal
        // the drain task. `notify_one` stores a permit even if the drain task has
        // not yet registered as a waiter, so the shutdown signal is never lost (a
        // `notify_waiters` here would be dropped if the drain task had not been
        // polled yet - e.g. on a single-worker runtime).
        self.inner.closing.store(true, Ordering::Release);
        self.inner.shutdown.notify_one();
        let handle = self
            .inner
            .drain
            .lock()
            .expect("drain mutex poisoned")
            .take();
        if let Some(handle) = handle {
            let _ = handle.await;
        }
        self.inner
            .session
            .close()
            .await
            .map_err(|e| BusError::Transport(e.to_string()))?;
        Ok(())
    }
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

#[allow(deprecated)]
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
    inner: Arc<BusInner>,
) {
    loop {
        tokio::select! {
            // Shutdown wins over draining so a steady publish stream cannot starve
            // close: on shutdown, flush the finite already-queued set, then stop.
            biased;
            _ = inner.shutdown.notified() => {
                while let Ok(out) = rx.try_recv() {
                    inner.queued_bytes.fetch_sub(out.bytes, Ordering::AcqRel);
                    out.metric.enqueue_finished();
                    put(&session, out).await;
                }
                break;
            }
            msg = rx.recv() => match msg {
                Some(out) => {
                    inner.queued_bytes.fetch_sub(out.bytes, Ordering::AcqRel);
                    out.metric.enqueue_finished();
                    put(&session, out).await;
                }
                None => break,
            },
        }
    }
}

async fn put(session: &zenoh::Session, out: Outbound) {
    let key = match OwnedKeyExpr::new(out.key.clone()) {
        Ok(key) => key,
        Err(e) => {
            tracing::error!(target: "phoxal.bus", key = %out.key, error = %e, "invalid publish key");
            return;
        }
    };
    if let Err(e) = session
        .put(key, ZBytes::from(out.payload))
        .encoding(Encoding::from(out.encoding))
        .attachment(out.attachment)
        .await
    {
        tracing::warn!(target: "phoxal.bus", key = %out.key, error = %e, "publish failed");
    }
}

/// Bound on how long a client-mode `Bus::open` retries a failed connect
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

fn zenoh_config(connect_endpoints: &[String]) -> Result<zenoh::Config> {
    let mut config = zenoh::Config::default();
    apply_phoxal_transport_policy(&mut config)?;
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
        zenoh_config(&[]).expect("pinned Zenoh must accept Phoxal lease settings");
        zenoh_config(&["tcp/127.0.0.1:7447".to_string()])
            .expect("pinned Zenoh must accept Phoxal client settings");
    }

    #[test]
    fn client_mode_bounds_the_connect_retry_but_in_process_does_not() {
        // In-process (no endpoints) never dials anything, so there is nothing
        // to retry - the key must be absent, not merely zero, so it cannot be
        // mistaken for "connect once, no retry" on a config path that never
        // connects at all.
        let in_process = zenoh_config(&[]).expect("in-process config");
        assert_eq!(
            in_process
                .get_json("connect/timeout_ms")
                .expect("key is always present, unresolved by default"),
            "null",
            "in-process config must leave Zenoh's mode-dependent default \
             untouched - it never dials anything, so there is nothing to \
             bound",
        );

        // Client mode (this is what a real launch uses, D-participant#run_with)
        // must carry the bounded, nonzero timeout that switches Zenoh onto its
        // own retry-with-backoff path - see CONNECT_TIMEOUT_MS's docs for why
        // this exact value.
        let client = zenoh_config(&["tcp/127.0.0.1:7447".to_string()]).expect("client config");
        assert_eq!(
            client
                .get_json("connect/timeout_ms")
                .expect("client config must set a connect timeout"),
            CONNECT_TIMEOUT_MS.to_string(),
        );
    }
}
