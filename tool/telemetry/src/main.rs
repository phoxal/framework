use std::time::Duration;

use phoxal::prelude::*;
use phoxal::raw::{Bus, LogicalTime, OwnerCap, Publisher};
use phoxal_api::y2026_9 as api;
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};

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
            .with_memory(MemoryRefreshKind::nothing().with_ram()),
    );
    let mut interval = tokio::time::interval(SAMPLE_INTERVAL);

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
        let sample = sample_host(&mut system);
        if let Err(error) = publisher.publish_at(now(), sample).await {
            tracing::warn!(target: "tool_telemetry", error = %error, "host telemetry publish failed");
        }
    }
}

fn sample_host(system: &mut System) -> api::telemetry::Host {
    system.refresh_cpu_usage();
    system.refresh_memory();

    api::telemetry::Host {
        cpu_pct: system.global_cpu_usage(),
        ram_used_bytes: system.used_memory(),
        ram_total_bytes: system.total_memory(),
        // Not available on every platform (notably Windows); `sysinfo`
        // reports `0.0` there rather than erroring, which is exactly the
        // "publish 0.0 where unsupported" behavior this field wants.
        load_1m: System::load_average().one as f32,
        window_ns: u64::try_from(SAMPLE_INTERVAL.as_nanos()).unwrap_or(u64::MAX),
    }
}

fn now() -> LogicalTime {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    LogicalTime::new(0, u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX))
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
                .with_memory(MemoryRefreshKind::nothing().with_ram()),
        );
        let sample = sample_host(&mut system);
        assert_eq!(sample.window_ns, 1_000_000_000);
    }
}
