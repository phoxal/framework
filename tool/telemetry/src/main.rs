use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use phoxal::prelude::*;
use phoxal::raw::{
    Bus, Codec, MessagePack, OwnerCap, Publisher, QueryFailure, Subscriber, host_time,
};
use phoxal_api::{v1 as stable, v2 as api};
use sysinfo::{CpuRefreshKind, DiskRefreshKind, Disks, MemoryRefreshKind, RefreshKind, System};

/// Host sampling cadence: frequent enough for a live CLI dashboard to feel
/// current, far below anything that would make this tool itself a meaningful
/// load source on the host it is reporting on.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const MAX_DISK_ROWS: usize = 32;
const MAX_DISK_TEXT_BYTES: usize = 128;
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
const MAX_HOST_WIRE_BYTES: usize = 16 * 1024;
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
        let publisher = host_publisher(bus.clone(), cap)?;
        ctx.spawn_managed_with("host-sampler", ManagedTaskPolicy::FaultOnExit, async move {
            sample_host_forever(publisher).await;
        });

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
    let mut overflow = overflow.map(normalize_runtime_overflow);
    let mut normal = BTreeMap::<
        (
            String,
            stable::tool::RuntimeDirection,
            stable::tool::RuntimeBufferKind,
        ),
        stable::tool::RuntimeTopic,
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
            match normal.entry(key) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(row);
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    merge_runtime_values(entry.get_mut(), &row);
                }
            }
        } else {
            let omitted_rows = row.overflowed_rows.saturating_add(1);
            merge_runtime_overflow(&mut overflow, row, omitted_rows);
        }
    }

    let mut retained = Vec::with_capacity(normal.len().min(MAX_RUNTIME_TOPIC_ROWS));
    for row in normal.into_values() {
        if retained.len() < MAX_RUNTIME_TOPIC_ROWS {
            retained.push(row);
        } else {
            merge_runtime_overflow(&mut overflow, row, 1);
        }
    }
    (retained, overflow)
}

fn normalize_runtime_overflow(mut row: stable::tool::RuntimeTopic) -> stable::tool::RuntimeTopic {
    row.topic.clear();
    row.direction = stable::tool::RuntimeDirection::Mixed;
    row.buffer_kind = stable::tool::RuntimeBufferKind::Mixed;
    row.rate_hz = finite_rate(row.rate_hz);
    row
}

