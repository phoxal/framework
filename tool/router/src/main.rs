use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use phoxal::prelude::*;
use phoxal::raw::{
    Bus, BusMetadata, Codec, CodecId, ContractBody, MessagePack, OwnerCap, Publish, Source, Topic,
    encoding_string, host_time, parse_encoding_string,
};
use phoxal_api::v1 as api;
use phoxal_api::v2 as preview_api;
use zenoh_router as zenoh;

const DEFAULT_LISTEN: &str = "tcp/localhost:7447";
const DEFAULT_RETRY_INITIAL_MS: u64 = 1_000;
const DEFAULT_RETRY_MAX_MS: u64 = 30_000;
const SERIAL_LISTEN_TRANSPORT_DIAGNOSTIC: &str = "serial_listen_transport_unavailable";

/// The mirror-subscription measurement window: per-topic counts are rolled up
/// and republished on this cadence.
const METRICS_WINDOW: Duration = Duration::from_secs(1);
const UPLINK_STATE_WINDOW: Duration = Duration::from_secs(1);
const UPLINK_DNS_REFRESH: Duration = Duration::from_secs(30);
const UPLINK_DNS_INITIAL_RETRY: Duration = Duration::from_secs(2);
const UPLINK_DNS_TIMEOUT: Duration = Duration::from_millis(500);
/// Bound both the router's long-lived counter map and each published snapshot.
/// Traffic beyond the cap still contributes to total throughput, while the API
/// sentinel row discloses that the detailed table is incomplete.
const MAX_METRIC_ROWS: usize = 256;
/// A non-blocking handoff prevents metrics observation from applying
/// backpressure to Zenoh's forwarding path. The callback performs only bounded
/// key/attachment copies and cheap shape checks; decoding happens in the ingest
/// task. When full, samples are counted in the aggregate overflow row.
const METRICS_INGEST_QUEUE: usize = 4_096;
const MAX_METRIC_TOPIC_BYTES: usize = 256;
const MAX_METRIC_ENCODING_BYTES: usize = 128;
const MAX_METRIC_ATTACHMENT_BYTES: usize = 512;
const MAX_METRIC_PARTICIPANT_BYTES: usize = 128;
/// Bound the combined identity text retained by the detailed table. Together
/// with `MAX_METRIC_ROWS`, this keeps the once-per-second MessagePack snapshot
/// below 64 KiB (pinned by a worst-case regression test); oversized identities
/// remain accounted for by the aggregate sentinel.
const MAX_METRIC_IDENTITY_BYTES: usize = 32 * 1_024;
#[cfg(test)]
const MAX_METRICS_WIRE_BYTES: usize = 64 * 1_024;
/// A small fixed candidate set tracks frequent rejected keys without making
/// high-cardinality overflow work proportional to the 255-row detail table.
const MAX_ADMISSION_CANDIDATES: usize = 16;
/// A newcomer must beat the quietest admitted row by this many samples in one
/// window. This prevents one-shot keys from churning sparse but live topics.
const ADMISSION_MARGIN: u64 = 4;
/// Keep a few quiet samples visible so a topic does not flicker out between
/// sparse publications.
const PUBLISH_IDLE_WINDOWS: u32 = 5;
/// Retire a topic/producer counter as soon as its quiet display grace expires. Its cumulative
/// count intentionally starts over if that pair later returns; bounding the
/// router's useful telemetry slots is more important than retaining an all-time
/// count for inactive participants.
const EVICT_IDLE_WINDOWS: u32 = PUBLISH_IDLE_WINDOWS + 1;
/// A mirrored key counts toward `Metrics` only if it has a Phoxal bus shape:
/// either a raw version-qualified contract key (`v1/drive/state`) or a rooted
/// key (`<namespace>/robots/<robot-id>/v1/drive/state`). Merely containing a
/// `v<n>` chunk is not enough: generic paths such as `http/v1/users` must not
/// be reported as Phoxal traffic.
fn is_version_topic(key: &str) -> bool {
    fn is_version_segment(segment: &str) -> bool {
        segment
            .strip_prefix('v')
            .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
    }

    let mut segments = key.split('/');
    let Some(first) = segments.next() else {
        return false;
    };
    // Keep the predicate useful for raw contract keys in tests and diagnostics;
    // the production mirror always reaches the rooted branch below.
    if is_version_segment(first) {
        return segments.next().is_some_and(|segment| !segment.is_empty());
    }

    let mut one: Option<&str> = None;
    let mut two: Option<&str> = None;
    let mut three: Option<&str> = Some(first);
    let mut current_index = 1_usize;
    for current in segments {
        if one == Some("robots")
            && two.is_some_and(|robot| !robot.is_empty())
            && three.is_some_and(is_version_segment)
            && !current.is_empty()
            && current_index >= 4
        {
            return true;
        }
        one = two;
        two = three;
        three = Some(current);
        current_index = current_index.saturating_add(1);
    }
    false
}

/// Launch-time router configuration carried in `PHOXAL_CONFIG`.
#[derive(Clone, Debug, Default, serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
struct RouterConfig {
    /// Additional listen endpoints merged by the launch plan from `bus.listen`.
    #[serde(default)]
    listen: Vec<String>,
    /// Optional upstream router connection for deployed robots.
    #[serde(default)]
    uplink: Option<UplinkConfig>,
}

/// Optional upstream connection configuration for the site router.
#[derive(Clone, Debug, serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
struct UplinkConfig {
    /// Upstream Zenoh endpoint the router should connect to.
    connect: String,
    /// Optional mTLS material by path, installed by deploy outside the release dir.
    #[serde(default)]
    auth: Option<MtlsAuth>,
    /// Capped retry backoff; defaults retry forever and never gates readiness.
    #[serde(default)]
    retry: RetryConfig,
}

/// mTLS material used when connecting the router uplink.
#[derive(Clone, Debug, serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
struct MtlsAuth {
    /// Root CA certificate path used to verify the upstream router.
    ca: String,
    /// Client certificate path identifying this robot/site to the upstream.
    cert: String,
    /// Client private-key path paired with `cert`.
    key: String,
}

/// Capped backoff configuration for the optional uplink.
#[derive(Clone, Debug, serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
struct RetryConfig {
    /// Initial retry delay in milliseconds.
    #[serde(default = "default_retry_initial_ms")]
    initial_ms: u64,
    /// Maximum retry delay in milliseconds.
    #[serde(default = "default_retry_max_ms")]
    max_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            initial_ms: DEFAULT_RETRY_INITIAL_MS,
            max_ms: DEFAULT_RETRY_MAX_MS,
        }
    }
}

fn default_retry_initial_ms() -> u64 {
    DEFAULT_RETRY_INITIAL_MS
}

fn default_retry_max_ms() -> u64 {
    DEFAULT_RETRY_MAX_MS
}

// Tools stay raw-bus only (decided 2026-07-09): no declared `Api` surface,
// just `ctx.raw_bus()` and the raw handle constructors.
#[phoxal::tool(id = "router", config = RouterConfig)]
struct ToolRouter {
    router: zenoh::Session,
}

#[phoxal::behavior]
impl ToolRouter {
    #[setup]
    async fn setup(
        ctx: &mut SetupContext<Self>,
        config: RouterConfig,
    ) -> Result<(Self, Self::Api)> {
        let zenoh_config = zenoh_router_config(&config)?;
        let router = zenoh::open(zenoh_config)
            .await
            .map_err(|error| anyhow::anyhow!("failed to open embedded Zenoh router: {error}"))?;

        let bus = ctx.raw_bus();
        let cap = ctx.owner_capability();
        spawn_uplink_state(ctx, &router, bus.clone(), cap, config.uplink.clone()).await?;

        spawn_metrics(ctx, &router, bus).await?;

        // Dynamic egress downsampling belongs here later: this process owns the
        // uplink session and can re-establish it with refreshed rules.
        tracing::info!(
            target: "tool_router",
            listen = ?listen_endpoints(&config),
            uplink = config.uplink.as_ref().map(|uplink| uplink.connect.as_str()),
            "router ready"
        );

        Ok((Self { router }, ()))
    }

    #[shutdown]
    async fn shutdown(&mut self, _api: &mut Self::Api, _ctx: ShutdownContext) -> Result<()> {
        if let Err(error) = self.router.close().await {
            tracing::warn!(target: "tool_router", error = %error, "router close failed");
        }
        Ok(())
    }
}

