//! Body-typed handles over the version-qualified bus boundary (D35).
//!
//! - [`Publisher<B>`] - MessagePack-encodes the plain body and enqueues it on the
//!   non-blocking outbound queue (a publish never blocks the step loop).
//! - [`Subscriber<B>`] - a drop-oldest ring (depth 32 by default) of decoded
//!   bodies, for consumers that want a short backlog under congestion.
//! - [`Latest<B>`] - keep-last-1: only the most recent decoded body is retained.
//! - [`Querier<Req, Resp>`] - the caller side of the request/response leg,
//!   returning `Result<Resp, `[`QueryError`]`>`.
//!
//! # Periodic-state QoS
//!
//! Pub/sub here is tuned for periodic state streams, where the freshest sample
//! matters more than every sample arriving. Both ends shed load instead of
//! blocking or growing without bound:
//!
//! - **Publish never blocks.** [`Publisher::publish_at`] is just
//!   [`Publisher::try_publish`]: it MessagePack-encodes the body and enqueues it
//!   on the bounded outbound queue, returning immediately. A saturated queue
//!   (sample or byte bound) drops the sample, bumps `outbound_drops`, and returns
//!   [`BusError::Saturated`] so the caller can observe the loss - it never stalls
//!   the step loop (D35/D43e). There is no reliable/blocking publish variant.
//! - **Receivers bound their backlog.** [`Latest<B>`] keeps only the last sample
//!   (keep-last-1); [`Subscriber<B>`] keeps a drop-oldest ring, evicting the
//!   oldest buffered sample and bumping `inbound_drops` when a slow consumer lets
//!   the ring fill. Choose `Latest` when only current state matters and
//!   `Subscriber` when a bounded history is useful.
//!
//! Identity now lives entirely in the Zenoh key (D1: the version is folded
//! into `<Body as ContractBody>::TOPIC`), so a receiver's per-key subscription
//! is the fast-reject; the decode path only still validates the codec before
//! touching the payload. A decode failure is counted (`decode_errors`) + logged
//! as a health signal, never a silent accept. Epoch-aware handles separately
//! count purged or retired-execution samples in `epoch_filtered`, so quarantine
//! churn is not confused with active-buffer loss.

use std::collections::VecDeque;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::task::JoinHandle;
use zenoh::bytes::Encoding;
use zenoh::key_expr::OwnedKeyExpr;
use zenoh::sample::Sample;

use crate::abi::{CodecId, encoding_string, parse_encoding_string};
use crate::codec::{Codec, MessagePack};
use crate::contract::ContractBody;
use crate::error::{BusError, Result};
use crate::metadata::{BusMetadata, Source};
use crate::query::{QueryError, QueryFailure};
use crate::runtime_metrics::RuntimeMetricHandle;
use crate::session::Bus;
use crate::session::OUTBOUND_CAPACITY;
use crate::topic::{AskQuery, Publish, Subscribe, Topic};
use crate::{LogicalTime, RetiredEpochs};

/// The Phoxal-pinned finite query timeout (D31) - not Zenoh's 10 s default.
pub const DEFAULT_QUERY_TIMEOUT: Duration = Duration::from_secs(5);

/// Foreign-epoch samples are quarantined for a small number of possible next
/// executions. Each epoch remains bounded by the receiving handle's ordinary
/// capacity, and active-epoch data always has its own independent storage.
const PENDING_EPOCH_CAPACITY: usize = 4;

/// Publishes plain bodies of `B` on `B`'s version-qualified key (D1); the
/// [`BusMetadata`] attachment carries only provenance (source + logical time)
/// and the codec, never identity (D62). A publish is a non-blocking enqueue, so
/// it is safe to call from the step loop (D35/D43e).
pub struct Publisher<B> {
    bus: Bus,
    key: String,
    metric: RuntimeMetricHandle,
    _body: PhantomData<fn() -> B>,
}

// Manual (not `#[derive(Clone)]`) so cloning a `Publisher<B>` never spuriously
// requires `B: Clone` - every field it actually holds (`Bus`, `String`,
// `PhantomData<fn() -> B>`) is `Clone` regardless of `B`. All real operations
// take `&self` (`try_publish`/`publish_at`), so a clone is just a second
// handle to the same publish key on the same session - safe to hand to a
// concurrent task (the new-model runner's `Arc<Self::Api>` snapshot-sharing,
// D3, relies on every `Api` field type being cheaply `Clone` this way).
impl<B> Clone for Publisher<B> {
    fn clone(&self) -> Self {
        Publisher {
            bus: self.bus.clone(),
            key: self.key.clone(),
            metric: self.metric.clone(),
            _body: PhantomData,
        }
    }
}

impl<B: ContractBody> Publisher<B> {
    /// Framework-internal (macro/runner-only): build a publisher over a topic.
    /// The author-facing path is `ctx.publisher(...)` in `#[setup]`. `#[doc(hidden)]`.
    #[doc(hidden)]
    pub fn new(bus: Bus, topic: &Topic<Publish<B>>) -> Result<Self> {
        let topic_key = topic.publish_key()?;
        let metric = bus
            .runtime_metrics()
            .register_outbound(topic_key, OUTBOUND_CAPACITY);
        let key = bus.full_key(topic_key);
        Ok(Publisher {
            bus,
            key,
            metric,
            _body: PhantomData,
        })
    }

