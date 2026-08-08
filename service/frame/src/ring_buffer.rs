//! Time-windowed, capacity-bounded ring buffer holding one dynamic frame's
//! recent transform samples for nearest-instant lookup.
//!
//! The buffer is **timeline-scoped**: every entry is a [`RobotInstant`], and a
//! sample from a different world history replaces the buffer's contents rather
//! than being compared against them. After a simulation reset, instants from
//! the replaced world are not "old", they are incomparable - which is why the
//! entries cannot be keyed on a bare nanosecond counter.

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

    /// Buffer `value` at `at`. A sample from a different timeline discards the
    /// retained history first: the buffered instants describe a world that has
    /// been replaced.
    pub(crate) fn push(&mut self, at: RobotInstant, value: T) {
        if self.max_entries == 0 {
            return;
        }
        if self
            .entries
            .front()
            .is_some_and(|(entry_at, _)| entry_at.timeline() != at.timeline())
        {
            self.entries.clear();
        }
        while self.entries.front().is_some_and(|(entry_at, _)| {
            at.duration_since(*entry_at)
                .is_ok_and(|age| age > self.window)
        }) {
            self.entries.pop_front();
        }
        while self.entries.len() >= self.max_entries {
            self.entries.pop_front();
        }
        self.entries.push_back((at, value));
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }
}

impl<T: Clone> RingBuffer<T> {
    pub(crate) fn latest(&self) -> Option<(RobotInstant, T)> {
        self.entries.back().map(|(at, value)| (*at, value.clone()))
    }

    /// The buffered sample nearest `at`. Returns `None` when `at` belongs to a
    /// different world history, is older than everything retained, or is
    /// further into the future than the window allows.
    pub(crate) fn nearest(&self, at: RobotInstant) -> Option<(RobotInstant, T)> {
        let (oldest, _) = self.entries.front()?;
        if oldest.timeline() != at.timeline() {
            return None;
        }
        if at.checked_cmp(*oldest).ok()? == std::cmp::Ordering::Less {
            return None;
        }
        let (newest, _) = self.entries.back()?;
        if at.checked_cmp(*newest).ok()? == std::cmp::Ordering::Greater {
            return (at.duration_since(*newest).ok()? <= self.window)
                .then(|| self.latest())
                .flatten();
        }
        self.entries
            .iter()
            .min_by_key(|(entry_at, _)| entry_at.ticks().abs_diff(at.ticks()))
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
            buffer.push(at(ticks), ticks);
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
            buffer.nearest(at(100)),
            None,
            "a lookup on the current timeline must not resolve against a replaced world"
        );

        buffer.push(at(10), 2);
        assert_eq!(
            buffer.entries.len(),
            1,
            "the replaced world's samples are discarded, not aged out"
        );
        assert_eq!(buffer.nearest(at(10)), Some((at(10), 2)));
    }
}
