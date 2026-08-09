//! The receiving side: decoded observations, keep-last state, bounded ordered
//! sample queues, refusal-preserving stream queues, and their shared
//! background subscription machinery.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use phoxal_runtime_contract::identity::TimelineId;
use tokio::sync::Notify;
use zenoh::key_expr::OwnedKeyExpr;

use crate::contract::{DeliveryFamily, EndpointDescriptor, EventContract};
use crate::error::{BusError, KeyProblem, Result};
use crate::handle::decode_sample;
use crate::lock::lock;
use crate::metadata::BusMetadata;
use crate::runtime_metrics::RuntimeMetricHandle;
use crate::session::{BusFault, BusHandle};
use crate::time::{LocalInstant, RetiredTimelines, TimeWindow};
use crate::topic::{Subscribe, Topic};

/// Foreign-timeline samples are quarantined for a small number of possible next
/// world histories. Each timeline remains bounded by the receiving handle's
/// ordinary capacity, and active-timeline data always has its own independent
/// storage.
const PENDING_TIMELINE_CAPACITY: usize = 4;

/// Why a receive path stopped producing values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReceiveTerminal {
    /// The owner closed the session or the underlying subscription ended.
    Closed,
    /// The transport reported a terminal failure.
    Transport(String),
    /// A stream receiver exhausted its fixed producer-position history bound.
    /// Existing history is retained; no source entry is silently evicted.
    TooManyStreamSources { topic: String, limit: usize },
    /// A setpoint receiver exhausted its fixed producer-source bound. Existing
    /// actionable intents are retained; no producer can evict another before
    /// the authority lease sees it.
    TooManySetpointSources { topic: String, limit: usize },
}

/// Runner-owned timeline retention callback for a receive handle. This is
/// cloneable as framework plumbing, while the destructive receiver itself is
/// intentionally not cloneable on the author surface.
#[derive(Clone)]
pub struct TimelineRetention(Arc<dyn Fn(TimelineId) + Send + Sync>);

impl TimelineRetention {
    pub fn retain(&self, timeline: TimelineId) {
        (self.0)(timeline);
    }
}

struct TerminalState {
    value: Mutex<Option<ReceiveTerminal>>,
    notify: Notify,
}

impl TerminalState {
    fn new() -> Self {
        Self {
            value: Mutex::new(None),
            notify: Notify::new(),
        }
    }

    fn set(&self, terminal: ReceiveTerminal) {
        let mut value = lock(&self.value);
        if value.is_none() {
            *value = Some(terminal);
            drop(value);
            self.notify.notify_waiters();
        }
    }

    fn get(&self) -> Option<ReceiveTerminal> {
        lock(&self.value).clone()
    }
}

/// A decoded inbound sample: the body, its provenance, and when this receiver
/// observed it.
///
/// `observed_at` is stamped immediately after the transport handed the sample
/// over and **before** decode, so ring residence and decode cost are inside
/// every consumer's measured age. It is process-local and receiver-specific -
/// two receivers of the same sample legitimately observe it at different
/// instants - which is exactly why it lives here and never in [`BusMetadata`].
#[derive(Clone, Debug)]
pub struct Observed<B> {
    /// The decoded wire body.
    pub body: B,
    /// The sample's bus metadata.
    pub metadata: BusMetadata,
    /// When this receiver observed the sample, on the host's suspend-aware
    /// monotonic boot clock.
    pub observed_at: LocalInstant,
}

impl<B> Observed<B> {
    /// The timeline this sample's content belongs to, if it expresses robot
    /// time at all.
    pub fn timeline(&self) -> Option<TimelineId> {
        self.metadata.produced_at.map(TimeWindow::timeline)
    }

    /// How long ago this receiver observed the sample.
    pub fn age(&self, now: LocalInstant) -> Duration {
        now.saturating_duration_since(self.observed_at)
    }
}

/// Internal keep-last-1 view of a topic used by [`StateView`]: the most recently received sample, provenance
/// included.
///
/// A background task overwrites a single slot with each decoded sample, so a
/// reader always sees current state and never a backlog. Decode failures are counted
/// and logged, not stored. Once a timeline barrier is active, possible
/// replacement timelines are kept in a separate bounded quarantine and remain
/// invisible until their matching timeline is activated. The subscription lives
/// until the internal view is dropped.
pub(crate) struct Latest<E: EndpointDescriptor> {
    state: Arc<Mutex<LatestState<E::Payload>>>,
    metric: RuntimeMetricHandle,
    terminal: Arc<TerminalState>,
    _guard: Arc<SubscriptionGuard>,
}

struct LatestState<B> {
    active_timeline: Option<TimelineId>,
    observed: Option<Arc<Observed<B>>>,
    pending: VecDeque<Arc<Observed<B>>>,
    retired_timelines: RetiredTimelines,
}

type Admission<B> = Arc<dyn Fn(&Observed<B>) -> bool + Send + Sync>;

enum LatestIngest {
    Active {
        overwrote: bool,
    },
    Pending {
        timeline: TimelineId,
        new_timeline: bool,
        filtered: u64,
    },
    Filtered,
}

impl<B> LatestState<B> {
    fn ingest(&mut self, observed: Observed<B>) -> LatestIngest {
        let timeline = observed.timeline();
        let observed = Arc::new(observed);
        // A sample that expresses no robot time belongs to no world history, so
        // it is never quarantined: a command or diagnostic stays valid across a
        // simulation reset by construction.
        let (Some(timeline), Some(active_timeline)) = (timeline, self.active_timeline) else {
            return LatestIngest::Active {
                overwrote: self.observed.replace(observed).is_some(),
            };
        };
        if timeline == active_timeline {
            return LatestIngest::Active {
                overwrote: self.observed.replace(observed).is_some(),
            };
        }
        if self.retired_timelines.contains(timeline) {
            return LatestIngest::Filtered;
        }

        if let Some(candidate) = self
            .pending
            .iter_mut()
            .find(|candidate| candidate.timeline() == Some(timeline))
        {
            *candidate = observed;
            return LatestIngest::Pending {
                timeline,
                new_timeline: false,
                filtered: 1,
            };
        }

        let filtered = if self.pending.len() == PENDING_TIMELINE_CAPACITY {
            self.pending.pop_front();
            1
        } else {
            0
        };
        self.pending.push_back(observed);
        LatestIngest::Pending {
            timeline,
            new_timeline: true,
            filtered,
        }
    }

    fn retain_timeline(&mut self, timeline: TimelineId) -> (u64, bool) {
        if self.active_timeline == Some(timeline) {
            return (0, self.observed.is_some());
        }

        if let Some(previous) = self.active_timeline.replace(timeline) {
            self.retired_timelines.retire(previous);
        }
        self.retired_timelines.activate(timeline);

        let mut filtered = 0_u64;
        let active = self
            .observed
            .take()
            .filter(|observed| {
                let keep = observed.timeline().is_none_or(|line| line == timeline);
                filtered += u64::from(!keep);
                keep
            })
            .or_else(|| {
                let index = self
                    .pending
                    .iter()
                    .position(|observed| observed.timeline() == Some(timeline))?;
                self.pending.remove(index)
            });
        filtered = filtered.saturating_add(u64::try_from(self.pending.len()).unwrap_or(u64::MAX));
        self.pending.clear();
        self.observed = active;
        (filtered, self.observed.is_some())
    }
}

// Manual, unbounded on `B` (mirrors `Outbox`'s reasoning). `state` is already
// `Arc`-shared; `_guard` is `Arc<SubscriptionGuard>` (below) so cloning shares
// the one background decode task rather than starting a second one - the task
// requests cooperative cancellation only when the *last* clone drops. `latest()` only ever reads the
// shared slot (`&self`), so every clone always observes the same freshest
// sample: safe to hand to a concurrent reader.
impl<E: EndpointDescriptor> Clone for Latest<E> {
    fn clone(&self) -> Self {
        Latest {
            state: Arc::clone(&self.state),
            metric: self.metric.clone(),
            terminal: Arc::clone(&self.terminal),
            _guard: Arc::clone(&self._guard),
        }
    }
}

impl<E: EndpointDescriptor> Latest<E> {
    /// Build a keep-last view over a topic.
    ///
    /// The author-facing path is `ctx.state_view(...)` in `Participant::setup`.
    /// `pub` only because the generated api tree and the runner live in other
    /// crates; see [`crate::handle::stamp`]'s module docs.
    #[doc(hidden)]
    pub async fn new(bus: &BusHandle, topic: &Topic<Subscribe<E>>) -> Result<Self> {
        Self::new_inner(bus, topic, None).await
    }

    /// Build a keep-last view that admits observations before coalescing them.
    /// The admission callback is synchronous in-memory policy and runs on the
    /// transport receive task, before the latest slot can overwrite an older
    /// accepted observation.
    #[doc(hidden)]
    pub async fn new_with_admission<F>(
        bus: &BusHandle,
        topic: &Topic<Subscribe<E>>,
        admission: F,
    ) -> Result<Self>
    where
        F: Fn(&Observed<E::Payload>) -> bool + Send + Sync + 'static,
    {
        Self::new_inner(bus, topic, Some(Arc::new(admission))).await
    }

    async fn new_inner(
        bus: &BusHandle,
        topic: &Topic<Subscribe<E>>,
        admission: Option<Admission<E::Payload>>,
    ) -> Result<Self> {
        let state = Arc::new(Mutex::new(LatestState {
            active_timeline: None,
            observed: None,
            pending: VecDeque::with_capacity(PENDING_TIMELINE_CAPACITY),
            retired_timelines: RetiredTimelines::default(),
        }));
        let store = Arc::clone(&state);
        let metric = bus.runtime_metrics()?.register_latest(topic.key());
        let terminal = Arc::new(TerminalState::new());
        let observe = metric.clone();
        let topic_owned = topic.key().to_string();
        let guard = spawn_subscription::<E, _>(
            bus,
            topic.key(),
            move |observed| {
                if admission.as_ref().is_some_and(|admit| !admit(&observed)) {
                    return;
                }
                let mut state = lock(&store);
                match state.ingest(observed) {
                    LatestIngest::Active { overwrote } => observe.record_latest(overwrote),
                    LatestIngest::Pending {
                        timeline,
                        new_timeline,
                        filtered,
                    } => {
                        observe.record_pending();
                        observe.record_timeline_filtered(filtered);
                        if new_timeline {
                            tracing::warn!(
                                target: "phoxal.bus",
                                topic = %topic_owned,
                                %timeline,
                                "quarantining sample from a foreign timeline pending its clock"
                            );
                        }
                    }
                    LatestIngest::Filtered => observe.record_timeline_filtered(1),
                }
            },
            metric.clone(),
            Arc::clone(&terminal),
        )
        .await?;
        Ok(Latest {
            state,
            metric,
            terminal,
            _guard: Arc::new(guard),
        })
    }

    /// The most recent sample with its provenance and observation stamp, or
    /// `None` if nothing has arrived yet.
    pub fn observed(&self) -> Option<Arc<Observed<E::Payload>>> {
        lock(&self.state).observed.clone()
    }

    /// The most recent decoded body, for consumers that need no provenance.
    pub fn latest(&self) -> Option<E::Payload>
    where
        E::Payload: Clone,
    {
        self.observed().map(|observed| observed.body.clone())
    }

    /// The receive path's terminal evidence, if it has ended.
    pub fn terminal(&self) -> Option<ReceiveTerminal> {
        self.terminal.get()
    }

