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

use crate::bus::lock::lock;

/// Which way samples move through the buffer a row describes.
///
/// This is the internal accounting vocabulary. The runner maps it onto the
/// wire-facing enum in the `runtime` contract family. The structural guard at
/// the bottom of this module fails when a variant is added here; the mapping
/// itself lives in `crate::participant::runtime_performance`, which is the one
/// place that names both enums.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RuntimeDirection {
    /// Samples this process publishes.
    Publish,
    /// Samples this process receives.
    Subscribe,
}

/// Which bounded buffer a row describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RuntimeBufferKind {
    /// The session-owned semantic outbound scheduler, viewed per topic.
    Outbound,
    /// A keep-last-1 slot.
    Latest,
    /// Delivery-family bounded receive storage.
    Subscriber,
}

/// Identifies one declared buffer. Two declarations sharing a key aggregate
/// into one row.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RuntimeMetricKey {
    /// The family-rooted topic key.
    pub topic: String,
    /// Which way samples move.
    pub direction: RuntimeDirection,
    /// Which bounded buffer.
    pub buffer_kind: RuntimeBufferKind,
}

/// One buffer's counters for one rollup window.
///
/// The interval counters (`count`, `drops`, `latest_overwrites`,
/// `bounded_evictions`, `decode_errors`, `timeline_filtered`) reset when they
/// are taken. `capacity` and the depth gauges are levels and persist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeMetricSnapshot {
    /// Which buffer this row describes.
    pub key: RuntimeMetricKey,
    /// Samples that passed through the buffer this window.
    pub count: u64,
    /// Samples lost this window, for any bounded-buffer reason.
    pub drops: u64,
    /// Keep-last-1 slots overwritten before being read.
    pub latest_overwrites: u64,
    /// Samples evicted because a bounded ring was full.
    pub bounded_evictions: u64,
    /// The buffer's declared bound.
    pub capacity: u64,
    /// Occupancy at the moment of the rollup.
    pub current_depth: u64,
    /// Peak occupancy since the previous rollup.
    pub high_water_depth: u64,
    /// Inbound samples this window whose body would not decode.
    pub decode_errors: u64,
    /// Samples set aside or discarded because they belong to another world
    /// history. Deliberately separate from `drops`: quarantine churn is not
    /// active-buffer loss.
    pub timeline_filtered: u64,
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
    timeline_filtered: AtomicU64,
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

    pub(crate) fn record_timeline_filtered(&self, count: u64) {
        self.counters
            .timeline_filtered
            .fetch_add(count, Ordering::Relaxed);
    }

    pub(crate) fn record_latest(&self, overwrote: bool) {
        self.record_message();
        if overwrote {
            self.record_latest_overwrite();
        }
        // Latest is one occupied slot, not a one-item backlog.
        self.set_inbound_depth(1);
    }

    /// A sample accepted into a foreign-timeline quarantine. It passed through
    /// the buffer, so it counts, but it changes no depth gauge: quarantine
    /// storage is separate from the active buffer this row describes.
    pub(crate) fn record_pending(&self) {
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

    pub(crate) fn record_subscriber_pop(&self, current_depth: usize) {
        self.set_inbound_depth(u64::try_from(current_depth).unwrap_or(u64::MAX));
    }

    /// A coalesced outbound state/setpoint value replaced an older unsent
    /// value. The existing `latest_overwrites` field is intentionally reused:
    /// it is the wire-facing evidence for any keep-newest slot, regardless of
    /// whether the slot is on the publish or receive side.
    pub(crate) fn record_latest_overwrite(&self) {
        self.counters
            .latest_overwrites
            .fetch_add(1, Ordering::Relaxed);
    }

    /// A bounded ordered outbound sample was evicted to admit a newer sample.
    pub(crate) fn record_bounded_eviction(&self) {
        self.counters
            .bounded_evictions
            .fetch_add(1, Ordering::Relaxed);
        self.record_drop();
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
        // Every outbound topic is a per-row view of its semantic scheduler
        // lane. Capacity is therefore repeated, never added across publisher
        // handles/rows. The scheduler's global byte bound is not a v1 row.
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
        let mut rows = lock(&self.rows);
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
        let rows = lock(&self.rows);
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
                    timeline_filtered: counters.timeline_filtered.swap(0, Ordering::Relaxed),
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
    use std::time::Duration;

    use serial_test::serial;
    use zenoh::bytes::Encoding;
    use zenoh::key_expr::OwnedKeyExpr;

    use crate::bus::abi::CodecId;
    use crate::bus::contract::EndpointDescriptor;
    use crate::bus::handle::publisher::StatePublisher;
    use crate::bus::handle::subscriber::{Latest, Subscriber};
    use crate::bus::session::BusOwner;
    use crate::bus::test_support::{Target, TargetEndpoint, metadata, participant_config, step};
    use crate::bus::topic::{Publish, Subscribe, Topic};

    /// Every variant here has to reach the wire.
    ///
    /// The `runtime` contract family declares its own serialized
    /// `RuntimeDirection` / `RuntimeBufferKind`, and the runner's rollup maps
    /// this enum onto that one. The duplication is deliberate
    /// wire-versus-internal layering, but it means a variant added here and
    /// nowhere else silently never reaches an operator.
    ///
    /// This module does not name the wire enum - the protocol tree is layered
    /// above it, not below - so the guard is structural: the matches below
    /// are exhaustive and the lists are exact, so adding a variant fails to
    /// compile here and fails the assertion, forcing whoever adds it to read
    /// this comment and extend the wire enum and the mapping too.
    #[test]
    fn every_direction_and_buffer_kind_is_accounted_for_on_the_wire() {
        const DIRECTIONS: [RuntimeDirection; 2] =
            [RuntimeDirection::Publish, RuntimeDirection::Subscribe];
        const BUFFER_KINDS: [RuntimeBufferKind; 3] = [
            RuntimeBufferKind::Outbound,
            RuntimeBufferKind::Latest,
            RuntimeBufferKind::Subscriber,
        ];

        for direction in DIRECTIONS {
            // Exhaustive on purpose: no wildcard arm may absorb a new variant.
            let name = match direction {
                RuntimeDirection::Publish => "publish",
                RuntimeDirection::Subscribe => "subscribe",
            };
            assert!(!name.is_empty());
        }
        for buffer_kind in BUFFER_KINDS {
            let name = match buffer_kind {
                RuntimeBufferKind::Outbound => "outbound",
                RuntimeBufferKind::Latest => "latest",
                RuntimeBufferKind::Subscriber => "subscriber",
            };
            assert!(!name.is_empty());
        }

        assert_eq!(
            DIRECTIONS.len(),
            2,
            "a new RuntimeDirection needs a wire variant in phoxal-protocol and an arm \
             in phoxal's runtime performance rollup"
        );
        assert_eq!(
            BUFFER_KINDS.len(),
            3,
            "a new RuntimeBufferKind needs a wire variant in phoxal-protocol and an arm \
             in phoxal's runtime performance rollup"
        );
    }

    #[test]
    fn identical_keys_aggregate_and_quiet_rows_persist() {
        let metrics = RuntimeMetrics::default();
        let first = metrics.register_subscriber("robot/drive/state", 4);
        let second = metrics.register_subscriber("robot/drive/state", 8);
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
        let first = metrics.register_outbound("robot/drive/target", 1_024);
        let second = metrics.register_outbound("robot/drive/target", 1_024);
        let _other = metrics.register_outbound("robot/motion/target", 1_024);
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
            let _declared = metrics.register_latest("robot/drive/state");
        }
        let rows = metrics.take();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].capacity, 1);
        assert_eq!(rows[0].current_depth, 0);
        assert_eq!(metrics.take().len(), 1);
    }

    /// The rollup against a live session: every declared buffer has a row from
    /// the moment it is declared, and each row counts its own traffic, its own
    /// overwrites/evictions, and its own decode errors.
    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn runtime_metrics_cover_quiet_latest_overwrite_eviction_and_decode_error_rows() {
        let (owner, bus) = BusOwner::open(participant_config("metrics")).await.unwrap();
        let pub_topic = Topic::<Publish<TargetEndpoint>>::new_static(TargetEndpoint::TOPIC);
        let sub_topic = Topic::<Subscribe<TargetEndpoint>>::new_static(TargetEndpoint::TOPIC);
        let publisher = StatePublisher::<TargetEndpoint>::new(bus.clone(), &pub_topic).unwrap();
        let latest = Latest::<TargetEndpoint>::new(&bus, &sub_topic)
            .await
            .unwrap();
        let subscriber = Subscriber::<TargetEndpoint>::new(&bus, &sub_topic)
            .await
            .unwrap();

        // Declarations are retained even before any traffic.
        let quiet = bus.take_runtime_metrics().unwrap();
        assert_eq!(quiet.len(), 3);
        assert!(quiet.iter().all(|row| row.count == 0));

        for value in [1.0_f32, 2.0, 3.0] {
            publisher
                .publish(
                    &step(1, value as u64),
                    Target {
                        linear_x_mps: value,
                        angular_z_radps: 0.0,
                    },
                )
                .unwrap();
            // State admission is intentionally coalescing. Wait for each
            // value to reach the receiver before publishing the next one so
            // this live metrics test measures three completed publications,
            // rather than racing the scheduler's newest-value slot.
            for _ in 0..50 {
                if latest
                    .latest()
                    .is_some_and(|sample| sample.linear_x_mps == value)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
        for _ in 0..50 {
            if latest
                .latest()
                .is_some_and(|sample| sample.linear_x_mps == 3.0)
                && bus
                    .health()
                    .inbound_drops
                    .load(std::sync::atomic::Ordering::Relaxed)
                    >= 2
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        // Inject a malformed body on the exact subscribed key. Both independent
        // subscriptions reject it and each exact buffer row counts its own error.
        bus.session()
            .unwrap()
            .put(
                OwnedKeyExpr::new(bus.full_key(TargetEndpoint::TOPIC)).unwrap(),
                vec![0xc1_u8],
            )
            .encoding(Encoding::from(CodecId::MessagePack.encoding_string()))
            .attachment(metadata().encode().expect("test metadata encodes"))
            .await
            .unwrap();
        for _ in 0..50 {
            if bus
                .health()
                .decode_errors
                .load(std::sync::atomic::Ordering::Relaxed)
                >= 2
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let rows = bus.take_runtime_metrics().unwrap();
        let outbound = rows
            .iter()
            .find(|row| row.key.direction == RuntimeDirection::Publish)
            .unwrap();
        assert_eq!(outbound.key.buffer_kind, RuntimeBufferKind::Outbound);
        assert_eq!(outbound.key.topic, TargetEndpoint::TOPIC);
        assert_eq!(outbound.count, 3);

        let latest_row = rows
            .iter()
            .find(|row| row.key.buffer_kind == RuntimeBufferKind::Latest)
            .unwrap();
        assert_eq!(latest_row.count, 3);
        assert_eq!(latest_row.latest_overwrites, 2);
        assert_eq!(latest_row.capacity, 1);
        assert_eq!(latest_row.current_depth, 1);
        assert_eq!(latest_row.decode_errors, 1);

        let subscriber_row = rows
            .iter()
            .find(|row| row.key.buffer_kind == RuntimeBufferKind::Subscriber)
            .unwrap();
        assert_eq!(subscriber_row.count, 3);
        assert_eq!(subscriber_row.bounded_evictions, 2);
        assert_eq!(subscriber_row.drops, 2);
        assert_eq!(subscriber_row.current_depth, 1);
        assert_eq!(subscriber_row.high_water_depth, 1);
        assert_eq!(subscriber_row.decode_errors, 1);

        drop(subscriber);
        owner.close().await;
    }
}
