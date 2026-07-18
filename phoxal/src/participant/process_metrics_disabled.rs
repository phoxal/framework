//! Stable runner fallback when preview-v2 telemetry is not enabled.

use phoxal_bus::{Bus, LogicalTime};
use tokio::sync::watch;
use tokio::task::JoinHandle;

#[derive(Clone)]
pub(crate) struct ProcessMetricsSample;

pub(crate) struct ProcessMetricsPublisher;

impl ProcessMetricsPublisher {
    pub(crate) fn attach(_bus: Bus) -> Self {
        Self
    }

    pub(crate) fn publish(&self, _at: LogicalTime, _body: ProcessMetricsSample) {}
}

pub(crate) fn spawn_sampler() -> (
    watch::Receiver<Option<ProcessMetricsSample>>,
    JoinHandle<()>,
) {
    let (tx, rx) = watch::channel(None);
    let task = tokio::spawn(async move {
        // Keep the channel open for the runner's lifetime. A closed channel
        // means the sampler ended unexpectedly; the stable no-telemetry path
        // should instead remain quietly pending until this task is aborted.
        let _tx = tx;
        std::future::pending::<()>().await;
    });
    (rx, task)
}
