use std::collections::BTreeMap;
use std::time::Duration;

use phoxal::prelude::*;
use phoxal::raw::{Bus, OwnerCap, Publisher, host_time};
use phoxal_api::v2 as api;
use sysinfo::{CpuRefreshKind, DiskRefreshKind, Disks, MemoryRefreshKind, RefreshKind, System};

/// Host sampling cadence: frequent enough for a live CLI dashboard to feel
/// current, far below anything that would make this tool itself a meaningful
/// load source on the host it is reporting on.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

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
        let publisher = host_publisher(ctx.raw_bus())?;
        ctx.spawn_managed_with("host-sampler", ManagedTaskPolicy::FaultOnExit, async move {
            sample_host_forever(publisher).await;
        });

        tracing::info!(target: "tool_telemetry", "telemetry ready");

        Ok((Self, ()))
    }
}

fn host_publisher(bus: Bus) -> Result<Publisher<api::telemetry::Host>> {
    let topic = api::topic::internal::new(OwnerCap::__mint())
        .telemetry()
        .host();
    let publisher = Publisher::new(bus, &topic)?;
    Ok(publisher)
}

/// Owns the `sysinfo::System` handle for the tool's lifetime, sampling at
/// [`SAMPLE_INTERVAL`] and publishing a `telemetry::Host` sample each tick.
/// Runs until the runner cancels it during managed shutdown.
async fn sample_host_forever(publisher: Publisher<api::telemetry::Host>) {
    let mut system = System::new_with_specifics(
        RefreshKind::nothing()
            .with_cpu(CpuRefreshKind::nothing().with_cpu_usage())
            .with_memory(MemoryRefreshKind::nothing().with_ram().with_swap()),
    );
    let mut disks = Disks::new_with_refreshed_list_specifics(disk_refresh_kind());
    let mut interval = tokio::time::interval(SAMPLE_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // `sysinfo`'s CPU usage is a delta against the PREVIOUS refresh, so the
    // very first refresh has nothing to diff against and reads inaccurately
    // (sysinfo's own documented caveat). Burn one tick priming the delta
    // before the loop below starts publishing, so every PUBLISHED sample is
    // a real measurement.
    interval.tick().await;
    system.refresh_cpu_usage();
    system.refresh_memory();

    loop {
        interval.tick().await;
        let sample = sample_host(&mut system, &mut disks);
        if let Err(error) = publisher.publish_at(host_time(), sample).await {
            tracing::warn!(target: "tool_telemetry", error = %error, "host telemetry publish failed");
        }
    }
}

fn sample_host(system: &mut System, disks: &mut Disks) -> api::telemetry::Host {
    system.refresh_cpu_usage();
    system.refresh_memory();
    disks.refresh_specifics(true, disk_refresh_kind());

    let load = System::load_average();
    let uptime_s = System::uptime();

    api::telemetry::Host {
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
        disks: disk_samples(disks),
        window_ns: u64::try_from(SAMPLE_INTERVAL.as_nanos()).unwrap_or(u64::MAX),
    }
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
    by_mount
        .into_values()
        .map(|disk| api::telemetry::Disk {
            mount_point: disk.mount_point,
            file_system: disk.file_system,
            used_bytes: disk.total_bytes.saturating_sub(disk.available_bytes),
            total_bytes: disk.total_bytes,
        })
        .collect()
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
    fn sample_host_window_ns_matches_the_sample_interval() {
        let mut system = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(CpuRefreshKind::nothing().with_cpu_usage())
                .with_memory(MemoryRefreshKind::nothing().with_ram().with_swap()),
        );
        let mut disks = Disks::new_with_refreshed_list_specifics(disk_refresh_kind());
        let sample = sample_host(&mut system, &mut disks);
        assert_eq!(sample.window_ns, 1_000_000_000);
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