    /// Publish `body` stamped at logical time `at`.
    ///
    /// The `async` form for symmetry with the rest of the bus surface; it does no
    /// awaiting and is exactly [`try_publish`](Self::try_publish). Non-blocking
    /// (D35/D43e): it returns immediately and reports `Saturated`/`Closed` rather
    /// than silently dropping, so loss is on the caller's error path.
    #[allow(clippy::unused_async)]
    pub async fn publish_at(&self, at: LogicalTime, body: B) -> Result<()> {
        self.try_publish(at, body)
    }

    /// The explicit non-blocking publish op (D43e).
    ///
    /// Encodes `body`, builds the [`BusMetadata`] (codec, `at`'s epoch +
    /// `produced_at_ns`, and this producer's next sequence), and enqueues it on
    /// the outbound queue. Returns immediately. A saturated outbound queue
    /// (sample or byte bound) returns [`BusError::Saturated`] - the sample was
    /// dropped and `outbound_drops` bumped - so the caller can observe the loss;
    /// a closed session returns [`BusError::Closed`].
    pub fn try_publish(&self, at: LogicalTime, body: B) -> Result<()> {
        let payload = MessagePack::encode(&body)?;
        let metadata = BusMetadata {
            codec: MessagePack::ID.as_u8(),
            produced_at_ns: at.time_ns(),
            epoch: at.epoch(),
            source: Source {
                participant: self.bus.participant().to_string(),
                incarnation: self.bus.incarnation(),
                sequence: self.bus.next_sequence(),
            },
        };
        let encoding = encoding_string(MessagePack::ID);
        self.bus.enqueue(
            self.key.clone(),
            encoding,
            metadata.encode(),
            payload,
            self.metric.clone(),
        )
    }
}

/// Issues queries on an exclusive query topic and returns
/// `Result<Resp, QueryError>` (D31).
///
/// The caller side of the request/response leg. A query carries a finite,
/// Phoxal-pinned [`timeout`](DEFAULT_QUERY_TIMEOUT) - not Zenoh's 10 s default -
/// and expects exactly one responder (an exclusive topic, D31/D43f):
///
/// - a success reply decodes to the plain `Resp` body;
/// - a handler error rides Zenoh's native `ReplyError` and surfaces as
///   [`QueryError::Server`] carrying the [`QueryFailure`];
/// - the deadline elapsing with no reply is [`QueryError::Timeout`], and the
///   reply stream closing with no reply is [`QueryError::Unavailable`];
/// - a second reply (a duplicate responder, also a `phoxal-cli check` topology
///   error) is [`QueryError::TooManyResponders`].
pub struct Querier<Req, Resp> {
    bus: Bus,
    key: String,
    timeout: Duration,
    _p: PhantomData<fn() -> (Req, Resp)>,
}

// Manual, unbounded on `Req`/`Resp` - see `Publisher`'s `Clone` impl docs for
// why (identical reasoning: `query` takes `&self`, so a clone is just a
// second handle to the same query key).
impl<Req, Resp> Clone for Querier<Req, Resp> {
    fn clone(&self) -> Self {
        Querier {
            bus: self.bus.clone(),
            key: self.key.clone(),
            timeout: self.timeout,
            _p: PhantomData,
        }
    }
}

