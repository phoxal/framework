//! Body-typed handles over the version-qualified bus boundary (D35).
//!
//! # Publishing is capability-gated (#952 section D)
//!
//! There is no caller-supplied publish time. The robot time a publisher can
//! express is determined by what the contract *is*, and each publisher handle
//! is bounded by its contract's temporal-role marker, so reaching for the wrong
//! one is a compile error:
//!
//! - [`StatePublisher<B>`] publishes at a step, and the step instant comes from
//!   a [`StepToken`] the runner mints for every scheduled participant, or a
//!   [`WorldStepToken`] a [`TimelineAuthority`] mints for the world authority
//!   alone. Neither token type is constructible by writing ordinary code - see
//!   [`StepToken::__mint`] and [`TimelineAuthority::__mint`] for the exact
//!   strength of that. The *authority itself* IS reachable through the
//!   documented authoring surface, but only for a `#[phoxal::simulator]`
//!   (`SetupContextSimulatorExt::timeline_authority`, `phoxal` crate); every
//!   other participant kind has no path to one at all.
//! - [`MeasurementPublisher<B>`] publishes with a [`CaptureStamp`] the driver
//!   derived from its device clock, and honestly represents an untranslated
//!   capture rather than inventing one.
//! - [`CommandPublisher<B>`] and [`DiagnosticPublisher<B>`] express no robot
//!   time at all.
//!
//! # Receiving is bus-stamped (#952 section E)
//!
//! - [`Subscriber<B>`] - a drop-oldest ring (depth 32 by default) of
//!   [`Observed<B>`] values, for consumers that want a short backlog under
//!   congestion.
//! - [`Latest<B>`] - keep-last-1: only the most recent [`Observed<B>`] is
//!   retained, provenance included.
//! - [`Querier<Req, Resp>`] - the caller side of the request/response leg,
//!   returning `Result<Resp, `[`QueryError`]`>`.
//!
//! Every subscription stamps a [`LocalInstant`] immediately after
//! `recv_async()` returns and **before** decode, so ring residence and decode
//! cost are inside every consumer's measured age rather than outside it.
//! Observation time is process-local and receiver-specific, so it rides on
//! [`Observed`] and never on the wire.
//!
//! # Periodic-state QoS
//!
//! Pub/sub here is tuned for periodic state streams, where the freshest sample
//! matters more than every sample arriving. Both ends shed load instead of
//! blocking or growing without bound:
//!
//! - **Publish never blocks.** Every publish MessagePack-encodes the body and
//!   enqueues it on the bounded outbound queue, returning immediately. A
//!   saturated queue (sample or byte bound) drops the sample, bumps
//!   `outbound_drops`, and returns [`BusError::Saturated`] so the caller can
//!   observe the loss - it never stalls the step loop (D35/D43e). Publishing is
//!   therefore a plain synchronous call: there is nothing to await.
//! - **Receivers bound their backlog.** [`Latest<B>`] keeps only the last
//!   sample (keep-last-1); [`Subscriber<B>`] keeps a drop-oldest ring, evicting
//!   the oldest buffered sample and bumping `inbound_drops` when a slow
//!   consumer lets the ring fill.
//!
//! Identity lives entirely in the Zenoh key (D1: the version is folded into
//! `<Body as ContractBody>::TOPIC`), so a receiver's per-key subscription is
//! the fast-reject; the decode path only still validates the codec. A decode
//! failure is counted (`decode_errors`) + logged as a health signal, never a
//! silent accept. Timeline-aware handles separately count purged or
//! retired-timeline samples in `timeline_filtered`, so quarantine churn is not
//! confused with active-buffer loss.

use std::collections::VecDeque;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::task::JoinHandle;
use zenoh::bytes::Encoding;
use zenoh::key_expr::OwnedKeyExpr;
use zenoh::sample::Sample;

