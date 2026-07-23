use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use phoxal::api;
use phoxal::prelude::*;
use phoxal::raw::{Bus, OwnerCap, Publisher, host_time};
use sysinfo::{CpuRefreshKind, DiskRefreshKind, Disks, MemoryRefreshKind, RefreshKind, System};

const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const MAX_DISK_ROWS: usize = 32;
const MAX_DISK_TEXT_BYTES: usize = 128;

#[phoxal::tool(id = "device")]
struct ToolDevice;

#[phoxal::behavior]
impl ToolDevice {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        let publisher = device_publisher(ctx.raw_bus(), ctx.owner_capability())?;
        let device_id = ctx.execution_device_id()?.clone();
        ctx.spawn_managed_with(
            "device-sampler",
            ManagedTaskPolicy::FaultOnExit,
            sample_device_forever(publisher, device_id.clone()),
        );
        tracing::info!(target: "tool_device", device_id = %device_id, "device telemetry ready");
        Ok((Self, ()))
    }
}

fn device_publisher(bus: Bus, cap: OwnerCap) -> Result<Publisher<api::tool::device::Sample>> {
    let topic = api::topic::internal::new(cap).tool().device().sample();
    Ok(Publisher::new(bus, &topic)?)
}

async fn sample_device_forever(
    publisher: Publisher<api::tool::device::Sample>,
    device_id: ExecutionDeviceId,
) {
    let initialized = tokio::task::spawn_blocking(new_sampler).await;
    let (mut system, mut disks, mut previous_refresh) = match initialized {
        Ok(initialized) => initialized,
        Err(error) => {
            tracing::warn!(target: "tool_device", error = %error, "device sampler initialization failed");
            return;
        }
    };
    let mut interval = tokio::time::interval(SAMPLE_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // CPU utilization is a delta. Prime once and wait a full interval before
    // publishing so zero never means "the sampler had no previous refresh".
    interval.tick().await;
    loop {
        interval.tick().await;
        let sample_device_id = device_id.clone();
        let refreshed = tokio::task::spawn_blocking(move || {
            let sample =
                sample_device(&mut system, &mut disks, previous_refresh, &sample_device_id);
            (system, disks, sample)
        })
        .await;
        let (returned_system, returned_disks, (sample, refreshed_at)) = match refreshed {
            Ok(refreshed) => refreshed,
            Err(error) => {
                tracing::warn!(target: "tool_device", error = %error, "device sampler failed");
                return;
            }
        };
        system = returned_system;
        disks = returned_disks;
        previous_refresh = refreshed_at;
        if let Err(error) = publisher.publish_at(host_time(), sample).await {
            tracing::warn!(target: "tool_device", error = %error, "device telemetry publish failed");
        }
    }
}

fn new_sampler() -> (System, Disks, Instant) {
    let mut system = System::new_with_specifics(
        RefreshKind::nothing()
            .with_cpu(CpuRefreshKind::nothing().with_cpu_usage())
            .with_memory(MemoryRefreshKind::nothing().with_ram().with_swap()),
    );
    let disks = Disks::new_with_refreshed_list_specifics(disk_refresh_kind());
    system.refresh_cpu_usage();
    system.refresh_memory();
    (system, disks, Instant::now())
}

fn sample_device(
    system: &mut System,
    disks: &mut Disks,
    previous_refresh: Instant,
    device_id: &ExecutionDeviceId,
) -> (api::tool::device::Sample, Instant) {
    system.refresh_cpu_usage();
    system.refresh_memory();
    disks.refresh_specifics(true, disk_refresh_kind());
    let refreshed_at = Instant::now();
    let load = System::load_average();
    let (ram_used_bytes, ram_total_bytes) = capacity(system.used_memory(), system.total_memory());
    let (swap_used_bytes, swap_total_bytes) = capacity(system.used_swap(), system.total_swap());

    let sample = api::tool::device::Sample {
        device_id: device_id.as_str().to_string(),
        cpu_pct: finite_percentage(system.global_cpu_usage()),
        ram_used_bytes,
        ram_total_bytes,
        swap_used_bytes,
        swap_total_bytes,
        load_1m: optional_load(load.one),
        load_5m: optional_load(load.five),
        load_15m: optional_load(load.fifteen),
        uptime_s: (System::uptime() > 0).then_some(System::uptime()),
        disks: Some(disk_samples(disks)),
        window_ns: u64::try_from(
            refreshed_at
                .saturating_duration_since(previous_refresh)
                .as_nanos(),
        )
        .unwrap_or(u64::MAX),
    };
    (sample, refreshed_at)
}

fn finite_percentage(value: f32) -> Option<f32> {
    value.is_finite().then(|| value.clamp(0.0, 100.0))
}

fn capacity(used: u64, total: u64) -> (Option<u64>, Option<u64>) {
    if total == 0 {
        (None, None)
    } else {
        (Some(used.min(total)), Some(total))
    }
}

#[cfg(target_os = "windows")]
fn optional_load(_value: f64) -> Option<f32> {
    None
}

#[cfg(not(target_os = "windows"))]
fn optional_load(value: f64) -> Option<f32> {
    let value = value as f32;
    value.is_finite().then_some(value.max(0.0))
}

fn disk_refresh_kind() -> DiskRefreshKind {
    DiskRefreshKind::nothing().with_storage()
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DiskObservation {
    name: String,
    mount_point: String,
    file_system: String,
    available_bytes: u64,
    total_bytes: u64,
}

fn disk_samples(disks: &Disks) -> Vec<api::tool::device::Disk> {
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
) -> Vec<api::tool::device::Disk> {
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
        .take(MAX_DISK_ROWS)
        .map(|disk| api::tool::device::Disk {
            mount_point: truncate_utf8(disk.mount_point, MAX_DISK_TEXT_BYTES),
            file_system: truncate_utf8(disk.file_system, MAX_DISK_TEXT_BYTES),
            used_bytes: disk.total_bytes.saturating_sub(disk.available_bytes),
            total_bytes: disk.total_bytes,
        })
        .collect()
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
    phoxal::run::<ToolDevice>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_capacity_is_not_fabricated_as_zero() {
        assert_eq!(capacity(0, 0), (None, None));
        assert_eq!(capacity(150, 100), (Some(100), Some(100)));
        assert_eq!(finite_percentage(f32::NAN), None);
        assert_eq!(finite_percentage(130.0), Some(100.0));
    }

    #[test]
    fn disks_are_deduplicated_filtered_and_bounded() {
        let observations = (0..40).map(|index| DiskObservation {
            name: format!("disk-{index}"),
            mount_point: format!("/mnt/{index}"),
            file_system: "ext4".to_string(),
            available_bytes: 25,
            total_bytes: 100,
        });
        let disks = normalize_disks(observations);
        assert_eq!(disks.len(), MAX_DISK_ROWS);
        assert!(disks.iter().all(|disk| disk.used_bytes == 75));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn live_macos_sample_reports_supervisor_identity_with_truthful_capabilities() {
        let (mut system, mut disks, started) = new_sampler();
        let device_id = ExecutionDeviceId::new("project-e2e").unwrap();
        std::thread::sleep(Duration::from_millis(250));
        let (sample, _) = sample_device(&mut system, &mut disks, started, &device_id);
        assert_eq!(sample.device_id, device_id.as_str());
        assert!(sample.cpu_pct.is_some_and(f32::is_finite));
        assert!(sample.ram_total_bytes.is_some_and(|total| total > 0));
        assert!(sample.ram_used_bytes <= sample.ram_total_bytes);
        assert!(sample.disks.is_some());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn separate_per_robot_samplers_preserve_the_same_project_identity() {
        let device_id = ExecutionDeviceId::new("project-e2e").unwrap();
        let (mut first_system, mut first_disks, first_started) = new_sampler();
        let (mut second_system, mut second_disks, second_started) = new_sampler();
        std::thread::sleep(Duration::from_millis(250));

        let (first, _) = sample_device(
            &mut first_system,
            &mut first_disks,
            first_started,
            &device_id,
        );
        let (second, _) = sample_device(
            &mut second_system,
            &mut second_disks,
            second_started,
            &device_id,
        );
        assert_eq!(first.device_id, "project-e2e");
        assert_eq!(second.device_id, first.device_id);
    }
}