    /// Install a timeline barrier: discard a retained value from another
    /// timeline while preserving a value already received for `timeline`.
    ///
    /// A framework lifecycle hook driven by the runner's clock handling; `pub`
    /// only because the runner lives in the `phoxal` crate. See
    /// [`crate::handle::stamp`]'s module docs.
    #[doc(hidden)]
    pub fn retain_timeline(&self, timeline: TimelineId) {
        let (filtered, occupied) = lock(&self.state).retain_timeline(timeline);
        self.metric.record_timeline_filtered(filtered);
        self.metric.record_latest_depth(occupied);
    }

    pub(crate) fn retention_handle(&self) -> TimelineRetention
    where
        E::Payload: 'static,
    {
        let retained = self.clone();
        TimelineRetention(Arc::new(move |timeline| retained.retain_timeline(timeline)))
    }
}

/// Internal ring subscription used by the delivery-specific receiver wrappers.
///
/// A background task pushes each decoded sample onto bounded storage whose
/// overflow policy is selected from the contract's delivery family. Samples
/// evict the oldest buffered value with explicit loss evidence; setpoints keep
/// the newest value per producer in first-pending-source order; streams refuse
/// admission and terminate with saturation evidence instead. The backlog never
/// grows without bound. [`StateView`] is used when only current state matters.
/// Decode failures are counted + logged, not buffered. Once a timeline barrier
/// is active, possible replacement timelines are kept in separate bounded rings
/// and remain invisible until their matching timeline is activated. The
/// subscription lives until the last internal owner is dropped.
///
/// # Internal cloning shares one queue - `recv`/`try_recv` compete
///
/// The private implementation is cheap to clone (both fields are `Arc`, so a
/// clone shares the one background decode task and the one backing ring), but
/// it is a **destructive** view: [`recv`](Self::recv)/
/// [`try_recv`](Self::try_recv) *pop* from the shared ring, delivering each
/// buffered sample to exactly one caller. So two clones of one `Subscriber`
/// are two **competing consumers** of the same queue, not two independent
/// views - whichever clone polls first gets the item; the other never sees
/// it. That is a correctness question for whoever holds the clones, not a
/// memory-safety one.
///
/// The public `SetpointReceiver`, `SampleReceiver`, and `StreamReceiver` types
/// intentionally do not expose this clone operation.
pub(crate) struct Subscriber<E: EndpointDescriptor> {
    ring: Arc<Ring<E::Payload>>,
    terminal: Arc<TerminalState>,
    _guard: Arc<SubscriptionGuard>,
}

// Manual, unbounded on `B` (mirrors `Latest`'s `Clone` impl: both fields are
// `Arc`, so cloning never starts a second decode task). The competing-consumer
// semantics of a shared clone are documented on the struct's rustdoc above.
impl<E: EndpointDescriptor> Clone for Subscriber<E> {
    fn clone(&self) -> Self {
        Subscriber {
            ring: Arc::clone(&self.ring),
            terminal: Arc::clone(&self.terminal),
            _guard: Arc::clone(&self._guard),
        }
    }
}

impl<E: EndpointDescriptor> Subscriber<E> {
    /// Build the delivery family's bounded receive storage over a topic.
    ///
    /// `pub` only because the delivery-specific wrappers and the runner live
    /// in other crates; see [`crate::handle::stamp`]'s module docs.
    #[doc(hidden)]
    pub async fn new(bus: &BusHandle, topic: &Topic<Subscribe<E>>) -> Result<Self> {
        let family = E::KIND.delivery_family();
        if family == DeliveryFamily::Stream && topic.key().contains('*') {
            return Err(BusError::invalid_key(topic.key(), KeyProblem::Wildcard));
        }
        // Buffering is a contract property, not a tuning knob each caller can
        // guess at. State retains one newest value; setpoints retain one newest
        // value per producer up to their fixed source bound; ordered samples
        // and streams use the bounded sample window.
        let depth = delivery_capacity(family);
        let metric = bus
            .runtime_metrics()?
            .register_subscriber(topic.key(), depth);
        let terminal = Arc::new(TerminalState::new());
        let policy = match family {
            DeliveryFamily::Stream => RingPolicy::Refuse,
            DeliveryFamily::Setpoint => RingPolicy::Setpoint,
            DeliveryFamily::State | DeliveryFamily::Sample | DeliveryFamily::Query => {
                RingPolicy::DropOldest
            }
        };
        let ring = Arc::new(Ring::new(
            depth,
            policy,
            metric.clone(),
            Arc::clone(&terminal),
            topic.key(),
        ));
        let push = Arc::clone(&ring);
        let drops = bus.clone();
        let topic_owned = topic.key().to_string();
        let guard = spawn_subscription::<E, _>(
            bus,
            topic.key(),
            move |observed| {
                let outcome = push.push(observed);
                if !outcome.accepted {
                    if outcome.saturated {
                        let error = match family {
                            DeliveryFamily::Setpoint => "setpoint receiver source bound exceeded",
                            _ => "ordered stream receive buffer saturated",
                        };
                        drops.signal_fatal(BusFault::SubscriptionReceive {
                            topic: topic_owned.clone(),
                            error: error.to_string(),
                        });
                    }
                    return;
                }
                if let Some(timeline) = outcome.new_pending_timeline {
                    tracing::warn!(
                        target: "phoxal.bus",
                        topic = %topic_owned,
                        %timeline,
                        "quarantining samples from a foreign timeline pending its clock"
                    );
                }
                if outcome.evicted {
                    drops.health().inbound_drops.fetch_add(1, Ordering::Relaxed);
                }
            },
            metric.clone(),
            Arc::clone(&terminal),
        )
        .await?;
        Ok(Subscriber {
            ring,
            terminal,
            _guard: Arc::new(guard),
        })
    }

    /// Await the next observed value from the delivery-family buffer.
    ///
    /// **Destructive**: this pops from the ring, so the sample is delivered to
    /// exactly this caller. If this `Subscriber` was cloned, every clone
    /// competes for the same queue (see the [type docs](Self)).
    pub async fn recv(&self) -> Result<Observed<E::Payload>> {
        let (observed, _current_depth) = self.ring.recv().await?;
        Ok(observed)
    }

    /// Take the next observed sample if one is buffered, without awaiting.
    ///
    /// **Destructive**, exactly like [`recv`](Self::recv): it pops from the
    /// shared ring, so clones compete for samples - see the [type docs](Self).
    pub fn try_recv(&self) -> Option<Observed<E::Payload>> {
        self.ring
            .try_pop()
            .map(|(observed, _current_depth)| observed)
    }

    /// The receive path's terminal evidence, if it has ended.
    pub fn terminal(&self) -> Option<ReceiveTerminal> {
        self.terminal.get()
    }

    /// Cumulative samples evicted from this subscriber's bounded ring.
    ///
    /// This counter is local to this subscription (unlike the aggregate bus
    /// health counter), allowing retention consumers to disclose their own
    /// ingestion loss explicitly.
    pub fn dropped(&self) -> u64 {
        self.ring.dropped.load(Ordering::Relaxed)
    }

    /// Install a timeline barrier: discard queued samples from other timelines
    /// while preserving samples already received for `timeline`.
    ///
    /// A framework lifecycle hook driven by the runner's clock handling; `pub`
    /// only because the runner lives in the `phoxal` crate. See
    /// [`crate::handle::stamp`]'s module docs.
    #[doc(hidden)]
    pub fn retain_timeline(&self, timeline: TimelineId) {
        self.ring.retain_timeline(timeline);
    }

    pub(crate) fn retention_handle(&self) -> TimelineRetention
    where
        E::Payload: 'static,
    {
        let retained = self.clone();
        TimelineRetention(Arc::new(move |timeline| retained.retain_timeline(timeline)))
    }
}

/// Delivery-specific keep-newest view for a state contract.
#[derive(Clone)]
pub struct StateView<E: EndpointDescriptor> {
    inner: Latest<E>,
}

impl<E: crate::contract::StateDeliveryContract> StateView<E> {
    /// Construct the state view for a typed subscription.
    #[doc(hidden)]
    pub async fn new(bus: &BusHandle, topic: &Topic<Subscribe<E>>) -> Result<Self> {
        Ok(Self {
            inner: Latest::new(bus, topic).await?,
        })
    }

    /// Construct a state view whose source admission runs before keep-last
    /// coalescing. The callback must be a bounded synchronous policy; it is
    /// invoked on the receive task.
    #[doc(hidden)]
    pub async fn new_with_admission<F>(
        bus: &BusHandle,
        topic: &Topic<Subscribe<E>>,
        admission: F,
    ) -> Result<Self>
    where
        F: Fn(&Observed<E::Payload>) -> bool + Send + Sync + 'static,
    {
        Ok(Self {
            inner: Latest::new_with_admission(bus, topic, admission).await?,
        })
    }

    pub fn observed(&self) -> Option<Arc<Observed<E::Payload>>> {
        self.inner.observed()
    }

    pub fn latest(&self) -> Option<E::Payload>
    where
        E::Payload: Clone,
    {
        self.inner.latest()
    }

    pub fn terminal(&self) -> Option<ReceiveTerminal> {
        self.inner.terminal()
    }

    #[doc(hidden)]
    pub fn retain_timeline(&self, timeline: TimelineId) {
        self.inner.retain_timeline(timeline);
    }

    #[doc(hidden)]
    pub fn timeline_retention(&self) -> TimelineRetention {
        self.inner.retention_handle()
    }
}

/// Delivery-specific receiver for newest-actionable setpoints.
pub struct SetpointReceiver<E: EndpointDescriptor> {
    inner: Subscriber<E>,
}

impl<E: crate::contract::SetpointDeliveryContract> SetpointReceiver<E> {
    #[doc(hidden)]
    pub async fn new(bus: &BusHandle, topic: &Topic<Subscribe<E>>) -> Result<Self> {
        Ok(Self {
            inner: Subscriber::new(bus, topic).await?,
        })
    }

    pub async fn recv(&self) -> Result<Observed<E::Payload>> {
        if let Some(terminal) = self.terminal() {
            return Err(terminal_error(terminal));
        }
        self.inner.recv().await
    }

    pub fn try_recv(&self) -> Option<Observed<E::Payload>> {
        if self.terminal().is_some() {
            return None;
        }
        self.inner.try_recv()
    }

    pub fn terminal(&self) -> Option<ReceiveTerminal> {
        self.inner.terminal()
    }

    #[doc(hidden)]
    pub fn retain_timeline(&self, timeline: TimelineId) {
        self.inner.retain_timeline(timeline);
    }

    #[doc(hidden)]
    pub fn timeline_retention(&self) -> TimelineRetention {
        self.inner.retention_handle()
    }
}

/// Delivery-specific bounded ordered sample receiver.
pub struct SampleReceiver<E: EndpointDescriptor> {
    inner: Subscriber<E>,
}

impl<E: crate::contract::SampleDeliveryContract> SampleReceiver<E> {
    #[doc(hidden)]
    pub async fn new(bus: &BusHandle, topic: &Topic<Subscribe<E>>) -> Result<Self> {
        Ok(Self {
            inner: Subscriber::new(bus, topic).await?,
        })
    }

    pub async fn recv(&self) -> Result<Observed<E::Payload>> {
        self.inner.recv().await
    }

    pub fn try_recv(&self) -> Option<Observed<E::Payload>> {
        self.inner.try_recv()
    }

    pub fn dropped(&self) -> u64 {
        self.inner.dropped()
    }

