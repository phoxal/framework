//! The Zenoh session wrapper: key root, namespace validation, the non-blocking
//! outbound queue (D43e), and health counters.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;
use zenoh::bytes::{Encoding, ZBytes};
use zenoh::key_expr::OwnedKeyExpr;

use crate::error::{BusError, Result};
use crate::identity::{ExecutionId, ProducerId};
use crate::metadata::{BusMetadata, MAX_SOURCE_PARTICIPANT_BYTES};
use crate::runtime_metrics::{RuntimeMetricHandle, RuntimeMetricSnapshot, RuntimeMetrics};
use crate::time::TimeWindow;

/// Capacity (in samples) of the runner-owned outbound queue. A publish that would
/// exceed this drops the sample and bumps the drop counter - it never blocks the
/// step loop (D35/D43e).
pub(crate) const OUTBOUND_CAPACITY: usize = 1024;

/// Byte bound of the outbound queue (D43e: limits in samples AND bytes). A
/// publish that would exceed it is dropped + counted rather than blocking.
const OUTBOUND_MAX_BYTES: usize = 16 * 1024 * 1024;

/// Connection inputs for opening a bus session.
#[derive(Clone, Debug)]
pub struct BusConfig {
    /// The bus namespace (`identity.namespace`); concrete, non-wildcard (D38).
    pub namespace: String,
    /// The robot id (`identity.id`); one concrete key segment.
    pub robot_id: String,
    /// The supervised run this session joins (#952 section B). It is part of
    /// the key root, so traffic from a previous execution - an ad hoc
    /// publisher, an attached tool, a replayed recording, a second checkout
    /// reusing the namespace - physically cannot be observed as current.
    pub execution: ExecutionId,
    /// The participant id (`ParticipantLaunch.participant_id`, never the static
    /// participant/artifact id - D53). A diagnostic label, never identity.
    pub participant: String,
    /// This process's producer identity. A spawned participant receives a
    /// supervisor-pre-minted one; an ad hoc publisher mints its own.
    pub producer: ProducerId,
    /// Zenoh connect endpoints. Empty = in-process (local sim / tests).
    pub connect_endpoints: Vec<String>,
}

impl BusConfig {
    /// An in-process config (no endpoints, multicast off) for local sim + tests,
    /// on a freshly minted execution.
    pub fn in_process(namespace: impl Into<String>, robot_id: impl Into<String>) -> Self {
        BusConfig {
            namespace: namespace.into(),
            robot_id: robot_id.into(),
            execution: ExecutionId::mint(),
            participant: "local".to_string(),
            producer: ProducerId::mint(),
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

/// A Zenoh session bound to one robot's key root, with a non-blocking publish
/// path and health counters. Cloning shares the underlying session + queue.
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
    /// Open a session, validate the namespace/robot-id, and start the outbound
    /// drain task.
    pub async fn open(config: BusConfig) -> Result<Self> {
        validate_segment(&config.namespace, "identity.namespace", true)?;
        validate_segment(&config.robot_id, "identity.id", false)?;
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
        let root = format!(
            "{}/robots/{}/{}",
            config.namespace,
            config.robot_id,
            config.execution.as_key_segment()
        );
        // Validate the composed root resolves to a legal Zenoh key.
        OwnedKeyExpr::new(root.clone())
            .map_err(|e| BusError::Namespace(format!("invalid key root '{root}': {e}")))?;

        let session = zenoh::open(zenoh_config(&config.connect_endpoints)?)
            .await
            .map_err(|e| BusError::Transport(e.to_string()))?;

        let (tx, rx) = mpsc::channel::<Outbound>(OUTBOUND_CAPACITY);
        let drain_session = session.clone();
        let inner = Arc::new(BusInner {
            session,
            root,
            execution: config.execution,
            participant: config.participant,
            producer: config.producer,
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

    /// The composed key root
    /// (`<namespace>/robots/<robot-id>/x<execution-id>`).
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

    /// This process's producer identity.
    pub fn producer(&self) -> ProducerId {
        self.inner.producer
    }

    /// Build the provenance for one outbound sample: this producer, its next
    /// sequence, and the production instant the caller's temporal role permits.
    pub(crate) fn metadata(&self, produced_at: Option<TimeWindow>) -> BusMetadata {
        BusMetadata {
            codec: crate::abi::CodecId::MessagePack.as_u8(),
            producer: self.inner.producer,
            sequence: self.next_sequence(),
            produced_at,
            participant: self.inner.participant.clone(),
        }
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

    /// Allocate the next per-producer sequence number.
    pub fn next_sequence(&self) -> u64 {
        self.inner.seq.fetch_add(1, Ordering::Relaxed)
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

/// Validate one or more `/`-joined key segments: no empty segments and no Zenoh
/// wildcards. `multi` allows `a/b`; otherwise a single segment is required.
fn validate_segment(value: &str, what: &str, multi: bool) -> Result<()> {
    if value.is_empty() {
        return Err(BusError::Namespace(format!("{what} must not be empty")));
    }
    if !multi && value.contains('/') {
        return Err(BusError::Namespace(format!(
            "{what} must be a single key segment, got '{value}'"
        )));
    }
    for seg in value.split('/') {
        if seg.is_empty() {
            return Err(BusError::Namespace(format!(
                "{what} '{value}' has an empty segment"
            )));
        }
        if seg.contains('*') {
            return Err(BusError::Namespace(format!(
                "{what} '{value}' must be a concrete (non-wildcard) path"
            )));
        }
    }
    Ok(())
}

fn zenoh_config(connect_endpoints: &[String]) -> Result<zenoh::Config> {
    let mut config = zenoh::Config::default();
    // Phoxal-owned links use a nominal three-second lease and Zenoh's documented
    // four keepalives per lease. Apply these after the authored defaults so the
    // runtime contract is explicit at the final configuration boundary.
    config
        .insert_json5("transport/link/tx/lease", "3000")
        .map_err(|error| BusError::Transport(error.to_string()))?;
    config
        .insert_json5("transport/link/tx/keep_alive", "4")
        .map_err(|error| BusError::Transport(error.to_string()))?;
    config
        .insert_json5("scouting/multicast/enabled", "false")
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
    fn phoxal_transport_settings_are_accepted_by_pinned_zenoh_config() {
        zenoh_config(&[]).expect("pinned Zenoh must accept Phoxal lease settings");
        zenoh_config(&["tcp/127.0.0.1:7447".to_string()])
            .expect("pinned Zenoh must accept Phoxal client settings");
    }
}
