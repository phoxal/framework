//! Time-windowed, capacity-bounded history holding one dynamic frame's recent
//! transform samples for causal lookup.
//!
//! The buffer is **timeline-scoped**: every entry is a [`RobotInstant`], and a
//! sample from a different world history replaces the buffer's contents rather
//! than being compared against them. After a simulation reset, instants from
//! the replaced world are not "old", they are incomparable - which is why the
//! entries cannot be keyed on a bare nanosecond counter. Samples on the active
//! timeline are kept in ascending robot-time order, regardless of arrival
//! order. A late sample older than the retained window is dropped explicitly;
//! a late sample inside the window is inserted at its timestamp.

use std::collections::VecDeque;
use std::time::Duration;

use phoxal::bus::RobotInstant;

#[derive(Clone, Debug)]
pub(crate) struct RingBuffer<T> {
    window: Duration,
    max_entries: usize,
    entries: VecDeque<(RobotInstant, T)>,
}

impl<T> RingBuffer<T> {
    pub(crate) fn new(window: Duration, max_entries: usize) -> Self {
        Self {
            window,
            max_entries,
            entries: VecDeque::with_capacity(max_entries.min(256)),
        }
    }

    /// Buffer `value` at `at`, returning whether it was retained.
    ///
    /// A sample from a different timeline discards the retained history first:
    /// the buffered instants describe a world that has been replaced. On the
    /// active timeline, samples are ordered by robot time. A late sample older
    /// than the current newest sample minus `window` is rejected; one inside
    /// that retention window is inserted in order. Equal timestamps replace
    /// the prior value, so `latest` remains deterministic when a producer
    /// retries a sample.
    pub(crate) fn push(&mut self, at: RobotInstant, value: T) -> bool {
        if self.max_entries == 0 {
            return false;
        }
        if self
            .entries
            .front()
            .is_some_and(|(entry_at, _)| entry_at.timeline() != at.timeline())
        {
            self.entries.clear();
        }

        let window_ns = u64::try_from(self.window.as_nanos()).unwrap_or(u64::MAX);
        if let Some((newest, _)) = self.entries.back()
            && newest.ticks().saturating_sub(at.ticks()) > window_ns
        {
            return false;
        }

        match self
            .entries
            .binary_search_by_key(&at.ticks(), |(entry_at, _)| entry_at.ticks())
        {
            Ok(index) => self.entries[index].1 = value,
            Err(index) => self.entries.insert(index, (at, value)),
        }

        let newest = self
            .entries
            .back()
            .map(|(entry_at, _)| entry_at.ticks())
            .unwrap_or(at.ticks());
        while self
            .entries
            .front()
            .is_some_and(|(entry_at, _)| newest.saturating_sub(entry_at.ticks()) > window_ns)
        {
            self.entries.pop_front();
        }
        while self.entries.len() > self.max_entries {
            self.entries.pop_front();
        }
        self.entries.iter().any(|(entry_at, _)| *entry_at == at)
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }
}

impl<T: Clone> RingBuffer<T> {
    pub(crate) fn latest(&self) -> Option<(RobotInstant, T)> {
        self.entries.back().map(|(at, value)| (*at, value.clone()))
    }

