//! Runner-owned per-participant process resource self-sampling.
//!
//! Every participant that runs through this runner publishes its own
//! `v2::telemetry::Process` sample (cpu%, RSS) - like presence
//! heartbeats, this is runner infrastructure, not part of the
//! participant-authored `emit-apis` contract surface.
//!
//! # Why the sampling is off the lifecycle task
//!
//! The plan requires OS sampling to NEVER couple to or delay the 1 s presence
//! heartbeat. `sysinfo`'s per-process refresh is a synchronous syscall burst of
//! non-trivial, unbounded cost, so it must not run on the lifecycle/heartbeat
//! task at all. This module splits the work in two:
//!
//! - a dedicated background task ([`spawn_sampler`]) owns the `sysinfo::System`
//!   and does every refresh inside [`tokio::task::spawn_blocking`], on its own
//!   [`PROCESS_METRICS_INTERVAL`] cadence, off both the lifecycle task and the
//!   async worker pool;
//! - the lifecycle loop holds only [`ProcessMetricsPublisher`], which receives
//!   an ALREADY-computed sample over a [`watch`] channel and publishes it -
//!   `Publisher::try_publish` just encodes + enqueues (non-blocking, D43e), so
//!   nothing a sample costs can delay the heartbeat.
//!
//! A single sampler task awaiting each `spawn_blocking` before the next tick
//! means two refreshes can never overlap, and consecutive refreshes are spaced
//! by the interval, so the cpu-usage delta window stays well-defined
//! (`== PROCESS_METRICS_INTERVAL`).

use std::time::Duration;

use phoxal_api::v2 as api;
use phoxal_bus::{Bus, LogicalTime, OwnerCap, Publisher};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// Runner process-metrics sampling cadence.
///
/// Slower than [`crate::participant::heartbeat::HEARTBEAT_INTERVAL`] on
/// purpose: this is diagnostic telemetry (CLI dashboards read it), never a
/// liveness signal, so it does not need heartbeat cadence.
pub(crate) const PROCESS_METRICS_INTERVAL: Duration = Duration::from_secs(3);

/// The publish half, held on the lifecycle task.
///
/// It never touches `sysinfo`: [`Self::publish`] only builds the envelope and
/// enqueues it (non-blocking, D43e), so no sample cost is ever on the
/// heartbeat/step path. The sampling half is the background [`spawn_sampler`]
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
    pub(crate) fn publish(&self, at: LogicalTime, body: api::telemetry::Process) {
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
/// NEVER on the lifecycle/heartbeat task.
pub(crate) fn spawn_sampler() -> (
    watch::Receiver<Option<api::telemetry::Process>>,
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
    let mut system = match tokio::task::spawn_blocking(|| {
        System::new_with_specifics(
            RefreshKind::nothing()
                .with_processes(ProcessRefreshKind::nothing().with_cpu().with_memory()),
        )
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

    loop {
        interval.tick().await;

        // Move the `System` into a blocking thread for the refresh (the sysinfo
        // syscalls never touch an async worker) and get it back with the
        // computed sample. Awaiting the join before the next tick guarantees
        // two refreshes can never overlap.
        let (returned, sample) = match tokio::task::spawn_blocking(move || {
            let sample = sample_process(&mut system, pid);
            (system, sample)
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
fn sample_process(system: &mut System, pid: Pid) -> Option<api::telemetry::Process> {
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        false,
        ProcessRefreshKind::nothing().with_cpu().with_memory(),
    );

    let process = system.process(pid)?;

    Some(api::telemetry::Process {
        cpu_pct: process.cpu_usage(),
        rss_bytes: process.memory(),
        window_ns: u64::try_from(PROCESS_METRICS_INTERVAL.as_nanos()).unwrap_or(u64::MAX),
    })
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
    }

    #[test]
    fn process_metrics_interval_is_slower_than_the_heartbeat() {
        assert!(PROCESS_METRICS_INTERVAL > crate::participant::heartbeat::HEARTBEAT_INTERVAL);
    }

    #[test]
    fn sample_process_reports_this_process_over_the_interval_window() {
        let mut system = System::new_with_specifics(
            RefreshKind::nothing()
                .with_processes(ProcessRefreshKind::nothing().with_cpu().with_memory()),
        );
        let pid = Pid::from_u32(std::process::id());

        let sample = sample_process(&mut system, pid)
            .expect("the current process is always present in its own sysinfo refresh");

        // The delta window is the sampling interval, not a timestamp.
        assert_eq!(sample.window_ns, 3_000_000_000);
        // This process holds a non-trivial resident set; a zero here would mean
        // the refresh kind never populated memory.
        assert!(sample.rss_bytes > 0);
    }
}
