use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use phoxal::api as stable;
use phoxal::prelude::*;
use phoxal::raw::{Codec, MessagePack, Publisher, QueryFailure, Subscriber, host_time};

const DEVICE_RETENTION: Duration = Duration::from_secs(5 * 60);
const MAX_DEVICE_RECORDS: usize = 512;
const MAX_DEVICE_RETAINED_BYTES: usize = 2 * 1024 * 1024;
const MAX_DEVICE_ID_BYTES: usize = 64;
const MAX_DEVICE_DISK_ROWS: usize = 32;
const MAX_DEVICE_TEXT_BYTES: usize = 128;
const DEFAULT_DEVICE_QUERY_RECORDS: usize = 64;
const MAX_DEVICE_QUERY_RECORDS: usize = 64;
const RUNTIME_RETENTION: Duration = Duration::from_secs(5 * 60);
const MAX_RUNTIME_RECORDS: usize = 4_096;
/// Sum of retained records' exact MessagePack sizes. Container bookkeeping is
/// fixed/bounded separately by `MAX_RUNTIME_RECORDS`.
const MAX_RUNTIME_RETAINED_BYTES: usize = 12 * 1024 * 1024;
const MAX_RUNTIME_TOPIC_ROWS: usize = 256;
const MAX_RUNTIME_PARTICIPANT_BYTES: usize = 512;
const MAX_RUNTIME_TOPIC_BYTES: usize = 256;
const DEFAULT_RUNTIME_QUERY_RECORDS: usize = 64;
const MAX_RUNTIME_QUERY_RECORDS: usize = 64;
#[cfg(test)]
const MAX_DEVICE_SNAPSHOT_WIRE_BYTES: usize = 1024 * 1024;
#[cfg(test)]
const MAX_RUNTIME_SNAPSHOT_WIRE_BYTES: usize = 16 * 1024 * 1024;

// Configless (Part 3 fix, shared runner/macro default): the `#[phoxal::tool]`
// macro now defaults an omitted `config = …` to `()` for tools, so this
// starts cleanly with `PHOXAL_CONFIG` ABSENT rather than requiring `'{}'`.
// Tools stay raw-bus only (decided 2026-07-09): no declared `Api` surface,
// just `ctx.raw_bus()` and the raw handle constructors.
#[phoxal::tool(id = "telemetry")]
struct ToolTelemetry;

#[phoxal::behavior]
impl ToolTelemetry {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        let bus = ctx.raw_bus();
        let cap = ctx.owner_capability();

        let device_history = Arc::new(Mutex::new(DeviceHistory::new(process_generation()?)));
        let device_samples =
            Subscriber::new(&bus, &stable::topic::new().tool().device().sample(), 32).await?;
        let device_follow = Publisher::new(
            bus.clone(),
            &stable::topic::internal::new(cap).tool().device().follow(),
        )?;
        let device_snapshot_topic = stable::topic::internal::new(cap).tool().device().snapshot();
        let device_snapshots = bus.declare_server(device_snapshot_topic.key()).await?;

        let ingest_devices = Arc::clone(&device_history);
        ctx.spawn_managed_with(
            "device-telemetry-ingest",
            ManagedTaskPolicy::FaultOnExit,
            async move {
                while let Ok(received) = device_samples.recv().await {
                    let follow = match ingest_devices
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .ingest(Instant::now(), received.body)
                    {
                        Ok(follow) => follow,
                        Err(error) => {
                            tracing::warn!(target: "tool_telemetry", error = %error, "device sample could not be retained");
                            continue;
                        }
                    };
                    if let Err(error) = device_follow.publish_at(host_time(), follow).await {
                        tracing::warn!(target: "tool_telemetry", error = %error, "device follow publish failed");
                    }
                }
            },
        );

        let query_devices = Arc::clone(&device_history);
        let device_query_bus = bus.clone();
        ctx.spawn_managed_with(
            "device-telemetry-query",
            ManagedTaskPolicy::FaultOnExit,
            async move {
                loop {
                    let incoming = match device_snapshots.recv().await {
                        Ok(incoming) => incoming,
                        Err(error) => {
                            tracing::warn!(target: "tool_telemetry", error = %error, "device snapshot server stopped");
                            return;
                        }
                    };
                    let request = match MessagePack::decode::<
                        stable::tool::device::SnapshotRequest,
                    >(&incoming.request_bytes().unwrap_or_default())
                    {
                        Ok(request) => request,
                        Err(error) => {
                            let _ = incoming
                                .reply_err(&QueryFailure::invalid_argument(format!(
                                    "decode device snapshot request: {error}"
                                )))
                                .await;
                            continue;
                        }
                    };
                    let snapshot = query_devices
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .snapshot(Instant::now(), &request);
                    match MessagePack::encode(&snapshot) {
                        Ok(payload) => {
                            if let Err(error) = incoming.reply(&device_query_bus, payload).await {
                                tracing::warn!(target: "tool_telemetry", error = %error, "device snapshot reply failed");
                            }
                        }
                        Err(error) => {
                            let _ = incoming
                                .reply_err(&QueryFailure::internal(format!(
                                    "encode device snapshot: {error}"
                                )))
                                .await;
                        }
                    }
                }
            },
        );