use crate::RetiredTimelines;
use crate::abi::{CodecId, encoding_string, parse_encoding_string};
use crate::codec::{Codec, MessagePack};
use crate::contract::{
    CommandContract, ContractBody, DiagnosticContract, MeasurementContract, StateContract,
    WorldClockContract,
};
use crate::error::{BusError, Result};
use crate::identity::TimelineId;
use crate::metadata::BusMetadata;
use crate::query::{QueryError, QueryFailure};
use crate::runtime_metrics::RuntimeMetricHandle;
use crate::session::Bus;
use crate::session::OUTBOUND_CAPACITY;
use crate::time::{CaptureStamp, LocalInstant, RobotInstant, TimeWindow};
use crate::topic::{AskQuery, Publish, Subscribe, Topic};

/// The Phoxal-pinned finite query timeout (D31) - not Zenoh's 10 s default.
pub const DEFAULT_QUERY_TIMEOUT: Duration = Duration::from_secs(5);

/// Foreign-timeline samples are quarantined for a small number of possible next
/// world histories. Each timeline remains bounded by the receiving handle's
/// ordinary capacity, and active-timeline data always has its own independent
/// storage.
const PENDING_TIMELINE_CAPACITY: usize = 4;

mod sealed {
    pub trait Sealed {}
}

/// The robot instant a completed step stamps its outputs with.
///
/// Implemented only by [`StepToken`] and [`WorldStepToken`], and sealed, so no
/// other type can ever stamp a checked publication. Both are framework-minted:
/// in ordinary authoring the only way to hold one is to have actually reached
/// the step it names (see [`StepToken::__mint`] for the exact strength of that
/// claim).
pub trait StepStamp: sealed::Sealed {
    /// The instant this step completed at.
    fn instant(&self) -> RobotInstant;
}

/// Proof that the runner released a scheduled `#[step]` at a robot instant.
///
/// The runner is the only minter on the documented surface. Handing it to
/// [`StatePublisher::publish`](StatePublisher::publish) is the sole way a
/// service expresses robot time: there is no other constructor a participant
/// reaches through the ordinary `phoxal::prelude` surface, and the role markers
/// make handing it to the wrong publisher a compile error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StepToken {
    at: RobotInstant,
}

impl StepToken {
    /// Framework-internal (runner-only) minter. `#[doc(hidden)]`.
    ///
    /// This is `pub` because the runner lives in the `phoxal` crate while the
    /// token lives here, and Rust has no visibility between "this crate" and
    /// "the world". So the honest statement of the guarantee is: a participant
    /// cannot express robot time it did not reach *by accident*, and cannot do
    /// it at all through the documented surface - but a participant that
    /// deliberately writes `StepToken::__mint` can. Closing that would mean
    /// merging the api, bus, and runtime crates so this could be
    /// `pub(crate)`; see `phoxal::raw`'s module docs.
    #[doc(hidden)]
    pub const fn __mint(at: RobotInstant) -> Self {
        StepToken { at }
    }
}

impl sealed::Sealed for StepToken {}

impl StepStamp for StepToken {
    fn instant(&self) -> RobotInstant {
        self.at
    }
}

/// Proof that the world authority completed one world step.
///
/// The externally driven simulation controller has no framework `#[step]`: it
/// is driven by the simulator's own advance call, so no runner-minted
/// [`StepToken`] can cover it. A [`TimelineAuthority`] mints this token once per
/// completed world advance, for all outputs of that advance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorldStepToken {
    at: RobotInstant,
}

impl sealed::Sealed for WorldStepToken {}

impl StepStamp for WorldStepToken {
    fn instant(&self) -> RobotInstant {
        self.at
    }
}

/// Ownership of exactly one timeline's coordinate.
///
/// This is the narrowly scoped answer to "who may say what time it is in a
/// world nobody schedules". A second authority in one process is rejected at
/// mint (a per-process runtime backstop, [`TIMELINE_AUTHORITY_HELD`]). Across
/// processes the invariant is a selection-time one: exactly one simulator
/// participant is launched into a simulation.
///
/// **Only a simulator can mint world steps, and this is now a compiler rule,
/// not just a doc comment (organization#957 leftover fixed).** The documented
/// authoring surface a participant actually writes against has exactly one
/// path to an authority - `SetupContextSimulatorExt::timeline_authority` in
/// the `phoxal` crate - and that trait's impl requires `Self: IsSimulator`, so
/// a `#[phoxal::service]` or `#[phoxal::driver]` reaching for it fails to
/// compile. [`__mint`](Self::__mint) below is `pub` only because the bus crate
/// (where the token type lives) and the participant crate (where the
/// `IsSimulator` marker lives) are split, so Rust has no `pub(crate)`-across-
/// crates visibility to express the real boundary with - exactly the same
/// constraint [`StepToken::__mint`] documents for the analogous case. A
/// participant that deliberately bypasses `SetupContextSimulatorExt` and
/// writes `TimelineAuthority::__mint` directly still can; closing that would
/// mean merging the bus, api, and participant crates.
pub struct TimelineAuthority {
    timeline: TimelineId,
}