impl<Req, Resp> Querier<Req, Resp>
where
    Req: ContractBody,
    Resp: ContractBody,
{
    /// Framework-internal (macro/runner-only): build a querier over a query topic.
    /// The author-facing path is `ctx.querier(...)` in `#[setup]`. `#[doc(hidden)]`.
    #[doc(hidden)]
    pub fn new(bus: Bus, topic: &Topic<AskQuery<Req, Resp>>, timeout: Duration) -> Result<Self> {
        let key = bus.full_key(topic.publish_key()?);
        Ok(Querier {
            bus,
            key,
            timeout,
            _p: PhantomData,
        })
    }

    /// Issue a query and await the single response (or a typed error).
    ///
    /// The request body is MessagePack-encoded with mirroring metadata; requests
    /// carry no logical time, so `produced_at_ns`/`epoch` are `0`. The wait is
    /// bounded by this querier's timeout.
    pub async fn query(&self, request: Req) -> std::result::Result<Resp, QueryError> {
        let payload =
            MessagePack::encode(&request).map_err(|e| QueryError::Protocol(e.to_string()))?;
        let metadata = BusMetadata {
            codec: MessagePack::ID.as_u8(),
            produced_at_ns: 0,
            epoch: 0,
            source: Source {
                participant: self.bus.participant().to_string(),
                incarnation: self.bus.incarnation(),
                sequence: self.bus.next_sequence(),
            },
        };
        let key = OwnedKeyExpr::new(self.key.clone())
            .map_err(|e| QueryError::Protocol(format!("invalid query key '{}': {e}", self.key)))?;

        let replies = self
            .bus
            .session()
            .get(key)
            .payload(payload)
            .encoding(Encoding::from(encoding_string(MessagePack::ID)))
            .attachment(metadata.encode())
            // Target ALL matching responders (not just BestMatching) and do not
            // consolidate, so a duplicate responder on an exclusive topic surfaces
            // as a second reply (→ `TooManyResponders`) rather than being hidden.
            .target(zenoh::query::QueryTarget::All)
            .consolidation(zenoh::query::ConsolidationMode::None)
            .await
            .map_err(|e| QueryError::Protocol(e.to_string()))?;

        // An exclusive query topic has exactly one responder (D31/D43f): collect
        // replies until the stream closes, returning the single reply. A second
        // reply is `TooManyResponders` (a duplicate responder - also a
        // `phoxal-cli check` topology error). The Phoxal-pinned finite timeout
        // bounds the wait: deadline with no reply → `Timeout`; the stream closing
        // with no reply → `Unavailable`.
        let deadline = tokio::time::Instant::now() + self.timeout;
        let mut outcome: Option<std::result::Result<Resp, QueryError>> = None;
        loop {
            match tokio::time::timeout_at(deadline, replies.recv_async()).await {
                Ok(Ok(reply)) => {
                    if outcome.is_some() {
                        return Err(QueryError::TooManyResponders);
                    }
                    outcome = Some(decode_reply_result::<Resp>(reply.into_result()));
                }
                Ok(Err(_)) => break, // reply stream closed
                Err(_elapsed) => {
                    return outcome.unwrap_or_else(|| {
                        Err(QueryError::Timeout(QueryFailure::deadline_exceeded(
                            "query deadline exceeded",
                        )))
                    });
                }
            }
        }
        outcome.unwrap_or(Err(QueryError::Unavailable))
    }
}

fn decode_reply_result<Resp: ContractBody>(
    result: std::result::Result<Sample, zenoh::query::ReplyError>,
) -> std::result::Result<Resp, QueryError> {
    match result {
        Ok(sample) => decode_reply::<Resp>(&sample),
        Err(reply_error) => {
            let bytes = reply_error.payload().to_bytes();
            match crate::query::QueryFailure::decode(bytes.as_ref()) {
                Ok(failure) => Err(QueryError::Server(failure)),
                Err(e) => Err(QueryError::Protocol(format!("malformed error reply: {e}"))),
            }
        }
    }
}

fn decode_reply<Resp: ContractBody>(sample: &Sample) -> std::result::Result<Resp, QueryError> {
    match decode_sample::<Resp>(sample, Resp::TOPIC) {
        Ok((body, _)) => Ok(body),
        Err(e) => Err(QueryError::Decode(e.to_string())),
    }
}

/// A decoded inbound sample: the body plus its metadata.
#[derive(Clone, Debug)]
pub struct Received<B> {
    /// The decoded wire body.
    pub body: B,
    /// The sample's bus metadata.
    pub metadata: BusMetadata,
}

/// Keep-last-1 view of a topic: the most recently received decoded body.
///
/// A background task overwrites a single slot with each decoded sample, so a
/// reader always sees current state and never a backlog. Use this when only the
/// latest value matters (the common case for periodic state); reach for
/// [`Subscriber`] when a bounded history is needed. Decode failures are counted
/// and logged, not stored. Once an epoch barrier is active, possible replacement
/// epochs are kept in a separate bounded quarantine and remain invisible until
/// their matching epoch is activated. The subscription lives until the
/// `Latest` is dropped.
pub struct Latest<B> {
    state: Arc<Mutex<LatestState<B>>>,
    metric: RuntimeMetricHandle,
    _guard: Arc<SubscriptionGuard>,
}

struct LatestState<B> {
    active_epoch: Option<u64>,
    received: Option<Arc<Received<B>>>,
    pending: VecDeque<Arc<Received<B>>>,
    retired_epochs: RetiredEpochs,
}

enum LatestIngest {
    Active {
        overwrote: bool,
    },
    Pending {
        epoch: u64,
        new_epoch: bool,
        filtered: u64,
    },
    Filtered,
}

impl<B> LatestState<B> {
    fn ingest(&mut self, received: Received<B>) -> LatestIngest {
        let epoch = received.metadata.epoch;
        let received = Arc::new(received);
        let Some(active_epoch) = self.active_epoch else {
            return LatestIngest::Active {
                overwrote: self.received.replace(received).is_some(),
            };
        };
        if epoch == active_epoch {
            return LatestIngest::Active {
                overwrote: self.received.replace(received).is_some(),
            };
        }
        if self.retired_epochs.contains(epoch) {
            return LatestIngest::Filtered;
        }

        if let Some(candidate) = self
            .pending
            .iter_mut()
            .find(|candidate| candidate.metadata.epoch == epoch)
        {
            *candidate = received;
            return LatestIngest::Pending {
                epoch,
                new_epoch: false,
                filtered: 1,
            };
        }

        let filtered = if self.pending.len() == PENDING_EPOCH_CAPACITY {
            self.pending.pop_front();
            1
        } else {
            0
        };
        self.pending.push_back(received);
        LatestIngest::Pending {
            epoch,
            new_epoch: true,
            filtered,
        }
    }