        let runtime_history = Arc::new(Mutex::new(RuntimeHistory::new(process_generation()?)));
        let runtime_rollups =
            Subscriber::new(&bus, &stable::topic::new().tool().runtime().rollup(), 128).await?;
        let runtime_follow = Publisher::new(
            bus.clone(),
            &stable::topic::internal::new(cap).tool().runtime().follow(),
        )?;
        let runtime_snapshot_topic = stable::topic::internal::new(cap)
            .tool()
            .runtime()
            .snapshot();
        let runtime_snapshots = bus.declare_server(runtime_snapshot_topic.key()).await?;

        let ingest_history = Arc::clone(&runtime_history);
        ctx.spawn_managed_with(
            "runtime-performance-ingest",
            ManagedTaskPolicy::FaultOnExit,
            async move {
                while let Ok(received) = runtime_rollups.recv().await {
                    let follow = match ingest_history
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .ingest(
                            Instant::now(),
                            received.metadata.source.participant,
                            received.body,
                        ) {
                        Ok(follow) => follow,
                        Err(error) => {
                            tracing::warn!(target: "tool_telemetry", error = %error, "runtime-performance rollup could not be retained");
                            continue;
                        }
                    };
                    if let Err(error) = runtime_follow.publish_at(host_time(), follow).await {
                        tracing::warn!(target: "tool_telemetry", error = %error, "runtime-performance follow publish failed");
                    }
                }
            },
        );

        let query_history = Arc::clone(&runtime_history);
        let query_bus = bus.clone();
        ctx.spawn_managed_with(
            "runtime-performance-query",
            ManagedTaskPolicy::FaultOnExit,
            async move {
                loop {
                    let incoming = match runtime_snapshots.recv().await {
                        Ok(incoming) => incoming,
                        Err(error) => {
                            tracing::warn!(target: "tool_telemetry", error = %error, "runtime-performance snapshot server stopped");
                            return;
                        }
                    };
                    let request = match MessagePack::decode::<
                        stable::tool::runtime::SnapshotRequest,
                    >(&incoming.request_bytes().unwrap_or_default())
                    {
                        Ok(request) => request,
                        Err(error) => {
                            let _ = incoming
                                .reply_err(&QueryFailure::invalid_argument(format!(
                                    "decode runtime-performance snapshot request: {error}"
                                )))
                                .await;
                            continue;
                        }
                    };
                    let snapshot = query_history
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .snapshot(Instant::now(), &request);
                    match MessagePack::encode(&snapshot) {
                        Ok(payload) => {
                            if let Err(error) = incoming.reply(&query_bus, payload).await {
                                tracing::warn!(target: "tool_telemetry", error = %error, "runtime-performance snapshot reply failed");
                            }
                        }
                        Err(error) => {
                            let _ = incoming
                                .reply_err(&QueryFailure::internal(format!(
                                    "encode runtime-performance snapshot: {error}"
                                )))
                                .await;
                        }
                    }
                }
            },
        );

        tracing::info!(target: "tool_telemetry", "telemetry ready");

        Ok((Self, ()))
    }
}

#[derive(Debug)]
struct TimedDeviceRecord {
    received_at: Instant,
    encoded_bytes: usize,
    record: stable::tool::device::Record,
}

#[derive(Debug)]
struct DeviceHistory {
    generation: String,
    sequence: u64,
    capacity_evictions: u64,
    retained_bytes: usize,
    records: VecDeque<TimedDeviceRecord>,
}

impl DeviceHistory {
    fn new(generation: String) -> Self {
        Self {
            generation,
            sequence: 0,
            capacity_evictions: 0,
            retained_bytes: 0,
            records: VecDeque::new(),
        }
    }

    fn ingest(
        &mut self,
        now: Instant,
        sample: stable::tool::device::Sample,
    ) -> Result<stable::tool::device::Follow> {
        self.prune(now);
        let sequence = self
            .sequence
            .checked_add(1)
            .expect("tool-telemetry device sequence exhausted");
        let (sample, truncated) = bounded_device_sample(sample);
        let record = stable::tool::device::Record {
            sequence,
            sample,
            truncated,
        };
        let encoded_bytes = MessagePack::encode(&record)?.len();
        self.sequence = sequence;
        self.retained_bytes = self.retained_bytes.saturating_add(encoded_bytes);
        self.records.push_back(TimedDeviceRecord {
            received_at: now,
            encoded_bytes,
            record: record.clone(),
        });
        while self.records.len() > MAX_DEVICE_RECORDS
            || self.retained_bytes > MAX_DEVICE_RETAINED_BYTES
        {
            self.pop_front(true);
        }
        Ok(stable::tool::device::Follow {
            cursor: self.cursor(),
            record,
        })
    }

    fn snapshot(
        &mut self,
        now: Instant,
        request: &stable::tool::device::SnapshotRequest,
    ) -> stable::tool::device::Snapshot {
        self.prune(now);
        let requested = if request.limit == 0 {
            DEFAULT_DEVICE_QUERY_RECORDS
        } else {
            usize::try_from(request.limit).unwrap_or(usize::MAX)
        };
        let limit = requested.min(MAX_DEVICE_QUERY_RECORDS);
        let matches = |record: &&TimedDeviceRecord| {
            request
                .before_sequence
                .is_none_or(|before| record.record.sequence < before)
                && request
                    .device_id
                    .as_ref()
                    .is_none_or(|device| record.record.sample.device_id == *device)
        };
        let mut records = self
            .records
            .iter()
            .rev()
            .filter(matches)
            .take(limit)
            .map(|timed| timed.record.clone())
            .collect::<Vec<_>>();
        records.reverse();
        let next_before_sequence = records.first().and_then(|first| {
            self.records
                .iter()
                .any(|timed| {
                    timed.record.sequence < first.sequence
                        && request
                            .device_id
                            .as_ref()
                            .is_none_or(|device| timed.record.sample.device_id == *device)
                })
                .then_some(first.sequence)
        });
        stable::tool::device::Snapshot {
            cursor: self.cursor(),
            records,
            capacity_evictions: self.capacity_evictions,
            next_before_sequence,
        }
    }