/// One authority per process: the runtime backstop. The cross-process "exactly
/// one authority" rule is a selection-time property of which participants are
/// launched, not something this process can observe.
static TIMELINE_AUTHORITY_HELD: AtomicBool = AtomicBool::new(false);

impl TimelineAuthority {
    /// Framework-internal minter. The author-facing path is
    /// `SetupContextSimulatorExt::timeline_authority` in `#[setup]` (`Self:
    /// IsSimulator`-gated - see the struct's docs for the exact strength of
    /// that guarantee). Fails if this process already holds one.
    /// `#[doc(hidden)]`.
    #[doc(hidden)]
    pub fn __mint(timeline: TimelineId) -> Result<Self> {
        if TIMELINE_AUTHORITY_HELD.swap(true, Ordering::AcqRel) {
            return Err(BusError::Namespace(
                "a second timeline authority was requested; exactly one participant may own a \
                 timeline"
                    .to_string(),
            ));
        }
        Ok(TimelineAuthority { timeline })
    }

    /// The timeline this authority owns.
    pub const fn timeline(&self) -> TimelineId {
        self.timeline
    }

    /// Begin a new world history on this authority (a reset or replay branch).
    ///
    /// The authority itself is unique for the process; the *timeline* it owns
    /// is replaced, which is exactly the "simulation reset creates a new
    /// timeline within the same execution" lifecycle rule.
    pub fn replace_timeline(&mut self, timeline: TimelineId) {
        self.timeline = timeline;
    }

    /// Mint the token for one completed world step at `ticks` on this
    /// authority's timeline.
    pub const fn completed_step(&self, ticks: u64) -> WorldStepToken {
        WorldStepToken {
            at: RobotInstant::new(self.timeline, ticks),
        }
    }
}

impl Drop for TimelineAuthority {
    fn drop(&mut self) {
        TIMELINE_AUTHORITY_HELD.store(false, Ordering::Release);
    }
}

/// The shared publish path: encode, build provenance, enqueue. Private, so the
/// only public way to reach it is through a role-bounded publisher.
struct Outbox<B> {
    bus: Bus,
    key: String,
    metric: RuntimeMetricHandle,
    _body: PhantomData<fn() -> B>,
}

// Manual (not `#[derive(Clone)]`) so cloning never spuriously requires
// `B: Clone` - every field it actually holds is `Clone` regardless of `B`. All
// real operations take `&self`, so a clone is just a second handle to the same
// publish key on the same session (the runner's `Arc<Self::Api>`
// snapshot-sharing, D3, relies on every `Api` field type being cheaply `Clone`
// this way).
impl<B> Clone for Outbox<B> {
    fn clone(&self) -> Self {
        Outbox {
            bus: self.bus.clone(),
            key: self.key.clone(),
            metric: self.metric.clone(),
            _body: PhantomData,
        }
    }
}

impl<B: ContractBody> Outbox<B> {
    fn new(bus: Bus, topic: &Topic<Publish<B>>) -> Result<Self> {
        let topic_key = topic.publish_key()?;
        let metric = bus
            .runtime_metrics()
            .register_outbound(topic_key, OUTBOUND_CAPACITY);
        let key = bus.full_key(topic_key);
        Ok(Outbox {
            bus,
            key,
            metric,
            _body: PhantomData,
        })
    }

