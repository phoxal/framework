//! The Zenoh session wrapper: key root, namespace validation, the non-blocking
//! outbound queue (D43e), and health counters.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;
use zenoh::bytes::{Encoding, ZBytes};
use zenoh::key_expr::OwnedKeyExpr;

use crate::bus::error::{BusError, Result};

/// Capacity (in samples) of the runner-owned outbound queue. A publish that would
/// exceed this drops the sample and bumps the drop counter — it never blocks the
/// step loop (D35/D43e). A bytes bound is a documented follow-up.
const OUTBOUND_CAPACITY: usize = 1024;

/// Connection inputs for opening a bus session.
#[derive(Clone, Debug)]
pub struct BusConfig {
    /// The bus namespace (`identity.namespace`); concrete, non-wildcard (D38).
    pub namespace: String,
    /// The robot id (`identity.id`); one concrete key segment.
    pub robot_id: String,
    /// The participant id (`ParticipantLaunch.participant_id`, never the static
    /// runtime/artifact id — D53).
    pub participant: String,
    /// Per-process incarnation (bumped on restart).
    pub incarnation: u64,
    /// Zenoh connect endpoints. Empty = in-process (local sim / tests).
    pub connect_endpoints: Vec<String>,
}

impl BusConfig {
    /// An in-process config (no endpoints, multicast off) for local sim + tests.
    pub fn in_process(namespace: impl Into<String>, robot_id: impl Into<String>) -> Self {
        BusConfig {
            namespace: namespace.into(),
            robot_id: robot_id.into(),
            participant: "local".to_string(),
            incarnation: 0,
            connect_endpoints: Vec::new(),
        }
    }
}

/// Live health counters for one session (D32/D35/D37).
#[derive(Debug, Default)]
pub struct BusHealth {
    /// Samples dropped because the outbound queue was full.
    pub outbound_drops: AtomicU64,
    /// Inbound samples dropped because the ring was full (slow consumer).
    pub inbound_drops: AtomicU64,
    /// Inbound samples rejected for an `api_version` mismatch.
    pub api_mismatches: AtomicU64,
    /// Inbound samples that failed to decode.
    pub decode_errors: AtomicU64,
}

struct Outbound {
    key: String,
    encoding: String,
    attachment: Vec<u8>,
    payload: Vec<u8>,
}

struct BusInner {
    session: zenoh::Session,
    root: String,
    participant: String,
    incarnation: u64,
    seq: AtomicU64,
    outbound: mpsc::Sender<Outbound>,
    shutdown: Notify,
    drain: std::sync::Mutex<Option<JoinHandle<()>>>,
    health: BusHealth,
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

        let root = format!("{}/robots/{}", config.namespace, config.robot_id);
        // Validate the composed root resolves to a legal Zenoh key.
        OwnedKeyExpr::new(root.clone())
            .map_err(|e| BusError::Namespace(format!("invalid key root '{root}': {e}")))?;

        let session = zenoh::open(zenoh_config(&config.connect_endpoints))
            .await
            .map_err(|e| BusError::Transport(e.to_string()))?;

        let (tx, rx) = mpsc::channel::<Outbound>(OUTBOUND_CAPACITY);
        let drain_session = session.clone();
        let inner = Arc::new(BusInner {
            session,
            root,
            participant: config.participant,
            incarnation: config.incarnation,
            seq: AtomicU64::new(0),
            outbound: tx,
            shutdown: Notify::new(),
            drain: std::sync::Mutex::new(None),
            health: BusHealth::default(),
        });

        let drain = tokio::spawn(drain_loop(drain_session, rx, Arc::clone(&inner)));
        *inner.drain.lock().expect("drain mutex poisoned") = Some(drain);

        Ok(Bus { inner })
    }

    /// The composed key root (`<namespace>/robots/<robot-id>`).
    pub fn root(&self) -> &str {
        &self.inner.root
    }

    /// The participant id used as the metadata source.
    pub fn participant(&self) -> &str {
        &self.inner.participant
    }

    /// Per-process incarnation.
    pub fn incarnation(&self) -> u64 {
        self.inner.incarnation
    }

    /// Live health counters.
    pub fn health(&self) -> &BusHealth {
        &self.inner.health
    }

    /// The underlying Zenoh session (for subscriber/queryable declaration).
    pub fn session(&self) -> &zenoh::Session {
        &self.inner.session
    }

    /// Compose a full bus key from a versionless topic key.
    pub fn full_key(&self, topic_key: &str) -> String {
        format!("{}/{}", self.inner.root, topic_key)
    }

    /// Allocate the next per-producer sequence number.
    pub fn next_sequence(&self) -> u64 {
        self.inner.seq.fetch_add(1, Ordering::Relaxed)
    }

    /// Non-blocking enqueue onto the outbound queue. Full queue → drop + counter,
    /// never a block (D35/D43e).
    pub(crate) fn enqueue(
        &self,
        key: String,
        encoding: String,
        attachment: Vec<u8>,
        payload: Vec<u8>,
    ) {
        let outbound = Outbound {
            key,
            encoding,
            attachment,
            payload,
        };
        if self.inner.outbound.try_send(outbound).is_err() {
            self.inner
                .health
                .outbound_drops
                .fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                target: "phoxal.bus",
                participant = %self.inner.participant,
                "outbound queue full; dropped sample (publish never blocks the step loop)"
            );
        }
    }

    /// Flush the outbound queue and close the session.
    pub async fn close(&self) -> Result<()> {
        // `notify_one` stores a permit even if the drain task has not yet
        // registered as a waiter, so the shutdown signal is never lost (a
        // `notify_waiters` here would be dropped if the drain task had not been
        // polled yet — e.g. on a single-worker runtime).
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

async fn drain_loop(
    session: zenoh::Session,
    mut rx: mpsc::Receiver<Outbound>,
    inner: Arc<BusInner>,
) {
    loop {
        tokio::select! {
            biased;
            msg = rx.recv() => match msg {
                Some(out) => put(&session, out).await,
                None => break,
            },
            _ = inner.shutdown.notified() => {
                // Best-effort flush of whatever is already queued, then stop.
                while let Ok(out) = rx.try_recv() {
                    put(&session, out).await;
                }
                break;
            }
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

fn zenoh_config(connect_endpoints: &[String]) -> zenoh::Config {
    let mut config = zenoh::Config::default();
    let _ = config.insert_json5("scouting/multicast/enabled", "false");
    if connect_endpoints.is_empty() {
        // In-process: no listeners, no scouting. A single session still delivers
        // its own publications to its own subscribers.
        let _ = config.insert_json5("listen/endpoints", "[]");
    } else {
        let _ = config.insert_json5("mode", "\"client\"");
        if let Ok(json) = serde_json::to_string(connect_endpoints) {
            let _ = config.insert_json5("connect/endpoints", &json);
        }
    }
    config
}