    fn retain_epoch(&mut self, epoch: u64) -> (u64, bool) {
        if self.active_epoch == Some(epoch) {
            return (0, self.received.is_some());
        }

        if let Some(previous) = self.active_epoch.replace(epoch) {
            self.retired_epochs.retire(previous);
        }
        self.retired_epochs.activate(epoch);

        let mut filtered = 0_u64;
        let active = self
            .received
            .take()
            .filter(|received| {
                let keep = received.metadata.epoch == epoch;
                filtered += u64::from(!keep);
                keep
            })
            .or_else(|| {
                let index = self
                    .pending
                    .iter()
                    .position(|received| received.metadata.epoch == epoch)?;
                self.pending.remove(index)
            });
        filtered = filtered.saturating_add(u64::try_from(self.pending.len()).unwrap_or(u64::MAX));
        self.pending.clear();
        self.received = active;
        (filtered, self.received.is_some())
    }
}

// Manual, unbounded on `B` (mirrors `Publisher`'s reasoning). `slot` is
// already `Arc`-shared; `_guard` is `Arc<SubscriptionGuard>` (below) so
// cloning shares the one background decode task rather than starting a
// second one - the task aborts only when the *last* clone drops. `latest()`
// only ever reads the shared slot (`&self`), so every clone always observes
// the same freshest sample: safe to hand to a concurrent reader (D3's
// snapshot-server `Api` sharing).
impl<B> Clone for Latest<B> {
    fn clone(&self) -> Self {
        Latest {
            state: Arc::clone(&self.state),
            metric: self.metric.clone(),
            _guard: Arc::clone(&self._guard),
        }
    }
}

impl<B: ContractBody> Latest<B> {
    /// Framework-internal (macro/runner-only): build a keep-last view over a topic.
    /// The author-facing path is `ctx.subscribe(...).latest()` in `#[setup]`. `#[doc(hidden)]`.
    #[doc(hidden)]
    pub async fn new(bus: &Bus, topic: &Topic<Subscribe<B>>) -> Result<Self> {
        let state = Arc::new(Mutex::new(LatestState {
            active_epoch: None,
            received: None,
            pending: VecDeque::with_capacity(PENDING_EPOCH_CAPACITY),
            retired_epochs: RetiredEpochs::default(),
        }));
        let store = Arc::clone(&state);
        let metric = bus.runtime_metrics().register_latest(topic.key());
        let observe = metric.clone();
        let topic_owned = topic.key().to_string();
        let guard = spawn_subscription::<B, _>(
            bus,
            topic.key(),
            move |body, metadata| {
                let mut state = store.lock().expect("latest mutex poisoned");
                match state.ingest(Received { body, metadata }) {
                    LatestIngest::Active { overwrote } => observe.record_latest(overwrote),
                    LatestIngest::Pending {
                        epoch,
                        new_epoch,
                        filtered,
                    } => {
                        observe.record_pending_latest();
                        observe.record_epoch_filtered(filtered);
                        if new_epoch {
                            tracing::warn!(
                                target: "phoxal.bus",
                                topic = %topic_owned,
                                epoch,
                                "quarantining sample from a foreign simulation epoch pending its clock"
                            );
                        }
                    }
                    LatestIngest::Filtered => observe.record_epoch_filtered(1),
                }
            },
            metric.clone(),
        )
        .await?;
        Ok(Latest {
            state,
            metric,
            _guard: Arc::new(guard),
        })
    }

    /// The most recent decoded body, or `None` if nothing has arrived yet.
    pub fn latest(&self) -> Option<B> {
        let received = self
            .state
            .lock()
            .expect("latest mutex poisoned")
            .received
            .clone();
        received.map(|received| received.body.clone())
    }

    /// Framework lifecycle hook: discard a retained value from another
    /// simulation execution while preserving a value already received for
    /// `epoch`.
    #[doc(hidden)]
    pub fn __retain_epoch(&self, epoch: u64) {
        let mut state = self.state.lock().expect("latest mutex poisoned");
        let (filtered, occupied) = state.retain_epoch(epoch);
        self.metric.record_epoch_filtered(filtered);
        self.metric.record_latest_depth(occupied);
    }
}