    /// Encode `body`, build the [`BusMetadata`], and enqueue it. Returns
    /// immediately. A saturated outbound queue (sample or byte bound) returns
    /// [`BusError::Saturated`] - the sample was dropped and `outbound_drops`
    /// bumped - so the caller can observe the loss; a closed session returns
    /// [`BusError::Closed`].
    fn emit(&self, produced_at: Option<TimeWindow>, body: B) -> Result<()> {
        let payload = MessagePack::encode(&body)?;
        let metadata = self.bus.metadata(produced_at);
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

macro_rules! role_publisher {
    ($name:ident, $bound:ident, $doc:literal) => {
        #[doc = $doc]
        ///
        /// The role marker is a bound on the *type*, not just on its methods,
        /// so naming the wrong publisher for a contract is rejected where the
        /// `Api` struct declares the field - the earliest and clearest place.
        pub struct $name<B: $bound>(Outbox<B>);

        impl<B: $bound> Clone for $name<B> {
            fn clone(&self) -> Self {
                $name(self.0.clone())
            }
        }

        impl<B: $bound> $name<B> {
            /// Framework-internal (macro/runner-only): build the handle over a
            /// topic. The author-facing path is the matching `ctx.*_publisher(...)`
            /// builder in `#[setup]`. `#[doc(hidden)]`.
            #[doc(hidden)]
            pub fn new(bus: Bus, topic: &Topic<Publish<B>>) -> Result<Self> {
                Ok($name(Outbox::new(bus, topic)?))
            }
        }
    };
}

role_publisher!(
    StatePublisher,
    StateContract,
    "Publishes state at a logical step.\n\nThe step instant comes from a \
     framework-minted [`StepToken`] or [`WorldStepToken`], so a participant \
     cannot publish state at a time it did not reach. Non-blocking, so it is \
     safe to call from the step loop (D35/D43e). The framework's own \
     world-clock contract is deliberately NOT a `StateContract` and so cannot \
     be named here; see [`WorldClockPublisher`]."
);

role_publisher!(
    MeasurementPublisher,
    MeasurementContract,
    "Publishes a sensor observation with its capture stamp.\n\nThe driver owns \
     mapping its device clock into robot time - including reset, drift, \
     wraparound, batching, and exposure-versus-readout semantics - and says so \
     honestly through [`CaptureStamp`], which can represent an untranslated \
     capture rather than inventing an instant."
);

role_publisher!(
    CommandPublisher,
    CommandContract,
    "Sends a command.\n\nA command is a request, not an observation: it \
     expresses no robot time. The owning service stamps its own observation and \
     applies the result at a logical step."
);

role_publisher!(
    DiagnosticPublisher,
    DiagnosticContract,
    "Publishes an output that describes the participant rather than the world \
     (health, logs, runtime evidence). It expresses no robot time."
);

role_publisher!(
    WorldClockPublisher,
    WorldClockContract,
    "Publishes the framework's own world-clock contract at a logical step.\n\n\
     A near-twin of [`StatePublisher`] - same step-stamped publish path, same \
     `#[doc(hidden)] new` - kept as its own type rather than folded into \
     `StatePublisher` precisely so `StatePublisher`'s bound can stay the \
     precise `StateContract` (see that type's docs). The only documented way \
     to build one is `SetupContextSimulatorExt::world_clock_publisher` in the \
     `phoxal` crate (`Self: IsSimulator`-gated) - organization#957's leftover, \
     now a compiler rule."
);

impl<B: StateContract> StatePublisher<B> {
    /// Publish `body` as the state this step produced.
    pub fn publish(&self, step: &impl StepStamp, body: B) -> Result<()> {
        self.0.emit(Some(TimeWindow::exact(step.instant())), body)
    }
}

impl<B: WorldClockContract> WorldClockPublisher<B> {
    /// Publish `body` as the state this step produced.
    pub fn publish(&self, step: &impl StepStamp, body: B) -> Result<()> {
        self.0.emit(Some(TimeWindow::exact(step.instant())), body)
    }
}

impl<B: MeasurementContract> MeasurementPublisher<B> {
    /// Publish `body` as captured at `stamp`.
    pub fn publish(&self, stamp: CaptureStamp, body: B) -> Result<()> {
        self.0.emit(stamp.into_window(), body)
    }
}

impl<B: CommandContract> CommandPublisher<B> {
    /// Send `body` to the contract's owning service.
    pub fn send(&self, body: B) -> Result<()> {
        self.0.emit(None, body)
    }
}

impl<B: DiagnosticContract> DiagnosticPublisher<B> {
    /// Publish `body`.
    pub fn publish(&self, body: B) -> Result<()> {
        self.0.emit(None, body)
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

// Manual, unbounded on `Req`/`Resp` - see `Outbox`'s `Clone` impl docs for
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
    /// The request body is MessagePack-encoded with mirroring provenance; a
    /// request expresses no robot time, so `produced_at` is `None`. The wait is
    /// bounded by this querier's timeout.
    pub async fn query(&self, request: Req) -> std::result::Result<Resp, QueryError> {
        let payload =
            MessagePack::encode(&request).map_err(|e| QueryError::Protocol(e.to_string()))?;
        let metadata = self.bus.metadata(None);
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

/// Keep-last-1 view of a topic: the most recently received sample, provenance
/// included.
///
/// A background task overwrites a single slot with each decoded sample, so a
/// reader always sees current state and never a backlog. Use this when only the
/// latest value matters (the common case for periodic state); reach for
/// [`Subscriber`] when a bounded history is useful. Decode failures are counted
/// and logged, not stored. Once a timeline barrier is active, possible
/// replacement timelines are kept in a separate bounded quarantine and remain
/// invisible until their matching timeline is activated. The subscription lives
/// until the `Latest` is dropped.
pub struct Latest<B> {
    state: Arc<Mutex<LatestState<B>>>,
    metric: RuntimeMetricHandle,
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
// aborts only when the *last* clone drops. `latest()` only ever reads the
// shared slot (`&self`), so every clone always observes the same freshest
// sample: safe to hand to a concurrent reader (D3's snapshot-server `Api`
// sharing).
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
    /// The author-facing path is `ctx.latest(...)` in `#[setup]`. `#[doc(hidden)]`.
    #[doc(hidden)]
    pub async fn new(bus: &Bus, topic: &Topic<Subscribe<B>>) -> Result<Self> {
        let state = Arc::new(Mutex::new(LatestState {
            active_timeline: None,
            observed: None,
            pending: VecDeque::with_capacity(PENDING_TIMELINE_CAPACITY),
            retired_timelines: RetiredTimelines::default(),
        }));
        let store = Arc::clone(&state);
        let metric = bus.runtime_metrics().register_latest(topic.key());
        let observe = metric.clone();
        let topic_owned = topic.key().to_string();
        let guard = spawn_subscription::<B, _>(
            bus,
            topic.key(),
            move |observed| {
                let mut state = store.lock().expect("latest mutex poisoned");
                match state.ingest(observed) {
                    LatestIngest::Active { overwrote } => observe.record_latest(overwrote),
                    LatestIngest::Pending {
                        timeline,
                        new_timeline,
                        filtered,
                    } => {
                        observe.record_pending_latest();
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
        )
        .await?;
        Ok(Latest {
            state,
            metric,
            _guard: Arc::new(guard),
        })
    }

    /// The most recent sample with its provenance and observation stamp, or
    /// `None` if nothing has arrived yet.
    pub fn observed(&self) -> Option<Observed<B>> {
        let observed = self
            .state
            .lock()
            .expect("latest mutex poisoned")
            .observed
            .clone();
        observed.map(|observed| Observed {
            body: observed.body.clone(),
            metadata: observed.metadata.clone(),
            observed_at: observed.observed_at,
        })
    }

    /// The most recent decoded body, for consumers that need no provenance.
    pub fn latest(&self) -> Option<B> {
        self.observed().map(|observed| observed.body)
    }

    /// Framework lifecycle hook: discard a retained value from another timeline
    /// while preserving a value already received for `timeline`.
    #[doc(hidden)]
    pub fn __retain_timeline(&self, timeline: TimelineId) {
        let mut state = self.state.lock().expect("latest mutex poisoned");
        let (filtered, occupied) = state.retain_timeline(timeline);
        self.metric.record_timeline_filtered(filtered);
        self.metric.record_latest_depth(occupied);
    }
}

/// A drop-oldest ring subscription of observed samples.
///
/// A background task pushes each decoded sample onto a bounded ring (the depth
/// is set at construction). When a slow consumer lets the ring fill, the oldest
/// buffered sample is evicted and `inbound_drops` is bumped - the newest sample
/// always wins, the backlog never grows without bound. Use this when a short
/// history is useful; reach for [`Latest`] when only current state matters.
/// Decode failures are counted + logged, not buffered. Once a timeline barrier
/// is active, possible replacement timelines are kept in separate bounded rings
/// and remain invisible until their matching timeline is activated. The
/// subscription lives until the last clone of the `Subscriber` is dropped.
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
/// than one place (its `.observed()` is a non-destructive clone from one
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
    /// The author-facing path is `ctx.subscriber(...)` in `#[setup]`. `#[doc(hidden)]`.
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
        )
        .await?;
        Ok(Subscriber {
            ring,
            _guard: Arc::new(guard),
        })
    }

    /// Await the next observed sample (drop-oldest under congestion).
    ///
    /// **Destructive**: this pops from the ring, so the sample is delivered to
    /// exactly this caller. If this `Subscriber` was cloned (e.g. into the
    /// `Arc<Self::Api>` snapshot the runner hands concurrent
    /// `#[server_snapshot]` handlers), every clone competes for the same queue
    /// (see the [type docs](Self)). Do not `recv` a `Subscriber` from a
    /// snapshot server; read committed `Snapshot` state instead.
    pub async fn recv(&self) -> Result<Observed<B>> {
        let (observed, _current_depth) = self.ring.recv().await;
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

    /// Cumulative samples evicted from this subscriber's bounded ring.
    ///
    /// This counter is local to this subscription (unlike the aggregate bus
    /// health counter), allowing retention consumers to disclose their own
    /// ingestion loss explicitly.
    pub fn dropped(&self) -> u64 {
        self.ring.dropped.load(Ordering::Relaxed)
    }

    /// Framework lifecycle hook: discard queued samples from other timelines
    /// while preserving samples already received for `timeline`.
    #[doc(hidden)]
    pub fn __retain_timeline(&self, timeline: TimelineId) {
        self.ring.retain_timeline(timeline);
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
    fn new(cap: usize, metric: RuntimeMetricHandle) -> Self {
        Ring {
            state: Mutex::new(RingState {
                active_timeline: None,
                buf: VecDeque::with_capacity(cap),
                pending: VecDeque::with_capacity(PENDING_TIMELINE_CAPACITY),
                retired_timelines: RetiredTimelines::default(),
            }),
            notify: Notify::new(),
            cap,
            dropped: AtomicU64::new(0),
            metric,
        }
    }

    /// Push into the active queue or a bounded foreign-timeline quarantine.
    fn push(&self, item: Observed<B>) -> RingPush {
        let mut state = self.state.lock().expect("ring mutex poisoned");
        // A sample expressing no robot time belongs to no world history and is
        // never quarantined.
        let timeline = item.timeline();
        if let (Some(timeline), Some(active_timeline)) = (timeline, state.active_timeline) {
            if timeline != active_timeline {
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
                        if state.pending.len() == PENDING_TIMELINE_CAPACITY {
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
                    pending.buf.pop_front();
                    self.metric.record_timeline_filtered(1);
                }
                pending.buf.push_back(item);
                self.metric.record_pending_subscriber();
                return RingPush {
                    accepted: true,
                    evicted: false,
                    new_pending_timeline,
                };
            }
        }

        let mut dropped = false;
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
            new_pending_timeline: None,
        }
    }

    fn try_pop(&self) -> Option<(Observed<B>, usize)> {
        let mut state = self.state.lock().expect("ring mutex poisoned");
        let item = state.buf.pop_front()?;
        let depth = state.buf.len();
        self.metric.record_subscriber_pop(depth);
        Some((item, depth))
    }

    fn retain_timeline(&self, timeline: TimelineId) {
        let mut state = self.state.lock().expect("ring mutex poisoned");
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
            let mut promoted = state
                .pending
                .remove(index)
                .expect("pending timeline index must remain valid")
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
        self.metric.record_timeline_filtered(filtered);
        self.metric.record_subscriber_pop(state.buf.len());
        let notify = !state.buf.is_empty();
        drop(state);
        if notify {
            self.notify.notify_waiters();
        }
    }

    async fn recv(&self) -> (Observed<B>, usize) {
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
/// task that stamps, decodes, and feeds each sample to `on_sample`.
///
/// The observation instant is taken **before** decode, so ring residence and
/// decode cost land inside the measured age rather than outside it. Decode
/// failures are counted + logged, never silently accepted.
async fn spawn_subscription<B, F>(
    bus: &Bus,
    topic_key: &str,
    mut on_sample: F,
    metric: RuntimeMetricHandle,
) -> Result<SubscriptionGuard>
where
    B: ContractBody,
    F: FnMut(Observed<B>) + Send + 'static,
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
    use crate::identity::ProducerId;
    use std::sync::Barrier;

    fn timeline(value: u64) -> TimelineId {
        TimelineId::from_raw(value).expect("test timeline must be nonzero")
    }

    fn observed(body: u8, line: Option<u64>) -> Observed<u8> {
        Observed {
            body,
            metadata: BusMetadata {
                codec: CodecId::MessagePack.as_u8(),
                producer: ProducerId::mint(),
                sequence: u64::from(body),
                produced_at: line
                    .map(|line| TimeWindow::exact(RobotInstant::new(timeline(line), 0))),
                participant: "test".to_string(),
            },
            observed_at: LocalInstant::try_now().expect("test host clock"),
        }
    }

    #[test]
    fn ring_counts_each_drop_oldest_eviction_cumulatively() {
        let metrics = crate::runtime_metrics::RuntimeMetrics::default();
        let metric = metrics.register_subscriber("v0.1/test/state", 1);
        let ring = Ring::new(1, metric);
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
    fn a_sample_expressing_no_robot_time_survives_every_timeline_barrier() {
        let metrics = crate::runtime_metrics::RuntimeMetrics::default();
        let metric = metrics.register_subscriber("v0.1/test/command", 4);
        let ring = Ring::new(4, metric);
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
            ingress_state
                .lock()
                .expect("latest mutex poisoned")
                .ingest(observed(2, Some(2)));
        });
        let clock_state = Arc::clone(&state);
        let clock_barrier = Arc::clone(&barrier);
        let clock = std::thread::spawn(move || {
            clock_barrier.wait();
            clock_state
                .lock()
                .expect("latest mutex poisoned")
                .retain_timeline(timeline(2));
        });
        barrier.wait();
        ingress.join().expect("ingress thread should join");
        clock.join().expect("clock thread should join");

        let mut state = state.lock().expect("latest mutex poisoned");
        assert_eq!(state.active_timeline, Some(timeline(2)));
        assert_eq!(state.observed.as_ref().map(|sample| sample.body), Some(2));
        assert!(matches!(
            state.ingest(observed(3, Some(1))),
            LatestIngest::Filtered
        ));
        assert_eq!(state.observed.as_ref().map(|sample| sample.body), Some(2));
    }

    #[test]
    fn subscriber_activation_is_safe_when_replacement_ingress_races_the_clock() {
        let metrics = crate::runtime_metrics::RuntimeMetrics::default();
        let metric = metrics.register_subscriber("v0.1/test/state", 4);
        let ring = Arc::new(Ring::new(4, metric));
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

    #[test]
    fn only_one_timeline_authority_exists_at_a_time() {
        let first = TimelineAuthority::__mint(timeline(1)).expect("first authority should mint");
        assert!(
            TimelineAuthority::__mint(timeline(2)).is_err(),
            "a second authority must be rejected at startup"
        );
        assert_eq!(first.completed_step(50).instant().ticks(), 50);
        drop(first);
        TimelineAuthority::__mint(timeline(3)).expect("the slot is released on drop");
    }
}