fn merge_runtime_overflow(
    overflow: &mut Option<stable::tool::RuntimeTopic>,
    row: stable::tool::RuntimeTopic,
    omitted_rows: u32,
) {
    let target = overflow.get_or_insert_with(empty_runtime_overflow);
    merge_runtime_values(target, &row);
    target.overflowed_rows = target.overflowed_rows.saturating_add(omitted_rows);
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

fn host_publisher(bus: Bus, cap: OwnerCap) -> Result<Publisher<api::telemetry::Host>> {
    let topic = api::topic::internal::new(cap).telemetry().host();
    let publisher = Publisher::new(bus, &topic)?;
    Ok(publisher)
}

/// Owns the `sysinfo::System` handle for the tool's lifetime, sampling at
/// [`SAMPLE_INTERVAL`] and publishing a `telemetry::Host` sample each tick.
/// Runs until the runner cancels it during managed shutdown.
async fn sample_host_forever(publisher: Publisher<api::telemetry::Host>) {
    let initialized = tokio::task::spawn_blocking(|| {
        let mut system = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(CpuRefreshKind::nothing().with_cpu_usage())
                .with_memory(MemoryRefreshKind::nothing().with_ram().with_swap()),
        );
        let disks = Disks::new_with_refreshed_list_specifics(disk_refresh_kind());
        system.refresh_cpu_usage();
        system.refresh_memory();
        (system, disks, Instant::now())
    })
    .await;
    let (mut system, mut disks, mut previous_refresh) = match initialized {
        Ok(initialized) => initialized,
        Err(error) => {
            tracing::warn!(target: "tool_telemetry", error = %error, "host telemetry sampler initialization failed");
            return;
        }
    };
    let mut interval = tokio::time::interval(SAMPLE_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // `sysinfo`'s CPU usage is a delta against the PREVIOUS refresh, so the
    // very first refresh has nothing to diff against and reads inaccurately
    // (sysinfo's own documented caveat). Burn one tick priming the delta
    // before the loop below starts publishing, so every PUBLISHED sample is
    // a real measurement.
    interval.tick().await;
    loop {
        interval.tick().await;
        let refreshed = tokio::task::spawn_blocking(move || {
            let (sample, refreshed_at) = sample_host(&mut system, &mut disks, previous_refresh);
            (system, disks, sample, refreshed_at)
        })
        .await;
        let (returned_system, returned_disks, sample, refreshed_at) = match refreshed {
            Ok(refreshed) => refreshed,
            Err(error) => {
                tracing::warn!(target: "tool_telemetry", error = %error, "host telemetry sampler failed");
                return;
            }
        };
        system = returned_system;
        disks = returned_disks;
        previous_refresh = refreshed_at;
        if let Err(error) = publisher.publish_at(host_time(), sample).await {
            tracing::warn!(target: "tool_telemetry", error = %error, "host telemetry publish failed");
        }
    }
}

fn sample_host(
    system: &mut System,
    disks: &mut Disks,
    previous_refresh: Instant,
) -> (api::telemetry::Host, Instant) {
    system.refresh_cpu_usage();
    system.refresh_memory();
    disks.refresh_specifics(true, disk_refresh_kind());
    let refreshed_at = Instant::now();

    let load = System::load_average();
    let uptime_s = System::uptime();

    let disks = disk_samples(disks);
    let sample = api::telemetry::Host {
        cpu_pct: system.global_cpu_usage(),
        ram_used_bytes: system.used_memory(),
        ram_total_bytes: system.total_memory(),
        swap_used_bytes: system.used_swap(),
        swap_total_bytes: system.total_swap(),
        // Not available on every platform (notably Windows); `sysinfo`
        // reports `0.0` there rather than erroring, which is exactly the
        // "publish 0.0 where unsupported" behavior this field wants.
        load_1m: load.one as f32,
        load_5m: load.five as f32,
        load_15m: load.fifteen as f32,
        // `sysinfo` reports zero on unsupported targets. A real sample is not
        // emitted until one second after startup, so zero is an unavailable
        // value rather than a plausible host uptime here.
        uptime_s: optional_uptime(uptime_s),
        disks,
        window_ns: u64::try_from(
            refreshed_at
                .saturating_duration_since(previous_refresh)
                .as_nanos(),
        )
        .unwrap_or(u64::MAX),
    };
    (sample, refreshed_at)
}

fn disk_refresh_kind() -> DiskRefreshKind {
    DiskRefreshKind::nothing().with_storage()
}

fn optional_uptime(uptime_s: u64) -> Option<u64> {
    (uptime_s > 0).then_some(uptime_s)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DiskObservation {
    name: String,
    mount_point: String,
    file_system: String,
    available_bytes: u64,
    total_bytes: u64,
}

fn disk_samples(disks: &Disks) -> Vec<api::telemetry::Disk> {
    normalize_disks(disks.iter().map(|disk| DiskObservation {
        name: disk.name().to_string_lossy().into_owned(),
        mount_point: disk.mount_point().to_string_lossy().into_owned(),
        file_system: disk.file_system().to_string_lossy().into_owned(),
        available_bytes: disk.available_space(),
        total_bytes: disk.total_space(),
    }))
}

fn normalize_disks(
    observations: impl IntoIterator<Item = DiskObservation>,
) -> Vec<api::telemetry::Disk> {
    let mut by_mount = BTreeMap::<String, DiskObservation>::new();
    for disk in observations.into_iter().filter(is_real_disk) {
        match by_mount.entry(disk.mount_point.clone()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(disk);
            }
            std::collections::btree_map::Entry::Occupied(mut entry)
                if disk_preference(&disk) > disk_preference(entry.get()) =>
            {
                entry.insert(disk);
            }
            std::collections::btree_map::Entry::Occupied(_) => {}
        }
    }
    let total = by_mount.len();
    let keep = if total > MAX_DISK_ROWS {
        MAX_DISK_ROWS.saturating_sub(1)
    } else {
        MAX_DISK_ROWS
    };
    let mut disks = by_mount
        .into_values()
        .take(keep)
        .map(|disk| api::telemetry::Disk {
            mount_point: truncate_utf8(disk.mount_point, MAX_DISK_TEXT_BYTES),
            file_system: truncate_utf8(disk.file_system, MAX_DISK_TEXT_BYTES),
            used_bytes: disk.total_bytes.saturating_sub(disk.available_bytes),
            total_bytes: disk.total_bytes,
        })
        .collect::<Vec<_>>();
    let truncated = total.saturating_sub(keep);
    if truncated > 0 {
        disks.push(api::telemetry::Disk {
            mount_point: String::new(),
            file_system: format!("+{truncated} omitted"),
            used_bytes: 0,
            total_bytes: 0,
        });
    }
    disks
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

fn disk_preference(disk: &DiskObservation) -> (u64, u64, &str, &str) {
    (
        disk.total_bytes,
        disk.total_bytes.saturating_sub(disk.available_bytes),
        disk.file_system.as_str(),
        disk.name.as_str(),
    )
}

fn is_real_disk(disk: &DiskObservation) -> bool {
    let name = disk.name.to_ascii_lowercase();
    let file_system = disk.file_system.to_ascii_lowercase();
    let pseudo = matches!(
        file_system.as_str(),
        "autofs"
            | "binfmt_misc"
            | "cgroup"
            | "cgroup2"
            | "configfs"
            | "debugfs"
            | "devpts"
            | "devtmpfs"
            | "fusectl"
            | "hugetlbfs"
            | "mqueue"
            | "nsfs"
            | "overlay"
            | "proc"
            | "pstore"
            | "ramfs"
            | "securityfs"
            | "squashfs"
            | "sysfs"
            | "tmpfs"
            | "tracefs"
    );
    !disk.mount_point.is_empty()
        && disk.total_bytes > 0
        && !pseudo
        && !name.starts_with("/dev/loop")
        && !name.starts_with("loop")
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
                let prefix = format!("v1/{index:03}/");
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
            .map(|index| runtime_topic(format!("v1/test/{index:03}")))
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
        assert_eq!(record.topics.first().unwrap().topic, "v1/test/000");
        assert_eq!(record.topics.last().unwrap().topic, "v1/test/255");
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
                let mut row = runtime_topic(format!("v1/rate/{index}"));
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
        let mut rows = (0..MAX_RUNTIME_TOPIC_ROWS)
            .map(|index| runtime_topic(format!("v1/duplicate/{index:03}")))
            .collect::<Vec<_>>();
        let first = rows.first_mut().unwrap();
        first.count = u64::MAX;
        first.rate_hz = f32::MAX;
        first.drops = u64::MAX;
        first.latest_overwrites = u64::MAX;
        first.bounded_evictions = u64::MAX;
        first.capacity = u64::MAX;
        first.current_depth = u64::MAX;
        first.high_water_depth = u64::MAX;
        first.decode_errors = u64::MAX;

        let mut duplicate = runtime_topic("v1/duplicate/000".to_string());
        duplicate.rate_hz = f32::MAX;
        duplicate.latest_overwrites = 1;
        duplicate.bounded_evictions = 1;
        rows.push(duplicate);

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
        assert_eq!(aggregate.topic, "v1/duplicate/000");
        assert_eq!(aggregate.count, u64::MAX);
        assert_eq!(aggregate.rate_hz, f32::MAX);
        assert_eq!(aggregate.drops, u64::MAX);
        assert_eq!(aggregate.latest_overwrites, u64::MAX);
        assert_eq!(aggregate.bounded_evictions, u64::MAX);
        assert_eq!(aggregate.capacity, u64::MAX);
        assert_eq!(aggregate.current_depth, u64::MAX);
        assert_eq!(aggregate.high_water_depth, u64::MAX);
        assert_eq!(aggregate.decode_errors, u64::MAX);

        let overflow = overflow.unwrap();
        assert_eq!(overflow.count, 9);
        assert_eq!(overflow.rate_hz, 3.0);
        assert_eq!(overflow.overflowed_rows, 2);
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

    #[test]
    fn sample_host_window_ns_uses_the_measured_refresh_window() {
        let mut system = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(CpuRefreshKind::nothing().with_cpu_usage())
                .with_memory(MemoryRefreshKind::nothing().with_ram().with_swap()),
        );
        let mut disks = Disks::new_with_refreshed_list_specifics(disk_refresh_kind());
        let previous_refresh = Instant::now()
            .checked_sub(Duration::from_secs(2))
            .expect("two seconds before now");
        let (sample, _) = sample_host(&mut system, &mut disks, previous_refresh);
        assert!(sample.window_ns >= 2_000_000_000);
        assert!(sample.window_ns < 3_000_000_000);
    }

    #[test]
    fn disk_samples_filter_pseudo_loop_zero_and_duplicate_mounts() {
        let observations = [
            observation("overlay", "/", "overlay", 100, 10),
            observation("/dev/loop0", "/snap/core", "ext4", 100, 10),
            observation("/dev/disk1", "/", "apfs", 1_000, 250),
            observation("/dev/disk2", "/", "apfs", 2_000, 500),
            observation("/dev/disk3", "/empty", "ext4", 0, 0),
            observation("/dev/disk4", "/data", "ext4", 500, 125),
        ];

        let samples = normalize_disks(observations.clone());
        let reversed = normalize_disks(observations.into_iter().rev());

        assert_eq!(samples, reversed);
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].mount_point, "/");
        assert_eq!(samples[0].used_bytes, 1_500);
        assert_eq!(samples[0].total_bytes, 2_000);
        assert_eq!(samples[1].mount_point, "/data");
        assert_eq!(samples[1].used_bytes, 375);
    }

    #[test]
    fn unsupported_uptime_and_empty_disk_inventory_are_non_fatal() {
        assert_eq!(optional_uptime(0), None);
        assert_eq!(optional_uptime(42), Some(42));
        assert!(normalize_disks([]).is_empty());
    }

    #[test]
    fn disk_inventory_is_bounded_disclosed_and_wire_safe() {
        let observations = (0..(MAX_DISK_ROWS + 5)).map(|index| {
            observation(
                &format!("/dev/disk{index}"),
                &format!("/{}-{index}", "m".repeat(MAX_DISK_TEXT_BYTES * 2)),
                &"f".repeat(MAX_DISK_TEXT_BYTES * 2),
                1_000,
                250,
            )
        });
        let disks = normalize_disks(observations);
        assert_eq!(disks.len(), MAX_DISK_ROWS);
        let sentinel = disks.last().expect("truncation sentinel should exist");
        assert!(sentinel.mount_point.is_empty());
        assert_eq!(sentinel.file_system, "+6 omitted");
        assert!(disks[..disks.len() - 1].iter().all(|disk| {
            disk.mount_point.len() <= MAX_DISK_TEXT_BYTES
                && disk.file_system.len() <= MAX_DISK_TEXT_BYTES
        }));

        let host = api::telemetry::Host {
            cpu_pct: 100.0,
            ram_used_bytes: u64::MAX,
            ram_total_bytes: u64::MAX,
            swap_used_bytes: u64::MAX,
            swap_total_bytes: u64::MAX,
            load_1m: f32::MAX,
            load_5m: f32::MAX,
            load_15m: f32::MAX,
            uptime_s: Some(u64::MAX),
            disks,
            window_ns: u64::MAX,
        };
        let encoded = MessagePack::encode(&host).expect("host telemetry should encode");
        assert!(
            encoded.len() <= MAX_HOST_WIRE_BYTES,
            "{}-byte host snapshot exceeds the wire cap",
            encoded.len()
        );
    }

    fn observation(
        name: &str,
        mount_point: &str,
        file_system: &str,
        total_bytes: u64,
        available_bytes: u64,
    ) -> DiskObservation {
        DiskObservation {
            name: name.to_string(),
            mount_point: mount_point.to_string(),
            file_system: file_system.to_string(),
            available_bytes,
            total_bytes,
        }
    }
}