/// Per-topic, per-producer ingress counters. `cumulative` never resets (the
/// contract's `count` field); `window` resets every [`METRICS_WINDOW`] tick
/// (feeds `ingress_rate_hz`). Keeping the producer in the key prevents a
/// shared topic from assigning all traffic to whichever publisher wrote last.
#[derive(Default)]
struct TopicCounter {
    cumulative: u64,
    window: u64,
    last_window: u64,
    idle_windows: u32,
}

#[derive(Clone, Copy, Default)]
struct AdmissionCandidate {
    /// Misra-Gries score used only for bounded promotion ordering.
    score: u64,
    /// Exact samples observed while this identity occupied a candidate slot.
    observed: u64,
}

/// Shared ingest state: one background task increments it per received sample,
/// another drains + resets the window count on a timer. Overflow traffic is
/// kept out of the bounded row map but remains part of total throughput.
#[derive(Default)]
struct IngressState {
    counters: HashMap<(String, String), TopicCounter>,
    identity_bytes: usize,
    /// Bounded Misra-Gries candidates let a frequently rejected newcomer
    /// displace a low-rate admitted row at the next window without allowing
    /// high-cardinality one-shot identities to grow memory or seize the table.
    admission_candidates: HashMap<(String, String), AdmissionCandidate>,
    overflow_window: u64,
    overflow_cumulative: u64,
}

type IngressCounters = Mutex<IngressState>;

struct MetricIdentity {
    topic: String,
    encoding: Option<String>,
    attachment: Option<Vec<u8>>,
}

/// A drained window's worth of one topic's ingress, ready to become a
/// `TopicMetric`.
struct TopicWindowSample {
    topic: String,
    from_participant: String,
    window_count: u64,
    cumulative_count: u64,
}

struct DrainedWindow {
    samples: Vec<TopicWindowSample>,
    overflow_count: u64,
    overflow_cumulative: u64,
    topics_truncated: bool,
}

/// Declare the wildcard mirror subscription on the embedded router's own
/// session and spawn the two managed tasks that turn it into
/// `router::Metrics`: one ingests samples into the shared counters, the
/// other drains + publishes on [`METRICS_WINDOW`].
///
/// The mirror is scoped to this robot's bus root. That prevents its subscription
/// from propagating across an uplink and pulling other robots' traffic back to
/// this router. It does not amplify itself: the publish cadence is timer-driven,
/// not triggered by inbound samples, so `Metrics` being itself mirrored and
/// counted (like any other local topic) never feeds back into more publishes.
async fn spawn_metrics(
    ctx: &mut SetupContext<ToolRouter>,
    router: &zenoh::Session,
    metrics_bus: Bus,
) -> Result<()> {
    let mirror_key_expr = mirror_key_expr(metrics_bus.root());
    let (mirror_tx, mut mirror_rx) = tokio::sync::mpsc::channel(METRICS_INGEST_QUEUE);
    let dropped_mirror_samples = Arc::new(AtomicU64::new(0));
    let callback_drops = Arc::clone(&dropped_mirror_samples);
    let mirror_subscriber = router
        .declare_subscriber(mirror_key_expr)
        .callback(move |sample| {
            enqueue_metric_sample(sample, &mirror_tx, &callback_drops);
        })
        .await
        .map_err(|error| anyhow::anyhow!("failed to declare router mirror subscriber: {error}"))?;

    let counters: Arc<IngressCounters> = Arc::new(Mutex::new(IngressState::default()));

    let ingest_counters = Arc::clone(&counters);
    ctx.spawn_managed_with(
        "router-metrics-ingest",
        ManagedTaskPolicy::FaultOnExit,
        async move {
            // Keep the callback declaration alive for the whole ingest task.
            let _mirror_subscriber = mirror_subscriber;
            while let Some(identity) = mirror_rx.recv().await {
                let from_participant = participant_from_attachment(
                    identity.encoding.as_deref(),
                    identity.attachment.as_deref(),
                );
                record_sample(&ingest_counters, identity.topic, from_participant);
            }
        },
    );

    let cap = ctx.owner_capability();
    let metrics_publisher = RouterPublisher::new(
        router,
        metrics_bus,
        &preview_api::topic::internal::new(cap).router().metrics(),
    )?;
    let publish_counters = Arc::clone(&counters);
    let publish_drops = Arc::clone(&dropped_mirror_samples);
    ctx.spawn_managed_with(
        "router-metrics-publish",
        ManagedTaskPolicy::FaultOnExit,
        async move {
            let mut ticker = tokio::time::interval(METRICS_WINDOW);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // Tokio intervals yield their first tick immediately. Consume that
            // bootstrap tick so the first published snapshot covers a real
            // window instead of reporting a near-zero interval as one second.
            ticker.tick().await;
            let mut window_started = tokio::time::Instant::now();
            loop {
                ticker.tick().await;
                let window_ended = tokio::time::Instant::now();
                record_unobserved_samples(
                    &publish_counters,
                    publish_drops.swap(0, Ordering::Relaxed),
                );
                let drained = drain_window(&publish_counters);
                let metrics = build_metrics(
                    &drained.samples,
                    drained.overflow_count,
                    drained.overflow_cumulative,
                    drained.topics_truncated,
                    window_ended.duration_since(window_started),
                );
                window_started = window_ended;
                if let Err(error) = metrics_publisher.publish_at(host_time(), metrics).await {
                    tracing::warn!(target: "tool_router", error = %error, "router metrics publish failed");
                }
            }
        },
    );

    Ok(())
}