    fn prune(&mut self, now: Instant) {
        while self.records.front().is_some_and(|record| {
            now.saturating_duration_since(record.received_at) > DEVICE_RETENTION
        }) {
            self.pop_front(false);
        }
    }

    fn pop_front(&mut self, capacity_eviction: bool) {
        let Some(removed) = self.records.pop_front() else {
            return;
        };
        self.retained_bytes = self.retained_bytes.saturating_sub(removed.encoded_bytes);
        if capacity_eviction {
            self.capacity_evictions = self.capacity_evictions.saturating_add(1);
        }
    }

    fn cursor(&self) -> stable::tool::Cursor {
        stable::tool::Cursor {
            generation: self.generation.clone(),
            sequence: self.sequence,
        }
    }
}

fn bounded_device_sample(
    mut sample: stable::tool::device::Sample,
) -> (stable::tool::device::Sample, u32) {
    let mut truncated = 0_u32;
    if sample.device_id.len() > MAX_DEVICE_ID_BYTES {
        sample.device_id = truncate_utf8(sample.device_id, MAX_DEVICE_ID_BYTES);
        truncated = truncated.saturating_add(1);
    }
    if sample.device_id.is_empty() {
        sample.device_id = "unknown".to_string();
        truncated = truncated.saturating_add(1);
    }
    sample.cpu_pct = sample
        .cpu_pct
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(0.0, 100.0));
    sample.load_1m = finite_optional(sample.load_1m);
    sample.load_5m = finite_optional(sample.load_5m);
    sample.load_15m = finite_optional(sample.load_15m);
    normalize_capacity(&mut sample.ram_used_bytes, &mut sample.ram_total_bytes);
    normalize_capacity(&mut sample.swap_used_bytes, &mut sample.swap_total_bytes);
    if let Some(disks) = sample.disks.as_mut() {
        let omitted = disks.len().saturating_sub(MAX_DEVICE_DISK_ROWS);
        disks.truncate(MAX_DEVICE_DISK_ROWS);
        truncated = truncated.saturating_add(u32::try_from(omitted).unwrap_or(u32::MAX));
        for disk in disks {
            if disk.mount_point.len() > MAX_DEVICE_TEXT_BYTES {
                disk.mount_point =
                    truncate_utf8(std::mem::take(&mut disk.mount_point), MAX_DEVICE_TEXT_BYTES);
                truncated = truncated.saturating_add(1);
            }
            if disk.file_system.len() > MAX_DEVICE_TEXT_BYTES {
                disk.file_system =
                    truncate_utf8(std::mem::take(&mut disk.file_system), MAX_DEVICE_TEXT_BYTES);
                truncated = truncated.saturating_add(1);
            }
            if disk.total_bytes == 0 {
                disk.used_bytes = 0;
            } else {
                disk.used_bytes = disk.used_bytes.min(disk.total_bytes);
            }
        }
    }
    (sample, truncated)
}

fn finite_optional(value: Option<f32>) -> Option<f32> {
    value
        .filter(|value| value.is_finite())
        .map(|value| value.max(0.0))
}

fn normalize_capacity(used: &mut Option<u64>, total: &mut Option<u64>) {
    match *total {
        Some(0) | None => {
            *used = None;
            *total = None;
        }
        Some(limit) => {
            *used = used.map(|value| value.min(limit));
        }
    }
}

#[derive(Debug)]
struct TimedRuntimeRecord {
    received_at: Instant,
    encoded_bytes: usize,
    record: stable::tool::runtime::Record,
}

#[derive(Debug)]
struct RuntimeHistory {
    generation: String,
    sequence: u64,
    capacity_evictions: u64,
    /// Exact MessagePack byte sum for `records`, maintained on every push/pop.
    retained_bytes: usize,
    records: VecDeque<TimedRuntimeRecord>,
}

impl RuntimeHistory {
    fn new(generation: String) -> Self {
        Self {
            generation,
            sequence: 0,
            capacity_evictions: 0,
            retained_bytes: 0,
            records: VecDeque::new(),
        }
    }

    fn ingest(
        &mut self,
        now: Instant,
        participant_id: String,
        rollup: stable::tool::runtime::Rollup,
    ) -> Result<stable::tool::runtime::Follow> {
        self.prune(now);
        let sequence = self
            .sequence
            .checked_add(1)
            .expect("tool-telemetry runtime sequence exhausted");
        let participant_was_truncated = participant_id.len() > MAX_RUNTIME_PARTICIPANT_BYTES;
        let participant_id = truncate_utf8(participant_id, MAX_RUNTIME_PARTICIPANT_BYTES);
        let (topics, overflow) = bounded_runtime_topics(rollup.topics, rollup.overflow);
        let record = stable::tool::runtime::Record {
            sequence,
            participant_id,
            truncated: u32::from(participant_was_truncated),
            window_ns: rollup.window_ns,
            step: rollup.step,
            topics,
            overflow,
        };
        let encoded_bytes = MessagePack::encode(&record)?.len();
        self.sequence = sequence;
        self.retained_bytes = self.retained_bytes.saturating_add(encoded_bytes);
        self.records.push_back(TimedRuntimeRecord {
            received_at: now,
            encoded_bytes,
            record: record.clone(),
        });
        while self.records.len() > MAX_RUNTIME_RECORDS
            || self.retained_bytes > MAX_RUNTIME_RETAINED_BYTES
        {
            self.pop_front(true);
        }
        Ok(stable::tool::runtime::Follow {
            cursor: self.cursor(),
            record,
        })
    }