/// A drop-oldest ring subscription of decoded bodies.
///
/// A background task pushes each decoded sample onto a bounded ring (the depth
/// is set at construction). When a slow consumer lets the ring fill, the oldest
/// buffered sample is evicted and `inbound_drops` is bumped - the newest sample
/// always wins, the backlog never grows without bound. Use this when a short
/// history is useful; reach for [`Latest`] when only current state matters.
/// Decode failures are counted + logged, not buffered. Once an epoch barrier is
/// active, possible replacement epochs are kept in separate bounded rings and
/// remain invisible until their matching epoch is activated. The subscription
/// lives until the last clone of the `Subscriber` is dropped.
///
/// # Cloning shares one queue - `recv`/`try_recv` compete
///
/// [`Clone`] is cheap (both fields are `Arc`, so a clone shares the one
/// background decode task and the one backing ring), but unlike [`Latest`] a
/// `Subscriber` is a **destructive** view: [`recv`](Self::recv)/
/// [`try_recv`](Self::try_recv) *pop* from the shared ring, delivering each
/// buffered sample to exactly one caller. So two clones of one `Subscriber`
/// are two **competing consumers** of the same queue, not two independent
/// views - whichever clone polls first gets the item; the other never sees
/// it. That is a correctness question for whoever holds the clones, not a
/// memory-safety one.
///
/// This matters for the `Api` struct (a `Subscriber` field is
/// cloned when the runner makes the `Arc<Self::Api>` snapshot it hands to
/// concurrent `#[server_snapshot]` handlers - see
/// `phoxal::participant::runner`): a `#[server_snapshot]` handler must
/// **read committed `Snapshot` state, never `recv` a `Subscriber`**, or it
/// would steal samples from the `#[step]`/exclusive-server side that owns the
/// subscription. Prefer [`Latest`] whenever a value needs to be read from more
/// than one place (its `.latest()` is a non-destructive clone from one
/// mutex-serialized retained slot, so every clone sees the same current
/// value); reserve sharing a
/// `Subscriber` clone for a deliberate "first clone to poll wins" work-queue
/// fan-out.
pub struct Subscriber<B> {
    ring: Arc<Ring<B>>,
    _guard: Arc<SubscriptionGuard>,
}

// Manual, unbounded on `B` (mirrors `Latest`'s `Clone` impl: both fields are
// `Arc`, so cloning never starts a second decode task). The competing-consumer
// semantics of a shared clone are documented on the struct's rustdoc above.
impl<B> Clone for Subscriber<B> {
    fn clone(&self) -> Self {
        Subscriber {
            ring: Arc::clone(&self.ring),
            _guard: Arc::clone(&self._guard),
        }
    }
}

impl<B: ContractBody> Subscriber<B> {
    /// Framework-internal (macro/runner-only): build a drop-oldest ring over a topic.
    /// The author-facing path is `ctx.subscribe(...)` in `#[setup]`. `#[doc(hidden)]`.
    #[doc(hidden)]
    pub async fn new(bus: &Bus, topic: &Topic<Subscribe<B>>, depth: usize) -> Result<Self> {
        let depth = depth.max(1);
        let metric = bus
            .runtime_metrics()
            .register_subscriber(topic.key(), depth);
        let ring = Arc::new(Ring::new(depth, metric.clone()));
        let push = Arc::clone(&ring);
        let drops = bus.clone();
        let topic_owned = topic.key().to_string();
        let guard = spawn_subscription::<B, _>(
            bus,
            topic.key(),
            move |body, metadata| {
                let outcome = push.push(Received { body, metadata });
                if !outcome.accepted {
                    return;
                }
                if let Some(epoch) = outcome.new_pending_epoch {
                    tracing::warn!(
                        target: "phoxal.bus",
                        topic = %topic_owned,
                        epoch,
                        "quarantining samples from a foreign simulation epoch pending its clock"
                    );
                }
                if outcome.evicted {
                    drops.health().inbound_drops.fetch_add(1, Ordering::Relaxed);
                }
            },
            metric.clone(),
        )
        .await?;
        Ok(Subscriber {
            ring,
            _guard: Arc::new(guard),
        })
    }

    /// Await the next decoded body (drop-oldest under congestion).
    ///
    /// **Destructive**: this pops from the ring, so the sample is delivered to
    /// exactly this caller. If this `Subscriber` was cloned (e.g. into the
    /// `Arc<Self::Api>` snapshot the runner hands concurrent
    /// `#[server_snapshot]` handlers), every clone competes for the same queue
    /// (see the [type docs](Self)). Do not `recv` a `Subscriber` from a
    /// snapshot server; read committed `Snapshot` state instead.
    pub async fn recv(&self) -> Result<Received<B>> {
        let (received, _current_depth) = self.ring.recv().await;
        Ok(received)
    }

    /// Take the next decoded body if one is buffered, without awaiting.
    ///
    /// **Destructive**, exactly like [`recv`](Self::recv): it pops from the
    /// shared ring, so clones compete for samples - see the [type docs](Self).
    pub fn try_recv(&self) -> Option<Received<B>> {
        self.ring
            .try_pop()
            .map(|(received, _current_depth)| received)
    }

    /// Cumulative samples evicted from this subscriber's bounded ring.
    ///
    /// This counter is local to this subscription (unlike the aggregate bus
    /// health counter), allowing retention consumers to disclose their own
    /// ingestion loss explicitly.
    pub fn dropped(&self) -> u64 {
        self.ring.dropped.load(Ordering::Relaxed)
    }

    /// Framework lifecycle hook: discard queued samples from other simulation
    /// executions while preserving samples already received for `epoch`.
    #[doc(hidden)]
    pub fn __retain_epoch(&self, epoch: u64) {
        self.ring.retain_epoch(epoch);
    }
}