    pub fn terminal(&self) -> Option<ReceiveTerminal> {
        self.inner.terminal()
    }

    #[doc(hidden)]
    pub fn retain_timeline(&self, timeline: TimelineId) {
        self.inner.retain_timeline(timeline);
    }

    #[doc(hidden)]
    pub fn timeline_retention(&self) -> TimelineRetention {
        self.inner.retention_handle()
    }
}

/// Delivery-specific ordered stream receiver.
pub struct StreamReceiver<E: EndpointDescriptor> {
    inner: Subscriber<E>,
    topic: String,
    next_positions: Mutex<HashMap<crate::ProducerId, Option<u64>>>,
}

/// The fixed number of producer histories one setpoint receiver retains.
///
/// A setpoint receiver keeps one newest actionable value for each source. It
/// refuses a new source once this bound is reached instead of allowing a flood
/// from an unrelated producer to evict a source whose authority has not yet
/// been evaluated.
pub const MAX_SETPOINT_SOURCES: usize = 16;

/// The fixed number of producer histories one stream receiver retains.
///
/// A stream receiver cannot evict an old producer's position history: doing so
/// would turn a later return from that producer into an unobservable baseline
/// reset. Once this bound is reached, the receiver terminates with explicit
/// [`ReceiveTerminal::TooManyStreamSources`] evidence instead.
pub const MAX_STREAM_SOURCES: usize = 16;

/// One ordered stream observation, including explicit gap evidence.
#[derive(Debug)]
pub enum StreamEvent<B> {
    Item(Observed<B>),
    Gap {
        expected: u64,
        observed: u64,
        item: Observed<B>,
    },
}

impl<E: crate::contract::StreamDeliveryContract> StreamReceiver<E> {
    #[doc(hidden)]
    pub async fn new(bus: &BusHandle, topic: &Topic<Subscribe<E>>) -> Result<Self> {
        Ok(Self {
            inner: Subscriber::new(bus, topic).await?,
            topic: topic.key().to_string(),
            next_positions: Mutex::new(HashMap::new()),
        })
    }

    /// Receive the next ordered item or explicit gap evidence.
    pub async fn recv_event(&self) -> Result<StreamEvent<E::Payload>> {
        self.ensure_open()?;
        let observed = self.inner.recv().await?;
        self.classify(observed)
    }

    /// Receive the next item, failing rather than hiding a detected gap.
    pub async fn recv(&self) -> Result<Observed<E::Payload>> {
        match self.recv_event().await? {
            StreamEvent::Item(observed) => Ok(observed),
            StreamEvent::Gap {
                expected,
                observed,
                item,
            } => Err(BusError::StreamGap {
                topic: self.topic.clone(),
                producer: item.metadata.source.producer(),
                expected,
                observed,
            }),
        }
    }

    /// Take the next buffered stream event without waiting.
    pub fn try_recv_event(&self) -> Result<Option<StreamEvent<E::Payload>>> {
        self.ensure_open()?;
        self.inner
            .try_recv()
            .map(|observed| self.classify(observed))
            .transpose()
    }

    /// Take the next item, failing rather than hiding a detected gap.
    pub fn try_recv(&self) -> Result<Option<Observed<E::Payload>>> {
        match self.try_recv_event()? {
            Some(StreamEvent::Item(observed)) => Ok(Some(observed)),
            Some(StreamEvent::Gap {
                expected,
                observed,
                item,
            }) => Err(BusError::StreamGap {
                topic: self.topic.clone(),
                producer: item.metadata.source.producer(),
                expected,
                observed,
            }),
            None => Ok(None),
        }
    }

    pub fn terminal(&self) -> Option<ReceiveTerminal> {
        self.inner.terminal()
    }

    #[doc(hidden)]
    pub fn retain_timeline(&self, timeline: TimelineId) {
        self.inner.retain_timeline(timeline);
    }

    #[doc(hidden)]
    pub fn timeline_retention(&self) -> TimelineRetention {
        self.inner.retention_handle()
    }

    fn classify(&self, item: Observed<E::Payload>) -> Result<StreamEvent<E::Payload>> {
        let result = classify_stream(&self.topic, &self.next_positions, item);
        if let Err(BusError::TooManyStreamSources { topic, limit }) = &result {
            self.inner
                .terminal
                .set(ReceiveTerminal::TooManyStreamSources {
                    topic: topic.clone(),
                    limit: *limit,
                });
        }
        result
    }

    fn ensure_open(&self) -> Result<()> {
        match self.inner.terminal() {
            Some(terminal) => Err(terminal_error(terminal)),
            None => Ok(()),
        }
    }
}

/// Delivery-specific ordered receiver for event endpoints.
///
/// Events share the stream transport guarantee (ordered admission and
/// explicit per-producer gap evidence) but have their own endpoint kind so
/// temporal/event semantics remain independent from the transport metadata.
pub struct EventReceiver<E: EventContract> {
    inner: StreamReceiver<E>,
}

impl<E: EventContract> EventReceiver<E> {
    #[doc(hidden)]
    pub async fn new(bus: &BusHandle, topic: &Topic<Subscribe<E>>) -> Result<Self> {
        Ok(Self {
            inner: StreamReceiver::new(bus, topic).await?,
        })
    }

    /// Receive the next event or explicit per-producer gap evidence.
    pub async fn recv_event(&self) -> Result<StreamEvent<E::Payload>> {
        self.inner.recv_event().await
    }

    /// Receive the next event, failing rather than hiding a detected gap.
    pub async fn recv(&self) -> Result<Observed<E::Payload>> {
        self.inner.recv().await
    }

    /// Take the next buffered event without waiting.
    pub fn try_recv_event(&self) -> Result<Option<StreamEvent<E::Payload>>> {
        self.inner.try_recv_event()
    }

    /// Take the next buffered event, failing rather than hiding a gap.
    pub fn try_recv(&self) -> Result<Option<Observed<E::Payload>>> {
        self.inner.try_recv()
    }

    pub fn terminal(&self) -> Option<ReceiveTerminal> {
        self.inner.terminal()
    }

    #[doc(hidden)]
    pub fn retain_timeline(&self, timeline: TimelineId) {
        self.inner.retain_timeline(timeline);
    }

    #[doc(hidden)]
    pub fn timeline_retention(&self) -> TimelineRetention {
        self.inner.timeline_retention()
    }
}

fn classify_stream<B>(
    topic: &str,
    next_positions: &Mutex<HashMap<crate::ProducerId, Option<u64>>>,
    item: Observed<B>,
) -> Result<StreamEvent<B>> {
    let producer = item.metadata.source.producer();
    let observed = item
        .metadata
        .stream_position
        .as_ref()
        .ok_or_else(|| BusError::MissingStreamPosition {
            topic: topic.to_string(),
        })?
        .sequence;
    let mut positions = lock(next_positions);
    let Some(next) = positions.get_mut(&producer) else {
        if positions.len() >= MAX_STREAM_SOURCES {
            return Err(BusError::TooManyStreamSources {
                topic: topic.to_string(),
                limit: MAX_STREAM_SOURCES,
            });
        }
        positions.insert(producer, observed.checked_add(1));
        return Ok(StreamEvent::Item(item));
    };
    let expected = (*next).ok_or(BusError::SequenceExhausted)?;
    if observed == expected {
        *next = observed.checked_add(1);
        Ok(StreamEvent::Item(item))
    } else if observed > expected {
        *next = observed.checked_add(1);
        Ok(StreamEvent::Gap {
            expected,
            observed,
            item,
        })
    } else {
        Err(BusError::StreamPositionRegressed {
            topic: topic.to_string(),
            producer,
            expected,
            observed,
        })
    }
}

const DEFAULT_ORDERED_CAPACITY: usize = 32;

const fn delivery_capacity(family: DeliveryFamily) -> usize {
    match family {
        DeliveryFamily::State | DeliveryFamily::Query => 1,
        DeliveryFamily::Setpoint => MAX_SETPOINT_SOURCES,
        DeliveryFamily::Sample | DeliveryFamily::Stream => DEFAULT_ORDERED_CAPACITY,
    }
}

struct Ring<B> {
    topic: String,
    state: Mutex<RingState<B>>,
    notify: Notify,
    cap: usize,
    policy: RingPolicy,
    dropped: AtomicU64,
    metric: RuntimeMetricHandle,
    terminal: Arc<TerminalState>,
}

#[derive(Clone, Copy)]
enum RingPolicy {
    DropOldest,
    Refuse,
    Setpoint,
}

struct RingState<B> {
    active_timeline: Option<TimelineId>,
    buf: VecDeque<Observed<B>>,
    setpoints: SetpointBuffer<B>,
    pending: VecDeque<PendingTimeline<B>>,
    pending_setpoints: VecDeque<PendingSetpointTimeline<B>>,
    retired_timelines: RetiredTimelines,
}

/// One bounded producer-scoped setpoint lane. The map coalesces each source's
/// newest value while the order queue records when each source first became
/// pending. A separate instance is kept for each quarantined timeline so the
/// delivery family never bypasses temporal barriers.
struct SetpointBuffer<B> {
    values: HashMap<crate::ProducerId, Observed<B>>,
    order: VecDeque<crate::ProducerId>,
}

impl<B> SetpointBuffer<B> {
    fn with_capacity(cap: usize) -> Self {
        Self {
            values: HashMap::with_capacity(cap),
            order: VecDeque::with_capacity(cap),
        }
    }

    fn len(&self) -> usize {
        self.values.len()
    }

    fn contains(&self, producer: crate::ProducerId) -> bool {
        self.values.contains_key(&producer)
    }

    fn insert(&mut self, item: Observed<B>) -> bool {
        let producer = item.metadata.source.producer();
        if let Some(previous) = self.values.get_mut(&producer) {
            *previous = item;
            true
        } else {
            self.order.push_back(producer);
            self.values.insert(producer, item);
            false
        }
    }

    fn pop_front(&mut self) -> Option<(crate::ProducerId, Observed<B>)> {
        while let Some(producer) = self.order.pop_front() {
            if let Some(item) = self.values.remove(&producer) {
                return Some((producer, item));
            }
        }
        None
    }

    fn retain_timeline(&mut self, timeline: TimelineId) -> u64 {
        let mut retained = VecDeque::with_capacity(self.order.len());
        let mut filtered = 0_u64;
        while let Some(producer) = self.order.pop_front() {
            let keep = self
                .values
                .get(&producer)
                .is_some_and(|item| item.timeline().is_none_or(|line| line == timeline));
            if keep {
                retained.push_back(producer);
            } else if self.values.remove(&producer).is_some() {
                filtered = filtered.saturating_add(1);
            }
        }
        self.order = retained;
        filtered
    }
}

struct PendingTimeline<B> {
    timeline: TimelineId,
    buf: VecDeque<Observed<B>>,
}

struct PendingSetpointTimeline<B> {
    timeline: TimelineId,
    buffer: SetpointBuffer<B>,
}

impl<B> RingState<B> {
    fn setpoint_source_present(&self, producer: crate::ProducerId) -> bool {
        self.setpoints.contains(producer)
            || self
                .pending_setpoints
                .iter()
                .any(|pending| pending.buffer.contains(producer))
    }

    fn setpoint_source_count(&self) -> usize {
        let mut producers = HashSet::new();
        producers.extend(self.setpoints.values.keys().copied());
        for pending in &self.pending_setpoints {
            producers.extend(pending.buffer.values.keys().copied());
        }
        producers.len()
    }
}