fn enqueue_metric_sample(
    sample: zenoh::sample::Sample,
    mirror_tx: &tokio::sync::mpsc::Sender<MetricIdentity>,
    dropped_samples: &AtomicU64,
) {
    let topic = sample.key_expr().as_str();
    if !is_version_topic(topic) {
        return;
    }
    if topic.len() > MAX_METRIC_TOPIC_BYTES {
        dropped_samples.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let encoding = sample.encoding().to_string();
    if encoding.len() > MAX_METRIC_ENCODING_BYTES {
        dropped_samples.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let attachment = match sample.attachment() {
        Some(attachment) if attachment.len() > MAX_METRIC_ATTACHMENT_BYTES => {
            dropped_samples.fetch_add(1, Ordering::Relaxed);
            return;
        }
        Some(attachment) => Some(attachment.to_bytes().into_owned()),
        None => None,
    };
    let metric_identity = MetricIdentity {
        topic: topic.to_string(),
        encoding: Some(encoding),
        attachment,
    };
    if mirror_tx.try_send(metric_identity).is_err() {
        dropped_samples.fetch_add(1, Ordering::Relaxed);
    }
}

async fn spawn_uplink_state(
    ctx: &mut SetupContext<ToolRouter>,
    router: &zenoh::Session,
    bus: Bus,
    cap: OwnerCap,
    uplink: Option<UplinkConfig>,
) -> Result<()> {
    let publisher = uplink_state_publisher(router, bus, cap)?;
    let matcher = match uplink.as_ref() {
        Some(uplink) => Some(UplinkMatcher::new(&uplink.connect).await?),
        None => None,
    };
    ctx.spawn_managed_with(
        "router-uplink-state-publish",
        ManagedTaskPolicy::FaultOnExit,
        async move {
            let mut matcher = matcher;
            let mut ticker = tokio::time::interval(UPLINK_STATE_WINDOW);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // Repeat even immutable Disabled state so late subscribers can
            // distinguish an intentionally absent uplink from missing telemetry.
            loop {
                ticker.tick().await;
                let state = match uplink.as_ref() {
                    Some(uplink) => {
                        let Some(matcher) = matcher.as_mut() else {
                            tracing::error!(target: "tool_router", "configured uplink matcher is unavailable");
                            continue;
                        };
                        observed_uplink_state(
                            &publisher.router,
                            uplink,
                            matcher,
                        )
                        .await
                    }
                    None => disabled_uplink_state(),
                };
                if let Err(error) = publish_uplink_state(&publisher, state).await {
                    tracing::warn!(target: "tool_router", error = %error, "uplink state publish failed");
                }
            }
        },
    );
    Ok(())
}

/// A typed publisher bound to the embedded router's own Zenoh session.
///
/// The normal runner bus is opened before `ToolRouter::setup`. With no connect
/// endpoint that session is deliberately in-process-only, so publishing router
/// telemetry through it makes the samples invisible to clients attached to the
/// router that setup opens moments later. Router-owned state must instead leave
/// through the embedded router session while retaining the same Phoxal key,
/// codec, provenance, and sequence envelope as an ordinary `Publisher`.
/// Unlike the normal bus handle, this direct Zenoh path cannot contribute to
/// `Bus::outbound_drops`; callers must treat a successful put as best-effort
/// delivery under Zenoh's congestion policy. Unlike the regular publisher's
/// non-blocking `try_publish` path, this helper awaits the Zenoh put and must
/// stay in a dedicated managed task rather than a runtime step loop.
struct RouterPublisher<B> {
    router: zenoh::Session,
    bus: Bus,
    key: String,
    _body: PhantomData<fn() -> B>,
}

impl<B: ContractBody> RouterPublisher<B> {
    fn new(router: &zenoh::Session, bus: Bus, topic: &Topic<Publish<B>>) -> Result<Self> {
        let key = bus.full_key(topic.publish_key()?);
        Ok(Self {
            router: router.clone(),
            bus,
            key,
            _body: PhantomData,
        })
    }

    async fn publish_at(&self, at: LogicalTime, body: B) -> Result<()> {
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
        self.router
            .put(self.key.clone(), payload)
            .encoding(encoding_string(MessagePack::ID))
            .attachment(metadata.encode())
            .await
            .map_err(|error| anyhow::anyhow!("router telemetry publish failed: {error}"))?;
        Ok(())
    }
}

/// Best-effort producer id decoded in the ingest task from the bounded copy of
/// a Phoxal `BusMetadata` attachment. `None` (not an error) when the attachment
/// is absent, malformed, or empty - not every mirrored key carries Phoxal's
/// envelope, and the measured rate/count stay honest either way.
fn participant_from_attachment(
    encoding: Option<&str>,
    attachment: Option<&[u8]>,
) -> Option<String> {
    let encoding = parse_encoding_string(encoding?).ok()?;
    if encoding.codec_id() != Some(CodecId::MessagePack) {
        return None;
    }
    let metadata = BusMetadata::decode(attachment?).ok()?;
    if metadata.codec != encoding.codec
        || metadata.source.participant.is_empty()
        || metadata.source.participant.len() > MAX_METRIC_PARTICIPANT_BYTES
    {
        None
    } else {
        Some(metadata.source.participant)
    }
}

fn mirror_key_expr(root: &str) -> String {
    format!("{root}/**")
}

fn record_admission_candidate(
    candidates: &mut HashMap<(String, String), AdmissionCandidate>,
    key: (String, String),
) {
    if let Some(candidate) = candidates.get_mut(&key) {
        candidate.score = candidate.score.saturating_add(1);
        candidate.observed = candidate.observed.saturating_add(1);
        return;
    }
    if candidates.len() < MAX_ADMISSION_CANDIDATES {
        candidates.insert(
            key,
            AdmissionCandidate {
                score: 1,
                observed: 1,
            },
        );
        return;
    }

    for candidate in candidates.values_mut() {
        candidate.score = candidate.score.saturating_sub(1);
    }
    candidates.retain(|_, candidate| candidate.score > 0);
}

fn record_unobserved_samples(counters: &IngressCounters, count: u64) {
    if count == 0 {
        return;
    }
    let mut guard = counters
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.overflow_window = guard.overflow_window.saturating_add(count);
    guard.overflow_cumulative = guard.overflow_cumulative.saturating_add(count);
}

fn record_sample(counters: &IngressCounters, topic: String, from_participant: Option<String>) {
    let mut guard = counters
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let key = (topic, from_participant.unwrap_or_default());
    let is_new = !guard.counters.contains_key(&key);
    let key_bytes = metric_identity_bytes(&key);
    if is_new
        && (guard.counters.len() >= MAX_METRIC_ROWS.saturating_sub(1)
            || guard.identity_bytes.saturating_add(key_bytes) > MAX_METRIC_IDENTITY_BYTES)
    {
        guard.overflow_window = guard.overflow_window.saturating_add(1);
        guard.overflow_cumulative = guard.overflow_cumulative.saturating_add(1);
        record_admission_candidate(&mut guard.admission_candidates, key);
        return;
    }
    if is_new {
        guard.identity_bytes = guard.identity_bytes.saturating_add(key_bytes);
    }
    let entry = guard.counters.entry(key).or_default();
    entry.cumulative = entry.cumulative.saturating_add(1);
    entry.window = entry.window.saturating_add(1);
    entry.idle_windows = 0;
}

/// Snapshot every topic's window count (resetting it to 0) and cumulative
/// count (left untouched).
fn drain_window(counters: &IngressCounters) -> DrainedWindow {
    let mut guard = counters
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for counter in guard.counters.values_mut() {
        if counter.window == 0 {
            counter.idle_windows = counter.idle_windows.saturating_add(1);
        } else {
            counter.idle_windows = 0;
        }
        counter.last_window = std::mem::take(&mut counter.window);
    }
    promote_admission_candidates(&mut guard);
    guard
        .counters
        .retain(|_, counter| counter.idle_windows < EVICT_IDLE_WINDOWS);
    let samples = guard
        .counters
        .iter()
        .filter(|(_, counter)| counter.idle_windows <= PUBLISH_IDLE_WINDOWS)
        .map(|((topic, from_participant), counter)| TopicWindowSample {
            topic: topic.clone(),
            from_participant: from_participant.clone(),
            window_count: counter.last_window,
            cumulative_count: counter.cumulative,
        })
        .collect();
    let overflow_count = std::mem::take(&mut guard.overflow_window);
    let overflow_cumulative = guard.overflow_cumulative;
    let topics_truncated = overflow_count > 0 || overflow_cumulative > 0;
    DrainedWindow {
        samples,
        overflow_count,
        overflow_cumulative,
        topics_truncated,
    }
}

fn promote_admission_candidates(state: &mut IngressState) {
    // Keep the byte budget derived from the actual table so eviction and test
    // fixtures cannot leave the cached total stale.
    state.identity_bytes = state.counters.keys().map(metric_identity_bytes).sum();
    let mut candidates = std::mem::take(&mut state.admission_candidates)
        .into_iter()
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .1
            .score
            .cmp(&left.1.score)
            .then_with(|| left.0.cmp(&right.0))
    });

    for (candidate, admission) in candidates {
        if state.counters.contains_key(&candidate) {
            continue;
        }
        let candidate_bytes = metric_identity_bytes(&candidate);
        if state.counters.len() < MAX_METRIC_ROWS.saturating_sub(1)
            && state.identity_bytes.saturating_add(candidate_bytes) <= MAX_METRIC_IDENTITY_BYTES
        {
            reattribute_promoted_candidate(state, admission, None);
            state.counters.insert(
                candidate,
                TopicCounter {
                    cumulative: admission.observed,
                    last_window: admission.observed,
                    idle_windows: 0,
                    window: 0,
                },
            );
            state.identity_bytes = state.identity_bytes.saturating_add(candidate_bytes);
            continue;
        }
        let Some((lowest_key, lowest_window)) = state
            .counters
            .iter()
            .filter(|(key, counter)| {
                // Never destroy the cumulative count of a live row to admit a
                // bursty newcomer. Replacement is allowed only at the end of
                // the same quiet grace after which the row would be evicted.
                counter.idle_windows >= PUBLISH_IDLE_WINDOWS
                    && state
                        .identity_bytes
                        .saturating_sub(metric_identity_bytes(key))
                        .saturating_add(candidate_bytes)
                        <= MAX_METRIC_IDENTITY_BYTES
            })
            .min_by(|left, right| {
                left.1
                    .last_window
                    .cmp(&right.1.last_window)
                    .then_with(|| left.0.cmp(right.0))
            })
            .map(|(key, counter)| (key.clone(), counter.last_window))
        else {
            continue;
        };
        if admission.score < lowest_window.saturating_add(ADMISSION_MARGIN) {
            continue;
        }
        let evicted = state.counters.remove(&lowest_key);
        reattribute_promoted_candidate(state, admission, evicted);
        state.identity_bytes = state
            .identity_bytes
            .saturating_sub(metric_identity_bytes(&lowest_key))
            .saturating_add(candidate_bytes);
        state.counters.insert(
            candidate,
            TopicCounter {
                cumulative: admission.observed,
                last_window: admission.observed,
                idle_windows: 0,
                window: 0,
            },
        );
    }
}