    /// The latest buffered sample at or before `at`.
    ///
    /// This is deliberately causal: a future sample is never selected because
    /// it happens to be numerically closer. Returns `None` when `at` belongs to
    /// a different world history, is older than everything retained, or is
    /// further into the future than the retention window allows.
    pub(crate) fn at_or_before(&self, at: RobotInstant) -> Option<(RobotInstant, T)> {
        let (oldest, _) = self.entries.front()?;
        if oldest.timeline() != at.timeline() {
            return None;
        }
        if at.checked_cmp(*oldest).ok()? == std::cmp::Ordering::Less {
            return None;
        }
        let (newest, _) = self.entries.back()?;
        if at.checked_cmp(*newest).ok()? == std::cmp::Ordering::Greater {
            let window_ns = u64::try_from(self.window.as_nanos()).unwrap_or(u64::MAX);
            return (at.ticks().saturating_sub(newest.ticks()) <= window_ns)
                .then(|| self.latest())
                .flatten();
        }
        self.entries
            .iter()
            .rev()
            .find(|(entry_at, _)| entry_at.ticks() <= at.ticks())
            .map(|(entry_at, value)| (*entry_at, value.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One fixed timeline, so the buffered instants read like the tick counters
    /// these cases care about.
    fn at(ticks: u64) -> RobotInstant {
        RobotInstant::new(
            phoxal::bus::TimelineId::from_raw(1).expect("test timeline must be nonzero"),
            ticks,
        )
    }

    #[test]
    fn entries_outside_the_time_window_or_the_cap_are_evicted() {
        let mut buffer = RingBuffer::new(Duration::from_nanos(5), 3);

        for ticks in 0..10 {
            assert!(buffer.push(at(ticks), ticks));
        }

        assert_eq!(buffer.entries.len(), 3);
        assert_eq!(buffer.entries.front().unwrap().0, at(7));
        assert_eq!(buffer.entries.back().unwrap().0, at(9));
    }

    /// A replaced world discards the retained history rather than comparing
    /// against it: instants from the previous timeline are not "old", they are
    /// incomparable.
    #[test]
    fn a_sample_from_a_replaced_world_discards_the_history() {
        let replaced = phoxal::bus::TimelineId::mint();
        let mut buffer = RingBuffer::new(Duration::from_secs(1), 8);
        buffer.push(RobotInstant::new(replaced, 100), 1_u64);
        assert_eq!(buffer.entries.len(), 1);
        assert_eq!(
            buffer.at_or_before(at(100)),
            None,
            "a lookup on the current timeline must not resolve against a replaced world"
        );

        assert!(buffer.push(at(10), 2));
        assert_eq!(
            buffer.entries.len(),
            1,
            "the replaced world's samples are discarded, not aged out"
        );
        assert_eq!(buffer.at_or_before(at(10)), Some((at(10), 2)));
    }

    #[test]
    fn late_samples_inside_retention_are_inserted_by_robot_time() {
        let mut buffer = RingBuffer::new(Duration::from_nanos(10), 8);
        assert!(buffer.push(at(10), 10));
        assert!(buffer.push(at(30), 30));
        assert!(buffer.push(at(20), 20));

        let ticks = buffer
            .entries
            .iter()
            .map(|(instant, _)| instant.ticks())
            .collect::<Vec<_>>();
        assert_eq!(ticks, vec![20, 30]);
        assert_eq!(buffer.latest(), Some((at(30), 30)));
        assert_eq!(buffer.at_or_before(at(25)), Some((at(20), 20)));
    }

    #[test]
    fn late_samples_older_than_retention_are_dropped_and_never_become_latest() {
        let mut buffer = RingBuffer::new(Duration::from_nanos(10), 8);
        assert!(buffer.push(at(20), 20));
        assert!(buffer.push(at(30), 30));
        assert!(!buffer.push(at(19), 19));
        assert_eq!(buffer.latest(), Some((at(30), 30)));
        assert_eq!(buffer.at_or_before(at(25)), Some((at(20), 20)));
    }

    #[test]
    fn a_late_sample_trimmed_by_capacity_reports_that_it_was_not_retained() {
        let mut buffer = RingBuffer::new(Duration::from_nanos(10), 1);
        assert!(buffer.push(at(20), 20));
        assert!(!buffer.push(at(19), 19));
        assert_eq!(buffer.latest(), Some((at(20), 20)));
    }

    #[test]
    fn causal_lookup_never_selects_a_future_sample() {
        let mut buffer = RingBuffer::new(Duration::from_nanos(100), 8);
        assert!(buffer.push(at(100), 100));
        assert!(buffer.push(at(200), 200));
        assert_eq!(buffer.at_or_before(at(175)), Some((at(100), 100)));
        assert_eq!(buffer.at_or_before(at(200)), Some((at(200), 200)));
    }
}