struct RingPush {
    accepted: bool,
    evicted: bool,
    saturated: bool,
    new_pending_timeline: Option<TimelineId>,
}

impl<B> Ring<B> {
    fn new(
        cap: usize,
        policy: RingPolicy,
        metric: RuntimeMetricHandle,
        terminal: Arc<TerminalState>,
        topic: impl Into<String>,
    ) -> Self {
        Ring {
            topic: topic.into(),
            state: Mutex::new(RingState {
                active_timeline: None,
                buf: VecDeque::with_capacity(cap),
                setpoints: SetpointBuffer::with_capacity(cap),
                pending: VecDeque::with_capacity(PENDING_TIMELINE_CAPACITY),
                pending_setpoints: VecDeque::with_capacity(PENDING_TIMELINE_CAPACITY),
                retired_timelines: RetiredTimelines::default(),
            }),
            notify: Notify::new(),
            cap,
            policy,
            dropped: AtomicU64::new(0),
            metric,
            terminal,
        }
    }

    /// Push into the active queue or a bounded foreign-timeline quarantine.
    fn push(&self, item: Observed<B>) -> RingPush {
        if matches!(self.policy, RingPolicy::Setpoint) {
            return self.push_setpoint(item);
        }
        let mut state = lock(&self.state);
        // A sample expressing no robot time belongs to no world history and is
        // never quarantined.
        let timeline = item.timeline();
        if let (Some(timeline), Some(active_timeline)) = (timeline, state.active_timeline)
            && timeline != active_timeline
        {
            if state.retired_timelines.contains(timeline) {
                self.metric.record_timeline_filtered(1);
                return RingPush {
                    accepted: false,
                    evicted: false,
                    saturated: false,
                    new_pending_timeline: None,
                };
            }

            let mut new_pending_timeline = None;
            let pending_index = state
                .pending
                .iter()
                .position(|pending| pending.timeline == timeline);
            let pending_index = match pending_index {
                Some(index) => index,
                None => {
                    if state.pending.len() == PENDING_TIMELINE_CAPACITY {
                        if matches!(self.policy, RingPolicy::Refuse) {
                            self.terminal.set(ReceiveTerminal::Transport(
                                "stream receiver timeline quarantine saturated".to_string(),
                            ));
                            return RingPush {
                                accepted: false,
                                evicted: false,
                                saturated: true,
                                new_pending_timeline: None,
                            };
                        }
                        if let Some(removed) = state.pending.pop_front() {
                            self.metric.record_timeline_filtered(
                                u64::try_from(removed.buf.len()).unwrap_or(u64::MAX),
                            );
                        }
                    }
                    state.pending.push_back(PendingTimeline {
                        timeline,
                        buf: VecDeque::with_capacity(self.cap),
                    });
                    new_pending_timeline = Some(timeline);
                    state.pending.len() - 1
                }
            };
            let pending = &mut state.pending[pending_index];
            if pending.buf.len() == self.cap {
                if matches!(self.policy, RingPolicy::Refuse) {
                    self.terminal.set(ReceiveTerminal::Transport(
                        "stream receiver timeline quarantine saturated".to_string(),
                    ));
                    return RingPush {
                        accepted: false,
                        evicted: false,
                        saturated: true,
                        new_pending_timeline: None,
                    };
                }
                pending.buf.pop_front();
                self.metric.record_timeline_filtered(1);
            }
            pending.buf.push_back(item);
            self.metric.record_pending();
            return RingPush {
                accepted: true,
                evicted: false,
                saturated: false,
                new_pending_timeline,
            };
        }

        let mut dropped = false;
        if state.buf.len() == self.cap {
            if matches!(self.policy, RingPolicy::Refuse) {
                self.terminal.set(ReceiveTerminal::Transport(
                    "stream receiver buffer saturated".to_string(),
                ));
                return RingPush {
                    accepted: false,
                    evicted: false,
                    saturated: true,
                    new_pending_timeline: None,
                };
            }
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
            saturated: false,
            new_pending_timeline: None,
        }
    }

    /// Keep one newest actionable value per producer while preserving the
    /// order in which producers first became pending. A new producer beyond
    /// the fixed bound terminates the receiver rather than evicting an older
    /// producer's intent before authority can inspect it.
    fn push_setpoint(&self, item: Observed<B>) -> RingPush {
        let producer = item.metadata.source.producer();
        let mut state = lock(&self.state);
        // Check while holding the same mutex that serializes dequeue. This
        // makes terminal state win over a wrapper-level check racing a
        // buffered actionable intent: once terminal evidence is visible, no
        // later setpoint may be admitted or delivered.
        if self.terminal.get().is_some() {
            return RingPush {
                accepted: false,
                evicted: false,
                saturated: false,
                new_pending_timeline: None,
            };
        }

        let timeline = item.timeline();
        let foreign_timeline = timeline
            .zip(state.active_timeline)
            .and_then(|(timeline, active)| (timeline != active).then_some(timeline));
        let (overwrote, depth, new_pending_timeline) = if let Some(timeline) = foreign_timeline {
            if state.retired_timelines.contains(timeline) {
                self.metric.record_timeline_filtered(1);
                return RingPush {
                    accepted: false,
                    evicted: false,
                    saturated: false,
                    new_pending_timeline: None,
                };
            }

            let source_exists = state.setpoint_source_present(producer);
            let (pending_index, new_pending_timeline) = match state
                .pending_setpoints
                .iter()
                .position(|pending| pending.timeline == timeline)
            {
                Some(index) => (index, None),
                None => {
                    if !source_exists && state.setpoint_source_count() >= self.cap {
                        return self.setpoint_source_overflow();
                    }
                    if state.pending_setpoints.len() == PENDING_TIMELINE_CAPACITY
                        && let Some(removed) = state.pending_setpoints.pop_front()
                    {
                        self.metric.record_timeline_filtered(
                            u64::try_from(removed.buffer.len()).unwrap_or(u64::MAX),
                        );
                    }
                    state.pending_setpoints.push_back(PendingSetpointTimeline {
                        timeline,
                        buffer: SetpointBuffer::with_capacity(self.cap),
                    });
                    (state.pending_setpoints.len() - 1, Some(timeline))
                }
            };
            let replacing = state.pending_setpoints[pending_index]
                .buffer
                .contains(producer);
            if !replacing {
                let at_capacity = state.pending_setpoints[pending_index].buffer.len() >= self.cap;
                if at_capacity || (!source_exists && state.setpoint_source_count() >= self.cap) {
                    return self.setpoint_source_overflow();
                }
            }
            let pending = &mut state.pending_setpoints[pending_index];
            let overwrote = pending.buffer.insert(item);
            (overwrote, None, new_pending_timeline)
        } else {
            let source_exists = state.setpoint_source_present(producer);
            if !state.setpoints.contains(producer)
                && (state.setpoints.len() >= self.cap
                    || (!source_exists && state.setpoint_source_count() >= self.cap))
            {
                return self.setpoint_source_overflow();
            }
            let overwrote = state.setpoints.insert(item);
            (overwrote, Some(state.setpoints.len()), None)
        };

        if overwrote {
            self.metric.record_latest_overwrite();
        }
        if let Some(depth) = depth {
            self.metric.record_subscriber(false, depth);
        } else {
            self.metric.record_pending();
        }
        drop(state);
        self.notify.notify_one();
        RingPush {
            accepted: true,
            evicted: false,
            saturated: false,
            new_pending_timeline,
        }
    }

    fn setpoint_source_overflow(&self) -> RingPush {
        self.record_setpoint_source_overflow();
        RingPush {
            accepted: false,
            evicted: false,
            saturated: true,
            new_pending_timeline: None,
        }
    }

    fn record_setpoint_source_overflow(&self) {
        self.metric.record_drop();
        self.terminal.set(ReceiveTerminal::TooManySetpointSources {
            topic: self.topic.clone(),
            limit: self.cap,
        });
    }

    fn try_pop(&self) -> Option<(Observed<B>, usize)> {
        let mut state = lock(&self.state);
        if matches!(self.policy, RingPolicy::Setpoint) {
            if self.terminal.get().is_some() {
                return None;
            }
            if let Some((producer, item)) = state.setpoints.pop_front() {
                // Re-check while the ring mutex is still held. If a
                // worker publishes terminal evidence after the initial
                // check but before this pop's linearization point, put
                // the item back at the front and refuse delivery.
                if self.terminal.get().is_some() {
                    state.setpoints.order.push_front(producer);
                    state.setpoints.values.insert(producer, item);
                    return None;
                }
                let depth = state.setpoints.len();
                self.metric.record_subscriber_pop(depth);
                return Some((item, depth));
            }
            return None;
        }
        let item = state.buf.pop_front()?;
        let depth = state.buf.len();
        self.metric.record_subscriber_pop(depth);
        Some((item, depth))
    }

