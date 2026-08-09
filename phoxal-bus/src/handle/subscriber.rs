//! The receiving side: a decoded sample, a keep-last-1 view, and a drop-oldest
//! ring, plus the background subscription task all three share.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use phoxal_runtime_contract::identity::TimelineId;
use tokio::sync::Notify;
use zenoh::key_expr::OwnedKeyExpr;

use crate::contract::{ContractBody, DeliveryFamily};
use crate::error::{BusError, Result};
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
pub(crate) struct Latest<B> {
    state: Arc<Mutex<LatestState<B>>>,
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
impl<B> Clone for Latest<B> {
    fn clone(&self) -> Self {
        Latest {
            state: Arc::clone(&self.state),
            metric: self.metric.clone(),
            terminal: Arc::clone(&self.terminal),
            _guard: Arc::clone(&self._guard),
        }
    }
}

impl<B: ContractBody> Latest<B> {
    /// Build a keep-last view over a topic.
    ///
    /// The author-facing path is `ctx.latest(...)` in `Participant::setup`.
    /// `pub` only because the generated api tree and the runner live in other
    /// crates; see [`crate::handle::stamp`]'s module docs.
    #[doc(hidden)]
    pub async fn new(bus: &BusHandle, topic: &Topic<Subscribe<B>>) -> Result<Self> {
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
        let guard = spawn_subscription::<B, _>(
            bus,
            topic.key(),
            move |observed| {
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
    pub fn observed(&self) -> Option<Arc<Observed<B>>> {
        lock(&self.state).observed.clone()
    }

    /// The most recent decoded body, for consumers that need no provenance.
    pub fn latest(&self) -> Option<B>
    where
        B: Clone,
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
        B: 'static,
    {
        let retained = self.clone();
        TimelineRetention(Arc::new(move |timeline| retained.retain_timeline(timeline)))
    }
}

/// Internal ring subscription used by the delivery-specific receiver wrappers.
///
/// A background task pushes each decoded sample onto a bounded ring (the depth
/// is set at construction). When a slow consumer lets the ring fill, the oldest
/// buffered sample is evicted and `inbound_drops` is bumped - the newest sample
/// always wins, the backlog never grows without bound. Use this when a short
/// history is useful; [`StateView`] is used when only current state matters.
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
pub(crate) struct Subscriber<B> {
    ring: Arc<Ring<B>>,
    terminal: Arc<TerminalState>,
    _guard: Arc<SubscriptionGuard>,
}

// Manual, unbounded on `B` (mirrors `Latest`'s `Clone` impl: both fields are
// `Arc`, so cloning never starts a second decode task). The competing-consumer
// semantics of a shared clone are documented on the struct's rustdoc above.
impl<B> Clone for Subscriber<B> {
    fn clone(&self) -> Self {
        Subscriber {
            ring: Arc::clone(&self.ring),
            terminal: Arc::clone(&self.terminal),
            _guard: Arc::clone(&self._guard),
        }
    }
}

impl<B: ContractBody> Subscriber<B> {
    /// Build a drop-oldest ring over a topic.
    ///
    /// `pub` only because the delivery-specific wrappers and the runner live
    /// in other crates; see [`crate::handle::stamp`]'s module docs.
    #[doc(hidden)]
    pub async fn new(bus: &BusHandle, topic: &Topic<Subscribe<B>>) -> Result<Self> {
        // Buffering is a contract property, not a tuning knob each caller can
        // guess at. State and setpoint observations retain one newest value;
        // ordered samples and streams use the bounded sample window.
        let depth = delivery_capacity(B::DELIVERY);
        let metric = bus
            .runtime_metrics()?
            .register_subscriber(topic.key(), depth);
        let terminal = Arc::new(TerminalState::new());
        let policy = match B::DELIVERY {
            DeliveryFamily::Stream => RingPolicy::Refuse,
            DeliveryFamily::State
            | DeliveryFamily::Setpoint
            | DeliveryFamily::Sample
            | DeliveryFamily::Query => RingPolicy::DropOldest,
        };
        let ring = Arc::new(Ring::new(
            depth,
            policy,
            metric.clone(),
            Arc::clone(&terminal),
        ));
        let push = Arc::clone(&ring);
        let drops = bus.clone();
        let topic_owned = topic.key().to_string();
        let guard = spawn_subscription::<B, _>(
            bus,
            topic.key(),
            move |observed| {
                let outcome = push.push(observed);
                if !outcome.accepted {
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

    /// Await the next observed sample (drop-oldest under congestion).
    ///
    /// **Destructive**: this pops from the ring, so the sample is delivered to
    /// exactly this caller. If this `Subscriber` was cloned, every clone
    /// competes for the same queue (see the [type docs](Self)).
    pub async fn recv(&self) -> Result<Observed<B>> {
        let (observed, _current_depth) = self.ring.recv().await?;
        Ok(observed)
    }

    /// Take the next observed sample if one is buffered, without awaiting.
    ///
    /// **Destructive**, exactly like [`recv`](Self::recv): it pops from the
    /// shared ring, so clones compete for samples - see the [type docs](Self).
    pub fn try_recv(&self) -> Option<Observed<B>> {
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
        B: 'static,
    {
        let retained = self.clone();
        TimelineRetention(Arc::new(move |timeline| retained.retain_timeline(timeline)))
    }
}

/// Delivery-specific keep-newest view for a state contract.
#[derive(Clone)]
pub struct StateView<B> {
    inner: Latest<B>,
}

impl<B: crate::contract::StateDeliveryContract> StateView<B> {
    /// Construct the state view for a typed subscription.
    #[doc(hidden)]
    pub async fn new(bus: &BusHandle, topic: &Topic<Subscribe<B>>) -> Result<Self> {
        Ok(Self {
            inner: Latest::new(bus, topic).await?,
        })
    }

    pub fn observed(&self) -> Option<Arc<Observed<B>>> {
        self.inner.observed()
    }

    pub fn latest(&self) -> Option<B>
    where
        B: Clone,
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
pub struct SetpointReceiver<B> {
    inner: Subscriber<B>,
}

impl<B: crate::contract::SetpointDeliveryContract> SetpointReceiver<B> {
    #[doc(hidden)]
    pub async fn new(bus: &BusHandle, topic: &Topic<Subscribe<B>>) -> Result<Self> {
        Ok(Self {
            inner: Subscriber::new(bus, topic).await?,
        })
    }

    pub async fn recv(&self) -> Result<Observed<B>> {
        self.inner.recv().await
    }

    pub fn try_recv(&self) -> Option<Observed<B>> {
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
pub struct SampleReceiver<B> {
    inner: Subscriber<B>,
}

impl<B: crate::contract::SampleDeliveryContract> SampleReceiver<B> {
    #[doc(hidden)]
    pub async fn new(bus: &BusHandle, topic: &Topic<Subscribe<B>>) -> Result<Self> {
        Ok(Self {
            inner: Subscriber::new(bus, topic).await?,
        })
    }

    pub async fn recv(&self) -> Result<Observed<B>> {
        self.inner.recv().await
    }

    pub fn try_recv(&self) -> Option<Observed<B>> {
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
pub struct StreamReceiver<B> {
    inner: Subscriber<B>,
}

impl<B: crate::contract::StreamDeliveryContract> StreamReceiver<B> {
    #[doc(hidden)]
    pub async fn new(bus: &BusHandle, topic: &Topic<Subscribe<B>>) -> Result<Self> {
        Ok(Self {
            inner: Subscriber::new(bus, topic).await?,
        })
    }

    pub async fn recv(&self) -> Result<Observed<B>> {
        self.inner.recv().await
    }

    pub fn try_recv(&self) -> Option<Observed<B>> {
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

const DEFAULT_ORDERED_CAPACITY: usize = 32;

const fn delivery_capacity(family: DeliveryFamily) -> usize {
    match family {
        DeliveryFamily::State | DeliveryFamily::Setpoint | DeliveryFamily::Query => 1,
        DeliveryFamily::Sample | DeliveryFamily::Stream => DEFAULT_ORDERED_CAPACITY,
    }
}

struct Ring<B> {
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
}

struct RingState<B> {
    active_timeline: Option<TimelineId>,
    buf: VecDeque<Observed<B>>,
    pending: VecDeque<PendingTimeline<B>>,
    retired_timelines: RetiredTimelines,
}

struct PendingTimeline<B> {
    timeline: TimelineId,
    buf: VecDeque<Observed<B>>,
}

struct RingPush {
    accepted: bool,
    evicted: bool,
    new_pending_timeline: Option<TimelineId>,
}

impl<B> Ring<B> {
    fn new(
        cap: usize,
        policy: RingPolicy,
        metric: RuntimeMetricHandle,
        terminal: Arc<TerminalState>,
    ) -> Self {
        Ring {
            state: Mutex::new(RingState {
                active_timeline: None,
                buf: VecDeque::with_capacity(cap),
                pending: VecDeque::with_capacity(PENDING_TIMELINE_CAPACITY),
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
                    if state.pending.len() == PENDING_TIMELINE_CAPACITY
                        && let Some(removed) = state.pending.pop_front()
                    {
                        self.metric.record_timeline_filtered(
                            u64::try_from(removed.buf.len()).unwrap_or(u64::MAX),
                        );
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
                pending.buf.pop_front();
                self.metric.record_timeline_filtered(1);
            }
            pending.buf.push_back(item);
            self.metric.record_pending();
            return RingPush {
                accepted: true,
                evicted: false,
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
            new_pending_timeline: None,
        }
    }

    fn try_pop(&self) -> Option<(Observed<B>, usize)> {
        let mut state = lock(&self.state);
        let item = state.buf.pop_front()?;
        let depth = state.buf.len();
        self.metric.record_subscriber_pop(depth);
        Some((item, depth))
    }

    fn retain_timeline(&self, timeline: TimelineId) {
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
async fn spawn_subscription<B, F>(
    bus: &BusHandle,
    topic_key: &str,
    mut on_sample: F,
    metric: RuntimeMetricHandle,
    terminal: Arc<TerminalState>,
) -> Result<SubscriptionGuard>
where
    B: ContractBody,
    F: FnMut(Observed<B>) + Send + 'static,
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
            match decode_sample::<B>(&sample, &topic_owned) {
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
    use crate::contract::{ApiVersion, ContractBody, DeliveryFamily, StateContract, TopicRole};
    use crate::handle::publisher::{CommandPublisher, StatePublisher};
    use crate::metadata::{ParticipantSourceIdentity, SourceAttribution};
    use crate::runtime_metrics::{RuntimeDirection, RuntimeMetrics};
    use crate::session::BusOwner;
    use crate::test_support::{Manual, Target, participant_config, producer, step, timeline};
    use crate::time::RobotInstant;
    use crate::topic::{Publish, Subscribe};

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct NonCloneBody {
        bytes: Vec<u8>,
    }

    enum NonCloneApi {}

    impl ApiVersion for NonCloneApi {
        const ID: &'static str = "nonclone";
    }

    impl ContractBody for NonCloneBody {
        type Api = NonCloneApi;
        const NAME: &'static str = "nonclone::state::Body";
        const VERSION: &'static str = "nonclone";
        const CONTRACT: &'static str = "state::Body";
        const TOPIC: &'static str = "nonclone/state";
        const ROLE: TopicRole = TopicRole::State;
        const DELIVERY: DeliveryFamily = DeliveryFamily::State;
    }

    impl StateContract for NonCloneBody {}

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

    #[test]
    fn ring_counts_each_drop_oldest_eviction_cumulatively() {
        let metrics = RuntimeMetrics::default();
        let metric = metrics.register_subscriber("v0.1/test/state", 1);
        let ring = Ring::new(
            1,
            RingPolicy::DropOldest,
            metric,
            Arc::new(TerminalState::new()),
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
        assert_eq!(delivery_capacity(DeliveryFamily::Setpoint), 1);
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
        let topic = Topic::<Subscribe<NonCloneBody>>::new_static(NonCloneBody::TOPIC);
        let latest = Latest::<NonCloneBody>::new(&bus, &topic)
            .await
            .expect("a non-Clone body still has a retained-state subscription");
        assert!(latest.observed().is_none());
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
        let pub_topic = Topic::<Publish<Target>>::new_static(<Target as ContractBody>::TOPIC);
        let sub_topic = Topic::<Subscribe<Target>>::new_static(<Target as ContractBody>::TOPIC);

        let publisher = StatePublisher::<Target>::new(bus.clone(), &pub_topic).unwrap();
        let latest = Latest::<Target>::new(&bus, &sub_topic).await.unwrap();

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
    async fn timeline_barrier_preserves_new_timeline_samples_and_rejects_late_old_samples() {
        let (owner, bus) = BusOwner::open(participant_config("timeline-barrier"))
            .await
            .unwrap();
        let pub_topic = Topic::<Publish<Target>>::new_static(<Target as ContractBody>::TOPIC);
        let sub_topic = Topic::<Subscribe<Target>>::new_static(<Target as ContractBody>::TOPIC);
        let publisher = StatePublisher::<Target>::new(bus.clone(), &pub_topic).unwrap();
        let latest = Latest::<Target>::new(&bus, &sub_topic).await.unwrap();
        let subscriber = Subscriber::<Target>::new(&bus, &sub_topic).await.unwrap();

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
        let pub_topic = Topic::<Publish<Manual>>::new_static(<Manual as ContractBody>::TOPIC);
        let sub_topic = Topic::<Subscribe<Manual>>::new_static(<Manual as ContractBody>::TOPIC);
        let commands = CommandPublisher::<Manual>::new(bus.clone(), &pub_topic).unwrap();
        let subscriber = Subscriber::<Manual>::new(&bus, &sub_topic).await.unwrap();
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
