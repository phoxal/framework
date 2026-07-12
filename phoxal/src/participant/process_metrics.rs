//! Runner-owned per-participant process resource self-sampling.
//!
//! Every participant that runs through this runner publishes its own
//! `y2026_9::telemetry::Process` sample (cpu%, RSS) - like presence
//! heartbeats, this is runner infrastructure, not part of the
//! participant-authored `emit-apis` contract surface. It runs on its OWN,
//! slower cadence, driven by a SEPARATE timer from
//! [`crate::participant::heartbeat::HEARTBEAT_INTERVAL`]: a `sysinfo`
//! per-process refresh must never delay the 1 s heartbeat tick or couple
//! liveness to how expensive sampling is (see `runner.rs`'s `main_loop`,
//! which drives this from its own select-loop branch/timer, never the
//! heartbeat one).

use std::time::Duration;

use phoxal_api::y2026_9 as api;
use phoxal_bus::{Bus, LogicalTime, OwnerCap, Publisher};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};

/// Runner process-metrics sampling cadence.
///
/// Slower than [`crate::participant::heartbeat::HEARTBEAT_INTERVAL`] on
/// purpose: this is diagnostic telemetry (CLI dashboards read it), never a
/// liveness signal, so it does not need heartbeat cadence and must not
/// compete with the heartbeat for main-loop time.
pub(crate) const PROCESS_METRICS_INTERVAL: Duration = Duration::from_secs(3);

pub(crate) struct ProcessMetricsPublisher {
    pid: Pid,
    system: System,
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

        Self {
            pid: Pid::from_u32(std::process::id()),
            system: System::new_with_specifics(
                RefreshKind::nothing()
                    .with_processes(ProcessRefreshKind::nothing().with_cpu().with_memory()),
            ),
            publisher,
        }
    }

    #[cfg(test)]
    fn disabled() -> Self {
        Self {
            pid: Pid::from_u32(std::process::id()),
            system: System::new(),
            publisher: None,
        }
    }

    /// Refresh this process's own cpu/memory sample and publish it.
    ///
    /// `sysinfo` computes a process's `cpu_usage()` as a delta against its
    /// PREVIOUS refresh, so the very first sample after [`Self::attach`]
    /// reads `0.0` - expected, not a bug: it settles to a real value from the
    /// second sample onward, one [`PROCESS_METRICS_INTERVAL`] later.
    pub(crate) fn sample_and_publish(&mut self, at: LogicalTime) {
        let Some(publisher) = &self.publisher else {
            return;
        };

        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[self.pid]),
            false,
            ProcessRefreshKind::nothing().with_cpu().with_memory(),
        );

        let Some(process) = self.system.process(self.pid) else {
            tracing::warn!(
                target: "phoxal.runtime",
                pid = %self.pid,
                "process telemetry: own pid missing from sysinfo refresh"
            );
            return;
        };

        let body = api::telemetry::Process {
            cpu_pct: process.cpu_usage(),
            rss_bytes: process.memory(),
            window_ns: u64::try_from(PROCESS_METRICS_INTERVAL.as_nanos()).unwrap_or(u64::MAX),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_process_metrics_publisher_is_a_noop() {
        let mut metrics = ProcessMetricsPublisher::disabled();
        metrics.sample_and_publish(LogicalTime::new(0, 1));
    }

    #[test]
    fn process_metrics_interval_is_slower_than_the_heartbeat() {
        assert!(PROCESS_METRICS_INTERVAL > crate::participant::heartbeat::HEARTBEAT_INTERVAL);
    }
}