fn reattribute_promoted_candidate(
    state: &mut IngressState,
    admission: AdmissionCandidate,
    evicted: Option<TopicCounter>,
) {
    state.overflow_window = state.overflow_window.saturating_sub(admission.observed);
    state.overflow_cumulative = state.overflow_cumulative.saturating_sub(admission.observed);
    if let Some(evicted) = evicted {
        state.overflow_window = state.overflow_window.saturating_add(evicted.last_window);
        state.overflow_cumulative = state.overflow_cumulative.saturating_add(evicted.cumulative);
    }
}

fn metric_identity_bytes((topic, participant): &(String, String)) -> usize {
    topic.len().saturating_add(participant.len())
}

/// Turn one window's drained samples into the wire `Metrics` state.
/// `ingress_rate_hz` is each topic's window count over the window length in
/// seconds; `throughput_msg_s` sums those rates; `count` is the cumulative
/// (all-time) total, unaffected by the window reset.
fn build_metrics(
    samples: &[TopicWindowSample],
    overflow_count: u64,
    overflow_cumulative: u64,
    topics_truncated: bool,
    window: Duration,
) -> preview_api::router::Metrics {
    let window_secs = window.as_secs_f32();
    let mut topics = Vec::with_capacity(samples.len());
    let mut throughput_msg_s = if window_secs > 0.0 {
        overflow_count as f32 / window_secs
    } else {
        0.0
    };
    for sample in samples {
        let ingress_rate_hz = if window_secs > 0.0 {
            sample.window_count as f32 / window_secs
        } else {
            0.0
        };
        throughput_msg_s += ingress_rate_hz;
        topics.push(preview_api::router::TopicMetric {
            topic: sample.topic.clone(),
            from_participant: sample.from_participant.clone(),
            ingress_rate_hz,
            count: sample.cumulative_count,
        });
    }
    if topics_truncated {
        topics.push(preview_api::router::TopicMetric {
            topic: String::new(),
            from_participant: String::new(),
            ingress_rate_hz: if window_secs > 0.0 {
                overflow_count as f32 / window_secs
            } else {
                0.0
            },
            count: overflow_cumulative,
        });
    }
    topics.sort_by(|left, right| {
        left.topic
            .cmp(&right.topic)
            .then_with(|| left.from_participant.cmp(&right.from_participant))
    });
    preview_api::router::Metrics {
        topics,
        throughput_msg_s,
        window_ns: u64::try_from(window.as_nanos()).unwrap_or(u64::MAX),
    }
}

fn zenoh_router_config(config: &RouterConfig) -> Result<zenoh::Config> {
    let listen = listen_endpoints(config);
    zenoh_router_config_with_listen(config, &listen)
}