    fn snapshot(
        &mut self,
        now: Instant,
        request: &stable::tool::runtime::SnapshotRequest,
    ) -> stable::tool::runtime::Snapshot {
        self.prune(now);
        let requested = if request.limit == 0 {
            DEFAULT_RUNTIME_QUERY_RECORDS
        } else {
            usize::try_from(request.limit).unwrap_or(usize::MAX)
        };
        let limit = requested.min(MAX_RUNTIME_QUERY_RECORDS);
        let mut records = self
            .records
            .iter()
            .rev()
            .filter(|timed| {
                request
                    .before_sequence
                    .is_none_or(|before| timed.record.sequence < before)
                    && request
                        .participant_id
                        .as_ref()
                        .is_none_or(|participant| timed.record.participant_id == *participant)
            })
            .take(limit)
            .map(|timed| timed.record.clone())
            .collect::<Vec<_>>();
        records.reverse();
        let next_before_sequence = records.first().and_then(|first| {
            self.records
                .iter()
                .any(|timed| {
                    timed.record.sequence < first.sequence
                        && request
                            .participant_id
                            .as_ref()
                            .is_none_or(|participant| timed.record.participant_id == *participant)
                })
                .then_some(first.sequence)
        });
        stable::tool::runtime::Snapshot {
            cursor: self.cursor(),
            records,
            capacity_evictions: self.capacity_evictions,
            next_before_sequence,
        }
    }

    fn prune(&mut self, now: Instant) {
        while self.records.front().is_some_and(|record| {
            now.saturating_duration_since(record.received_at) > RUNTIME_RETENTION
        }) {
            self.pop_front(false);
        }
    }

    fn pop_front(&mut self, capacity_eviction: bool) {
        let Some(removed) = self.records.pop_front() else {
            return;
        };
        self.retained_bytes = self.retained_bytes.saturating_sub(removed.encoded_bytes);
        if capacity_eviction {
            self.capacity_evictions = self.capacity_evictions.saturating_add(1);
        }
    }

    fn cursor(&self) -> stable::tool::Cursor {
        stable::tool::Cursor {
            generation: self.generation.clone(),
            sequence: self.sequence,
        }
    }
}

fn bounded_runtime_topics(
    topics: Vec<stable::tool::RuntimeTopic>,
    overflow: Option<stable::tool::RuntimeTopic>,
) -> (
    Vec<stable::tool::RuntimeTopic>,
    Option<stable::tool::RuntimeTopic>,
) {
    let mut overflow_rows = overflow
        .map(normalize_runtime_overflow)
        .map(|row| {
            let omitted_rows = row.overflowed_rows;
            (row, omitted_rows)
        })
        .into_iter()
        .collect::<Vec<_>>();
    let mut normal = BTreeMap::<
        (
            String,
            stable::tool::RuntimeDirection,
            stable::tool::RuntimeBufferKind,
        ),
        Vec<stable::tool::RuntimeTopic>,
    >::new();
    for mut row in topics {
        row.rate_hz = finite_rate(row.rate_hz);
        let is_normal = !row.topic.is_empty()
            && row.topic.len() <= MAX_RUNTIME_TOPIC_BYTES
            && row.direction != stable::tool::RuntimeDirection::Mixed
            && row.buffer_kind != stable::tool::RuntimeBufferKind::Mixed
            && row.overflowed_rows == 0;
        if is_normal {
            let key = (row.topic.clone(), row.direction, row.buffer_kind);
            normal.entry(key).or_default().push(row);
        } else {
            let omitted_rows = row.overflowed_rows.saturating_add(1);
            overflow_rows.push((row, omitted_rows));
        }
    }

    let mut retained = Vec::with_capacity(normal.len().min(MAX_RUNTIME_TOPIC_ROWS));
    for rows in normal.into_values() {
        let row = aggregate_runtime_values(rows);
        if retained.len() < MAX_RUNTIME_TOPIC_ROWS {
            retained.push(row);
        } else {
            overflow_rows.push((row, 1));
        }
    }
    (retained, aggregate_runtime_overflow(overflow_rows))
}

fn normalize_runtime_overflow(mut row: stable::tool::RuntimeTopic) -> stable::tool::RuntimeTopic {
    row.topic.clear();
    row.direction = stable::tool::RuntimeDirection::Mixed;
    row.buffer_kind = stable::tool::RuntimeBufferKind::Mixed;
    row.rate_hz = finite_rate(row.rate_hz);
    row
}

fn aggregate_runtime_values(
    mut rows: Vec<stable::tool::RuntimeTopic>,
) -> stable::tool::RuntimeTopic {
    // Floating-point addition is not associative, so every identity uses one
    // canonical rate order regardless of producer row order.
    rows.sort_by(|left, right| left.rate_hz.total_cmp(&right.rate_hz));
    let mut rows = rows.into_iter();
    let mut target = rows.next().expect("runtime value group is never empty");
    for row in rows {
        merge_runtime_values(&mut target, &row);
    }
    target
}

