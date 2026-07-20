//! Runner-owned per-participant process resource self-sampling.
//!
//! Every participant that runs through this runner publishes its own
//! `v2::telemetry::Process` sample (cpu%, RSS). This is runner infrastructure,
//! not part of the
//! participant-authored `emit-apis` contract surface.
//!
//! # Why the sampling is off the lifecycle task
//!
//! OS sampling must never delay the participant lifecycle task. `sysinfo`'s
//! per-process refresh is a synchronous syscall burst of
//! non-trivial, unbounded cost, so it must not run on the lifecycle
//! task at all. This module splits the work in two:
//!
//! - a dedicated background task ([`spawn_sampler`]) owns the `sysinfo::System`
//!   and does every refresh inside [`tokio::task::spawn_blocking`], on its own
//!   [`PROCESS_METRICS_INTERVAL`] cadence, off both the lifecycle task and the
//!   async worker pool;
//! - the lifecycle loop holds only [`ProcessMetricsPublisher`], which receives
//!   an ALREADY-computed sample over a [`watch`] channel and publishes it -
//!   `Publisher::try_publish` just encodes + enqueues (non-blocking, D43e), so
//!   nothing a sample costs can delay lifecycle work.
//!
//! A single sampler task awaiting each `spawn_blocking` before the next tick
//! means two refreshes can never overlap. Each sample carries the measured
//! elapsed wall time between completed refreshes, so a slow refresh or loaded
//! host cannot make the CPU delta's reported window misleading.

use std::time::{Duration, Instant};

use phoxal_api::v2 as api;
use phoxal_bus::{Bus, LogicalTime, OwnerCap, Publisher};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};
use tokio::sync::watch;
use tokio::task::JoinHandle;

pub(crate) type ProcessMetricsSample = api::telemetry::Process;

/// Runner process-metrics sampling cadence.
///
/// Diagnostic telemetry cadence; this is never a liveness signal.
pub(crate) const PROCESS_METRICS_INTERVAL: Duration = Duration::from_secs(3);

/// The publish half, held on the lifecycle task.
///
/// It never touches `sysinfo`: [`Self::publish`] only builds the envelope and
/// enqueues it (non-blocking, D43e), so no sample cost is ever on the
/// lifecycle/step path. The sampling half is the background [`spawn_sampler`]
/// task.
pub(crate) struct ProcessMetricsPublisher {
    publisher: Option<Publisher<api::telemetry::Process>>,
}

impl ProcessMetricsPublisher {
    pub(crate) fn attach(bus: Bus) -> Self {
        let topic = api::topic::internal::new(OwnerCap::__mint())
            .telemetry()
            .process();
        let publisher = Publisher::new(bus, &topic)
            .map_err(|error| {
                tracing::warn!(
                    target: "phoxal.runtime",
                    error = %error,
                    "process telemetry publisher could not be created"
                );
                error
            })
            .ok();

        Self { publisher }
    }

    #[cfg(test)]
    fn disabled() -> Self {
        Self { publisher: None }
    }

    /// Publish an already-computed sample. Cheap: encodes + enqueues only.
    pub(crate) fn publish(&self, at: LogicalTime, body: ProcessMetricsSample) {
        let Some(publisher) = &self.publisher else {
            return;
        };

        if let Err(error) = publisher.try_publish(at, body) {
            tracing::warn!(
                target: "phoxal.runtime",
                error = %error,
                "process telemetry publish failed"
            );
        }
    }
}

/// Spawn the background process sampler.
///
/// Returns the [`watch`] receiver the lifecycle loop reads already-computed
/// samples from (latest wins - this is a `state` contract), plus the task
/// handle the runner aborts at shutdown alongside its other background tasks.
/// All `sysinfo` work happens on this task via [`tokio::task::spawn_blocking`],
/// NEVER on the lifecycle task.
pub(crate) fn spawn_sampler() -> (
    watch::Receiver<Option<ProcessMetricsSample>>,
    JoinHandle<()>,
) {
    let (tx, rx) = watch::channel(None);
    let handle = tokio::spawn(run_sampler(tx));
    (rx, handle)
}