fn zenoh_router_config_with_listen(
    config: &RouterConfig,
    listen: &[String],
) -> Result<zenoh::Config> {
    reject_unsupported_serial_listen(listen)?;

    let mut zenoh_config = zenoh::Config::default();
    insert(&mut zenoh_config, "mode", r#""router""#)?;
    insert_json(&mut zenoh_config, "listen/endpoints", &listen)?;
    insert(&mut zenoh_config, "scouting/multicast/enabled", "false")?;
    insert(&mut zenoh_config, "scouting/gossip/enabled", "false")?;
    insert(
        &mut zenoh_config,
        "scouting/multicast/autoconnect",
        r#"{ router: [], peer: [], client: [] }"#,
    )?;
    insert(
        &mut zenoh_config,
        "scouting/gossip/autoconnect",
        r#"{ router: [], peer: [], client: [] }"#,
    )?;
    if let Some(uplink) = &config.uplink {
        insert_json(
            &mut zenoh_config,
            "connect/endpoints",
            &vec![uplink.connect.clone()],
        )?;
        insert(
            &mut zenoh_config,
            "connect/retry",
            &format!(
                "{{ period_init_ms: {}, period_max_ms: {}, period_increase_factor: 2.0, exit_on_failure: false }}",
                uplink.retry.initial_ms, uplink.retry.max_ms
            ),
        )?;
        insert(&mut zenoh_config, "connect/exit_on_failure", "false")?;
        if let Some(auth) = &uplink.auth {
            insert(&mut zenoh_config, "transport/link/tls/enable_mtls", "true")?;
            insert_json(
                &mut zenoh_config,
                "transport/link/tls/root_ca_certificate",
                &auth.ca,
            )?;
            insert_json(
                &mut zenoh_config,
                "transport/link/tls/connect_certificate",
                &auth.cert,
            )?;
            insert_json(
                &mut zenoh_config,
                "transport/link/tls/connect_private_key",
                &auth.key,
            )?;
        }
    } else {
        insert(&mut zenoh_config, "connect/endpoints", "[]")?;
    }
    Ok(zenoh_config)
}

fn reject_unsupported_serial_listen(endpoints: &[String]) -> Result<()> {
    for endpoint in endpoints {
        if endpoint.trim().starts_with("serial/") {
            anyhow::bail!(
                "{SERIAL_LISTEN_TRANSPORT_DIAGNOSTIC}: serial listen endpoints require the serial transport feature, not enabled in this build (endpoint: {endpoint})"
            );
        }
    }
    Ok(())
}

fn insert(config: &mut zenoh::Config, path: &str, json5: &str) -> Result<()> {
    config
        .insert_json5(path, json5)
        .map_err(|error| anyhow::anyhow!("failed to set Zenoh config path {path}: {error}"))
}

fn insert_json<T: serde::Serialize>(
    config: &mut zenoh::Config,
    path: &str,
    value: &T,
) -> Result<()> {
    let json = serde_json::to_string(value)?;
    insert(config, path, &json)
}

fn listen_endpoints(config: &RouterConfig) -> Vec<String> {
    let mut endpoints = vec![DEFAULT_LISTEN.to_string()];
    for endpoint in &config.listen {
        if !endpoints.contains(endpoint) {
            endpoints.push(endpoint.clone());
        }
    }
    endpoints
}

fn uplink_state_publisher(
    router: &zenoh::Session,
    bus: Bus,
    cap: OwnerCap,
) -> Result<RouterPublisher<api::bus::uplink::State>> {
    let topic = api::topic::internal::new(cap).bus().uplink().state();
    let publisher = RouterPublisher::new(router, bus, &topic)?;
    Ok(publisher)
}

async fn observed_uplink_state(
    router: &zenoh::Session,
    uplink: &UplinkConfig,
    matcher: &mut UplinkMatcher,
) -> api::bus::uplink::State {
    let connected = matcher.is_connected(router).await;
    api::bus::uplink::State {
        phase: if connected {
            api::bus::uplink::UplinkPhase::Connected
        } else {
            api::bus::uplink::UplinkPhase::Connecting
        },
        connect: Some(uplink.connect.clone()),
        retry_attempt: 0,
        detail: (!connected).then(|| matcher.connection_detail()),
    }
}

fn disabled_uplink_state() -> api::bus::uplink::State {
    api::bus::uplink::State {
        phase: api::bus::uplink::UplinkPhase::Disabled,
        connect: None,
        retry_attempt: 0,
        detail: None,
    }
}

struct UplinkMatcher {
    protocol: String,
    configured_address: String,
    resolved_addresses: Vec<String>,
    dns_backed: bool,
    ever_connected: bool,
    last_resolution: Option<tokio::time::Instant>,
    last_resolution_error: Option<String>,
}

impl UplinkMatcher {
    async fn new(connect: &str) -> Result<Self> {
        let endpoint = zenoh::config::EndPoint::try_from(connect.to_string())
            .map_err(|error| anyhow::anyhow!("invalid configured uplink endpoint: {error}"))?;
        let protocol = endpoint.protocol().as_str().to_string();
        let configured_address = endpoint.address().as_str().to_string();
        let dns_backed = protocol_supports_dns(&protocol)
            && configured_address.parse::<std::net::SocketAddr>().is_err();
        let resolved_addresses = if dns_backed {
            Vec::new()
        } else {
            vec![configured_address.clone()]
        };
        let mut matcher = Self {
            protocol,
            configured_address,
            resolved_addresses,
            dns_backed,
            ever_connected: false,
            last_resolution: None,
            last_resolution_error: None,
        };
        matcher.refresh_dns_if_due(false).await;
        Ok(matcher)
    }

    async fn is_connected(&mut self, router: &zenoh::Session) -> bool {
        let (matches, same_protocol_link) = self.link_match(router).await;
        if matches {
            self.ever_connected = true;
            return true;
        }
        // After a previously matched uplink, a new same-protocol destination is
        // strong evidence of DNS rotation. Refresh on the short retry cadence
        // so a healthy reconnected link is not reported down for the full cache
        // interval. Requiring a prior match avoids treating unrelated inbound
        // links as a reason to hammer DNS.
        if self
            .refresh_dns_if_due(self.ever_connected && same_protocol_link)
            .await
        {
            let matches = self.link_match(router).await.0;
            self.ever_connected |= matches;
            return matches;
        }
        false
    }

    async fn link_match(&self, router: &zenoh::Session) -> (bool, bool) {
        // Match the configured remote destination positively. An accepted
        // inbound link has the connecting participant's ephemeral address as
        // `dst`, so it cannot satisfy this check.
        let mut same_protocol_link = false;
        let links = router.info().links().await;
        for link in links {
            if link.dst().protocol().as_str() != self.protocol {
                continue;
            }
            same_protocol_link = true;
            if self
                .resolved_addresses
                .iter()
                .any(|address| link.dst().address().as_str() == address)
            {
                return (true, true);
            }
        }
        (false, same_protocol_link)
    }

    async fn refresh_dns_if_due(&mut self, fast_retry: bool) -> bool {
        let refresh_after = self.dns_refresh_interval(fast_retry);
        if !self.dns_backed
            || self
                .last_resolution
                .is_some_and(|last| last.elapsed() < refresh_after)
        {
            return false;
        }
        self.last_resolution = Some(tokio::time::Instant::now());
        let lookup = tokio::net::lookup_host(self.configured_address.clone());
        match tokio::time::timeout(UPLINK_DNS_TIMEOUT, lookup).await {
            Ok(Ok(addresses)) => {
                let mut addresses = addresses
                    .map(|address| address.to_string())
                    .collect::<Vec<_>>();
                addresses.sort();
                addresses.dedup();
                if addresses.is_empty() {
                    self.last_resolution_error =
                        Some("DNS returned no addresses for the configured uplink".to_string());
                } else {
                    let changed = self.resolved_addresses != addresses;
                    self.resolved_addresses = addresses;
                    self.last_resolution_error = None;
                    return changed;
                }
            }
            Ok(Err(error)) => {
                self.last_resolution_error = Some(format!("uplink DNS resolution failed: {error}"));
            }
            Err(_) => {
                self.last_resolution_error = Some("uplink DNS resolution timed out".to_string());
            }
        }
        false
    }

    fn dns_refresh_interval(&self, fast_retry: bool) -> Duration {
        if self.last_resolution_error.is_some() {
            UPLINK_DNS_REFRESH
        } else if self.resolved_addresses.is_empty() || fast_retry {
            UPLINK_DNS_INITIAL_RETRY
        } else {
            UPLINK_DNS_REFRESH
        }
    }

    fn connection_detail(&self) -> String {
        self.last_resolution_error
            .clone()
            .unwrap_or_else(|| "waiting for the configured remote link".to_string())
    }
}

fn protocol_supports_dns(protocol: &str) -> bool {
    // Zenoh's reliable and datagram QUIC transports share the `quic` locator
    // prefix. Unix-domain and serial locators are intentionally excluded.
    matches!(protocol, "tcp" | "tls" | "quic" | "udp" | "ws")
}

async fn publish_uplink_state(
    publisher: &RouterPublisher<api::bus::uplink::State>,
    state: api::bus::uplink::State,
) -> Result<()> {
    publisher.publish_at(host_time(), state).await?;
    Ok(())
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<ToolRouter>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirror_key_expr_is_a_valid_zenoh_key_expr() {
        // Regression: a partial version-chunk wildcard was rejected by Zenoh
        // (partial-chunk wildcard), crashing the router on startup. The key
        // must parse as a valid `KeyExpr` so `declare_subscriber` succeeds.
        let key = mirror_key_expr("dev/robots/rover");
        assert_eq!(key, "dev/robots/rover/**");
        let mirror =
            zenoh::key_expr::KeyExpr::try_from(key).expect("robot mirror key must be valid");
        let own = zenoh::key_expr::KeyExpr::try_from("dev/robots/rover/v1/health").unwrap();
        let other = zenoh::key_expr::KeyExpr::try_from("dev/robots/other/v1/health").unwrap();
        assert!(mirror.intersects(&own));
        assert!(!mirror.intersects(&other));
    }

    #[test]
    fn mirror_callback_filters_bounds_and_discloses_queue_drops() {
        use zenoh::sample::SampleBuilder;

        fn sample_with_encoding(
            key: String,
            encoding: String,
            attachment: Option<Vec<u8>>,
        ) -> zenoh::sample::Sample {
            let key = zenoh::key_expr::KeyExpr::try_from(key).unwrap();
            let mut builder = SampleBuilder::put(key, Vec::<u8>::new()).encoding(encoding);
            if let Some(attachment) = attachment {
                builder = builder.attachment(attachment);
            }
            builder.into()
        }

        fn sample(key: String, attachment: Option<Vec<u8>>) -> zenoh::sample::Sample {
            sample_with_encoding(key, encoding_string(MessagePack::ID), attachment)
        }

        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let drops = AtomicU64::new(0);
        enqueue_metric_sample(
            sample("dev/robots/rover/v1/health".to_string(), None),
            &tx,
            &drops,
        );
        enqueue_metric_sample(
            sample("dev/robots/rover/v1/health".to_string(), None),
            &tx,
            &drops,
        );
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert_eq!(rx.try_recv().unwrap().topic, "dev/robots/rover/v1/health");

        enqueue_metric_sample(
            sample("dev/robots/rover/admin".to_string(), None),
            &tx,
            &drops,
        );
        assert!(rx.try_recv().is_err());
        assert_eq!(drops.load(Ordering::Relaxed), 1);

        let oversized_topic = format!("dev/robots/rover/v1/{}", "x".repeat(MAX_METRIC_TOPIC_BYTES));
        enqueue_metric_sample(sample(oversized_topic, None), &tx, &drops);
        assert_eq!(drops.load(Ordering::Relaxed), 2);

        enqueue_metric_sample(
            sample(
                "dev/robots/rover/v1/health".to_string(),
                Some(vec![0; MAX_METRIC_ATTACHMENT_BYTES + 1]),
            ),
            &tx,
            &drops,
        );
        assert!(rx.try_recv().is_err());
        assert_eq!(drops.load(Ordering::Relaxed), 3);

        enqueue_metric_sample(
            sample_with_encoding(
                "dev/robots/rover/v1/health".to_string(),
                "x".repeat(MAX_METRIC_ENCODING_BYTES + 1),
                None,
            ),
            &tx,
            &drops,
        );
        assert!(rx.try_recv().is_err());
        assert_eq!(drops.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn is_version_topic_scopes_to_phoxal_versions() {
        assert!(is_version_topic("v1/drive/state"));
        assert!(is_version_topic("ns/robots/rover-01/v2/router/metrics"));
        assert!(is_version_topic(
            "tenant/site/robots/rover-01/v2/router/metrics"
        ));
        assert!(is_version_topic("v2/telemetry/host"));
        assert!(is_version_topic("v1/health"));
        assert!(is_version_topic("ns/robots/rover-01/v1/health"));
        // Not version-qualified / admin / malformed:
        assert!(!is_version_topic("@/router/admin"));
        assert!(!is_version_topic("some/other/topic"));
        assert!(!is_version_topic("http/v1/users"));
        assert!(!is_version_topic("robots/rover-01/v2/router/metrics"));
        assert!(!is_version_topic("ns/rover-01/v2/router/metrics"));
        assert!(!is_version_topic("ns/robots/rover-01/v2"));
        assert!(!is_version_topic("v/x")); // no digits after prefix
        assert!(!is_version_topic("v1x/x")); // non-digit tail
    }

    #[test]
    fn default_listen_is_always_present() {
        assert_eq!(
            listen_endpoints(&RouterConfig::default()),
            vec![DEFAULT_LISTEN.to_string()]
        );
    }

    #[test]
    fn listen_endpoints_deduplicate_default() {
        let config = RouterConfig {
            listen: vec![DEFAULT_LISTEN.to_string(), "tcp/127.0.0.1:7448".to_string()],
            uplink: None,
        };
        assert_eq!(
            listen_endpoints(&config),
            vec![DEFAULT_LISTEN.to_string(), "tcp/127.0.0.1:7448".to_string()]
        );
    }

    #[test]
    fn serial_listen_fails_with_named_diagnostic() {
        let config = RouterConfig {
            listen: vec!["serial//dev/ttyACM0#baudrate=115200".to_string()],
            uplink: None,
        };

        let error = zenoh_router_config(&config).expect_err("serial listen should be rejected");
        let error = error.to_string();
        assert!(error.contains(SERIAL_LISTEN_TRANSPORT_DIAGNOSTIC));
        assert!(error.contains("serial listen endpoints require the serial transport feature"));
        assert!(error.contains("serial//dev/ttyACM0#baudrate=115200"));
    }

    #[test]
    fn dns_matching_covers_every_enabled_network_locator() {
        for protocol in ["tcp", "tls", "quic", "udp", "ws"] {
            assert!(protocol_supports_dns(protocol), "{protocol}");
        }
        for protocol in ["unixsock-stream", "serial"] {
            assert!(!protocol_supports_dns(protocol), "{protocol}");
        }
    }

    #[test]
    fn dns_failures_back_off_before_retrying_the_blocking_resolver() {
        let mut matcher = UplinkMatcher {
            protocol: "tcp".to_string(),
            configured_address: "missing.example:7447".to_string(),
            resolved_addresses: Vec::new(),
            dns_backed: true,
            ever_connected: false,
            last_resolution: None,
            last_resolution_error: None,
        };
        assert_eq!(
            matcher.dns_refresh_interval(false),
            UPLINK_DNS_INITIAL_RETRY
        );
        matcher.last_resolution_error = Some("lookup failed".to_string());
        assert_eq!(matcher.dns_refresh_interval(true), UPLINK_DNS_REFRESH);
        matcher.last_resolution_error = None;
        matcher.resolved_addresses = vec!["127.0.0.1:7447".to_string()];
        assert_eq!(matcher.dns_refresh_interval(true), UPLINK_DNS_INITIAL_RETRY);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn observed_uplink_state_reports_disabled_when_absent() {
        let router =
            zenoh::open(zenoh_router_config_with_listen(&RouterConfig::default(), &[]).unwrap())
                .await
                .unwrap();
        let state = disabled_uplink_state();
        router.close().await.unwrap();

        assert_eq!(state.phase, api::bus::uplink::UplinkPhase::Disabled);
        assert_eq!(state.connect, None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn uplink_state_distinguishes_outbound_from_inbound_router_links() {
        let mut opened = None;
        for _ in 0..5 {
            let port = {
                let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                socket.local_addr().unwrap().port()
            };
            let endpoint = format!("tcp/127.0.0.1:{port}");
            let config = zenoh_router_config_with_listen(
                &RouterConfig::default(),
                std::slice::from_ref(&endpoint),
            )
            .unwrap();
            if let Ok(upstream) = zenoh::open(config).await {
                opened = Some((upstream, endpoint));
                break;
            }
        }
        let (upstream, endpoint) = opened.expect("test router should bind an ephemeral endpoint");
        let connect = endpoint.replacen("127.0.0.1", "localhost", 1);
        let uplink = UplinkConfig {
            connect,
            auth: None,
            retry: RetryConfig::default(),
        };
        let downstream_config = RouterConfig {
            listen: Vec::new(),
            uplink: Some(uplink.clone()),
        };
        let downstream =
            zenoh::open(zenoh_router_config_with_listen(&downstream_config, &[]).unwrap())
                .await
                .unwrap();
        let mut downstream_matcher = UplinkMatcher::new(&uplink.connect).await.unwrap();

        let downstream_state = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let state =
                    observed_uplink_state(&downstream, &uplink, &mut downstream_matcher).await;
                if state.phase == api::bus::uplink::UplinkPhase::Connected {
                    break state;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("configured outbound uplink should become visible");
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if upstream.info().links().await.next().is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("upstream should register the accepted inbound transport");
        let mut upstream_matcher = UplinkMatcher::new(&uplink.connect).await.unwrap();
        let upstream_state = observed_uplink_state(&upstream, &uplink, &mut upstream_matcher).await;

        downstream.close().await.unwrap();
        upstream.close().await.unwrap();

        assert_eq!(
            downstream_state.phase,
            api::bus::uplink::UplinkPhase::Connected
        );
        assert_eq!(downstream_state.retry_attempt, 0);
        assert_eq!(downstream_state.detail, None);
        assert_eq!(
            upstream_state.phase,
            api::bus::uplink::UplinkPhase::Connecting,
            "an accepted downstream connection is not this router's uplink"
        );
    }

    #[test]
    fn zenoh_config_accepts_retry_and_tls_paths() {
        let config = RouterConfig {
            listen: Vec::new(),
            uplink: Some(UplinkConfig {
                connect: "tls/root.example.io:7447".to_string(),
                auth: Some(MtlsAuth {
                    ca: "identity/ca.pem".to_string(),
                    cert: "identity/robot.pem".to_string(),
                    key: "identity/robot.key".to_string(),
                }),
                retry: RetryConfig {
                    initial_ms: 2_000,
                    max_ms: 10_000,
                },
            }),
        };

        zenoh_router_config(&config).expect("router config should be accepted by zenoh");
    }

    #[test]
    fn build_metrics_computes_rate_from_window_count() {
        let samples = vec![TopicWindowSample {
            topic: "dev/robots/robot/v2/joypad/devices".to_string(),
            from_participant: "joypad".to_string(),
            window_count: 10,
            cumulative_count: 1_000,
        }];
        let metrics = build_metrics(&samples, 0, 0, false, Duration::from_secs(1));
        assert_eq!(metrics.topics.len(), 1);
        assert_eq!(metrics.topics[0].ingress_rate_hz, 10.0);
        assert_eq!(metrics.topics[0].count, 1_000);
        assert_eq!(metrics.topics[0].from_participant, "joypad");
        assert_eq!(metrics.throughput_msg_s, 10.0);
        assert_eq!(metrics.window_ns, 1_000_000_000);
    }

    #[test]
    fn build_metrics_halves_the_rate_for_a_two_second_window() {
        let samples = vec![TopicWindowSample {
            topic: "dev/robots/robot/v2/router/metrics".to_string(),
            from_participant: String::new(),
            window_count: 10,
            cumulative_count: 10,
        }];
        let metrics = build_metrics(&samples, 0, 0, false, Duration::from_secs(2));
        assert_eq!(metrics.topics[0].ingress_rate_hz, 5.0);
        assert_eq!(metrics.window_ns, 2_000_000_000);
    }

    #[test]
    fn build_metrics_sums_throughput_across_topics() {
        let samples = vec![
            TopicWindowSample {
                topic: "b".to_string(),
                from_participant: String::new(),
                window_count: 4,
                cumulative_count: 4,
            },
            TopicWindowSample {
                topic: "a".to_string(),
                from_participant: String::new(),
                window_count: 6,
                cumulative_count: 6,
            },
        ];
        let metrics = build_metrics(&samples, 0, 0, false, Duration::from_secs(1));
        assert_eq!(metrics.throughput_msg_s, 10.0);
        assert_eq!(metrics.topics[0].topic, "a");
        assert_eq!(metrics.topics[1].topic, "b");
    }

    #[test]
    fn build_metrics_sorts_shared_topic_rows_by_producer() {
        let samples = ["bob", "alice", ""]
            .into_iter()
            .map(|producer| TopicWindowSample {
                topic: "shared/topic".to_string(),
                from_participant: producer.to_string(),
                window_count: 1,
                cumulative_count: 1,
            })
            .collect::<Vec<_>>();
        let metrics = build_metrics(&samples, 0, 0, false, Duration::from_secs(1));
        assert_eq!(
            metrics
                .topics
                .iter()
                .map(|metric| metric.from_participant.as_str())
                .collect::<Vec<_>>(),
            ["", "alice", "bob"]
        );
    }

    #[test]
    fn record_sample_keeps_shared_topic_producers_disjoint() {
        let counters: IngressCounters = Mutex::new(IngressState::default());
        record_sample(&counters, "topic".to_string(), Some("alice".to_string()));
        record_sample(&counters, "topic".to_string(), Some("alice".to_string()));
        record_sample(&counters, "topic".to_string(), Some("bob".to_string()));
        record_sample(&counters, "topic".to_string(), None);
        let guard = counters.lock().unwrap();
        let alice = guard
            .counters
            .get(&("topic".to_string(), "alice".to_string()))
            .expect("alice counted");
        let bob = guard
            .counters
            .get(&("topic".to_string(), "bob".to_string()))
            .expect("bob counted");
        let unknown = guard
            .counters
            .get(&("topic".to_string(), String::new()))
            .expect("unknown producer counted separately");
        assert_eq!((alice.cumulative, alice.window), (2, 2));
        assert_eq!((bob.cumulative, bob.window), (1, 1));
        assert_eq!((unknown.cumulative, unknown.window), (1, 1));
    }

    #[test]
    fn metric_rows_are_bounded_without_undercounting_total_throughput() {
        let counters: IngressCounters = Mutex::new(IngressState::default());
        for index in 0..(MAX_METRIC_ROWS + 10) {
            record_sample(
                &counters,
                format!("topic/{index}"),
                Some("producer".to_string()),
            );
        }
        assert_eq!(counters.lock().unwrap().counters.len(), MAX_METRIC_ROWS - 1);

        let drained = drain_window(&counters);
        let metrics = build_metrics(
            &drained.samples,
            drained.overflow_count,
            drained.overflow_cumulative,
            drained.topics_truncated,
            Duration::from_secs(1),
        );
        assert_eq!(metrics.topics.len(), MAX_METRIC_ROWS);
        assert_eq!(metrics.throughput_msg_s, (MAX_METRIC_ROWS + 10) as f32);
        let overflow = metrics
            .topics
            .iter()
            .find(|metric| metric.topic.is_empty() && metric.from_participant.is_empty())
            .expect("bounded table should expose an aggregate sentinel row");
        assert_eq!(overflow.ingress_rate_hz, 11.0);
        assert_eq!(overflow.count, 11);
    }

    #[test]
    fn metrics_snapshot_has_a_total_wire_size_bound() {
        let counters: IngressCounters = Mutex::new(IngressState::default());
        for index in 0..MAX_METRIC_ROWS * 2 {
            let suffix = format!("{index:04}");
            record_sample(
                &counters,
                format!(
                    "{}{}",
                    "t".repeat(MAX_METRIC_TOPIC_BYTES - suffix.len()),
                    suffix
                ),
                Some(format!(
                    "{}{}",
                    "p".repeat(MAX_METRIC_PARTICIPANT_BYTES - suffix.len()),
                    suffix
                )),
            );
        }
        let guard = counters.lock().unwrap();
        assert!(guard.identity_bytes <= MAX_METRIC_IDENTITY_BYTES);
        assert!(guard.overflow_cumulative > 0);
        drop(guard);

        let drained = drain_window(&counters);
        let metrics = build_metrics(
            &drained.samples,
            drained.overflow_count,
            drained.overflow_cumulative,
            drained.topics_truncated,
            Duration::from_secs(1),
        );
        let encoded = MessagePack::encode(&metrics).unwrap();
        assert!(
            encoded.len() <= MAX_METRICS_WIRE_BYTES,
            "{}-byte router metrics snapshot exceeds the wire cap",
            encoded.len()
        );

        // Exercise the other size extreme: maximum row count with short
        // identities, where named-field overhead dominates the encoding.
        let counters: IngressCounters = Mutex::new(IngressState::default());
        for index in 0..MAX_METRIC_ROWS.saturating_sub(1) {
            record_sample(&counters, format!("v1/t/{index}"), None);
        }
        let drained = drain_window(&counters);
        let metrics = build_metrics(
            &drained.samples,
            drained.overflow_count,
            drained.overflow_cumulative,
            drained.topics_truncated,
            Duration::from_secs(1),
        );
        assert_eq!(metrics.topics.len(), MAX_METRIC_ROWS - 1);
        let encoded = MessagePack::encode(&metrics).unwrap();
        assert!(
            encoded.len() <= MAX_METRICS_WIRE_BYTES,
            "{}-byte max-row router metrics snapshot exceeds the wire cap",
            encoded.len()
        );
    }

    #[test]
    fn truncation_sentinel_stays_visible_after_the_overflow_window_goes_idle() {
        let counters: IngressCounters = Mutex::new(IngressState::default());
        for index in 0..MAX_METRIC_ROWS {
            record_sample(
                &counters,
                format!("topic/{index}"),
                Some("producer".to_string()),
            );
        }
        let first = drain_window(&counters);
        assert!(first.topics_truncated);
        assert_eq!(first.overflow_count, 1);

        let second = drain_window(&counters);
        assert!(second.topics_truncated);
        assert_eq!(second.overflow_count, 0);
        assert_eq!(second.overflow_cumulative, 1);
        let metrics = build_metrics(
            &second.samples,
            second.overflow_count,
            second.overflow_cumulative,
            second.topics_truncated,
            Duration::from_secs(1),
        );
        let overflow = metrics
            .topics
            .iter()
            .find(|metric| metric.topic.is_empty() && metric.from_participant.is_empty())
            .expect("historical table truncation must remain disclosed");
        assert_eq!(overflow.ingress_rate_hz, 0.0);
        assert_eq!(overflow.count, 1);
    }

    #[test]
    fn frequent_overflow_candidate_displaces_a_low_rate_admitted_row() {
        let counters: IngressCounters = Mutex::new(IngressState::default());
        for index in 0..MAX_METRIC_ROWS.saturating_sub(1) {
            record_sample(
                &counters,
                format!("seed/{index}"),
                Some(format!("seed-{index}")),
            );
        }
        drain_window(&counters);
        for _ in 0..PUBLISH_IDLE_WINDOWS.saturating_sub(1) {
            drain_window(&counters);
        }
        for _ in 0..10 {
            record_sample(
                &counters,
                "v1/motion/state".to_string(),
                Some("motion".to_string()),
            );
        }

        let drained = drain_window(&counters);
        assert!(
            counters
                .lock()
                .unwrap()
                .counters
                .contains_key(&("v1/motion/state".to_string(), "motion".to_string()))
        );
        let promoted = drained
            .samples
            .iter()
            .find(|sample| sample.topic == "v1/motion/state" && sample.from_participant == "motion")
            .expect("promoted candidate must carry its current window into the detail table");
        assert_eq!(promoted.window_count, 10);
        assert_eq!(promoted.cumulative_count, 10);
        assert_eq!(drained.overflow_count, 0);
        assert_eq!(drained.overflow_cumulative, 1);
        assert!(drained.topics_truncated);

        let next = drain_window(&counters);
        assert!(
            next.samples.iter().any(|sample| {
                sample.topic == "v1/motion/state"
                    && sample.from_participant == "motion"
                    && sample.window_count == 0
                    && sample.cumulative_count == 10
            }),
            "promoted row must remain visible during the normal idle grace"
        );
    }

    #[test]
    fn multiple_promotions_keep_the_strongest_candidates() {
        let mut state = IngressState {
            counters: (0..MAX_METRIC_ROWS.saturating_sub(1))
                .map(|index| {
                    (
                        (format!("seed/{index}"), format!("seed-{index}")),
                        TopicCounter {
                            cumulative: 10,
                            last_window: 10,
                            idle_windows: PUBLISH_IDLE_WINDOWS,
                            ..TopicCounter::default()
                        },
                    )
                })
                .collect(),
            admission_candidates: HashMap::from([
                (
                    ("hot/a".to_string(), "a".to_string()),
                    AdmissionCandidate {
                        score: 100,
                        observed: 100,
                    },
                ),
                (
                    ("hot/b".to_string(), "b".to_string()),
                    AdmissionCandidate {
                        score: 50,
                        observed: 50,
                    },
                ),
                (
                    ("hot/c".to_string(), "c".to_string()),
                    AdmissionCandidate {
                        score: 20,
                        observed: 20,
                    },
                ),
            ]),
            overflow_window: 170,
            overflow_cumulative: 170,
            ..IngressState::default()
        };

        promote_admission_candidates(&mut state);

        for candidate in [("hot/a", "a"), ("hot/b", "b"), ("hot/c", "c")] {
            assert!(
                state
                    .counters
                    .contains_key(&(candidate.0.to_string(), candidate.1.to_string()))
            );
        }
        assert_eq!(state.counters.len(), MAX_METRIC_ROWS - 1);
        // Only the three displaced seed rows remain in the sentinel; all
        // candidate observations moved into their promoted detail rows.
        assert_eq!(state.overflow_window, 30);
        assert_eq!(state.overflow_cumulative, 30);
    }

    #[test]
    fn bursty_candidate_does_not_displace_a_live_row() {
        let counters: IngressCounters = Mutex::new(IngressState::default());
        for index in 0..MAX_METRIC_ROWS.saturating_sub(1) {
            record_sample(
                &counters,
                format!("seed/{index}"),
                Some(format!("seed-{index}")),
            );
        }
        for _ in 0..10 {
            record_sample(
                &counters,
                "stray/topic".to_string(),
                Some("stray".to_string()),
            );
        }
        drain_window(&counters);
        let guard = counters.lock().unwrap();
        assert!(
            !guard
                .counters
                .contains_key(&("stray/topic".to_string(), "stray".to_string()))
        );
        assert_eq!(guard.counters.len(), MAX_METRIC_ROWS - 1);
    }

    #[test]
    fn admission_candidates_stay_small_under_one_shot_cardinality() {
        let mut candidates = HashMap::new();
        for index in 0..MAX_ADMISSION_CANDIDATES {
            record_admission_candidate(
                &mut candidates,
                (format!("topic/{index}"), format!("producer-{index}")),
            );
        }
        assert_eq!(candidates.len(), MAX_ADMISSION_CANDIDATES);
        record_admission_candidate(
            &mut candidates,
            ("one-more".to_string(), "producer".to_string()),
        );
        assert!(candidates.is_empty());
        for _ in 0..8 {
            record_admission_candidate(
                &mut candidates,
                ("hot/topic".to_string(), "hot".to_string()),
            );
        }
        let hot = candidates
            .get(&("hot/topic".to_string(), "hot".to_string()))
            .expect("repeated key must occupy one candidate slot");
        assert_eq!(hot.score, 8);
        assert_eq!(hot.observed, 8);
    }

    #[test]
    fn dropped_mirror_samples_are_disclosed_as_overflow() {
        let counters: IngressCounters = Mutex::new(IngressState::default());
        record_unobserved_samples(&counters, 7);
        let drained = drain_window(&counters);
        assert_eq!(drained.overflow_count, 7);
        assert_eq!(drained.overflow_cumulative, 7);
        assert!(drained.topics_truncated);
    }

    #[test]
    fn drain_window_resets_window_but_keeps_cumulative() {
        let counters: IngressCounters = Mutex::new(IngressState::default());
        record_sample(&counters, "topic".to_string(), None);
        record_sample(&counters, "topic".to_string(), None);
        let first = drain_window(&counters);
        assert_eq!(first.samples[0].window_count, 2);
        assert_eq!(first.samples[0].cumulative_count, 2);

        record_sample(&counters, "topic".to_string(), None);
        let second = drain_window(&counters);
        assert_eq!(second.samples[0].window_count, 1);
        assert_eq!(second.samples[0].cumulative_count, 3);
    }

    #[test]
    fn drain_window_preserves_shared_topic_producers_independently() {
        let counters: IngressCounters = Mutex::new(IngressState::default());
        record_sample(&counters, "topic".to_string(), Some("alice".to_string()));
        record_sample(&counters, "topic".to_string(), Some("alice".to_string()));
        record_sample(&counters, "topic".to_string(), Some("bob".to_string()));
        let mut first = drain_window(&counters).samples;
        first.sort_by(|left, right| left.from_participant.cmp(&right.from_participant));
        assert_eq!(first.len(), 2);
        assert_eq!(
            (first[0].from_participant.as_str(), first[0].window_count),
            ("alice", 2)
        );
        assert_eq!(
            (first[1].from_participant.as_str(), first[1].window_count),
            ("bob", 1)
        );

        let mut second = drain_window(&counters).samples;
        second.sort_by(|left, right| left.from_participant.cmp(&right.from_participant));
        assert_eq!(second.len(), 2);
        assert!(second.iter().all(|sample| sample.window_count == 0));
        assert_eq!(second[0].cumulative_count, 2);
        assert_eq!(second[1].cumulative_count, 1);
    }

    #[test]
    fn idle_topic_producer_rows_are_evicted_when_the_display_grace_expires() {
        let counters: IngressCounters = Mutex::new(IngressState {
            counters: HashMap::from([(
                ("topic".to_string(), "alice".to_string()),
                TopicCounter {
                    cumulative: 7,
                    window: 0,
                    last_window: 0,
                    idle_windows: PUBLISH_IDLE_WINDOWS,
                },
            )]),
            ..IngressState::default()
        });
        assert!(drain_window(&counters).samples.is_empty());
        assert!(counters.lock().unwrap().counters.is_empty());
    }

    #[test]
    fn idle_topic_producer_row_remains_visible_at_the_publish_boundary() {
        let counters: IngressCounters = Mutex::new(IngressState {
            counters: HashMap::from([(
                ("topic".to_string(), "alice".to_string()),
                TopicCounter {
                    cumulative: 7,
                    window: 0,
                    last_window: 0,
                    idle_windows: PUBLISH_IDLE_WINDOWS - 1,
                },
            )]),
            ..IngressState::default()
        });
        let samples = drain_window(&counters).samples;
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].window_count, 0);
        assert_eq!(samples[0].cumulative_count, 7);
    }

    #[test]
    fn returning_evicted_row_starts_a_new_observed_lifetime() {
        let counters: IngressCounters = Mutex::new(IngressState::default());
        for _ in 0..7 {
            record_sample(&counters, "topic".to_string(), Some("alice".to_string()));
        }
        drain_window(&counters);
        for _ in 0..EVICT_IDLE_WINDOWS {
            drain_window(&counters);
        }
        assert!(counters.lock().unwrap().counters.is_empty());

        record_sample(&counters, "topic".to_string(), Some("alice".to_string()));
        let returned = drain_window(&counters);
        let sample = returned
            .samples
            .iter()
            .find(|sample| sample.topic == "topic")
            .expect("returning row must be observed");
        assert_eq!(sample.cumulative_count, 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn router_publisher_is_visible_to_an_external_bus_client() {
        let router_config = RouterConfig::default();
        let mut opened = None;
        for _ in 0..5 {
            let port = {
                let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                socket.local_addr().unwrap().port()
            };
            let endpoint = format!("tcp/127.0.0.1:{port}");
            let zenoh_config =
                zenoh_router_config_with_listen(&router_config, std::slice::from_ref(&endpoint))
                    .unwrap();
            if let Ok(router) = zenoh::open(zenoh_config).await {
                opened = Some((router, endpoint));
                break;
            }
        }
        let (router, endpoint) = opened.expect("test router should bind an ephemeral endpoint");

        let identity_bus = phoxal::raw::Bus::open(phoxal::raw::BusConfig {
            namespace: "dev".to_string(),
            robot_id: "rover".to_string(),
            participant: "router".to_string(),
            incarnation: 0,
            connect_endpoints: Vec::new(),
        })
        .await
        .unwrap();
        let topic = preview_api::topic::internal::new(OwnerCap::__mint())
            .router()
            .metrics();
        let publisher = RouterPublisher::new(&router, identity_bus.clone(), &topic).unwrap();

        let client = phoxal::raw::Bus::open(phoxal::raw::BusConfig {
            namespace: "dev".to_string(),
            robot_id: "rover".to_string(),
            participant: "observer".to_string(),
            incarnation: 0,
            connect_endpoints: vec![endpoint],
        })
        .await
        .unwrap();
        let subscriber = phoxal::raw::Subscriber::<preview_api::router::Metrics>::new(
            &client,
            &preview_api::topic::new().router().metrics(),
            1,
        )
        .await
        .unwrap();

        let expected = preview_api::router::Metrics {
            topics: Vec::new(),
            throughput_msg_s: 12.5,
            window_ns: 1_000_000_000,
        };
        // Zenoh discovery is asynchronous: opening the client and declaring a
        // subscriber does not guarantee that the router has installed the
        // route before the very next statement. Metrics are periodic in
        // production, so exercise that same cadence instead of making this
        // transport regression test depend on a one-shot discovery race.
        let received_result = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                publisher.publish_at(host_time(), expected.clone()).await?;
                if let Ok(sample) =
                    tokio::time::timeout(Duration::from_millis(200), subscriber.recv()).await
                {
                    break sample.map_err(anyhow::Error::from);
                }
            }
        })
        .await;

        client.close().await.unwrap();
        identity_bus.close().await.unwrap();
        router.close().await.unwrap();

        let received = received_result
            .expect("external client should receive router telemetry")
            .expect("external subscriber should stay healthy");
        assert_eq!(received.body.throughput_msg_s, expected.throughput_msg_s);
        assert_eq!(received.metadata.source.participant, "router");
    }
}