fn aggregate_runtime_overflow(
    mut rows: Vec<(stable::tool::RuntimeTopic, u32)>,
) -> Option<stable::tool::RuntimeTopic> {
    // Invalid, excess, and producer-overflow rows share the same canonical
    // rate fold as retained duplicate identities.
    rows.sort_by(|(left, _), (right, _)| left.rate_hz.total_cmp(&right.rate_hz));
    if rows.is_empty() {
        return None;
    }

    let mut target = empty_runtime_overflow();
    for (row, omitted_rows) in rows {
        merge_runtime_values(&mut target, &row);
        target.overflowed_rows = target.overflowed_rows.saturating_add(omitted_rows);
    }
    Some(target)
}

fn merge_runtime_values(target: &mut stable::tool::RuntimeTopic, row: &stable::tool::RuntimeTopic) {
    target.count = target.count.saturating_add(row.count);
    target.rate_hz = add_finite_rates(target.rate_hz, row.rate_hz);
    target.drops = target.drops.saturating_add(row.drops);
    target.latest_overwrites = target
        .latest_overwrites
        .saturating_add(row.latest_overwrites);
    target.bounded_evictions = target
        .bounded_evictions
        .saturating_add(row.bounded_evictions);
    target.capacity = target.capacity.saturating_add(row.capacity);
    target.current_depth = target.current_depth.saturating_add(row.current_depth);
    target.high_water_depth = target.high_water_depth.saturating_add(row.high_water_depth);
    target.decode_errors = target.decode_errors.saturating_add(row.decode_errors);
}

fn finite_rate(rate_hz: f32) -> f32 {
    if rate_hz.is_nan() || rate_hz <= 0.0 {
        0.0
    } else if rate_hz.is_infinite() {
        f32::MAX
    } else {
        rate_hz
    }
}

fn add_finite_rates(left: f32, right: f32) -> f32 {
    let left = finite_rate(left);
    let right = finite_rate(right);
    if left > f32::MAX - right {
        f32::MAX
    } else {
        left + right
    }
}

fn empty_runtime_overflow() -> stable::tool::RuntimeTopic {
    stable::tool::RuntimeTopic {
        topic: String::new(),
        direction: stable::tool::RuntimeDirection::Mixed,
        buffer_kind: stable::tool::RuntimeBufferKind::Mixed,
        count: 0,
        rate_hz: 0.0,
        drops: 0,
        latest_overwrites: 0,
        bounded_evictions: 0,
        capacity: 0,
        current_depth: 0,
        high_water_depth: 0,
        decode_errors: 0,
        overflowed_rows: 0,
    }
}