async fn run_sampler(tx: watch::Sender<Option<api::telemetry::Process>>) {
    let pid = Pid::from_u32(std::process::id());
    // `System::new_with_specifics` performs the requested refresh immediately,
    // so it belongs on the blocking pool too. This matters for callers that use
    // `phoxal::tokio::run` from a single-worker runtime: no sysinfo work may
    // briefly occupy the lifecycle executor before the first sample.
    let (mut system, mut previous_refresh) = match tokio::task::spawn_blocking(|| {
        let system = System::new_with_specifics(
            RefreshKind::nothing()
                .with_processes(ProcessRefreshKind::nothing().with_cpu().with_memory()),
        );
        (system, Instant::now())
    })
    .await
    {
        Ok(system) => system,
        Err(error) => {
            tracing::warn!(
                target: "phoxal.runtime",
                error = %error,
                "process telemetry sampler initialization failed; stopping self-sampling"
            );
            return;
        }
    };
    let mut interval = tokio::time::interval(PROCESS_METRICS_INTERVAL);
    // Delay (not Burst): if a refresh ever runs long, do not fire back-to-back
    // catch-up ticks - keep refreshes ~one interval apart so the cpu delta
    // window stays well-defined.
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Tokio intervals yield once immediately. Consume that bootstrap tick so
    // the initial sysinfo refresh has a real measurement window before the
    // first published CPU delta.
    interval.tick().await;

    loop {
        interval.tick().await;

        // Move the `System` into a blocking thread for the refresh (the sysinfo
        // syscalls never touch an async worker) and get it back with the
        // computed sample. Awaiting the join before the next tick guarantees
        // two refreshes can never overlap.
        let (returned, sample, refreshed_at) = match tokio::task::spawn_blocking(move || {
            let (sample, refreshed_at) = sample_process(&mut system, pid, previous_refresh);
            (system, sample, refreshed_at)
        })
        .await
        {
            Ok(pair) => pair,
            Err(error) => {
                tracing::warn!(
                    target: "phoxal.runtime",
                    error = %error,
                    "process telemetry sampler task failed; stopping self-sampling"
                );
                return;
            }
        };
        system = returned;
        previous_refresh = refreshed_at;

        if let Some(body) = sample {
            // `watch` keeps only the latest sample; a dropped receiver means the
            // runner is shutting down, so stop.
            if tx.send(Some(body)).is_err() {
                return;
            }
        }
    }
}

/// Refresh only THIS process and build a `Process` sample. Runs inside
/// [`tokio::task::spawn_blocking`], off the async runtime.
///
/// `sysinfo` computes `cpu_usage()` as a delta against the PREVIOUS refresh, so
/// the very first sample reads `0.0` - expected, not a bug: it settles to a
/// real value from the second refresh on, one [`PROCESS_METRICS_INTERVAL`]
/// later.
fn sample_process(
    system: &mut System,
    pid: Pid,
    previous_refresh: Instant,
) -> (Option<api::telemetry::Process>, Instant) {
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        false,
        ProcessRefreshKind::nothing().with_cpu().with_memory(),
    );
    let refreshed_at = Instant::now();

    let sample = system.process(pid).map(|process| api::telemetry::Process {
        cpu_pct: process.cpu_usage(),
        rss_bytes: process.memory(),
        window_ns: u64::try_from(
            refreshed_at
                .saturating_duration_since(previous_refresh)
                .as_nanos(),
        )
        .unwrap_or(u64::MAX),
    });

    (sample, refreshed_at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_process_metrics_publisher_is_a_noop() {
        let metrics = ProcessMetricsPublisher::disabled();
        metrics.publish(
            LogicalTime::new(0, 1),
            api::telemetry::Process {
                cpu_pct: 0.0,
                rss_bytes: 0,
                window_ns: 0,
            },
        );
        assert!(metrics.publisher.is_none());
    }

    #[serial_test::serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn process_metrics_publish_on_the_declared_topic() {
        let bus = Bus::open(phoxal_bus::BusConfig::in_process("dev", "metrics-test"))
            .await
            .expect("open bus");
        let latest = phoxal_bus::Latest::<api::telemetry::Process>::new(
            &bus,
            &api::topic::new().telemetry().process(),
        )
        .await
        .expect("subscribe process telemetry");
        let metrics = ProcessMetricsPublisher::attach(bus.clone());
        metrics.publish(
            LogicalTime::new(0, 1),
            api::telemetry::Process {
                cpu_pct: 12.5,
                rss_bytes: 42,
                window_ns: 3_000_000_000,
            },
        );

        let mut observed = None;
        for _ in 0..50 {
            if let Some(sample) = latest.latest() {
                observed = Some(sample);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let observed = observed.expect("process telemetry sample");
        assert_eq!(observed.rss_bytes, 42);
        assert_eq!(observed.window_ns, 3_000_000_000);
        bus.close().await.expect("close bus");
    }

    #[test]
    fn sample_process_reports_this_process_over_the_measured_window() {
        let mut system = System::new_with_specifics(
            RefreshKind::nothing()
                .with_processes(ProcessRefreshKind::nothing().with_cpu().with_memory()),
        );
        let pid = Pid::from_u32(std::process::id());
        let previous_refresh = Instant::now()
            .checked_sub(Duration::from_secs(2))
            .expect("two seconds before now");

        let (sample, _) = sample_process(&mut system, pid, previous_refresh);
        let sample =
            sample.expect("the current process is always present in its own sysinfo refresh");

        // The delta window is measured rather than copied from the nominal
        // cadence. Allow refresh overhead while proving the authored 3 s
        // interval was not substituted.
        assert!(sample.window_ns >= 2_000_000_000);
        assert!(sample.window_ns < 3_000_000_000);
        // This process holds a non-trivial resident set; a zero here would mean
        // the refresh kind never populated memory.
        assert!(sample.rss_bytes > 0);
    }
}
