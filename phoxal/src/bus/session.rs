//! The supervisor-owned private Zenoh session.
//!
//! This is deliberately only a transport owner. Payload identity, schemas,
//! and public operations live in the generated communication modules. The
//! private session contributes an execution-scoped key prefix and a bounded
//! lifetime, but no catalogue, codec registry, or metadata envelope.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use crate::bus::error::{BusError, Result};
use crate::identity::{ExecutionId, ParticipantId, ProducerId};
use tokio::task::JoinHandle;
use zenoh::config::ZenohId;

/// First key segment owned by Phoxal's private execution transport.
pub(crate) const BUS_KEY_PREFIX: &str = "phoxal";

/// Runtime configuration for one private Zenoh owner.
#[derive(Clone, Debug)]
pub(crate) struct BusConfig {
    execution: ExecutionId,
    connect_endpoints: Vec<String>,
}

impl BusConfig {
    /// Configure a process joining one execution.
    pub(crate) fn for_participant(
        execution: ExecutionId,
        _participant: ParticipantId,
        connect_endpoints: Vec<String>,
    ) -> Self {
        Self {
            execution,
            connect_endpoints,
        }
    }

    /// Configure a supervisor or external owner joining one execution.
    #[allow(dead_code, reason = "used by the supervisor profile")]
    pub(crate) fn for_external(
        execution: ExecutionId,
        _label: Option<String>,
        connect_endpoints: Vec<String>,
    ) -> Self {
        Self {
            execution,
            connect_endpoints,
        }
    }
}

struct BusInner {
    session: zenoh::Session,
    execution: ExecutionId,
    producer: ProducerId,
    next_sequence: AtomicU64,
    closed: AtomicBool,
    workers: std::sync::Mutex<Vec<RegisteredWorker>>,
}

/// The unique owner of one private Zenoh session.
pub(crate) struct BusOwner {
    inner: Arc<BusInner>,
}

/// A cloneable handle borrowing the owner while it remains alive.
#[derive(Clone)]
pub(crate) struct BusHandle {
    inner: Weak<BusInner>,
    execution: ExecutionId,
    root: String,
    producer: ProducerId,
}

struct RegisteredWorker {
    name: String,
    expected: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

impl std::fmt::Debug for BusOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BusOwner")
            .field("execution", &self.inner.execution)
            .field("producer", &self.inner.producer)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for BusHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BusHandle")
            .field("execution", &self.execution)
            .field("producer", &self.producer)
            .finish_non_exhaustive()
    }
}

impl Drop for BusOwner {
    fn drop(&mut self) {
        self.inner.closed.store(true, Ordering::Release);
        for worker in std::mem::take(&mut *lock_unpoisoned(&self.inner.workers)) {
            worker.handle.abort();
        }
    }
}

impl BusOwner {
    /// Open one private Zenoh session and return its borrowed handle.
    pub(crate) async fn open(config: BusConfig) -> Result<(Self, BusHandle)> {
        let producer = mint_producer_id()?;
        let session = zenoh::open(zenoh_config(&config.connect_endpoints, producer)?)
            .await
            .map_err(|error| BusError::Transport(error.to_string()))?;
        let observed = producer_from_zid(session.zid())?;
        if observed != producer {
            let _ = session.close().await;
            return Err(BusError::SessionIdentityMismatch {
                expected: producer.to_string(),
                observed: observed.to_string(),
            });
        }
        let root = format!("{BUS_KEY_PREFIX}/{}", config.execution);
        let inner = Arc::new(BusInner {
            session,
            execution: config.execution,
            producer,
            next_sequence: AtomicU64::new(0),
            closed: AtomicBool::new(false),
            workers: std::sync::Mutex::new(Vec::new()),
        });
        let handle = BusHandle {
            inner: Arc::downgrade(&inner),
            execution: config.execution,
            root,
            producer,
        };
        Ok((Self { inner }, handle))
    }

    /// Discover the execution identities of routers directly reachable from
    /// one endpoint.
    #[allow(dead_code, reason = "used by the supervisor profile")]
    pub(crate) async fn probe_routers(endpoint: &str) -> Result<Vec<ExecutionId>> {
        let session = zenoh::open(client_config(endpoint)?)
            .await
            .map_err(|error| BusError::Transport(error.to_string()))?;
        let ids = session.info().routers_zid().await.collect::<Vec<_>>();
        session
            .close()
            .await
            .map_err(|error| BusError::Transport(error.to_string()))?;
        ids.into_iter().map(execution_from_zid).collect()
    }

    /// Close the owner session and retain any transport error as evidence.
    pub(crate) async fn close(self) -> BusCloseReport {
        self.inner.closed.store(true, Ordering::Release);
        let workers = std::mem::take(&mut *lock_unpoisoned(&self.inner.workers));
        let mut worker_errors = Vec::new();
        for worker in workers {
            worker.expected.store(true, Ordering::Release);
            let name = worker.name;
            let mut handle = worker.handle;
            match tokio::time::timeout(Duration::from_secs(1), &mut handle).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => worker_errors.push(format!("{name}: {error}")),
                Err(_) => {
                    handle.abort();
                    worker_errors.push(format!("{name}: close timed out"));
                }
            }
        }
        let session_close_error = self
            .inner
            .session
            .close()
            .await
            .err()
            .map(|error| error.to_string());
        BusCloseReport {
            session_close_error,
            worker_errors,
        }
    }
}

impl BusHandle {
    fn live(&self) -> Result<Arc<BusInner>> {
        let inner = self.inner.upgrade().ok_or(BusError::Closed)?;
        if inner.closed.load(Ordering::Acquire) {
            return Err(BusError::Closed);
        }
        Ok(inner)
    }