struct Ring<B> {
    state: Mutex<RingState<B>>,
    notify: Notify,
    cap: usize,
    dropped: AtomicU64,
    metric: RuntimeMetricHandle,
}

struct RingState<B> {
    active_epoch: Option<u64>,
    buf: VecDeque<Received<B>>,
    pending: VecDeque<PendingEpoch<B>>,
    retired_epochs: RetiredEpochs,
}

struct PendingEpoch<B> {
    epoch: u64,
    buf: VecDeque<Received<B>>,
}

struct RingPush {
    accepted: bool,
    evicted: bool,
    new_pending_epoch: Option<u64>,
}

impl<B> Ring<B> {
    fn new(cap: usize, metric: RuntimeMetricHandle) -> Self {
        Ring {
            state: Mutex::new(RingState {
                active_epoch: None,
                buf: VecDeque::with_capacity(cap),
                pending: VecDeque::with_capacity(PENDING_EPOCH_CAPACITY),
                retired_epochs: RetiredEpochs::default(),
            }),
            notify: Notify::new(),
            cap,
            dropped: AtomicU64::new(0),
            metric,
        }
    }

    /// Push into the active queue or a bounded foreign-epoch quarantine.
    fn push(&self, item: Received<B>) -> RingPush {
        let mut dropped = false;
        let mut state = self.state.lock().expect("ring mutex poisoned");
        let Some(active_epoch) = state.active_epoch else {
            if state.buf.len() == self.cap {
                state.buf.pop_front();
                dropped = true;
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            state.buf.push_back(item);
            let depth = state.buf.len();
            self.metric.record_subscriber(dropped, depth);
            drop(state);
            self.notify.notify_one();
            return RingPush {
                accepted: true,
                evicted: dropped,
                new_pending_epoch: None,
            };
        };

        let epoch = item.metadata.epoch;
        if epoch != active_epoch {
            if state.retired_epochs.contains(epoch) {
                self.metric.record_epoch_filtered(1);
                return RingPush {
                    accepted: false,
                    evicted: false,
                    new_pending_epoch: None,
                };
            }

            let mut new_pending_epoch = None;
            let pending_index = state
                .pending
                .iter()
                .position(|pending| pending.epoch == epoch);
            let pending_index = match pending_index {
                Some(index) => index,
                None => {
                    if state.pending.len() == PENDING_EPOCH_CAPACITY {
                        if let Some(removed) = state.pending.pop_front() {
                            self.metric.record_epoch_filtered(
                                u64::try_from(removed.buf.len()).unwrap_or(u64::MAX),
                            );
                        }
                    }
                    state.pending.push_back(PendingEpoch {
                        epoch,
                        buf: VecDeque::with_capacity(self.cap),
                    });
                    new_pending_epoch = Some(epoch);
                    state.pending.len() - 1
                }
            };
            let pending = &mut state.pending[pending_index];
            if pending.buf.len() == self.cap {
                pending.buf.pop_front();
                self.metric.record_epoch_filtered(1);
            }
            pending.buf.push_back(item);
            self.metric.record_pending_subscriber();
            return RingPush {
                accepted: true,
                evicted: false,
                new_pending_epoch,
            };
        }

        if state.buf.len() == self.cap {
            state.buf.pop_front();
            dropped = true;
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        state.buf.push_back(item);
        let depth = state.buf.len();
        // Serialize the local depth gauge with the queue mutation. Updating it
        // after unlocking permits an older pop to overwrite a newer push.
        self.metric.record_subscriber(dropped, depth);
        drop(state);
        self.notify.notify_one();
        RingPush {
            accepted: true,
            evicted: dropped,
            new_pending_epoch: None,
        }
    }

    fn try_pop(&self) -> Option<(Received<B>, usize)> {
        let mut state = self.state.lock().expect("ring mutex poisoned");
        let item = state.buf.pop_front()?;
        let depth = state.buf.len();
        self.metric.record_subscriber_pop(depth);
        Some((item, depth))
    }

    fn retain_epoch(&self, epoch: u64) {
        let mut state = self.state.lock().expect("ring mutex poisoned");
        if state.active_epoch == Some(epoch) {
            return;
        }
        if let Some(previous) = state.active_epoch.replace(epoch) {
            state.retired_epochs.retire(previous);
        }
        state.retired_epochs.activate(epoch);

        let mut filtered = 0_u64;
        state.buf.retain(|received| {
            let keep = received.metadata.epoch == epoch;
            filtered += u64::from(!keep);
            keep
        });
        if let Some(index) = state
            .pending
            .iter()
            .position(|pending| pending.epoch == epoch)
        {
            let mut promoted = state
                .pending
                .remove(index)
                .expect("pending epoch index must remain valid")
                .buf;
            if state.buf.is_empty() {
                state.buf = promoted;
            } else {
                while let Some(item) = promoted.pop_front() {
                    if state.buf.len() == self.cap {
                        state.buf.pop_front();
                        filtered = filtered.saturating_add(1);
                    }
                    state.buf.push_back(item);
                }
            }
        }
        filtered = filtered.saturating_add(state.pending.iter().fold(0_u64, |total, pending| {
            total.saturating_add(u64::try_from(pending.buf.len()).unwrap_or(u64::MAX))
        }));
        state.pending.clear();
        self.metric.record_epoch_filtered(filtered);
        self.metric.record_subscriber_pop(state.buf.len());
        let notify = !state.buf.is_empty();
        drop(state);
        if notify {
            self.notify.notify_waiters();
        }
    }

    async fn recv(&self) -> (Received<B>, usize) {
        loop {
            // Register the waiter *before* checking, so a push between the check
            // and the await is not missed (tokio::sync::Notify semantics).
            let notified = self.notify.notified();
            // Hold the std mutex only to pop; never across the await below.
            if let Some(item) = self.try_pop() {
                return item;
            }
            notified.await;
        }
    }
}

/// Keeps a subscription's background task alive; aborts it on drop.
struct SubscriptionGuard {
    task: JoinHandle<()>,
}

impl Drop for SubscriptionGuard {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Declare a Zenoh subscriber on `topic_key` (under the bus root) and spawn a
/// task that decodes each sample and feeds it to `on_sample`. Decode failures
/// are counted + logged, never silently accepted.
async fn spawn_subscription<B, F>(
    bus: &Bus,
    topic_key: &str,
    mut on_sample: F,
    metric: RuntimeMetricHandle,
) -> Result<SubscriptionGuard>
where
    B: ContractBody,
    F: FnMut(B, BusMetadata) + Send + 'static,
{
    let full_key = bus.full_key(topic_key);
    let key_expr = OwnedKeyExpr::new(full_key.clone())
        .map_err(|e| BusError::Namespace(format!("invalid subscribe key '{full_key}': {e}")))?;
    let subscriber = bus
        .session()
        .declare_subscriber(key_expr)
        .await
        .map_err(|e| BusError::Transport(e.to_string()))?;

    let topic_owned = topic_key.to_string();
    let health_bus = bus.clone();

    let task = tokio::spawn(async move {
        while let Ok(sample) = subscriber.recv_async().await {
            match decode_sample::<B>(&sample, &topic_owned) {
                Ok((body, metadata)) => on_sample(body, metadata),
                Err(err) => {
                    metric.record_decode_error();
                    health_bus
                        .health()
                        .decode_errors
                        .fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(target: "phoxal.bus", topic = %topic_owned, error = %err, "dropped inbound sample");
                }
            }
        }
    });

    Ok(SubscriptionGuard { task })
}

/// Decode one Zenoh sample into a body of `B`, validating the codec before
/// touching the payload. Identity (which contract, which version) is no
/// longer checked here - it is guaranteed by the Zenoh key itself (D1): this
/// function is only ever invoked for samples received on a subscription already
/// scoped to `B`'s version-qualified topic.
pub(crate) fn decode_sample<B: ContractBody>(
    sample: &Sample,
    topic: &str,
) -> Result<(B, BusMetadata)> {
    let encoding =
        parse_encoding_string(&sample.encoding().to_string()).map_err(|e| BusError::Metadata {
            topic: topic.to_string(),
            detail: format!("malformed encoding string: {e}"),
        })?;
    match encoding.codec_id() {
        Some(CodecId::MessagePack) => {}
        None => {
            return Err(BusError::UnsupportedCodec(
                encoding.codec,
                topic.to_string(),
            ));
        }
    }

    let attachment = sample.attachment().ok_or_else(|| BusError::Metadata {
        topic: topic.to_string(),
        detail: "missing BusMetadata attachment".to_string(),
    })?;
    let metadata =
        BusMetadata::decode(attachment.to_bytes().as_ref()).map_err(|e| BusError::Metadata {
            topic: topic.to_string(),
            detail: format!("malformed BusMetadata: {e}"),
        })?;

    if metadata.codec != encoding.codec {
        return Err(BusError::Metadata {
            topic: topic.to_string(),
            detail: format!(
                "encoding/BusMetadata codec mismatch: encoding codec={}, metadata codec={}",
                encoding.codec, metadata.codec
            ),
        });
    }

    match metadata.codec_id() {
        Some(CodecId::MessagePack) => {}
        None => {
            return Err(BusError::UnsupportedCodec(
                metadata.codec,
                topic.to_string(),
            ));
        }
    }

    let body = MessagePack::decode::<B>(sample.payload().to_bytes().as_ref())?;
    Ok((body, metadata))
}

#[cfg(test)]
mod subscriber_ring_tests {
    use super::*;
    use std::sync::Barrier;

    fn received(body: u8, epoch: u64) -> Received<u8> {
        Received {
            body,
            metadata: BusMetadata {
                codec: CodecId::MessagePack.as_u8(),
                produced_at_ns: 0,
                epoch,
                source: Source {
                    participant: "test".to_string(),
                    incarnation: 0,
                    sequence: u64::from(body),
                },
            },
        }
    }

    #[test]
    fn ring_counts_each_drop_oldest_eviction_cumulatively() {
        let metrics = crate::runtime_metrics::RuntimeMetrics::default();
        let metric = metrics.register_subscriber("v0.1/test/state", 1);
        let ring = Ring::new(1, metric);
        let first = ring.push(received(1, 0));
        assert!(first.accepted);
        assert!(!first.evicted);
        let second = ring.push(received(2, 0));
        assert!(second.accepted);
        assert!(second.evicted);
        let third = ring.push(received(3, 0));
        assert!(third.accepted);
        assert!(third.evicted);
        assert_eq!(ring.dropped.load(Ordering::Relaxed), 2);
        let (received, depth) = ring.try_pop().unwrap();
        assert_eq!(received.body, 3);
        assert_eq!(depth, 0);
        let row = metrics.take().pop().unwrap();
        assert_eq!(row.count, 3);
        assert_eq!(row.drops, 2);
        assert_eq!(row.bounded_evictions, 2);
        assert_eq!(row.current_depth, 0);
        assert_eq!(row.high_water_depth, 1);
    }

    #[test]
    fn latest_quarantines_replacement_epoch_until_atomic_activation() {
        let mut state = LatestState {
            active_epoch: None,
            received: None,
            pending: VecDeque::with_capacity(PENDING_EPOCH_CAPACITY),
            retired_epochs: RetiredEpochs::default(),
        };
        assert!(matches!(
            state.ingest(received(1, 1)),
            LatestIngest::Active { overwrote: false }
        ));
        assert_eq!(state.retain_epoch(1), (0, true));

        assert!(matches!(
            state.ingest(received(2, 2)),
            LatestIngest::Pending {
                epoch: 2,
                new_epoch: true,
                filtered: 0
            }
        ));
        assert_eq!(state.received.as_ref().map(|sample| sample.body), Some(1));
        assert_eq!(state.retain_epoch(2), (1, true));
        assert_eq!(state.received.as_ref().map(|sample| sample.body), Some(2));
        assert!(matches!(
            state.ingest(received(3, 1)),
            LatestIngest::Filtered
        ));
        assert_eq!(state.received.as_ref().map(|sample| sample.body), Some(2));
    }

    #[test]
    fn latest_activation_is_safe_when_replacement_ingress_races_the_clock() {
        let state = Arc::new(Mutex::new(LatestState {
            active_epoch: Some(1),
            received: Some(Arc::new(received(1, 1))),
            pending: VecDeque::with_capacity(PENDING_EPOCH_CAPACITY),
            retired_epochs: RetiredEpochs::default(),
        }));
        let barrier = Arc::new(Barrier::new(3));

        let ingress_state = Arc::clone(&state);
        let ingress_barrier = Arc::clone(&barrier);
        let ingress = std::thread::spawn(move || {
            ingress_barrier.wait();
            ingress_state
                .lock()
                .expect("latest mutex poisoned")
                .ingest(received(2, 2));
        });
        let clock_state = Arc::clone(&state);
        let clock_barrier = Arc::clone(&barrier);
        let clock = std::thread::spawn(move || {
            clock_barrier.wait();
            clock_state
                .lock()
                .expect("latest mutex poisoned")
                .retain_epoch(2);
        });
        barrier.wait();
        ingress.join().expect("ingress thread should join");
        clock.join().expect("clock thread should join");

        let mut state = state.lock().expect("latest mutex poisoned");
        assert_eq!(state.active_epoch, Some(2));
        assert_eq!(state.received.as_ref().map(|sample| sample.body), Some(2));
        assert!(matches!(
            state.ingest(received(3, 1)),
            LatestIngest::Filtered
        ));
        assert_eq!(state.received.as_ref().map(|sample| sample.body), Some(2));
    }

    #[test]
    fn subscriber_activation_is_safe_when_replacement_ingress_races_the_clock() {
        let metrics = crate::runtime_metrics::RuntimeMetrics::default();
        let metric = metrics.register_subscriber("v0.1/test/state", 4);
        let ring = Arc::new(Ring::new(4, metric));
        assert!(ring.push(received(1, 1)).accepted);
        ring.retain_epoch(1);
        assert_eq!(ring.try_pop().map(|(sample, _)| sample.body), Some(1));

        let barrier = Arc::new(Barrier::new(3));
        let ingress_ring = Arc::clone(&ring);
        let ingress_barrier = Arc::clone(&barrier);
        let ingress = std::thread::spawn(move || {
            ingress_barrier.wait();
            assert!(ingress_ring.push(received(2, 2)).accepted);
        });
        let clock_ring = Arc::clone(&ring);
        let clock_barrier = Arc::clone(&barrier);
        let clock = std::thread::spawn(move || {
            clock_barrier.wait();
            clock_ring.retain_epoch(2);
        });
        barrier.wait();
        ingress.join().expect("ingress thread should join");
        clock.join().expect("clock thread should join");

        assert_eq!(ring.try_pop().map(|(sample, _)| sample.body), Some(2));
        assert!(!ring.push(received(3, 1)).accepted);
        assert!(ring.try_pop().is_none());
        assert_eq!(metrics.take().pop().unwrap().epoch_filtered, 1);
    }
}