    fn retain_timeline(&self, timeline: TimelineId) {
        if matches!(self.policy, RingPolicy::Setpoint) {
            self.retain_setpoint_timeline(timeline);
            return;
        }
        let mut state = lock(&self.state);
        if state.active_timeline == Some(timeline) {
            return;
        }
        if let Some(previous) = state.active_timeline.replace(timeline) {
            state.retired_timelines.retire(previous);
        }
        state.retired_timelines.activate(timeline);

        let mut filtered = 0_u64;
        state.buf.retain(|observed| {
            let keep = observed.timeline().is_none_or(|line| line == timeline);
            filtered += u64::from(!keep);
            keep
        });
        if let Some(index) = state
            .pending
            .iter()
            .position(|pending| pending.timeline == timeline)
        {
            // `position` just proved the index is in range, so the removal
            // cannot be `None`; taking the buffer this way keeps the promotion
            // a move rather than a copy.
            let mut promoted = match state.pending.remove(index) {
                Some(pending) => pending.buf,
                None => VecDeque::new(),
            };
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
        self.metric.record_timeline_filtered(filtered);
        self.metric.record_subscriber_pop(state.buf.len());
        let notify = !state.buf.is_empty();
        drop(state);
        if notify {
            self.notify.notify_waiters();
        }
    }

    fn retain_setpoint_timeline(&self, timeline: TimelineId) {
        let mut state = lock(&self.state);
        if state.active_timeline == Some(timeline) {
            return;
        }
        if let Some(previous) = state.active_timeline.replace(timeline) {
            state.retired_timelines.retire(previous);
        }
        state.retired_timelines.activate(timeline);

        let mut filtered = state.setpoints.retain_timeline(timeline);
        if let Some(index) = state
            .pending_setpoints
            .iter()
            .position(|pending| pending.timeline == timeline)
        {
            let mut promoted = match state.pending_setpoints.remove(index) {
                Some(pending) => pending.buffer,
                None => SetpointBuffer::with_capacity(self.cap),
            };
            while let Some((_producer, item)) = promoted.pop_front() {
                let producer = item.metadata.source.producer();
                if let Some(previous) = state.setpoints.values.get_mut(&producer) {
                    // Producer sequence is transport-wide and monotonic, so it
                    // gives deterministic newest-value selection when a
                    // clockless value and a quarantined timed value meet at
                    // promotion. The source's first-pending order remains the
                    // active order in either case.
                    if item.metadata.sequence >= previous.metadata.sequence {
                        *previous = item;
                        self.metric.record_latest_overwrite();
                    }
                } else {
                    if state.setpoints.len() >= self.cap {
                        // Admission enforces this invariant for every
                        // producer/timeline lane. Keep the release build
                        // fail-closed if a future change ever violates it
                        // during promotion rather than growing the slot.
                        self.record_setpoint_source_overflow();
                        break;
                    }
                    state.setpoints.insert(item);
                }
            }
        }
        filtered = filtered.saturating_add(
            state
                .pending_setpoints
                .iter()
                .map(|pending| u64::try_from(pending.buffer.len()).unwrap_or(u64::MAX))
                .sum(),
        );
        state.pending_setpoints.clear();
        self.metric.record_timeline_filtered(filtered);
        self.metric.record_subscriber_pop(state.setpoints.len());
        let notify = !state.setpoints.values.is_empty();
        drop(state);
        if notify {
            self.notify.notify_waiters();
        }
    }

    async fn recv(&self) -> Result<(Observed<B>, usize)> {
        loop {
            // Register the waiter *before* checking, so a push between the check
            // and the await is not missed (tokio::sync::Notify semantics).
            let notified = self.notify.notified();
            let terminal = self.terminal.notify.notified();
            // Hold the std mutex only to pop; never across the await below.
            if let Some(item) = self.try_pop() {
                return Ok(item);
            }
            if let Some(terminal) = self.terminal.get() {
                return Err(terminal_error(terminal));
            }
            tokio::select! {
                _ = notified => {}
                _ = terminal => {}
            }
        }
    }
}

fn terminal_error(terminal: ReceiveTerminal) -> BusError {
    match terminal {
        ReceiveTerminal::Closed => BusError::Closed,
        ReceiveTerminal::Transport(error) => BusError::Transport(error),
        ReceiveTerminal::TooManyStreamSources { topic, limit } => {
            BusError::TooManyStreamSources { topic, limit }
        }
        ReceiveTerminal::TooManySetpointSources { topic, limit } => {
            BusError::TooManySetpointSources { topic, limit }
        }
    }
}

/// A cancellation capability for one owner-registered subscription worker.
/// Dropping it wakes the worker; the owner keeps the task join handle and
/// explicitly joins it during close.
struct SubscriptionGuard {
    cancel: Arc<Notify>,
    expected: Arc<AtomicBool>,
}

impl Drop for SubscriptionGuard {
    fn drop(&mut self) {
        self.expected.store(true, Ordering::Release);
        self.cancel.notify_one();
    }
}

/// Declare a Zenoh subscriber on `topic_key` (under the bus root) and spawn a
/// task that stamps, decodes, and feeds each sample to `on_sample`.
///
/// The observation instant is taken **before** decode, so ring residence and
/// decode cost land inside the measured age rather than outside it. Decode
/// failures are counted + logged, never silently accepted.
async fn spawn_subscription<E, F>(
    bus: &BusHandle,
    topic_key: &str,
    mut on_sample: F,
    metric: RuntimeMetricHandle,
    terminal: Arc<TerminalState>,
) -> Result<SubscriptionGuard>
where
    E: EndpointDescriptor,
    F: FnMut(Observed<E::Payload>) + Send + 'static,
{
    let full_key = bus.full_key(topic_key);
    let key_expr = OwnedKeyExpr::new(full_key.clone())
        .map_err(|e| BusError::not_a_key_expression(&full_key, e))?;
    let subscriber = bus
        .session()?
        .declare_subscriber(key_expr)
        .await
        .map_err(|e| BusError::Transport(e.to_string()))?;

    let topic_owned = topic_key.to_string();
    let health_bus = bus.clone();
    let shutdown_bus = bus.clone();
    let cancel = Arc::new(Notify::new());
    let cancel_task = Arc::clone(&cancel);
    let expected = Arc::new(AtomicBool::new(false));
    let terminal_task = Arc::clone(&terminal);

    let task = tokio::spawn(async move {
        loop {
            let shutdown = shutdown_bus.wait_for_shutdown();
            let sample = tokio::select! {
                biased;
                _ = cancel_task.notified() => {
                    terminal_task.set(ReceiveTerminal::Closed);
                    break;
                }
                _ = shutdown => {
                    terminal_task.set(ReceiveTerminal::Closed);
                    break;
                }
                result = subscriber.recv_async() => match result {
                    Ok(sample) => sample,
                    Err(error) => {
                        let error = error.to_string();
                        terminal_task.set(ReceiveTerminal::Transport(error.clone()));
                        health_bus.signal_fatal(BusFault::SubscriptionReceive {
                            topic: topic_owned.clone(),
                            error,
                        });
                        break;
                    }
                }
            };
            // The observation stamp is the receiver's own evidence of when
            // this arrived, and every freshness decision downstream is
            // measured from it. A sample that cannot be stamped is dropped:
            // inventing an instant here is what would let a stale command look
            // freshly observed.
            let Some(observed_at) = LocalInstant::try_now() else {
                // Not a decode error - the bytes were fine. The clock fault is
                // latched process-wide by `try_now`, and the runner turns that
                // into ordinary failure on its next beat.
                tracing::error!(
                    target: "phoxal.bus",
                    topic = %topic_owned,
                    "dropped inbound sample: the host boot clock could not be read"
                );
                continue;
            };
            match decode_sample::<E>(&sample, &topic_owned) {
                Ok((body, metadata)) => on_sample(Observed {
                    body,
                    metadata,
                    observed_at,
                }),
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

    if let Err(task) = bus.register_named_worker(
        format!("subscription:{topic_key}"),
        Arc::clone(&expected),
        task,
    ) {
        // Close won the registration race. Its shutdown notification is
        // already published, so the worker exits cooperatively and this setup
        // path joins it before returning the typed terminal error.
        let _ = task.await;
        return Err(BusError::Closed);
    }
    Ok(SubscriptionGuard { cancel, expected })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    use serial_test::serial;

    use crate::abi::CodecId;
    use crate::contract::{
        ApiVersion, EndpointDescriptor, EndpointKind, StateContract, StreamContract,
        StreamDeliveryContract,
    };
    use crate::handle::publisher::{SetpointPublisher, StatePublisher};
    use crate::lease::{FixedSourceLease, LeaseDecision, LeaseRejection};
    use crate::liveliness::ParticipantReadyStatus;
    use crate::metadata::{ParticipantSourceIdentity, SourceAttribution};
    use crate::runtime_metrics::{RuntimeDirection, RuntimeMetrics};
    use crate::session::BusOwner;
    use crate::test_support::{
        Manual, ManualEndpoint, Target, TargetEndpoint, participant_config, producer, step,
        timeline,
    };
    use crate::time::RobotInstant;
    use crate::topic::{Publish, Subscribe};
    use phoxal_runtime_contract::identity::{ParticipantId, ProducerId};

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct NonCloneBody {
        bytes: Vec<u8>,
    }

    enum NonCloneApi {}

    impl ApiVersion for NonCloneApi {
        const ID: &'static str = "nonclone";
    }

    struct NonCloneEndpoint;
    impl EndpointDescriptor for NonCloneEndpoint {
        type Api = NonCloneApi;
        type Payload = NonCloneBody;
        const NAME: &'static str = "nonclone::state::Body";
        const VERSION: &'static str = "nonclone";
        const CONTRACT: &'static str = "state::Body";
        const TOPIC: &'static str = "nonclone/state";
        const KIND: EndpointKind = EndpointKind::State;
    }

    impl StateContract for NonCloneEndpoint {}

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct OrderedChunk(u8);

    struct OrderedEndpoint;
    impl EndpointDescriptor for OrderedEndpoint {
        type Api = NonCloneApi;
        type Payload = OrderedChunk;
        const NAME: &'static str = "nonclone::stream::Chunk";
        const VERSION: &'static str = "nonclone";
        const CONTRACT: &'static str = "stream::Chunk";
        const TOPIC: &'static str = "nonclone/stream/chunk";
        const KIND: EndpointKind = EndpointKind::Stream;
    }

    impl StreamContract for OrderedEndpoint {}
    impl StreamDeliveryContract for OrderedEndpoint {}

    fn observed(body: u8, line: Option<u64>) -> Observed<u8> {
        Observed {
            body,
            metadata: BusMetadata {
                codec: CodecId::MessagePack.as_u8(),
                sequence: u64::from(body),
                stream_position: None,
                produced_at: line
                    .map(|line| TimeWindow::exact(RobotInstant::new(timeline(line), 0))),
                source: SourceAttribution::Participant(ParticipantSourceIdentity::new(
                    phoxal_runtime_contract::identity::ParticipantId::new("test")
                        .expect("test participant"),
                    producer(1),
                )),
            },
            observed_at: LocalInstant::try_now().expect("test host clock"),
        }
    }

    fn stream_observed(body: u8, position: Option<u64>) -> Observed<u8> {
        let mut observed = observed(body, None);
        observed.metadata.stream_position =
            position.map(|sequence| crate::metadata::StreamPosition { sequence });
        observed
    }

    fn setpoint_observed(body: u8, participant: &str, source: ProducerId) -> Observed<u8> {
        setpoint_observed_at(body, participant, source, None)
    }

    fn setpoint_observed_at(
        body: u8,
        participant: &str,
        source: ProducerId,
        line: Option<u64>,
    ) -> Observed<u8> {
        let mut observed = observed(body, line);
        observed.metadata.source = SourceAttribution::Participant(ParticipantSourceIdentity::new(
            ParticipantId::new(participant).expect("valid test participant"),
            source,
        ));
        observed
    }

    fn setpoint_ring(metrics: &RuntimeMetrics) -> Ring<u8> {
        let metric = metrics.register_subscriber("v0.1/test/setpoint", MAX_SETPOINT_SOURCES);
        Ring::new(
            MAX_SETPOINT_SOURCES,
            RingPolicy::Setpoint,
            metric,
            Arc::new(TerminalState::new()),
            "v0.1/test/setpoint",
        )
    }

    #[test]
    fn setpoint_keeps_newest_value_per_producer_in_first_pending_source_order() {
        let metrics = RuntimeMetrics::default();
        let ring = setpoint_ring(&metrics);

        assert!(
            ring.push(setpoint_observed(1, "motion", producer(1)))
                .accepted
        );
        assert!(
            ring.push(setpoint_observed(2, "motion", producer(2)))
                .accepted
        );
        assert!(
            ring.push(setpoint_observed(3, "motion", producer(1)))
                .accepted
        );
        assert!(
            ring.push(setpoint_observed(4, "motion", producer(2)))
                .accepted
        );

        assert_eq!(ring.try_pop().map(|(item, _)| item.body), Some(3));
        assert_eq!(ring.try_pop().map(|(item, _)| item.body), Some(4));
        assert!(ring.try_pop().is_none());

        let row = metrics.take().pop().expect("setpoint metric row");
        assert_eq!(row.count, 4);
        assert_eq!(row.latest_overwrites, 2);
        assert_eq!(row.bounded_evictions, 0);
        assert_eq!(row.drops, 0);
        assert_eq!(row.high_water_depth, 2);
        assert_eq!(row.current_depth, 0);
    }

    #[test]
    fn one_producer_flood_cannot_evict_another_producers_pending_setpoint() {
        let metrics = RuntimeMetrics::default();
        let ring = setpoint_ring(&metrics);

        assert!(
            ring.push(setpoint_observed(10, "motion", producer(1)))
                .accepted
        );
        for body in 0..u8::try_from(MAX_SETPOINT_SOURCES * 4).expect("test flood fits in u8") {
            assert!(
                ring.push(setpoint_observed(body, "motion", producer(2)))
                    .accepted
            );
        }

        assert_eq!(ring.try_pop().map(|(item, _)| item.body), Some(10));
        assert_eq!(
            ring.try_pop().map(|(item, _)| item.body),
            Some(u8::try_from(MAX_SETPOINT_SOURCES * 4 - 1).expect("test flood fits in u8"))
        );
        assert!(ring.try_pop().is_none());
    }

    #[test]
    fn fixed_source_rejects_wrong_producer_without_evicting_legitimate_intent() {
        let metrics = RuntimeMetrics::default();
        let ring = setpoint_ring(&metrics);
        let participant = ParticipantId::new("motion").expect("valid test participant");
        let legitimate = ParticipantSourceIdentity::new(participant.clone(), producer(1));
        let wrong = ParticipantSourceIdentity::new(participant.clone(), producer(2));
        let mut lease = FixedSourceLease::new(
            "motion/manual",
            participant,
            Duration::from_secs(1),
            Duration::from_secs(1),
        );
        lease.update_ready(&legitimate, ParticipantReadyStatus::Ready);

        assert!(
            ring.push(setpoint_observed(1, "motion", wrong.producer))
                .accepted
        );
        assert!(
            ring.push(setpoint_observed(2, "motion", legitimate.producer))
                .accepted
        );
        for body in 3..u8::try_from(MAX_SETPOINT_SOURCES * 4).expect("test flood fits in u8") {
            assert!(
                ring.push(setpoint_observed(body, "motion", wrong.producer))
                    .accepted
            );
        }

        let wrong_item = ring.try_pop().expect("wrong source remains first").0;
        assert!(matches!(
            lease.offer(
                wrong_item.metadata.source.participant_source(),
                wrong_item.metadata.sequence,
                wrong_item.observed_at,
                wrong_item.body,
            ),
            LeaseDecision::Rejected(LeaseRejection::SourceConflict)
        ));
        let legitimate_item = ring.try_pop().expect("legitimate source was not evicted").0;
        assert!(matches!(
            lease.offer(
                legitimate_item.metadata.source.participant_source(),
                legitimate_item.metadata.sequence,
                legitimate_item.observed_at,
                legitimate_item.body,
            ),
            LeaseDecision::Acquired
        ));
        assert_eq!(lease.producer(), Some(legitimate.producer));
    }

    #[test]
    fn setpoint_source_bound_is_typed_and_retains_all_existing_sources() {
        let metrics = RuntimeMetrics::default();
        let ring = setpoint_ring(&metrics);

        for source in 0..MAX_SETPOINT_SOURCES {
            assert!(
                ring.push(setpoint_observed(
                    u8::try_from(source).expect("source index fits in u8"),
                    "motion",
                    producer(u128::try_from(source + 1).expect("source index")),
                ))
                .accepted
            );
        }
        let extra = ring.push(setpoint_observed(
            u8::try_from(MAX_SETPOINT_SOURCES).expect("source index fits in u8"),
            "motion",
            producer(u128::try_from(MAX_SETPOINT_SOURCES + 1).expect("source index")),
        ));
        assert!(!extra.accepted);
        assert!(extra.saturated);
        let terminal = ring.terminal.get().expect("source overflow is terminal");
        assert!(matches!(
            terminal.clone(),
            ReceiveTerminal::TooManySetpointSources {
                limit: MAX_SETPOINT_SOURCES,
                ..
            }
        ));
        assert!(matches!(
            terminal_error(terminal),
            BusError::TooManySetpointSources {
                limit: MAX_SETPOINT_SOURCES,
                ..
            }
        ));

        // Terminal precedence is part of the receiver contract: a buffered
        // actionable intent remains available for evidence, but is never
        // dequeued after the source-bound failure is visible.
        assert!(ring.try_pop().is_none());
        let state = lock(&ring.state);
        assert_eq!(state.setpoints.len(), MAX_SETPOINT_SOURCES);
        assert_eq!(state.setpoints.order.len(), MAX_SETPOINT_SOURCES);
        assert_eq!(state.setpoints.order.front().copied(), Some(producer(1)));
        drop(state);

        let row = metrics.take().pop().expect("setpoint metric row");
        assert_eq!(row.count, MAX_SETPOINT_SOURCES as u64);
        assert_eq!(row.drops, 1);
        assert_eq!(row.latest_overwrites, 0);
        assert_eq!(row.current_depth, MAX_SETPOINT_SOURCES as u64);
    }

    #[test]
    fn setpoint_quarantines_and_promotes_replacement_timeline_per_producer() {
        let metrics = RuntimeMetrics::default();
        let ring = setpoint_ring(&metrics);
        ring.retain_timeline(timeline(1));

        let first = ring.push(setpoint_observed_at(1, "motion", producer(1), Some(2)));
        assert!(first.accepted);
        assert_eq!(first.new_pending_timeline, Some(timeline(2)));
        assert!(
            ring.push(setpoint_observed_at(2, "motion", producer(2), Some(2)))
                .accepted
        );
        assert!(
            ring.push(setpoint_observed_at(3, "motion", producer(1), Some(2)))
                .accepted
        );
        // Only the first arrival for a foreign timeline creates a quarantine;
        // subsequent values join its producer-scoped lane.
        assert_eq!(
            ring.push(setpoint_observed_at(4, "motion", producer(2), Some(2)))
                .new_pending_timeline,
            None
        );
        assert!(ring.try_pop().is_none());

        {
            let state = lock(&ring.state);
            assert_eq!(state.pending_setpoints.len(), 1);
            assert_eq!(state.pending_setpoints[0].buffer.len(), 2);
            assert_eq!(
                state.pending_setpoints[0].buffer.order.front().copied(),
                Some(producer(1))
            );
        }

        ring.retain_timeline(timeline(2));
        assert_eq!(ring.try_pop().map(|(item, _)| item.body), Some(3));
        assert_eq!(ring.try_pop().map(|(item, _)| item.body), Some(4));
        assert!(ring.try_pop().is_none());

        // Timeline 1 is retired after promotion; delayed traffic from that
        // world is filtered rather than entering a new producer slot.
        assert!(
            !ring
                .push(setpoint_observed_at(4, "motion", producer(1), Some(1)))
                .accepted
        );
        assert!(ring.terminal.get().is_none());

        let row = metrics.take().pop().expect("setpoint metric row");
        assert_eq!(row.count, 4);
        assert_eq!(row.latest_overwrites, 2);
        assert_eq!(row.timeline_filtered, 1);
        assert_eq!(row.drops, 0);
        assert_eq!(row.bounded_evictions, 0);
    }

    #[test]
    fn active_timed_setpoint_is_filtered_on_timeline_replacement() {
        let metrics = RuntimeMetrics::default();
        let ring = setpoint_ring(&metrics);
        ring.retain_timeline(timeline(1));

        assert!(
            ring.push(setpoint_observed_at(7, "motion", producer(1), Some(1)))
                .accepted
        );
        ring.retain_timeline(timeline(2));

        assert!(ring.try_pop().is_none());
        let row = metrics.take().pop().expect("setpoint metric row");
        assert_eq!(row.timeline_filtered, 1);
        assert_eq!(row.current_depth, 0);
        assert_eq!(row.drops, 0);
    }

    #[test]
    fn clockless_setpoint_survives_timeline_replacement() {
        let metrics = RuntimeMetrics::default();
        let ring = setpoint_ring(&metrics);
        ring.retain_timeline(timeline(1));

        assert!(
            ring.push(setpoint_observed(9, "motion", producer(1)))
                .accepted
        );
        ring.retain_timeline(timeline(2));

        assert_eq!(ring.try_pop().map(|(item, _)| item.body), Some(9));
        assert!(ring.try_pop().is_none());
    }

    #[test]
    fn setpoint_timeline_quarantine_is_bounded_and_discloses_filtered_values() {
        let metrics = RuntimeMetrics::default();
        let ring = setpoint_ring(&metrics);
        ring.retain_timeline(timeline(1));

        for line in 2..=(PENDING_TIMELINE_CAPACITY as u64 + 2) {
            assert!(
                ring.push(setpoint_observed_at(
                    u8::try_from(line).expect("test timeline fits in u8"),
                    "motion",
                    producer(u128::from(line)),
                    Some(line),
                ))
                .accepted
            );
        }
        {
            let state = lock(&ring.state);
            assert_eq!(state.pending_setpoints.len(), PENDING_TIMELINE_CAPACITY);
            assert!(
                state
                    .pending_setpoints
                    .iter()
                    .all(|pending| pending.buffer.len() == 1)
            );
        }

        // The oldest candidate timeline was dropped to keep quarantine bounded.
        ring.retain_timeline(timeline(2));
        assert!(ring.try_pop().is_none());
        assert!(
            !ring
                .push(setpoint_observed_at(99, "motion", producer(99), Some(1)))
                .accepted
        );

        let row = metrics.take().pop().expect("setpoint metric row");
        assert_eq!(
            row.timeline_filtered,
            (PENDING_TIMELINE_CAPACITY + 2) as u64
        );
        assert_eq!(row.drops, 0);
    }

    #[test]
    fn setpoint_source_bound_applies_inside_a_timeline_quarantine() {
        let metrics = RuntimeMetrics::default();
        let ring = setpoint_ring(&metrics);
        ring.retain_timeline(timeline(1));

        for source in 0..MAX_SETPOINT_SOURCES {
            assert!(
                ring.push(setpoint_observed_at(
                    u8::try_from(source).expect("source index fits in u8"),
                    "motion",
                    producer(u128::try_from(source + 1).expect("source index")),
                    Some(2),
                ))
                .accepted
            );
        }
        let extra = ring.push(setpoint_observed_at(
            99,
            "motion",
            producer(u128::try_from(MAX_SETPOINT_SOURCES + 1).expect("source index")),
            Some(2),
        ));
        assert!(!extra.accepted);
        assert!(extra.saturated);
        assert!(matches!(
            ring.terminal.get(),
            Some(ReceiveTerminal::TooManySetpointSources {
                limit: MAX_SETPOINT_SOURCES,
                ..
            })
        ));
        let state = lock(&ring.state);
        assert_eq!(state.pending_setpoints.len(), 1);
        assert_eq!(
            state.pending_setpoints[0].buffer.len(),
            MAX_SETPOINT_SOURCES
        );
    }

    #[test]
    fn stream_positions_surface_gaps_and_reject_regressions() {
        let positions = Mutex::new(HashMap::new());
        assert!(matches!(
            classify_stream("v0.1/test/stream", &positions, stream_observed(1, Some(0)))
                .expect("first position is ordered"),
            StreamEvent::Item(Observed { body: 1, .. })
        ));

        let gap = classify_stream("v0.1/test/stream", &positions, stream_observed(3, Some(2)))
            .expect("a forward gap is explicit evidence, not a decode failure");
        assert!(matches!(
            gap,
            StreamEvent::Gap {
                expected: 1,
                observed: 2,
                item: Observed { body: 3, .. },
            }
        ));

        let regression =
            classify_stream("v0.1/test/stream", &positions, stream_observed(2, Some(1)))
                .expect_err("a repeated or reversed position is never ordered stream data");
        assert!(matches!(
            regression,
            BusError::StreamPositionRegressed {
                expected: 3,
                observed: 1,
                ..
            }
        ));
    }

    #[test]
    fn a_late_stream_subscription_establishes_its_first_observed_position() {
        let positions = Mutex::new(HashMap::new());
        assert!(matches!(
            classify_stream("v0.1/test/stream", &positions, stream_observed(1, Some(41)),)
                .expect("traffic before subscription is not receiver-observed loss"),
            StreamEvent::Item(Observed { body: 1, .. })
        ));
        assert!(matches!(
            classify_stream("v0.1/test/stream", &positions, stream_observed(2, Some(43)),)
                .expect("a discontinuity after the baseline is explicit"),
            StreamEvent::Gap {
                expected: 42,
                observed: 43,
                item: Observed { body: 2, .. },
            }
        ));
    }

    #[test]
    fn a_stream_sample_without_a_position_is_rejected() {
        let error = classify_stream(
            "v0.1/test/stream",
            &Mutex::new(HashMap::new()),
            stream_observed(1, None),
        )
        .expect_err("stream delivery requires a per-topic position");
        assert!(matches!(error, BusError::MissingStreamPosition { .. }));
    }

    #[test]
    fn stream_position_history_fails_closed_without_evicting_an_old_source() {
        let positions = Mutex::new(HashMap::new());
        for source in 0..MAX_STREAM_SOURCES {
            let mut item = stream_observed(1, Some(0));
            item.metadata.source = SourceAttribution::External {
                producer: producer(u128::try_from(source + 1).expect("source index")),
                label: None,
            };
            classify_stream("v0.1/test/stream", &positions, item)
                .expect("the fixed source bound admits each first source");
        }

        let mut extra = stream_observed(2, Some(0));
        extra.metadata.source = SourceAttribution::External {
            producer: producer(u128::try_from(MAX_STREAM_SOURCES + 1).expect("source index")),
            label: None,
        };
        let error = classify_stream("v0.1/test/stream", &positions, extra)
            .expect_err("a new source beyond the fixed history bound must terminate");
        assert!(matches!(
            error,
            BusError::TooManyStreamSources {
                limit: MAX_STREAM_SOURCES,
                ..
            }
        ));
        assert_eq!(lock(&positions).len(), MAX_STREAM_SOURCES);
    }

    #[test]
    fn too_many_stream_sources_is_typed_terminal_evidence() {
        let metrics = RuntimeMetrics::default();
        let metric = metrics.register_subscriber("v0.1/test/stream", DEFAULT_ORDERED_CAPACITY);
        let terminal = Arc::new(TerminalState::new());
        let receiver = StreamReceiver::<OrderedEndpoint> {
            inner: Subscriber {
                ring: Arc::new(Ring::new(
                    DEFAULT_ORDERED_CAPACITY,
                    RingPolicy::Refuse,
                    metric,
                    Arc::clone(&terminal),
                    "v0.1/test/stream",
                )),
                terminal: Arc::clone(&terminal),
                _guard: Arc::new(SubscriptionGuard {
                    cancel: Arc::new(Notify::new()),
                    expected: Arc::new(AtomicBool::new(false)),
                }),
            },
            topic: "v0.1/test/stream".to_string(),
            next_positions: Mutex::new(HashMap::new()),
        };
        for source in 0..MAX_STREAM_SOURCES {
            lock(&receiver.next_positions).insert(
                producer(u128::try_from(source + 1).expect("source index")),
                Some(1),
            );
        }
        let Observed {
            metadata,
            observed_at,
            ..
        } = stream_observed(2, Some(0));
        let mut item = Observed {
            body: OrderedChunk(2),
            metadata,
            observed_at,
        };
        item.metadata.source = SourceAttribution::External {
            producer: producer(u128::try_from(MAX_STREAM_SOURCES + 1).expect("source index")),
            label: None,
        };
        let Observed {
            metadata,
            observed_at,
            ..
        } = stream_observed(3, Some(1));
        let mut buffered = Observed {
            body: OrderedChunk(3),
            metadata,
            observed_at,
        };
        buffered.metadata.source = SourceAttribution::External {
            producer: producer(1),
            label: None,
        };
        assert!(receiver.inner.ring.push(buffered).accepted);
        let error = receiver
            .classify(item)
            .expect_err("the receiver must fail closed at the source-history bound");
        assert!(matches!(
            error,
            BusError::TooManyStreamSources {
                limit: MAX_STREAM_SOURCES,
                ..
            }
        ));
        let terminal = receiver.terminal().expect("source overflow is terminal");
        assert!(matches!(
            terminal.clone(),
            ReceiveTerminal::TooManyStreamSources {
                limit: MAX_STREAM_SOURCES,
                ..
            }
        ));
        assert!(matches!(
            terminal_error(terminal),
            BusError::TooManyStreamSources {
                limit: MAX_STREAM_SOURCES,
                ..
            }
        ));
        assert!(matches!(
            receiver.try_recv_event(),
            Err(BusError::TooManyStreamSources {
                limit: MAX_STREAM_SOURCES,
                ..
            })
        ));
        assert!(
            receiver.inner.try_recv().is_some(),
            "terminal stream receive must not dequeue an already-buffered chunk"
        );
    }

    #[test]
    fn ring_counts_each_drop_oldest_eviction_cumulatively() {
        let metrics = RuntimeMetrics::default();
        let metric = metrics.register_subscriber("v0.1/test/state", 1);
        let ring = Ring::new(
            1,
            RingPolicy::DropOldest,
            metric,
            Arc::new(TerminalState::new()),
            "v0.1/test/state",
        );
        let first = ring.push(observed(1, None));
        assert!(first.accepted);
        assert!(!first.evicted);
        let second = ring.push(observed(2, None));
        assert!(second.accepted);
        assert!(second.evicted);
        let third = ring.push(observed(3, None));
        assert!(third.accepted);
        assert!(third.evicted);
        assert_eq!(ring.dropped.load(Ordering::Relaxed), 2);
        let (observed, depth) = ring.try_pop().unwrap();
        assert_eq!(observed.body, 3);
        assert_eq!(depth, 0);
        let row = metrics.take().pop().unwrap();
        assert_eq!(row.count, 3);
        assert_eq!(row.drops, 2);
        assert_eq!(row.bounded_evictions, 2);
        assert_eq!(row.current_depth, 0);
        assert_eq!(row.high_water_depth, 1);
    }

    #[test]
    fn stream_quarantine_refuses_saturation_without_evicting_an_accepted_chunk() {
        let metrics = RuntimeMetrics::default();
        let metric = metrics.register_subscriber("v0.1/test/stream", 2);
        let terminal = Arc::new(TerminalState::new());
        let ring = Ring::new(
            2,
            RingPolicy::Refuse,
            metric,
            Arc::clone(&terminal),
            "v0.1/test/stream",
        );
        ring.retain_timeline(timeline(1));

        assert!(ring.push(observed(1, Some(2))).accepted);
        assert!(ring.push(observed(2, Some(2))).accepted);
        assert!(
            !ring.push(observed(3, Some(2))).accepted,
            "the third foreign-world chunk must be refused, not replace the first"
        );
        assert!(matches!(
            terminal.get(),
            Some(ReceiveTerminal::Transport(error))
                if error.contains("timeline quarantine saturated")
        ));

        ring.retain_timeline(timeline(2));
        assert_eq!(ring.try_pop().map(|(sample, _)| sample.body), Some(1));
        assert_eq!(ring.try_pop().map(|(sample, _)| sample.body), Some(2));
        assert!(ring.try_pop().is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_terminal_receive_wakes_a_waiter_without_abort() {
        let metrics = RuntimeMetrics::default();
        let metric = metrics.register_subscriber("v0.1/test/state", 1);
        let terminal = Arc::new(TerminalState::new());
        let ring = Arc::new(Ring::<u8>::new(
            1,
            RingPolicy::DropOldest,
            metric,
            Arc::clone(&terminal),
            "v0.1/test/state",
        ));
        let waiting = {
            let ring = Arc::clone(&ring);
            tokio::spawn(async move { ring.recv().await })
        };

        tokio::task::yield_now().await;
        terminal.set(ReceiveTerminal::Closed);
        let result = tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .expect("terminal evidence must wake the receive waiter")
            .expect("the receive task must join");
        assert!(matches!(result, Err(BusError::Closed)));
    }

    #[test]
    fn contract_families_choose_owned_buffer_semantics() {
        assert_eq!(delivery_capacity(DeliveryFamily::State), 1);
        assert_eq!(
            delivery_capacity(DeliveryFamily::Setpoint),
            MAX_SETPOINT_SOURCES
        );
        assert_eq!(delivery_capacity(DeliveryFamily::Query), 1);
        assert_eq!(
            delivery_capacity(DeliveryFamily::Sample),
            DEFAULT_ORDERED_CAPACITY
        );
        assert_eq!(
            delivery_capacity(DeliveryFamily::Stream),
            DEFAULT_ORDERED_CAPACITY
        );
    }

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn retained_state_does_not_require_a_cloneable_body() {
        let (owner, bus) = BusOwner::open(participant_config("nonclone"))
            .await
            .unwrap();
        let topic = Topic::<Subscribe<NonCloneEndpoint>>::new_static(NonCloneEndpoint::TOPIC);
        let latest = Latest::<NonCloneEndpoint>::new(&bus, &topic)
            .await
            .expect("a non-Clone body still has a retained-state subscription");
        assert!(latest.observed().is_none());
        owner.close().await;
    }

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn a_stream_receiver_rejects_a_wildcard_before_transport() {
        let (owner, bus) = BusOwner::open(participant_config("wildcard-stream"))
            .await
            .unwrap();
        for key in ["nonclone/stream/*", "nonclone/stream/foo$*/bar"] {
            let topic = Topic::<Subscribe<OrderedEndpoint>>::new_owned(key.to_string());
            let error = match StreamReceiver::new(&bus, &topic).await {
                Ok(_) => panic!("one position tracker cannot mix concrete stream topics"),
                Err(error) => error,
            };
            assert!(matches!(
                error,
                BusError::InvalidKey {
                    problem: KeyProblem::Wildcard,
                    ..
                }
            ));
        }
        owner.close().await;
    }

    #[test]
    fn a_sample_expressing_no_robot_time_survives_every_timeline_barrier() {
        let metrics = RuntimeMetrics::default();
        let metric = metrics.register_subscriber("v0.1/test/command", 4);
        let ring = Ring::new(
            4,
            RingPolicy::DropOldest,
            metric,
            Arc::new(TerminalState::new()),
            "v0.1/test/command",
        );
        ring.retain_timeline(timeline(1));
        assert!(ring.push(observed(1, None)).accepted);
        // A reset does not discard a command: it belongs to no world history.
        ring.retain_timeline(timeline(2));
        assert_eq!(ring.try_pop().map(|(sample, _)| sample.body), Some(1));
    }

    #[test]
    fn latest_quarantines_a_replacement_timeline_until_atomic_activation() {
        let mut state = LatestState {
            active_timeline: None,
            observed: None,
            pending: VecDeque::with_capacity(PENDING_TIMELINE_CAPACITY),
            retired_timelines: RetiredTimelines::default(),
        };
        assert!(matches!(
            state.ingest(observed(1, Some(1))),
            LatestIngest::Active { overwrote: false }
        ));
        assert_eq!(state.retain_timeline(timeline(1)), (0, true));

        assert!(matches!(
            state.ingest(observed(2, Some(2))),
            LatestIngest::Pending {
                new_timeline: true,
                filtered: 0,
                ..
            }
        ));
        assert_eq!(state.observed.as_ref().map(|sample| sample.body), Some(1));
        assert_eq!(state.retain_timeline(timeline(2)), (1, true));
        assert_eq!(state.observed.as_ref().map(|sample| sample.body), Some(2));
        assert!(matches!(
            state.ingest(observed(3, Some(1))),
            LatestIngest::Filtered
        ));
        assert_eq!(state.observed.as_ref().map(|sample| sample.body), Some(2));
    }

    #[test]
    fn latest_activation_is_safe_when_replacement_ingress_races_the_clock() {
        let state = Arc::new(Mutex::new(LatestState {
            active_timeline: Some(timeline(1)),
            observed: Some(Arc::new(observed(1, Some(1)))),
            pending: VecDeque::with_capacity(PENDING_TIMELINE_CAPACITY),
            retired_timelines: RetiredTimelines::default(),
        }));
        let barrier = Arc::new(Barrier::new(3));

        let ingress_state = Arc::clone(&state);
        let ingress_barrier = Arc::clone(&barrier);
        let ingress = std::thread::spawn(move || {
            ingress_barrier.wait();
            lock(&ingress_state).ingest(observed(2, Some(2)));
        });
        let clock_state = Arc::clone(&state);
        let clock_barrier = Arc::clone(&barrier);
        let clock = std::thread::spawn(move || {
            clock_barrier.wait();
            lock(&clock_state).retain_timeline(timeline(2));
        });
        barrier.wait();
        ingress.join().expect("ingress thread should join");
        clock.join().expect("clock thread should join");

        let mut state = lock(&state);
        assert_eq!(state.active_timeline, Some(timeline(2)));
        assert_eq!(state.observed.as_ref().map(|sample| sample.body), Some(2));
        assert!(matches!(
            state.ingest(observed(3, Some(1))),
            LatestIngest::Filtered
        ));
        assert_eq!(state.observed.as_ref().map(|sample| sample.body), Some(2));
    }

    /// The whole receive path, end to end in one process: a real publish
    /// arrives, decodes, and keeps its provenance and its receiver-side stamp.
    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_publisher_to_latest_round_trip() {
        let (owner, bus) = BusOwner::open(participant_config("rt")).await.unwrap();
        let pub_topic = Topic::<Publish<TargetEndpoint>>::new_static(TargetEndpoint::TOPIC);
        let sub_topic = Topic::<Subscribe<TargetEndpoint>>::new_static(TargetEndpoint::TOPIC);

        let publisher = StatePublisher::<TargetEndpoint>::new(bus.clone(), &pub_topic).unwrap();
        let latest = Latest::<TargetEndpoint>::new(&bus, &sub_topic)
            .await
            .unwrap();

        let published_at = step(1, 100);
        publisher
            .publish(
                &published_at,
                Target {
                    linear_x_mps: 0.9,
                    angular_z_radps: -0.1,
                },
            )
            .unwrap();

        let mut observed = None;
        for _ in 0..50 {
            if let Some(sample) = latest.observed() {
                observed = Some(sample);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let observed = observed.expect("Latest should observe the published body in-process");
        assert_eq!(observed.body.linear_x_mps, 0.9);
        assert_eq!(
            observed.metadata.produced_exactly_at(),
            Some(RobotInstant::new(timeline(1), 100)),
            "Latest must retain full provenance, not just the body"
        );
        assert_eq!(observed.metadata.source.producer(), bus.producer());
        assert!(
            observed.observed_at.boot_ns() > 0,
            "every subscription stamps its own observation instant"
        );

        owner.close().await;
    }

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn state_admission_rejects_a_newer_sample_before_keep_last_overwrite() {
        let (owner, bus) = BusOwner::open(participant_config("state-admission"))
            .await
            .unwrap();
        let pub_topic = Topic::<Publish<TargetEndpoint>>::new_static(TargetEndpoint::TOPIC);
        let sub_topic = Topic::<Subscribe<TargetEndpoint>>::new_static(TargetEndpoint::TOPIC);
        let publisher = StatePublisher::<TargetEndpoint>::new(bus.clone(), &pub_topic).unwrap();
        let latest = Latest::<TargetEndpoint>::new_with_admission(&bus, &sub_topic, |observed| {
            observed.body.linear_x_mps > 0.0
        })
        .await
        .unwrap();

        publisher
            .publish(
                &step(8, 1),
                Target {
                    linear_x_mps: 1.0,
                    angular_z_radps: 0.0,
                },
            )
            .unwrap();
        for _ in 0..50 {
            if latest.latest().is_some_and(|body| body.linear_x_mps == 1.0) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(latest.latest().map(|body| body.linear_x_mps), Some(1.0));

        publisher
            .publish(
                &step(8, 2),
                Target {
                    linear_x_mps: 0.0,
                    angular_z_radps: 0.0,
                },
            )
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            latest.latest().map(|body| body.linear_x_mps),
            Some(1.0),
            "a rejected newer observation must not overwrite accepted state"
        );

        owner.close().await;
    }

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn timeline_barrier_preserves_new_timeline_samples_and_rejects_late_old_samples() {
        let (owner, bus) = BusOwner::open(participant_config("timeline-barrier"))
            .await
            .unwrap();
        let pub_topic = Topic::<Publish<TargetEndpoint>>::new_static(TargetEndpoint::TOPIC);
        let sub_topic = Topic::<Subscribe<TargetEndpoint>>::new_static(TargetEndpoint::TOPIC);
        let publisher = StatePublisher::<TargetEndpoint>::new(bus.clone(), &pub_topic).unwrap();
        let latest = Latest::<TargetEndpoint>::new(&bus, &sub_topic)
            .await
            .unwrap();
        let subscriber = Subscriber::<TargetEndpoint>::new(&bus, &sub_topic)
            .await
            .unwrap();

        let old_timeline = timeline(6);
        publisher
            .publish(
                &step(6, 10),
                Target {
                    linear_x_mps: 6.0,
                    angular_z_radps: 0.0,
                },
            )
            .unwrap();
        for _ in 0..50 {
            if latest.latest().is_some_and(|body| body.linear_x_mps == 6.0) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        latest.retain_timeline(old_timeline);
        subscriber.retain_timeline(old_timeline);
        assert_eq!(
            subscriber
                .try_recv()
                .map(|observed| observed.body.linear_x_mps),
            Some(6.0)
        );

        // The controller publishes world outputs before its clock. Installing the
        // replacement clock's timeline barrier must promote those quarantined
        // new-world samples without ever exposing them under the retired one.
        let new_timeline = timeline(7);
        publisher
            .publish(
                &step(7, 10),
                Target {
                    linear_x_mps: 7.0,
                    angular_z_radps: 0.0,
                },
            )
            .unwrap();
        publisher
            .publish(
                &step(7, 11),
                Target {
                    linear_x_mps: 8.0,
                    angular_z_radps: 0.0,
                },
            )
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(latest.latest().map(|body| body.linear_x_mps), Some(6.0));
        assert!(
            subscriber.try_recv().is_none(),
            "a foreign-timeline candidate must remain unobservable before its clock"
        );

        latest.retain_timeline(new_timeline);
        subscriber.retain_timeline(new_timeline);
        assert_eq!(latest.latest().map(|body| body.linear_x_mps), Some(8.0));
        assert_eq!(
            subscriber
                .try_recv()
                .map(|observed| observed.body.linear_x_mps),
            Some(8.0)
        );
        assert_eq!(
            bus.health().inbound_drops.load(Ordering::Relaxed),
            0,
            "replacement-timeline quarantine churn is filtering, not active-queue loss"
        );

        // A one-shot purge is insufficient: a delayed old-world sample can arrive
        // after reset. The installed barrier rejects it at ingestion.
        publisher
            .publish(
                &step(6, 999),
                Target {
                    linear_x_mps: 6.0,
                    angular_z_radps: 0.0,
                },
            )
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(latest.latest().map(|body| body.linear_x_mps), Some(8.0));
        assert!(
            subscriber.try_recv().is_none(),
            "late samples from a replaced timeline must be rejected"
        );
        let metrics = bus.take_runtime_metrics().unwrap();
        let timeline_filtered = metrics
            .iter()
            .filter(|row| row.key.direction == RuntimeDirection::Subscribe)
            .map(|row| row.timeline_filtered)
            .sum::<u64>();
        assert!(
            matches!(timeline_filtered, 3 | 5),
            "coalescing may collapse the two unsent replacement states, but all timeline filtering must be disclosed: {timeline_filtered}"
        );
        assert!(
            metrics
                .iter()
                .filter(|row| row.key.direction == RuntimeDirection::Subscribe)
                .all(|row| row.drops == 0 && row.bounded_evictions == 0),
            "quarantine churn must not be reported as active-queue drops or bounded evictions"
        );
        assert_eq!(
            bus.health().inbound_drops.load(Ordering::Relaxed),
            0,
            "quarantine churn and retired samples must not inflate bus health"
        );

        owner.close().await;
    }

    /// A command expresses no robot time, so its envelope carries none - and a
    /// timeline barrier therefore never quarantines it.
    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_command_carries_no_production_instant_and_survives_a_reset() {
        let (owner, bus) = BusOwner::open(participant_config("commands"))
            .await
            .unwrap();
        let pub_topic = Topic::<Publish<ManualEndpoint>>::new_static(ManualEndpoint::TOPIC);
        let sub_topic = Topic::<Subscribe<ManualEndpoint>>::new_static(ManualEndpoint::TOPIC);
        let commands = SetpointPublisher::<ManualEndpoint>::new(bus.clone(), &pub_topic).unwrap();
        let subscriber = Subscriber::<ManualEndpoint>::new(&bus, &sub_topic)
            .await
            .unwrap();
        subscriber.retain_timeline(timeline(1));

        commands.send(Manual { linear_x_mps: 0.4 }).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        // A simulation reset lands between arrival and consumption.
        subscriber.retain_timeline(timeline(2));

        let observed = subscriber
            .try_recv()
            .expect("a command must survive a timeline replacement");
        assert_eq!(observed.body.linear_x_mps, 0.4);
        assert_eq!(observed.metadata.produced_at, None);
        assert_eq!(observed.timeline(), None);

        owner.close().await;
    }

    #[test]
    fn subscriber_activation_is_safe_when_replacement_ingress_races_the_clock() {
        let metrics = RuntimeMetrics::default();
        let metric = metrics.register_subscriber("v0.1/test/state", 4);
        let ring = Arc::new(Ring::new(
            4,
            RingPolicy::DropOldest,
            metric,
            Arc::new(TerminalState::new()),
            "v0.1/test/state",
        ));
        assert!(ring.push(observed(1, Some(1))).accepted);
        ring.retain_timeline(timeline(1));
        assert_eq!(ring.try_pop().map(|(sample, _)| sample.body), Some(1));

        let barrier = Arc::new(Barrier::new(3));
        let ingress_ring = Arc::clone(&ring);
        let ingress_barrier = Arc::clone(&barrier);
        let ingress = std::thread::spawn(move || {
            ingress_barrier.wait();
            assert!(ingress_ring.push(observed(2, Some(2))).accepted);
        });
        let clock_ring = Arc::clone(&ring);
        let clock_barrier = Arc::clone(&barrier);
        let clock = std::thread::spawn(move || {
            clock_barrier.wait();
            clock_ring.retain_timeline(timeline(2));
        });
        barrier.wait();
        ingress.join().expect("ingress thread should join");
        clock.join().expect("clock thread should join");

        assert_eq!(ring.try_pop().map(|(sample, _)| sample.body), Some(2));
        assert!(!ring.push(observed(3, Some(1))).accepted);
        assert!(ring.try_pop().is_none());
        assert_eq!(metrics.take().pop().unwrap().timeline_filtered, 1);
    }
}
