//! Semantic outbound scheduling.
//!
//! The bus owns one Zenoh drain, but admission is deliberately split by
//! [`DeliveryFamily`]. State and setpoints keep only the newest unsent value
//! for each concrete topic. Samples retain a bounded ordered window and evict
//! the oldest item with evidence. Streams retain order and refuse admission
//! when their bounded lane cannot accept another chunk.

use std::collections::{BTreeMap, VecDeque};

use crate::contract::DeliveryFamily;
use crate::error::OutboundBound;
use crate::runtime_metrics::RuntimeMetricHandle;

/// One accepted item waiting for the session-owned Zenoh drain.
pub(crate) struct Outbound {
    pub(crate) key: String,
    pub(crate) encoding: String,
    pub(crate) attachment: Vec<u8>,
    pub(crate) payload: Vec<u8>,
    pub(crate) bytes: usize,
    pub(crate) metric: RuntimeMetricHandle,
    pub(crate) family: DeliveryFamily,
}

impl Outbound {
    pub(crate) fn new(
        key: String,
        encoding: String,
        attachment: Vec<u8>,
        payload: Vec<u8>,
        metric: RuntimeMetricHandle,
        family: DeliveryFamily,
    ) -> Option<Self> {
        let bytes = key
            .len()
            .checked_add(encoding.len())?
            .checked_add(attachment.len())?
            .checked_add(payload.len())?;
        Some(Self {
            key,
            encoding,
            attachment,
            payload,
            bytes,
            metric,
            family,
        })
    }
}

/// The result of a successful semantic admission.
pub(crate) struct Admission {
    /// An older state/setpoint item replaced by the accepted item.
    pub(crate) replaced: Option<Outbound>,
    /// Older sample items evicted to make room for the accepted item.
    pub(crate) evicted: Vec<Outbound>,
}

/// One scheduler with semantically distinct storage lanes.
pub(crate) struct OutboundScheduler {
    state: BTreeMap<String, Outbound>,
    state_order: VecDeque<String>,
    setpoint: BTreeMap<String, Outbound>,
    setpoint_order: VecDeque<String>,
    sample: VecDeque<Outbound>,
    stream: VecDeque<Outbound>,
    queued_bytes: usize,
    lane_capacity: usize,
    max_bytes: usize,
    next_lane: usize,
    /// The next stream position for each concrete topic. The producer is
    /// implicit because one scheduler belongs to one producer/session.
    stream_positions: BTreeMap<String, u64>,
}

impl OutboundScheduler {
    pub(crate) fn new(lane_capacity: usize, max_bytes: usize) -> Self {
        Self {
            state: BTreeMap::new(),
            state_order: VecDeque::new(),
            setpoint: BTreeMap::new(),
            setpoint_order: VecDeque::new(),
            sample: VecDeque::with_capacity(lane_capacity),
            stream: VecDeque::with_capacity(lane_capacity),
            queued_bytes: 0,
            lane_capacity,
            max_bytes,
            next_lane: 0,
            stream_positions: BTreeMap::new(),
        }
    }

    #[cfg(test)]
    fn queued_bytes(&self) -> usize {
        self.queued_bytes
    }

    #[cfg(test)]
    fn queued_items(&self) -> usize {
        self.state.len() + self.setpoint.len() + self.sample.len() + self.stream.len()
    }

    #[cfg(test)]
    pub(crate) fn stream_attachments(&self, key: &str) -> Vec<Vec<u8>> {
        self.stream
            .iter()
            .filter(|outbound| outbound.key == key)
            .map(|outbound| outbound.attachment.clone())
            .collect()
    }

    /// Return the next position without mutating it. The caller commits this
    /// only after the corresponding stream item has been admitted.
    pub(crate) fn next_stream_position(&self, key: &str) -> u64 {
        self.stream_positions.get(key).copied().unwrap_or(0)
    }

    /// Commit one stream position after successful admission. Position
    /// `u64::MAX` is valid as the final position; subsequent calls continue to
    /// return it and the session's ordinary sequence exhaustion remains the
    /// authoritative way to stop a publisher that can no longer progress.
    pub(crate) fn commit_stream_position(&mut self, key: &str) {
        let position = self.stream_positions.entry(key.to_string()).or_default();
        *position = position.saturating_add(1);
    }

