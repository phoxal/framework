use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use phoxal::api;
use phoxal::prelude::*;
#[cfg(test)]
use phoxal::raw::encoding_string;
use phoxal::raw::{
    Bus, BusMetadata, Codec, CodecId, MessagePack, OwnerCap, Publisher, QueryFailure, host_time,
    parse_encoding_string,
};

/// The mirror-subscription measurement window: per-topic counts are rolled up
/// and republished on this cadence.
const METRICS_WINDOW: Duration = Duration::from_secs(1);
const RETAINED_RATE_WINDOWS: usize = 60;
/// Bound both the bus tool's long-lived counter map and each published snapshot.
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
#[cfg(test)]
const MAX_SNAPSHOT_WIRE_BYTES: usize = 4 * 1_024 * 1_024;
/// A small fixed candidate set tracks frequent rejected keys without making
/// high-cardinality overflow work proportional to the 255-row detail table.
const MAX_ADMISSION_CANDIDATES: usize = 16;
/// A newcomer must beat the quietest admitted row by this many samples in one
/// window. This prevents one-shot keys from churning sparse but live topics.
const ADMISSION_MARGIN: u64 = 4;
/// Keep a few quiet samples visible so a topic does not flicker out between
/// sparse publications.
const PUBLISH_IDLE_WINDOWS: u32 = 5;
/// Retire a topic/producer counter as soon as its quiet display grace expires.
/// Its cumulative count intentionally starts over if that pair later returns;
/// bounding the tool's useful telemetry slots is more important than retaining
/// an all-time count for inactive participants.
const EVICT_IDLE_WINDOWS: u32 = PUBLISH_IDLE_WINDOWS + 1;
/// A mirrored key counts toward a rate window only if it has a Phoxal bus shape:
/// either a raw version-qualified contract key (`v0.1/drive/state`) or a rooted
/// key (`<namespace>/robots/<robot-id>/v0.1/drive/state`). Merely containing a
/// `v<n>` chunk is not enough: generic paths such as `http/v0.1/users` must not
/// be reported as Phoxal traffic.
fn is_version_topic(key: &str) -> bool {
    fn is_version_segment(segment: &str) -> bool {
        segment.strip_prefix('v').is_some_and(|rest| {
            let Some((major, revision)) = rest.split_once('.') else {
                return false;
            };
            !major.is_empty()
                && !revision.is_empty()
                && major.bytes().all(|b| b.is_ascii_digit())
                && revision.bytes().all(|b| b.is_ascii_digit())
        })
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

// Tools stay raw-bus only (decided 2026-07-09): no declared `Api` surface,
// just `ctx.raw_bus()` and the raw handle constructors.
#[phoxal::tool(id = "bus")]
pub struct ToolBus;

#[phoxal::behavior]
impl ToolBus {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        let bus = ctx.raw_bus();
        let cap = ctx.owner_capability();
        spawn_metrics(ctx, bus, cap).await?;
        tracing::info!(target: "tool_bus", "bus observation ready");
        Ok((Self, ()))
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

#[derive(Debug)]
struct BusHistory {
    generation: String,
    sequence: u64,
    windows: VecDeque<api::tool::bus::Window>,
}

impl BusHistory {
    fn new(generation: String) -> Self {
        Self {
            generation,
            sequence: 0,
            windows: VecDeque::with_capacity(RETAINED_RATE_WINDOWS),
        }
    }

    fn ingest(&mut self, mut window: api::tool::bus::Window) -> api::tool::bus::Follow {
        self.sequence = self
            .sequence
            .checked_add(1)
            .expect("tool-bus ingest sequence exhausted");
        window.sequence = self.sequence;
        self.windows.push_back(window.clone());
        if self.windows.len() > RETAINED_RATE_WINDOWS {
            self.windows.pop_front();
        }
        api::tool::bus::Follow {
            cursor: self.cursor(),
            window,
        }
    }

    fn cursor(&self) -> api::tool::Cursor {
        api::tool::Cursor {
            generation: self.generation.clone(),
            sequence: self.sequence,
        }
    }

    fn snapshot(&self, mut current: api::tool::bus::Window) -> api::tool::bus::Snapshot {
        current.sequence = self.sequence.saturating_add(1);
        api::tool::bus::Snapshot {
            cursor: self.cursor(),
            current: Some(current),
            windows: self.windows.iter().cloned().collect(),
        }
    }
}

/// Declare the wildcard mirror subscription on the tool's ordinary connected
/// bus session and spawn the two managed tasks that turn it into
/// retained `tool::bus` history: one ingests samples into the shared counters,
/// the other drains + publishes on [`METRICS_WINDOW`].
///
/// The mirror is scoped to this robot's bus root. That prevents its subscription
/// from propagating across an uplink and pulling other robots' traffic back to
/// this tool. It does not amplify itself: the publish cadence is timer-driven,
/// not triggered by inbound samples, so `Metrics` being itself mirrored and
/// counted (like any other local topic) never feeds back into more publishes.
async fn spawn_metrics(
    ctx: &mut SetupContext<ToolBus>,
    metrics_bus: Bus,
    cap: OwnerCap,
) -> Result<()> {
    let mirror_key_expr = mirror_key_expr(metrics_bus.root());
    let (mirror_tx, mut mirror_rx) = tokio::sync::mpsc::channel(METRICS_INGEST_QUEUE);
    let dropped_mirror_samples = Arc::new(AtomicU64::new(0));
    let callback_drops = Arc::clone(&dropped_mirror_samples);
    let mirror_subscriber = metrics_bus
        .session()
        .declare_subscriber(mirror_key_expr)
        .callback(move |sample| {
            enqueue_metric_sample(sample, &mirror_tx, &callback_drops);
        })
        .await
        .map_err(|error| anyhow::anyhow!("failed to declare bus mirror subscriber: {error}"))?;

    let counters: Arc<IngressCounters> = Arc::new(Mutex::new(IngressState::default()));
    let history = Arc::new(Mutex::new(BusHistory::new(process_generation()?)));
    // Serializes query snapshots with the once-per-second drain/history
    // transition. Sample ingestion remains independent and non-blocking.
    let snapshot_gate = Arc::new(Mutex::new(()));
    let window_started = Arc::new(Mutex::new(tokio::time::Instant::now()));

    let ingest_counters = Arc::clone(&counters);
    ctx.spawn_managed_with(
        "bus-metrics-ingest",
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

    let follow_publisher = Publisher::new(
        metrics_bus.clone(),
        &api::topic::internal::new(cap).tool().bus().follow(),
    )?;
    let snapshot_topic = api::topic::internal::new(cap).tool().bus().snapshot();
    let snapshots = metrics_bus.declare_server(snapshot_topic.key()).await?;
    let query_history = Arc::clone(&history);
    let query_counters = Arc::clone(&counters);
    let query_gate = Arc::clone(&snapshot_gate);
    let query_window_started = Arc::clone(&window_started);
    let query_bus = metrics_bus.clone();
    ctx.spawn_managed_with(
        "bus-snapshot-query",
        ManagedTaskPolicy::FaultOnExit,
        async move {
            loop {
                let incoming = match snapshots.recv().await {
                    Ok(incoming) => incoming,
                    Err(error) => {
                        tracing::warn!(target: "phoxal.bus", error = %error, "tool-bus snapshot server stopped");
                        return;
                    }
                };
                if let Err(error) = MessagePack::decode::<api::tool::bus::SnapshotRequest>(
                    &incoming.request_bytes().unwrap_or_default(),
                ) {
                    let _ = incoming
                        .reply_err(&QueryFailure::invalid_argument(format!(
                            "decode tool-bus snapshot request: {error}"
                        )))
                        .await;
                    continue;
                }
                let snapshot = {
                    let _gate = query_gate
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let started = *query_window_started
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let current = snapshot_current(
                        &query_counters,
                        tokio::time::Instant::now().duration_since(started),
                    );
                    query_history
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .snapshot(current)
                };
                match MessagePack::encode(&snapshot) {
                    Ok(payload) => {
                        if let Err(error) = incoming.reply(&query_bus, payload).await {
                            tracing::warn!(target: "phoxal.bus", error = %error, "tool-bus snapshot reply failed");
                        }
                    }
                    Err(error) => {
                        let _ = incoming
                            .reply_err(&QueryFailure::internal(format!(
                                "encode tool-bus snapshot: {error}"
                            )))
                            .await;
                    }
                }
            }
        },
    );
    let publish_counters = Arc::clone(&counters);
    let publish_drops = Arc::clone(&dropped_mirror_samples);
    let publish_history = Arc::clone(&history);
    let publish_gate = Arc::clone(&snapshot_gate);
    let publish_window_started = Arc::clone(&window_started);
    ctx.spawn_managed_with(
        "bus-metrics-publish",
        ManagedTaskPolicy::FaultOnExit,
        async move {
            let mut ticker = tokio::time::interval(METRICS_WINDOW);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // Tokio intervals yield their first tick immediately. Consume that
            // bootstrap tick so the first published snapshot covers a real
            // window instead of reporting a near-zero interval as one second.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let window_ended = tokio::time::Instant::now();
                record_unobserved_samples(
                    &publish_counters,
                    publish_drops.swap(0, Ordering::Relaxed),
                );
                let follow = {
                    let _gate = publish_gate
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let mut started = publish_window_started
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let drained = drain_window(&publish_counters);
                    let window = build_metrics(
                        &drained.samples,
                        drained.overflow_count,
                        drained.overflow_cumulative,
                        drained.topics_truncated,
                        window_ended.duration_since(*started),
                    );
                    *started = window_ended;
                    publish_history
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .ingest(window)
                };
                if let Err(error) = follow_publisher.publish_at(host_time(), follow).await {
                    tracing::warn!(target: "phoxal.bus", error = %error, "bus metrics follow publish failed");
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

/// Copy the in-progress counters without advancing idle ages, promoting
/// candidates, or resetting the measurement window.
fn snapshot_current(counters: &IngressCounters, elapsed: Duration) -> api::tool::bus::Window {
    let guard = counters
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let samples = guard
        .counters
        .iter()
        .filter(|(_, counter)| counter.idle_windows <= PUBLISH_IDLE_WINDOWS)
        .map(|((topic, from_participant), counter)| TopicWindowSample {
            topic: topic.clone(),
            from_participant: from_participant.clone(),
            window_count: counter.window,
            cumulative_count: counter.cumulative,
        })
        .collect::<Vec<_>>();
    build_metrics(
        &samples,
        guard.overflow_window,
        guard.overflow_cumulative,
        guard.overflow_window > 0 || guard.overflow_cumulative > 0,
        elapsed,
    )
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

/// Turn one window's drained samples into the retained wire state.
/// `ingress_rate_hz` is each topic's window count over the window length in
/// seconds; `throughput_msg_s` sums those rates; `count` is the cumulative
/// (all-time) total, unaffected by the window reset.
fn build_metrics(
    samples: &[TopicWindowSample],
    overflow_count: u64,
    overflow_cumulative: u64,
    topics_truncated: bool,
    window: Duration,
) -> api::tool::bus::Window {
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
        topics.push(api::tool::bus::TopicMetric {
            topic: sample.topic.clone(),
            from_participant: sample.from_participant.clone(),
            ingress_rate_hz,
            count: sample.cumulative_count,
        });
    }
    if topics_truncated {
        topics.push(api::tool::bus::TopicMetric {
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
    api::tool::bus::Window {
        sequence: 0,
        topics,
        throughput_msg_s,
        window_ns: u64::try_from(window.as_nanos()).unwrap_or(u64::MAX),
    }
}

fn process_generation() -> Result<String> {
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).map_err(|error| {
        anyhow::anyhow!("OS entropy unavailable for tool-bus generation: {error}")
    })?;
    Ok(random.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_generation_is_opaque_and_unique_per_call() {
        let first = process_generation().unwrap();
        let second = process_generation().unwrap();
        assert_eq!(first.len(), 32);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(first, second);
    }

    #[test]
    fn mirror_key_expr_is_a_valid_zenoh_key_expr() {
        // Regression: a partial version-chunk wildcard was rejected by Zenoh
        // (partial-chunk wildcard), crashing the router on startup. The key
        // must parse as a valid `KeyExpr` so `declare_subscriber` succeeds.
        let key = mirror_key_expr("dev/robots/rover");
        assert_eq!(key, "dev/robots/rover/**");
        let mirror =
            zenoh::key_expr::KeyExpr::try_from(key).expect("robot mirror key must be valid");
        let own = zenoh::key_expr::KeyExpr::try_from("dev/robots/rover/v0.1/health").unwrap();
        let other = zenoh::key_expr::KeyExpr::try_from("dev/robots/other/v0.1/health").unwrap();
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
            sample("dev/robots/rover/v0.1/health".to_string(), None),
            &tx,
            &drops,
        );
        enqueue_metric_sample(
            sample("dev/robots/rover/v0.1/health".to_string(), None),
            &tx,
            &drops,
        );
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert_eq!(rx.try_recv().unwrap().topic, "dev/robots/rover/v0.1/health");

        enqueue_metric_sample(
            sample("dev/robots/rover/admin".to_string(), None),
            &tx,
            &drops,
        );
        assert!(rx.try_recv().is_err());
        assert_eq!(drops.load(Ordering::Relaxed), 1);

        let oversized_topic = format!(
            "dev/robots/rover/v0.1/{}",
            "x".repeat(MAX_METRIC_TOPIC_BYTES)
        );
        enqueue_metric_sample(sample(oversized_topic, None), &tx, &drops);
        assert_eq!(drops.load(Ordering::Relaxed), 2);

        enqueue_metric_sample(
            sample(
                "dev/robots/rover/v0.1/health".to_string(),
                Some(vec![0; MAX_METRIC_ATTACHMENT_BYTES + 1]),
            ),
            &tx,
            &drops,
        );
        assert!(rx.try_recv().is_err());
        assert_eq!(drops.load(Ordering::Relaxed), 3);

        enqueue_metric_sample(
            sample_with_encoding(
                "dev/robots/rover/v0.1/health".to_string(),
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
        assert!(is_version_topic("v0.1/drive/state"));
        assert!(is_version_topic("ns/robots/rover-01/v0.1/router/metrics"));
        assert!(is_version_topic(
            "tenant/site/robots/rover-01/v0.1/router/metrics"
        ));
        assert!(is_version_topic("v0.1/telemetry/host"));
        assert!(is_version_topic("v0.1/health"));
        assert!(is_version_topic("ns/robots/rover-01/v0.1/health"));
        // Not version-qualified / admin / malformed:
        assert!(!is_version_topic("@/router/admin"));
        assert!(!is_version_topic("some/other/topic"));
        assert!(!is_version_topic("http/v0.1/users"));
        assert!(!is_version_topic("robots/rover-01/v0.1/router/metrics"));
        assert!(!is_version_topic("ns/rover-01/v0.1/router/metrics"));
        assert!(!is_version_topic("ns/robots/rover-01/v2"));
        assert!(!is_version_topic("v/x")); // no digits after prefix
        assert!(!is_version_topic("v1x/x")); // non-digit tail
    }

    #[test]
    fn build_metrics_computes_rate_from_window_count() {
        let samples = vec![TopicWindowSample {
            topic: "dev/robots/robot/v0.1/joypad/devices".to_string(),
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
            topic: "dev/robots/robot/v0.1/router/metrics".to_string(),
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
    fn history_retains_exactly_the_newest_sixty_windows() {
        let mut history = BusHistory::new("generation".to_string());
        for index in 0..(RETAINED_RATE_WINDOWS + 3) {
            history.ingest(api::tool::bus::Window {
                sequence: 0,
                topics: Vec::new(),
                throughput_msg_s: index as f32,
                window_ns: 1_000_000_000,
            });
        }

        let snapshot = history.snapshot(api::tool::bus::Window {
            sequence: 0,
            topics: Vec::new(),
            throughput_msg_s: 0.0,
            window_ns: 500_000_000,
        });
        assert_eq!(snapshot.cursor.sequence, 63);
        assert_eq!(snapshot.windows.len(), RETAINED_RATE_WINDOWS);
        assert_eq!(snapshot.windows.first().unwrap().sequence, 4);
        assert_eq!(snapshot.windows.last().unwrap().sequence, 63);
        assert_eq!(snapshot.current.as_ref().unwrap().sequence, 64);
        assert_eq!(snapshot.current.as_ref().unwrap().window_ns, 500_000_000);
    }

    #[test]
    fn current_snapshot_is_non_destructive_and_uses_partial_window() {
        let counters: IngressCounters = Mutex::new(IngressState::default());
        record_sample(
            &counters,
            "v0.1/drive/state".to_string(),
            Some("drive".to_string()),
        );
        let first = snapshot_current(&counters, Duration::from_millis(250));
        let second = snapshot_current(&counters, Duration::from_millis(500));

        assert_eq!(first.topics[0].count, 1);
        assert_eq!(first.topics[0].ingress_rate_hz, 4.0);
        assert_eq!(second.topics[0].count, 1);
        assert_eq!(second.topics[0].ingress_rate_hz, 2.0);
        assert_eq!(
            counters
                .lock()
                .unwrap()
                .counters
                .values()
                .next()
                .unwrap()
                .window,
            1
        );
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
            "{}-byte bus rate window exceeds the wire cap",
            encoded.len()
        );

        let snapshot = api::tool::bus::Snapshot {
            cursor: api::tool::Cursor {
                generation: "generation".to_string(),
                sequence: RETAINED_RATE_WINDOWS as u64,
            },
            current: Some(metrics.clone()),
            windows: vec![metrics; RETAINED_RATE_WINDOWS],
        };
        let encoded = MessagePack::encode(&snapshot).unwrap();
        assert!(
            encoded.len() <= MAX_SNAPSHOT_WIRE_BYTES,
            "{}-byte retained bus snapshot exceeds the wire cap",
            encoded.len()
        );

        // Exercise the other size extreme: maximum row count with short
        // identities, where named-field overhead dominates the encoding.
        let counters: IngressCounters = Mutex::new(IngressState::default());
        for index in 0..MAX_METRIC_ROWS.saturating_sub(1) {
            record_sample(&counters, format!("v0.1/t/{index}"), None);
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
            "{}-byte max-row bus rate window exceeds the wire cap",
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
                "v0.1/motion/state".to_string(),
                Some("motion".to_string()),
            );
        }

        let drained = drain_window(&counters);
        assert!(
            counters
                .lock()
                .unwrap()
                .counters
                .contains_key(&("v0.1/motion/state".to_string(), "motion".to_string()))
        );
        let promoted = drained
            .samples
            .iter()
            .find(|sample| {
                sample.topic == "v0.1/motion/state" && sample.from_participant == "motion"
            })
            .expect("promoted candidate must carry its current window into the detail table");
        assert_eq!(promoted.window_count, 10);
        assert_eq!(promoted.cumulative_count, 10);
        assert_eq!(drained.overflow_count, 0);
        assert_eq!(drained.overflow_cumulative, 1);
        assert!(drained.topics_truncated);

        let next = drain_window(&counters);
        assert!(
            next.samples.iter().any(|sample| {
                sample.topic == "v0.1/motion/state"
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
    async fn metrics_use_the_ordinary_bus_publisher() {
        let bus = phoxal::raw::Bus::open(phoxal::raw::BusConfig {
            namespace: "dev".to_string(),
            robot_id: "rover".to_string(),
            participant: "bus".to_string(),
            incarnation: 7,
            connect_endpoints: Vec::new(),
        })
        .await
        .expect("open bus");
        let topic = api::topic::internal::new(OwnerCap::__mint())
            .tool()
            .bus()
            .follow();
        let publisher = Publisher::new(bus.clone(), &topic).expect("metrics publisher");
        let subscriber = phoxal::raw::Subscriber::<api::tool::bus::Follow>::new(
            &bus,
            &api::topic::new().tool().bus().follow(),
            1,
        )
        .await
        .expect("metrics subscriber");

        let expected = api::tool::bus::Follow {
            cursor: api::tool::Cursor {
                generation: "test".to_string(),
                sequence: 1,
            },
            window: api::tool::bus::Window {
                sequence: 1,
                topics: Vec::new(),
                throughput_msg_s: 12.5,
                window_ns: 1_000_000_000,
            },
        };
        publisher
            .publish_at(host_time(), expected.clone())
            .await
            .expect("publish metrics");
        let received = tokio::time::timeout(Duration::from_secs(1), subscriber.recv())
            .await
            .expect("metrics timeout")
            .expect("receive metrics");

        assert_eq!(received.body, expected);
        assert_eq!(received.metadata.source.participant, "bus");
        assert_eq!(received.metadata.source.incarnation, 7);
        assert_eq!(bus.health().outbound_drops.load(Ordering::Relaxed), 0);
        bus.close().await.expect("close bus");
    }
}