fn process_generation() -> Result<String> {
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).map_err(|error| {
        anyhow::anyhow!("OS entropy unavailable for tool-telemetry generation: {error}")
    })?;
    Ok(random.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn truncate_utf8(mut value: String, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value;
    }
    let mut boundary = max_bytes;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
    value
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<ToolTelemetry>()
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

    fn runtime_rollup(window_ns: u64) -> stable::tool::runtime::Rollup {
        stable::tool::runtime::Rollup {
            window_ns,
            step: None,
            topics: Vec::new(),
            overflow: None,
        }
    }

    fn runtime_topic(topic: String) -> stable::tool::RuntimeTopic {
        stable::tool::RuntimeTopic {
            topic,
            direction: stable::tool::RuntimeDirection::Publish,
            buffer_kind: stable::tool::RuntimeBufferKind::Outbound,
            count: 1,
            rate_hz: 1.0,
            drops: 1,
            latest_overwrites: 0,
            bounded_evictions: 0,
            capacity: 1_024,
            current_depth: 1,
            high_water_depth: 1,
            decode_errors: 1,
            overflowed_rows: 0,
        }
    }

    fn maximal_runtime_rollup() -> stable::tool::runtime::Rollup {
        let topics = (0..MAX_RUNTIME_TOPIC_ROWS)
            .map(|index| {
                let prefix = format!("v0.1/{index:03}/");
                runtime_topic(format!(
                    "{prefix}{}",
                    "x".repeat(MAX_RUNTIME_TOPIC_BYTES - prefix.len())
                ))
            })
            .collect();
        stable::tool::runtime::Rollup {
            window_ns: u64::MAX,
            step: Some(stable::tool::RuntimeStep {
                target_period_ns: u64::MAX,
                completed: u64::MAX,
                errors: u64::MAX,
                mean_duration_ns: u64::MAX,
                max_duration_ns: u64::MAX,
                mean_lateness_ns: u64::MAX,
                max_lateness_ns: u64::MAX,
                missed_ticks: u64::MAX,
                overruns: u64::MAX,
            }),
            topics,
            overflow: Some(stable::tool::RuntimeTopic {
                topic: String::new(),
                direction: stable::tool::RuntimeDirection::Mixed,
                buffer_kind: stable::tool::RuntimeBufferKind::Mixed,
                count: u64::MAX,
                rate_hz: f32::MAX,
                drops: u64::MAX,
                latest_overwrites: u64::MAX,
                bounded_evictions: u64::MAX,
                capacity: u64::MAX,
                current_depth: u64::MAX,
                high_water_depth: u64::MAX,
                decode_errors: u64::MAX,
                overflowed_rows: u32::MAX,
            }),
        }
    }

    #[test]
    fn runtime_history_retains_five_minutes_and_uses_cursor_sequence() {
        let mut history = RuntimeHistory::new("generation-a".to_string());
        let start = Instant::now();
        let first = history
            .ingest(start, "drive".to_string(), runtime_rollup(1_000_000_000))
            .unwrap();
        let second = history
            .ingest(
                start + Duration::from_secs(1),
                "map".to_string(),
                runtime_rollup(1_000_000_001),
            )
            .unwrap();
        assert_eq!(first.cursor.sequence, 1);
        assert_eq!(second.cursor.sequence, 2);

        let snapshot = history.snapshot(
            start + RUNTIME_RETENTION,
            &stable::tool::runtime::SnapshotRequest {
                participant_id: None,
                limit: 0,
                before_sequence: None,
            },
        );
        assert_eq!(snapshot.records.len(), 2);

        let expired = history.snapshot(
            start + RUNTIME_RETENTION + Duration::from_nanos(1),
            &stable::tool::runtime::SnapshotRequest {
                participant_id: None,
                limit: 0,
                before_sequence: None,
            },
        );
        assert_eq!(expired.records.len(), 1);
        assert_eq!(expired.records[0].participant_id, "map");
        assert_eq!(expired.cursor.sequence, 2);
    }

    #[test]
    fn runtime_snapshot_filters_and_returns_newest_bounded_records_in_order() {
        let mut history = RuntimeHistory::new("generation-a".to_string());
        let start = Instant::now();
        for index in 0..5 {
            let participant = if index % 2 == 0 { "drive" } else { "map" };
            history
                .ingest(
                    start + Duration::from_secs(index),
                    participant.to_string(),
                    runtime_rollup(index),
                )
                .unwrap();
        }
        let snapshot = history.snapshot(
            start + Duration::from_secs(5),
            &stable::tool::runtime::SnapshotRequest {
                participant_id: Some("drive".to_string()),
                limit: 2,
                before_sequence: None,
            },
        );
        assert_eq!(snapshot.cursor.sequence, 5);
        assert_eq!(snapshot.records.len(), 2);
        assert_eq!(snapshot.records[0].window_ns, 2);
        assert_eq!(snapshot.records[1].window_ns, 4);
        assert_eq!(snapshot.next_before_sequence, Some(3));

        let older = history.snapshot(
            start + Duration::from_secs(5),
            &stable::tool::runtime::SnapshotRequest {
                participant_id: Some("drive".to_string()),
                limit: 2,
                before_sequence: snapshot.next_before_sequence,
            },
        );
        assert_eq!(older.records.len(), 1);
        assert_eq!(older.records[0].window_ns, 0);
        assert_eq!(older.next_before_sequence, None);
    }

    #[test]
    fn runtime_ingest_clamps_identity_and_rows_with_deterministic_overflow() {
        let mut topics = (0..260)
            .rev()
            .map(|index| runtime_topic(format!("v0.1/test/{index:03}")))
            .collect::<Vec<_>>();
        topics.push(runtime_topic("z".repeat(MAX_RUNTIME_TOPIC_BYTES + 1)));
        let mut source_overflow = empty_runtime_overflow();
        source_overflow.count = 3;
        source_overflow.overflowed_rows = 2;
        let mut history = RuntimeHistory::new("generation-a".to_string());
        let follow = history
            .ingest(
                Instant::now(),
                "participant-é".repeat(MAX_RUNTIME_PARTICIPANT_BYTES),
                stable::tool::runtime::Rollup {
                    window_ns: 1,
                    step: None,
                    topics,
                    overflow: Some(source_overflow),
                },
            )
            .unwrap();

        let record = follow.record;
        assert_eq!(record.truncated, 1);
        assert!(record.participant_id.len() <= MAX_RUNTIME_PARTICIPANT_BYTES);
        assert!(
            record
                .participant_id
                .is_char_boundary(record.participant_id.len())
        );
        assert_eq!(record.topics.len(), MAX_RUNTIME_TOPIC_ROWS);
        assert_eq!(record.topics.first().unwrap().topic, "v0.1/test/000");
        assert_eq!(record.topics.last().unwrap().topic, "v0.1/test/255");
        assert!(
            record
                .topics
                .iter()
                .all(|row| row.topic.len() <= MAX_RUNTIME_TOPIC_BYTES)
        );
        let overflow = record.overflow.unwrap();
        assert!(overflow.topic.is_empty());
        // Two source-overflow rows + four excess valid rows + one oversized row.
        assert_eq!(overflow.overflowed_rows, 7);
        assert_eq!(overflow.count, 8);
    }

    #[test]
    fn runtime_ingest_sanitizes_all_non_finite_and_saturating_rates() {
        let mut normal = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, f32::MAX]
            .into_iter()
            .enumerate()
            .map(|(index, rate_hz)| {
                let mut row = runtime_topic(format!("v0.1/rate/{index}"));
                row.rate_hz = rate_hz;
                row
            })
            .collect::<Vec<_>>();
        for rate_hz in [f32::INFINITY, f32::NEG_INFINITY, f32::MAX, f32::MAX] {
            let mut row = runtime_topic("x".repeat(MAX_RUNTIME_TOPIC_BYTES + 1));
            row.rate_hz = rate_hz;
            normal.push(row);
        }
        let mut source_overflow = empty_runtime_overflow();
        source_overflow.rate_hz = f32::NAN;

        let (topics, overflow) = bounded_runtime_topics(normal, Some(source_overflow));
        let rates = topics.iter().map(|row| row.rate_hz).collect::<Vec<_>>();
        assert_eq!(rates, vec![0.0, f32::MAX, 0.0, f32::MAX]);
        assert!(rates.iter().all(|rate| rate.is_finite()));
        let overflow = overflow.unwrap();
        assert_eq!(overflow.rate_hz, f32::MAX);
        assert!(overflow.rate_hz.is_finite());
    }

    #[test]
    fn shuffled_duplicate_keys_aggregate_before_row_cap_without_double_overflow() {
        const ADVERSARIAL_RATES: [f32; 3] = [323.832_76, 150.849_17, 650.934_45];

        let mut rows = (0..MAX_RUNTIME_TOPIC_ROWS)
            .map(|index| runtime_topic(format!("v0.1/duplicate/{index:03}")))
            .collect::<Vec<_>>();
        let first = rows.first_mut().unwrap();
        first.count = u64::MAX;
        first.rate_hz = ADVERSARIAL_RATES[0];
        first.drops = u64::MAX;
        first.latest_overwrites = u64::MAX;
        first.bounded_evictions = u64::MAX;
        first.capacity = u64::MAX;
        first.current_depth = u64::MAX;
        first.high_water_depth = u64::MAX;
        first.decode_errors = u64::MAX;

        let mut duplicate = runtime_topic("v0.1/duplicate/000".to_string());
        duplicate.rate_hz = ADVERSARIAL_RATES[1];
        duplicate.latest_overwrites = 1;
        duplicate.bounded_evictions = 1;
        rows.push(duplicate);
        let mut duplicate = runtime_topic("v0.1/duplicate/000".to_string());
        duplicate.rate_hz = ADVERSARIAL_RATES[2];
        rows.push(duplicate);

        for rate_hz in ADVERSARIAL_RATES {
            let mut invalid = runtime_topic("x".repeat(MAX_RUNTIME_TOPIC_BYTES + 1));
            invalid.rate_hz = rate_hz;
            rows.push(invalid);
        }

        let mut source_overflow = empty_runtime_overflow();
        source_overflow.count = 9;
        source_overflow.rate_hz = 3.0;
        source_overflow.overflowed_rows = 2;

        let ordered = bounded_runtime_topics(rows.clone(), Some(source_overflow.clone()));
        rows.rotate_left(73);
        rows.reverse();
        let shuffled = bounded_runtime_topics(rows, Some(source_overflow));
        assert_eq!(
            MessagePack::encode(&ordered).unwrap(),
            MessagePack::encode(&shuffled).unwrap()
        );

        let (topics, overflow) = ordered;
        assert_eq!(topics.len(), MAX_RUNTIME_TOPIC_ROWS);
        let aggregate = topics.first().unwrap();
        assert_eq!(aggregate.topic, "v0.1/duplicate/000");
        assert_eq!(aggregate.count, u64::MAX);
        assert_eq!(aggregate.rate_hz.to_bits(), 0x448c_b3ba);
        assert_eq!(aggregate.drops, u64::MAX);
        assert_eq!(aggregate.latest_overwrites, u64::MAX);
        assert_eq!(aggregate.bounded_evictions, u64::MAX);
        assert_eq!(aggregate.capacity, u64::MAX);
        assert_eq!(aggregate.current_depth, u64::MAX);
        assert_eq!(aggregate.high_water_depth, u64::MAX);
        assert_eq!(aggregate.decode_errors, u64::MAX);

        let overflow = overflow.unwrap();
        assert_eq!(overflow.count, 12);
        assert_eq!(overflow.rate_hz.to_bits(), 0x448d_13ba);
        assert_eq!(overflow.overflowed_rows, 5);
    }

    #[test]
    fn runtime_history_is_byte_bounded_and_maximal_query_is_wire_safe() {
        let start = Instant::now();
        let mut history = RuntimeHistory::new("generation-a".to_string());
        for index in 0..MAX_RUNTIME_QUERY_RECORDS {
            history
                .ingest(
                    start + Duration::from_millis(index as u64),
                    "p".repeat(MAX_RUNTIME_PARTICIPANT_BYTES),
                    maximal_runtime_rollup(),
                )
                .unwrap();
        }
        assert_eq!(history.records.len(), MAX_RUNTIME_QUERY_RECORDS);
        assert_eq!(history.capacity_evictions, 0);
        assert!(history.retained_bytes <= MAX_RUNTIME_RETAINED_BYTES);

        let snapshot = history.snapshot(
            start + Duration::from_secs(1),
            &stable::tool::runtime::SnapshotRequest {
                participant_id: None,
                limit: MAX_RUNTIME_QUERY_RECORDS as u32,
                before_sequence: None,
            },
        );
        assert_eq!(snapshot.records.len(), MAX_RUNTIME_QUERY_RECORDS);
        let encoded = MessagePack::encode(&snapshot).unwrap();
        assert!(
            encoded.len() <= MAX_RUNTIME_SNAPSHOT_WIRE_BYTES,
            "{}-byte maximal runtime snapshot exceeds the decoder ceiling",
            encoded.len()
        );

        for index in MAX_RUNTIME_QUERY_RECORDS..(MAX_RUNTIME_QUERY_RECORDS * 3) {
            history
                .ingest(
                    start + Duration::from_millis(index as u64),
                    "p".repeat(MAX_RUNTIME_PARTICIPANT_BYTES),
                    maximal_runtime_rollup(),
                )
                .unwrap();
        }
        assert!(history.capacity_evictions > 0);
        assert!(history.retained_bytes <= MAX_RUNTIME_RETAINED_BYTES);
    }

    fn device_sample(device_id: &str, window_ns: u64) -> stable::tool::device::Sample {
        stable::tool::device::Sample {
            device_id: device_id.to_string(),
            cpu_pct: Some(42.0),
            ram_used_bytes: Some(50),
            ram_total_bytes: Some(100),
            swap_used_bytes: None,
            swap_total_bytes: None,
            load_1m: Some(1.0),
            load_5m: Some(0.5),
            load_15m: Some(0.25),
            uptime_s: Some(10),
            disks: Some(Vec::new()),
            window_ns,
        }
    }

    #[test]
    fn device_history_is_bounded_filterable_and_cursor_ordered() {
        let start = Instant::now();
        let mut history = DeviceHistory::new("generation-a".to_string());
        for index in 0..5 {
            let id = if index % 2 == 0 { "main" } else { "attached" };
            history
                .ingest(start + Duration::from_secs(index), device_sample(id, index))
                .unwrap();
        }
        let snapshot = history.snapshot(
            start + Duration::from_secs(5),
            &stable::tool::device::SnapshotRequest {
                device_id: Some("main".to_string()),
                limit: 2,
                before_sequence: None,
            },
        );
        assert_eq!(snapshot.cursor.sequence, 5);
        assert_eq!(snapshot.records.len(), 2);
        assert_eq!(snapshot.records[0].sample.window_ns, 2);
        assert_eq!(snapshot.records[1].sample.window_ns, 4);
        assert_eq!(snapshot.next_before_sequence, Some(3));
    }

    #[test]
    fn device_ingest_discloses_bounds_and_never_fabricates_capabilities() {
        let mut sample = device_sample(&"é".repeat(MAX_DEVICE_ID_BYTES), 1);
        sample.cpu_pct = Some(f32::NAN);
        sample.ram_used_bytes = Some(200);
        sample.ram_total_bytes = Some(100);
        sample.swap_used_bytes = Some(0);
        sample.swap_total_bytes = Some(0);
        sample.load_1m = Some(f32::INFINITY);
        sample.disks = Some(
            (0..(MAX_DEVICE_DISK_ROWS + 3))
                .map(|index| stable::tool::device::Disk {
                    mount_point: format!("/{}-{index}", "m".repeat(MAX_DEVICE_TEXT_BYTES * 2)),
                    file_system: "f".repeat(MAX_DEVICE_TEXT_BYTES * 2),
                    used_bytes: 200,
                    total_bytes: 100,
                })
                .collect(),
        );
        let mut history = DeviceHistory::new("generation-a".to_string());
        let record = history.ingest(Instant::now(), sample).unwrap().record;
        assert!(record.truncated >= 4);
        assert!(record.sample.device_id.len() <= MAX_DEVICE_ID_BYTES);
        assert_eq!(record.sample.cpu_pct, None);
        assert_eq!(record.sample.ram_used_bytes, Some(100));
        assert_eq!(record.sample.swap_used_bytes, None);
        assert_eq!(record.sample.swap_total_bytes, None);
        assert_eq!(record.sample.load_1m, None);
        let disks = record.sample.disks.unwrap();
        assert_eq!(disks.len(), MAX_DEVICE_DISK_ROWS);
        assert!(disks.iter().all(|disk| {
            disk.mount_point.len() <= MAX_DEVICE_TEXT_BYTES
                && disk.file_system.len() <= MAX_DEVICE_TEXT_BYTES
                && disk.used_bytes <= disk.total_bytes
        }));
    }

    #[test]
    fn maximal_device_snapshot_is_wire_and_memory_bounded() {
        let start = Instant::now();
        let mut history = DeviceHistory::new("generation-a".to_string());
        for index in 0..MAX_DEVICE_RECORDS * 2 {
            let mut sample = device_sample("main", u64::MAX);
            sample.disks = Some(
                (0..MAX_DEVICE_DISK_ROWS)
                    .map(|disk| stable::tool::device::Disk {
                        mount_point: format!("/{disk}-{}", "m".repeat(MAX_DEVICE_TEXT_BYTES)),
                        file_system: "f".repeat(MAX_DEVICE_TEXT_BYTES),
                        used_bytes: u64::MAX,
                        total_bytes: u64::MAX,
                    })
                    .collect(),
            );
            history
                .ingest(start + Duration::from_millis(index as u64), sample)
                .unwrap();
        }
        assert!(history.records.len() <= MAX_DEVICE_RECORDS);
        assert!(history.retained_bytes <= MAX_DEVICE_RETAINED_BYTES);
        assert!(history.capacity_evictions > 0);
        let snapshot = history.snapshot(
            start + Duration::from_secs(2),
            &stable::tool::device::SnapshotRequest {
                device_id: None,
                limit: u32::MAX,
                before_sequence: None,
            },
        );
        assert!(snapshot.records.len() <= MAX_DEVICE_QUERY_RECORDS);
        let encoded = MessagePack::encode(&snapshot).unwrap();
        assert!(
            encoded.len() <= MAX_DEVICE_SNAPSHOT_WIRE_BYTES,
            "{}-byte device snapshot exceeds the wire cap",
            encoded.len()
        );
    }
}