    /// Admit an item according to its delivery family.
    pub(crate) fn admit(
        &mut self,
        outbound: Outbound,
    ) -> std::result::Result<Admission, OutboundBound> {
        debug_assert!(matches!(
            outbound.family,
            DeliveryFamily::State
                | DeliveryFamily::Setpoint
                | DeliveryFamily::Sample
                | DeliveryFamily::Stream
        ));

        match outbound.family {
            DeliveryFamily::State => self.admit_coalesced(outbound, true),
            DeliveryFamily::Setpoint => self.admit_coalesced(outbound, false),
            DeliveryFamily::Sample => self.admit_sample(outbound),
            DeliveryFamily::Stream => self.admit_stream(outbound),
            DeliveryFamily::Query => {
                // Query traffic does not use this scheduler. Keeping this
                // branch defensive makes an accidental future call fail as a
                // bounded admission rather than silently choosing a lane.
                Err(OutboundBound::Sample)
            }
        }
    }

    fn admit_coalesced(
        &mut self,
        outbound: Outbound,
        state: bool,
    ) -> std::result::Result<Admission, OutboundBound> {
        let (map, order) = if state {
            (&mut self.state, &mut self.state_order)
        } else {
            (&mut self.setpoint, &mut self.setpoint_order)
        };
        let old_bytes = map.get(&outbound.key).map_or(0, |old| old.bytes);
        let Some(bytes_without_old) = self.queued_bytes.checked_sub(old_bytes) else {
            return Err(OutboundBound::Byte);
        };
        let Some(next_bytes) = bytes_without_old.checked_add(outbound.bytes) else {
            return Err(OutboundBound::Byte);
        };
        if next_bytes > self.max_bytes {
            return Err(OutboundBound::Byte);
        }

        let key = outbound.key.clone();
        if !map.contains_key(&key) {
            order.push_back(key.clone());
        }
        let replaced = map.insert(key, outbound);
        self.queued_bytes = next_bytes;
        Ok(Admission {
            replaced,
            evicted: Vec::new(),
        })
    }

    fn admit_sample(
        &mut self,
        outbound: Outbound,
    ) -> std::result::Result<Admission, OutboundBound> {
        if outbound.bytes > self.max_bytes {
            return Err(OutboundBound::Byte);
        }

        // Plan evictions before mutating the queue. If a global byte cap is
        // occupied by state/stream work and this sample cannot fit even after
        // evicting every sample, the failed admission must preserve the sample
        // lane exactly as it was.
        let mut evict_count = 0_usize;
        let mut evicted_bytes = 0_usize;
        loop {
            let remaining = self.sample.len().saturating_sub(evict_count);
            let count_fits = remaining < self.lane_capacity;
            let bytes_fits = self
                .queued_bytes
                .saturating_sub(evicted_bytes)
                .checked_add(outbound.bytes)
                .is_some_and(|bytes| bytes <= self.max_bytes);
            if count_fits && bytes_fits {
                break;
            }
            if evict_count == self.sample.len() {
                let bound = if !count_fits {
                    OutboundBound::Sample
                } else {
                    OutboundBound::Byte
                };
                return Err(bound);
            }
            evicted_bytes = evicted_bytes.saturating_add(self.sample[evict_count].bytes);
            evict_count += 1;
        }

        let mut evicted = Vec::with_capacity(evict_count);
        for _ in 0..evict_count {
            if let Some(old) = self.sample.pop_front() {
                self.queued_bytes = self.queued_bytes.saturating_sub(old.bytes);
                evicted.push(old);
            }
        }
        self.queued_bytes = self.queued_bytes.saturating_add(outbound.bytes);
        self.sample.push_back(outbound);
        Ok(Admission {
            replaced: None,
            evicted,
        })
    }

    fn admit_stream(
        &mut self,
        outbound: Outbound,
    ) -> std::result::Result<Admission, OutboundBound> {
        if self.stream.len() >= self.lane_capacity {
            return Err(OutboundBound::Sample);
        }
        let Some(next_bytes) = self.queued_bytes.checked_add(outbound.bytes) else {
            return Err(OutboundBound::Byte);
        };
        if next_bytes > self.max_bytes {
            return Err(OutboundBound::Byte);
        }
        self.queued_bytes = next_bytes;
        self.stream.push_back(outbound);
        Ok(Admission {
            replaced: None,
            evicted: Vec::new(),
        })
    }

