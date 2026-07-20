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
const DEFAULT_RUNTIME_QUERY_RECORDS: usize = 64;
const MAX_RUNTIME_QUERY_RECORDS: usize = 64;
#[cfg(test)]
const MAX_HOST_WIRE_BYTES: usize = 16 * 1024;

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
                loop {
                    let received = match runtime_rollups.recv().await {
                        Ok(received) => received,
                        Err(error) => {
                            tracing::warn!(target: "tool_telemetry", error = %error, "runtime-performance ingest stopped");
                            return;
                        }
                    };
                    let follow = ingest_history
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .ingest(
                            Instant::now(),
                            received.metadata.source.participant,
                            received.body,
                        );
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
    record: stable::tool::runtime::Record,
}

#[derive(Debug)]
struct RuntimeHistory {
    generation: String,
    sequence: u64,
    capacity_evictions: u64,
    records: VecDeque<TimedRuntimeRecord>,
}

impl RuntimeHistory {
    fn new(generation: String) -> Self {
        Self {
            generation,
            sequence: 0,
            capacity_evictions: 0,
            records: VecDeque::new(),
        }
    }

    fn ingest(
        &mut self,
        now: Instant,
        participant_id: String,
        rollup: stable::tool::runtime::Rollup,
    ) -> stable::tool::runtime::Follow {
        self.prune(now);
        self.sequence = self
            .sequence
            .checked_add(1)
            .expect("tool-telemetry runtime sequence exhausted");
        let record = stable::tool::runtime::Record {
            sequence: self.sequence,
            participant_id,
            window_ns: rollup.window_ns,
            step: rollup.step,
            topics: rollup.topics,
            overflow: rollup.overflow,
        };
        self.records.push_back(TimedRuntimeRecord {
            received_at: now,
            record: record.clone(),
        });
        if self.records.len() > MAX_RUNTIME_RECORDS {
            self.records.pop_front();
            self.capacity_evictions = self.capacity_evictions.saturating_add(1);
        }
        stable::tool::runtime::Follow {
            cursor: self.cursor(),
            record,
        }
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
            self.records.pop_front();
        }
    }

    fn cursor(&self) -> stable::tool::Cursor {
        stable::tool::Cursor {
            generation: self.generation.clone(),
            sequence: self.sequence,
        }
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

    #[test]
    fn runtime_history_retains_five_minutes_and_uses_cursor_sequence() {
        let mut history = RuntimeHistory::new("generation-a".to_string());
        let start = Instant::now();
        let first = history.ingest(start, "drive".to_string(), runtime_rollup(1_000_000_000));
        let second = history.ingest(
            start + Duration::from_secs(1),
            "map".to_string(),
            runtime_rollup(1_000_000_001),
        );
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
            history.ingest(
                start + Duration::from_secs(index),
                participant.to_string(),
                runtime_rollup(index),
            );
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
