//! Runner-facing portable queue-pressure accounting.
//!
//! Counters live beside the bounded buffers they describe. The participant
//! runner drains interval counters once per rollup; current depth and declared
//! quiet rows persist across windows. Rows describe the fixed set of buffers
//! declared during participant setup and remain for the process lifetime;
//! dropping an authoring handle does not dynamically unregister its row. That
//! fixed declaration invariant makes a `Drop` unregister path both misleading
//! and unnecessary; teardown discards the complete registry with the process.
//!
//! Interval counters are best-effort boundary samples, not a transactional
//! snapshot of every buffer: concurrent activity may land on either side of
//! the sequence of atomic swaps. Depth/high-water gauges remain monotonic-safe
//! for their individual buffers, but the complete multi-row rollup has no
//! global stop-the-world instant.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RuntimeDirection {
    Publish,
    Subscribe,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RuntimeBufferKind {
    Outbound,
    Latest,
    Subscriber,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RuntimeMetricKey {
    pub topic: String,
    pub direction: RuntimeDirection,
    pub buffer_kind: RuntimeBufferKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeMetricSnapshot {
    pub key: RuntimeMetricKey,
    pub count: u64,
    pub drops: u64,
    pub latest_overwrites: u64,
    pub bounded_evictions: u64,
    pub capacity: u64,
    pub current_depth: u64,
    pub high_water_depth: u64,
    pub decode_errors: u64,
    pub epoch_filtered: u64,
}

#[derive(Debug, Default)]
struct Counters {
    count: AtomicU64,
    drops: AtomicU64,
    latest_overwrites: AtomicU64,
    bounded_evictions: AtomicU64,
    capacity: AtomicU64,
    current_depth: AtomicU64,
    high_water_depth: AtomicU64,
    decode_errors: AtomicU64,
    epoch_filtered: AtomicU64,
}

#[derive(Clone, Debug)]
pub(crate) struct RuntimeMetricHandle {
    counters: Arc<Counters>,
    /// One independently declared inbound buffer's contribution to the shared
    /// identical-key depth. Clones share this gauge; distinct declarations do
    /// not. Outbound handles use the one session queue directly instead.
    local_depth: Option<Arc<AtomicU64>>,
}

impl RuntimeMetricHandle {
    pub(crate) fn record_message(&self) {
        self.counters.count.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_drop(&self) {
        self.counters.drops.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_decode_error(&self) {
        self.counters.decode_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_epoch_filtered(&self, count: u64) {
        self.counters
            .epoch_filtered
            .fetch_add(count, Ordering::Relaxed);
    }

    pub(crate) fn record_latest(&self, overwrote: bool) {
        self.record_message();
        if overwrote {
            self.counters
                .latest_overwrites
                .fetch_add(1, Ordering::Relaxed);
        }
        // Latest is one occupied slot, not a one-item backlog.
        self.set_inbound_depth(1);
    }

    pub(crate) fn record_pending_latest(&self) {
        self.record_message();
    }

    pub(crate) fn record_latest_depth(&self, occupied: bool) {
        self.set_inbound_depth(u64::from(occupied));
    }

    pub(crate) fn record_subscriber(&self, evicted: bool, current_depth: usize) {
        self.record_message();
        if evicted {
            self.counters
                .bounded_evictions
                .fetch_add(1, Ordering::Relaxed);
            self.record_drop();
        }
        self.set_inbound_depth(u64::try_from(current_depth).unwrap_or(u64::MAX));
    }

    pub(crate) fn record_pending_subscriber(&self) {
        self.record_message();
    }

    pub(crate) fn record_subscriber_pop(&self, current_depth: usize) {
        self.set_inbound_depth(u64::try_from(current_depth).unwrap_or(u64::MAX));
    }

    pub(crate) fn enqueue_started(&self) {
        let current = self
            .counters
            .current_depth
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        update_max(&self.counters.high_water_depth, current);
    }

    pub(crate) fn enqueue_finished(&self) {
        let _ = self.counters.current_depth.fetch_update(
            Ordering::Relaxed,
            Ordering::Relaxed,
            |value| Some(value.saturating_sub(1)),
        );
    }

    fn set_inbound_depth(&self, depth: u64) {
        let Some(local) = self.local_depth.as_ref() else {
            debug_assert!(
                false,
                "outbound runtime metric cannot update an inbound depth gauge"
            );
            return;
        };
        let previous = local.swap(depth, Ordering::Relaxed);
        let current = if depth >= previous {
            self.counters
                .current_depth
                .fetch_add(depth - previous, Ordering::Relaxed)
                .saturating_add(depth - previous)
        } else {
            self.counters
                .current_depth
                .fetch_sub(previous - depth, Ordering::Relaxed)
                .saturating_sub(previous - depth)
        };
        update_max(&self.counters.high_water_depth, current);
    }
}

#[derive(Debug, Default)]
pub(crate) struct RuntimeMetrics {
    rows: Mutex<BTreeMap<RuntimeMetricKey, Arc<Counters>>>,
}

impl RuntimeMetrics {
    pub(crate) fn register_outbound(&self, topic: &str, capacity: usize) -> RuntimeMetricHandle {
        // Every outbound topic is a per-row view of the same process queue.
        // Capacity is therefore repeated, never added across publishers/rows.
        // The queue's separate byte bound is intentionally not a v1 metric.
        self.register(
            RuntimeMetricKey {
                topic: topic.to_string(),
                direction: RuntimeDirection::Publish,
                buffer_kind: RuntimeBufferKind::Outbound,
            },
            capacity,
            false,
        )
    }

    pub(crate) fn register_latest(&self, topic: &str) -> RuntimeMetricHandle {
        self.register(
            RuntimeMetricKey {
                topic: topic.to_string(),
                direction: RuntimeDirection::Subscribe,
                buffer_kind: RuntimeBufferKind::Latest,
            },
            1,
            true,
        )
    }

    pub(crate) fn register_subscriber(&self, topic: &str, capacity: usize) -> RuntimeMetricHandle {
        self.register(
            RuntimeMetricKey {
                topic: topic.to_string(),
                direction: RuntimeDirection::Subscribe,
                buffer_kind: RuntimeBufferKind::Subscriber,
            },
            capacity,
            true,
        )
    }

    fn register(
        &self,
        key: RuntimeMetricKey,
        capacity: usize,
        additive_capacity: bool,
    ) -> RuntimeMetricHandle {
        let mut rows = self.rows.lock().expect("runtime metrics mutex poisoned");
        let counters = rows.entry(key).or_default();
        let capacity = u64::try_from(capacity).unwrap_or(u64::MAX);
        if additive_capacity {
            counters.capacity.fetch_add(capacity, Ordering::Relaxed);
        } else {
            update_max(&counters.capacity, capacity);
        }
        RuntimeMetricHandle {
            counters: Arc::clone(counters),
            local_depth: additive_capacity.then(|| Arc::new(AtomicU64::new(0))),
        }
    }

    pub(crate) fn take(&self) -> Vec<RuntimeMetricSnapshot> {
        let rows = self.rows.lock().expect("runtime metrics mutex poisoned");
        rows.iter()
            .map(|(key, counters)| {
                let current_depth = counters.current_depth.load(Ordering::Relaxed);
                RuntimeMetricSnapshot {
                    key: key.clone(),
                    count: counters.count.swap(0, Ordering::Relaxed),
                    drops: counters.drops.swap(0, Ordering::Relaxed),
                    latest_overwrites: counters.latest_overwrites.swap(0, Ordering::Relaxed),
                    bounded_evictions: counters.bounded_evictions.swap(0, Ordering::Relaxed),
                    capacity: counters.capacity.load(Ordering::Relaxed),
                    current_depth,
                    high_water_depth: counters
                        .high_water_depth
                        .swap(current_depth, Ordering::Relaxed),
                    decode_errors: counters.decode_errors.swap(0, Ordering::Relaxed),
                    epoch_filtered: counters.epoch_filtered.swap(0, Ordering::Relaxed),
                }
            })
            .collect()
    }
}

fn update_max(target: &AtomicU64, value: u64) {
    let _ = target.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        (value > current).then_some(value)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_keys_aggregate_and_quiet_rows_persist() {
        let metrics = RuntimeMetrics::default();
        let first = metrics.register_subscriber("v0.1/drive/state", 4);
        let second = metrics.register_subscriber("v0.1/drive/state", 8);
        first.record_subscriber(false, 1);
        second.record_subscriber(true, 8);

        let rows = metrics.take();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].count, 2);
        assert_eq!(rows[0].bounded_evictions, 1);
        assert_eq!(rows[0].drops, 1);
        assert_eq!(rows[0].capacity, 12);
        assert_eq!(rows[0].current_depth, 9);
        assert_eq!(rows[0].high_water_depth, 9);

        let quiet = metrics.take();
        assert_eq!(quiet.len(), 1);
        assert_eq!(quiet[0].count, 0);
        assert_eq!(quiet[0].capacity, 12);
        assert_eq!(quiet[0].current_depth, 9);
    }

    #[test]
    fn outbound_capacity_is_a_non_additive_view_of_one_shared_queue() {
        let metrics = RuntimeMetrics::default();
        let first = metrics.register_outbound("v0.1/drive/target", 1_024);
        let second = metrics.register_outbound("v0.1/drive/target", 1_024);
        let _other = metrics.register_outbound("v0.1/motion/target", 1_024);
        first.enqueue_started();
        second.enqueue_started();

        let rows = metrics.take();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.capacity == 1_024));
        assert_eq!(rows[0].current_depth, 2);
    }

    #[test]
    fn fixed_setup_rows_persist_after_the_declaring_handle_is_dropped() {
        let metrics = RuntimeMetrics::default();
        {
            let _declared = metrics.register_latest("v0.1/drive/state");
        }
        let rows = metrics.take();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].capacity, 1);
        assert_eq!(rows[0].current_depth, 0);
        assert_eq!(metrics.take().len(), 1);
    }
}
