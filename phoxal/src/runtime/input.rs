use std::any::Any;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};

/// How a single input source buffers messages between logical steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputPolicy {
    /// Keep every message since the previous step, in arrival order.
    All,
    /// Keep only the most recent message; older unconsumed messages are dropped.
    Latest,
    /// Keep at most `max` messages; when full, drop the oldest.
    BoundedDropOldest { max: usize },
}

impl InputPolicy {
    pub const fn all() -> Self {
        Self::All
    }

    pub const fn latest() -> Self {
        Self::Latest
    }

    pub const fn bounded_drop_oldest(max: usize) -> Self {
        Self::BoundedDropOldest { max }
    }
}

/// Per-step aggregate input accounting. `received == delivered + dropped`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimeInputStats {
    pub received: u64,
    pub delivered: u64,
    pub dropped: u64,
}

/// Inputs delivered to a runtime for one logical step, plus accounting.
pub struct RuntimeInputs<I> {
    events: Vec<I>,
    stats: RuntimeInputStats,
}

impl<I> RuntimeInputs<I> {
    pub fn stats(&self) -> RuntimeInputStats {
        self.stats
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, I> {
        self.events.iter()
    }
}

impl<I> IntoIterator for RuntimeInputs<I> {
    type Item = I;
    type IntoIter = std::vec::IntoIter<I>;

    fn into_iter(self) -> Self::IntoIter {
        self.events.into_iter()
    }
}

impl<I> Default for RuntimeInputs<I> {
    fn default() -> Self {
        Self {
            events: Vec::new(),
            stats: RuntimeInputStats::default(),
        }
    }
}

impl<I> From<Vec<I>> for RuntimeInputs<I> {
    fn from(events: Vec<I>) -> Self {
        let len = events.len() as u64;
        Self {
            events,
            stats: RuntimeInputStats {
                received: len,
                delivered: len,
                dropped: 0,
            },
        }
    }
}

pub(super) struct SourceBuffer<I> {
    policy: InputPolicy,
    queue: VecDeque<I>,
    received: u64,
    dropped: u64,
}

impl<I> SourceBuffer<I> {
    pub(super) fn new(policy: InputPolicy) -> Self {
        Self {
            policy,
            queue: VecDeque::new(),
            received: 0,
            dropped: 0,
        }
    }

    pub(super) fn push(&mut self, item: I) {
        self.received = self.received.saturating_add(1);
        match self.policy {
            InputPolicy::All => {
                self.queue.push_back(item);
            }
            InputPolicy::Latest => {
                if !self.queue.is_empty() {
                    self.dropped = self.dropped.saturating_add(self.queue.len() as u64);
                    self.queue.clear();
                }
                self.queue.push_back(item);
            }
            InputPolicy::BoundedDropOldest { max } => {
                if max == 0 {
                    self.dropped = self.dropped.saturating_add(1);
                    return;
                }
                while self.queue.len() >= max {
                    self.queue.pop_front();
                    self.dropped = self.dropped.saturating_add(1);
                }
                self.queue.push_back(item);
            }
        }
    }

    pub(super) fn drain_into(&mut self, out: &mut Vec<I>, stats: &mut RuntimeInputStats) {
        let delivered = self.queue.len() as u64;
        stats.received = stats.received.saturating_add(self.received);
        stats.delivered = stats.delivered.saturating_add(delivered);
        stats.dropped = stats.dropped.saturating_add(self.dropped);
        out.extend(self.queue.drain(..));
        self.received = 0;
        self.dropped = 0;
    }
}

pub(super) type SourceHandle<I> = Arc<Mutex<SourceBuffer<I>>>;

pub(super) trait RecordingBuffer: Send + Sync {
    fn dyn_value(&self) -> &dyn Any;
}

pub(super) struct TypedRecordingBuffer<T> {
    pub(super) values: Arc<Mutex<Vec<T>>>,
}

impl<T: Send + 'static> RecordingBuffer for TypedRecordingBuffer<T> {
    fn dyn_value(&self) -> &dyn Any {
        self
    }
}

pub(super) fn collect_step_inputs<I>(sources: &[SourceHandle<I>]) -> Result<RuntimeInputs<I>> {
    let mut events = Vec::new();
    let mut stats = RuntimeInputStats::default();
    for source in sources {
        source
            .lock()
            .map_err(|error| anyhow!("runtime input source lock poisoned: {error}"))?
            .drain_into(&mut events, &mut stats);
    }
    Ok(RuntimeInputs { events, stats })
}

pub(super) fn push_source_input<I>(source: &SourceHandle<I>, input: I) {
    match source.lock() {
        Ok(mut source) => source.push(input),
        Err(error) => {
            tracing::warn!(%error, "runtime input source lock poisoned");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{InputPolicy, RuntimeInputStats, SourceBuffer};

    #[test]
    fn all_source_policy_preserves_arrival_order_without_drops() {
        let mut source = SourceBuffer::new(InputPolicy::All);
        source.push(1);
        source.push(2);
        source.push(3);

        let mut out = Vec::new();
        let mut stats = RuntimeInputStats::default();
        source.drain_into(&mut out, &mut stats);

        assert_eq!(out, vec![1, 2, 3]);
        assert_eq!(
            stats,
            RuntimeInputStats {
                received: 3,
                delivered: 3,
                dropped: 0,
            }
        );
        assert_eq!(stats.received, stats.delivered + stats.dropped);
    }

    #[test]
    fn latest_source_policy_keeps_only_newest_item() {
        let mut source = SourceBuffer::new(InputPolicy::Latest);
        for item in 1..=5 {
            source.push(item);
        }

        let mut out = Vec::new();
        let mut stats = RuntimeInputStats::default();
        source.drain_into(&mut out, &mut stats);

        assert_eq!(out, vec![5]);
        assert_eq!(
            stats,
            RuntimeInputStats {
                received: 5,
                delivered: 1,
                dropped: 4,
            }
        );
        assert_eq!(stats.received, stats.delivered + stats.dropped);
    }

    #[test]
    fn bounded_drop_oldest_source_policy_keeps_newest_max_items() {
        let mut source = SourceBuffer::new(InputPolicy::BoundedDropOldest { max: 3 });
        for item in 1..=6 {
            source.push(item);
        }

        let mut out = Vec::new();
        let mut stats = RuntimeInputStats::default();
        source.drain_into(&mut out, &mut stats);

        assert_eq!(out, vec![4, 5, 6]);
        assert_eq!(
            stats,
            RuntimeInputStats {
                received: 6,
                delivered: 3,
                dropped: 3,
            }
        );
        assert_eq!(stats.received, stats.delivered + stats.dropped);
    }

    #[test]
    fn source_policy_counters_reset_after_drain() {
        let mut source = SourceBuffer::new(InputPolicy::BoundedDropOldest { max: 2 });
        source.push(1);
        source.push(2);
        source.push(3);

        let mut out = Vec::new();
        let mut stats = RuntimeInputStats::default();
        source.drain_into(&mut out, &mut stats);

        let mut second_out = Vec::new();
        let mut second_stats = RuntimeInputStats::default();
        source.drain_into(&mut second_out, &mut second_stats);

        assert_eq!(second_out, Vec::<i32>::new());
        assert_eq!(second_stats, RuntimeInputStats::default());
    }
}