    /// Pop one item using a fair lane rotation. FIFO order is preserved inside
    /// both ordered lanes; map lanes are newest-per-topic storage, so their
    /// cross-topic order is intentionally unspecified.
    pub(crate) fn pop_next(&mut self) -> Option<Outbound> {
        for offset in 0..4 {
            let lane = (self.next_lane + offset) % 4;
            let item = match lane {
                0 => pop_map(&mut self.state, &mut self.state_order),
                1 => pop_map(&mut self.setpoint, &mut self.setpoint_order),
                2 => self.sample.pop_front(),
                3 => self.stream.pop_front(),
                _ => None,
            };
            if let Some(item) = item {
                self.queued_bytes = self.queued_bytes.saturating_sub(item.bytes);
                self.next_lane = (lane + 1) % 4;
                return Some(item);
            }
        }
        None
    }
}

fn pop_map(map: &mut BTreeMap<String, Outbound>, order: &mut VecDeque<String>) -> Option<Outbound> {
    while let Some(key) = order.pop_front() {
        if let Some(outbound) = map.remove(&key) {
            return Some(outbound);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::contract::DeliveryFamily;
    use crate::runtime_metrics::RuntimeMetrics;

    fn outbound(metrics: &RuntimeMetrics, family: DeliveryFamily, key: &str, body: u8) -> Outbound {
        let metric = metrics.register_outbound(key, 2);
        Outbound::new(
            key.to_string(),
            "encoding".to_string(),
            Vec::new(),
            vec![body],
            metric,
            family,
        )
        .expect("test outbound size")
    }

    fn body(item: &Outbound) -> u8 {
        item.payload[0]
    }

    #[test]
    fn state_replaces_only_the_unsent_value_per_topic() {
        let metrics = RuntimeMetrics::default();
        let mut scheduler = OutboundScheduler::new(2, 1024);

        let first = outbound(&metrics, DeliveryFamily::State, "state", 1);
        scheduler.admit(first).expect("first state admission");
        let second = outbound(&metrics, DeliveryFamily::State, "state", 2);
        let result = scheduler.admit(second).expect("replacement admission");
        assert_eq!(result.replaced.map(|old| body(&old)), Some(1));
        assert_eq!(scheduler.queued_items(), 1);
        assert_eq!(body(&scheduler.pop_next().expect("newest state")), 2);
    }

    #[test]
    fn setpoint_replaces_an_older_actionable_intent_before_transport() {
        let metrics = RuntimeMetrics::default();
        let mut scheduler = OutboundScheduler::new(2, 1024);

        scheduler
            .admit(outbound(&metrics, DeliveryFamily::Setpoint, "target", 1))
            .expect("first setpoint admission");
        let result = scheduler
            .admit(outbound(&metrics, DeliveryFamily::Setpoint, "target", 2))
            .expect("newer setpoint admission");
        assert_eq!(result.replaced.map(|old| body(&old)), Some(1));
        assert_eq!(body(&scheduler.pop_next().expect("newest setpoint")), 2);
    }

    #[test]
    fn continuously_refreshed_coalesced_topic_cannot_starve_its_sibling() {
        let metrics = RuntimeMetrics::default();
        let mut scheduler = OutboundScheduler::new(4, 1024);
        scheduler
            .admit(outbound(&metrics, DeliveryFamily::State, "a", 1))
            .unwrap();
        scheduler
            .admit(outbound(&metrics, DeliveryFamily::State, "z", 2))
            .unwrap();
        for value in 3..20 {
            scheduler
                .admit(outbound(&metrics, DeliveryFamily::State, "a", value))
                .unwrap();
        }

        let first = scheduler.pop_next().expect("first coalesced topic");
        assert_eq!(first.key, "a");
        scheduler
            .admit(outbound(&metrics, DeliveryFamily::State, "a", 20))
            .unwrap();
        assert_eq!(
            scheduler.pop_next().expect("waiting sibling").key,
            "z",
            "re-admitting the first topic must enqueue behind the sibling"
        );
        assert_eq!(
            scheduler.pop_next().expect("refreshed first topic").key,
            "a"
        );
    }

    #[test]
    fn sample_admission_evicts_oldest_and_preserves_order_of_survivors() {
        let metrics = RuntimeMetrics::default();
        let mut scheduler = OutboundScheduler::new(2, 1024);

        scheduler
            .admit(outbound(&metrics, DeliveryFamily::Sample, "sample", 1))
            .unwrap();
        scheduler
            .admit(outbound(&metrics, DeliveryFamily::Sample, "sample", 2))
            .unwrap();
        let result = scheduler
            .admit(outbound(&metrics, DeliveryFamily::Sample, "sample", 3))
            .unwrap();
        assert_eq!(result.evicted.len(), 1);
        assert_eq!(body(&result.evicted[0]), 1);
        assert_eq!(body(&scheduler.pop_next().unwrap()), 2);
        assert_eq!(body(&scheduler.pop_next().unwrap()), 3);
    }

    #[test]
    fn stream_refuses_without_eviction_or_position_commit() {
        let metrics = RuntimeMetrics::default();
        let mut scheduler = OutboundScheduler::new(2, 1024);

        scheduler
            .admit(outbound(&metrics, DeliveryFamily::Stream, "stream", 1))
            .unwrap();
        scheduler.commit_stream_position("stream");
        scheduler
            .admit(outbound(&metrics, DeliveryFamily::Stream, "stream", 2))
            .unwrap();
        scheduler.commit_stream_position("stream");

        let before = scheduler.next_stream_position("stream");
        let result = scheduler.admit(outbound(&metrics, DeliveryFamily::Stream, "stream", 3));
        assert!(matches!(result, Err(OutboundBound::Sample)));
        assert_eq!(scheduler.next_stream_position("stream"), before);
        assert_eq!(body(&scheduler.pop_next().unwrap()), 1);
        assert_eq!(body(&scheduler.pop_next().unwrap()), 2);
    }

    #[test]
    fn stream_positions_are_independent_per_concrete_topic() {
        let metrics = RuntimeMetrics::default();
        let mut scheduler = OutboundScheduler::new(4, 1024);
        for (key, expected) in [("stream/a", 0), ("stream/a", 1), ("stream/b", 0)] {
            assert_eq!(scheduler.next_stream_position(key), expected);
            scheduler
                .admit(outbound(
                    &metrics,
                    DeliveryFamily::Stream,
                    key,
                    expected as u8,
                ))
                .unwrap();
            scheduler.commit_stream_position(key);
        }
    }

    #[test]
    fn sample_byte_pressure_evicts_oldest_before_refusing_newest() {
        let metrics = RuntimeMetrics::default();
        let mut scheduler = OutboundScheduler::new(4, 24);
        let first = outbound(&metrics, DeliveryFamily::Sample, "sample", 1);
        let size = first.bytes;
        scheduler.admit(first).unwrap();
        scheduler
            .admit(outbound(&metrics, DeliveryFamily::Sample, "sample", 2))
            .unwrap();
        let result = scheduler
            .admit(outbound(&metrics, DeliveryFamily::Sample, "sample", 3))
            .unwrap();
        assert_eq!(result.evicted.len(), 1);
        assert_eq!(scheduler.queued_bytes(), size);
        assert_eq!(body(&scheduler.pop_next().unwrap()), 3);
    }

    #[test]
    fn lane_drain_rotates_without_reordering_ordered_lanes() {
        let metrics = RuntimeMetrics::default();
        let mut scheduler = OutboundScheduler::new(4, 1024);
        scheduler
            .admit(outbound(&metrics, DeliveryFamily::Sample, "sample", 1))
            .unwrap();
        scheduler
            .admit(outbound(&metrics, DeliveryFamily::Sample, "sample", 2))
            .unwrap();
        scheduler
            .admit(outbound(&metrics, DeliveryFamily::Stream, "stream", 9))
            .unwrap();
        assert_eq!(body(&scheduler.pop_next().unwrap()), 1);
        assert_eq!(body(&scheduler.pop_next().unwrap()), 9);
        assert_eq!(body(&scheduler.pop_next().unwrap()), 2);
    }
}