    /// The Zenoh session, cloned for one bounded operation.
    pub(crate) fn session(&self) -> Result<zenoh::Session> {
        Ok(self.live()?.session.clone())
    }

    /// The execution this handle joined.
    pub(crate) fn execution(&self) -> ExecutionId {
        self.execution
    }

    /// The producer identity assigned to this session.
    #[allow(dead_code, reason = "kept for the runtime delivery boundary")]
    pub(crate) fn producer(&self) -> ProducerId {
        self.producer
    }

    /// Compose a key below the execution root.
    pub(crate) fn full_key(&self, relative: &str) -> String {
        format!("{}/{}", self.root, relative.trim_start_matches('/'))
    }

    /// Current owner state, used by bounded runtime cleanup paths.
    pub(crate) fn terminal(&self) -> BusTerminal {
        self.inner.upgrade().map_or(BusTerminal::Closed, |inner| {
            if inner.closed.load(Ordering::Acquire) {
                BusTerminal::Closed
            } else {
                BusTerminal::Open
            }
        })
    }

    /// Allocate one producer-local sequence for a generated transport record.
    #[allow(dead_code, reason = "kept for the runtime delivery boundary")]
    pub(crate) fn next_sequence(&self) -> Result<u64> {
        let inner = self.live()?;
        inner
            .next_sequence
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| BusError::SequenceExhausted)
    }

    /// Register one transport receiver under the owner that must fence it.
    ///
    /// Runtime delivery receivers are the only background workers that cross
    /// the process boundary. Keeping their join handles with the owner makes
    /// orderly close and synchronous drop deterministic without reviving the
    /// removed outbound scheduler or bus-wide health machinery.
    pub(crate) fn register_named_worker(
        &self,
        name: impl Into<String>,
        expected: Arc<AtomicBool>,
        worker: JoinHandle<()>,
    ) -> std::result::Result<(), JoinHandle<()>> {
        let inner = match self.inner.upgrade() {
            Some(inner) if !inner.closed.load(Ordering::Acquire) => inner,
            _ => return Err(worker),
        };
        let mut workers = lock_unpoisoned(&inner.workers);
        if inner.closed.load(Ordering::Acquire) {
            return Err(worker);
        }
        workers.push(RegisteredWorker {
            name: name.into(),
            expected,
            handle: worker,
        });
        Ok(())
    }
}

/// Transport state exposed to private runtime cleanup paths.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BusTerminal {
    Open,
    Closed,
}

/// Bounded evidence from closing a private owner.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct BusCloseReport {
    pub(crate) session_close_error: Option<String>,
    pub(crate) worker_errors: Vec<String>,
}

impl std::fmt::Display for BusCloseReport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.session_close_error, self.worker_errors.is_empty()) {
            (Some(error), true) => write!(formatter, "private transport close failed: {error}"),
            (None, true) => formatter.write_str("private transport closed cleanly"),
            (session_error, false) => {
                if let Some(error) = session_error {
                    write!(formatter, "private transport close failed: {error}; ")?;
                }
                write!(
                    formatter,
                    "runtime workers failed to close: {:?}",
                    self.worker_errors
                )
            }
        }
    }
}

impl BusCloseReport {
    /// Whether the owner closed without a transport error.
    #[must_use]
    #[allow(dead_code, reason = "used by the supervisor profile")]
    pub(crate) fn is_clean(&self) -> bool {
        self.session_close_error.is_none() && self.worker_errors.is_empty()
    }
}

fn lock_unpoisoned<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Convert a Phoxal execution identity to Zenoh's session identity.
#[allow(dead_code, reason = "used by the supervisor profile")]
pub(crate) fn zenoh_id_for(execution: ExecutionId) -> Result<ZenohId> {
    ZenohId::try_from(&u128::from(execution).to_le_bytes()[..])
        .map_err(|error| BusError::Transport(format!("execution {execution}: {error}")))
}

/// Convert a router Zenoh identity to an execution identity.
#[allow(dead_code, reason = "used by the supervisor profile")]
pub(crate) fn execution_from_zid(zid: ZenohId) -> Result<ExecutionId> {
    ExecutionId::try_from(u128::from_le_bytes(zid.to_le_bytes())).map_err(|_| {
        BusError::ForeignSessionId {
            value: zid.to_string(),
            role: "execution",
        }
    })
}

fn producer_from_zid(zid: ZenohId) -> Result<ProducerId> {
    ProducerId::try_from(u128::from_le_bytes(zid.to_le_bytes())).map_err(|_| {
        BusError::ForeignSessionId {
            value: zid.to_string(),
            role: "producer",
        }
    })
}

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

#[allow(dead_code, reason = "used by the supervisor profile")]
pub(crate) fn client_config(endpoint: &str) -> Result<zenoh::Config> {
    let mut config = zenoh::Config::default();
    apply_phoxal_transport_policy(&mut config)?;
    let endpoints = serde_json::to_string(std::slice::from_ref(&endpoint))
        .map_err(|error| BusError::Transport(error.to_string()))?;
    config
        .insert_json5("mode", "\"client\"")
        .map_err(|error| BusError::Transport(error.to_string()))?;
    config
        .insert_json5("connect/endpoints", &endpoints)
        .map_err(|error| BusError::Transport(error.to_string()))?;
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
        config
            .insert_json5("listen/endpoints", "[]")
            .map_err(|error| BusError::Transport(error.to_string()))?;
    } else {
        config
            .insert_json5("mode", "\"client\"")
            .map_err(|error| BusError::Transport(error.to_string()))?;
        let endpoints = serde_json::to_string(connect_endpoints)
            .map_err(|error| BusError::Transport(error.to_string()))?;
        config
            .insert_json5("connect/endpoints", &endpoints)
            .map_err(|error| BusError::Transport(error.to_string()))?;
    }
    Ok(config)
}
