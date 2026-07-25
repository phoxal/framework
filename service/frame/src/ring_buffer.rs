//! Time-windowed, capacity-bounded ring buffer used to hold each dynamic
//! frame's recent transform samples for nearest-instant lookup.
//!
//! The buffer is **timeline-scoped** (#952): every entry is a
//! [`RobotInstant`], and a sample from a different world history replaces the
//! buffer's contents rather than being compared against them. Keying on a bare
//! nanosecond counter is exactly the mistake this train deletes - after a
//! simulation reset, instants from the replaced world are not "old", they are
//! incomparable.

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
    /// retained history first: the buffered instants describe a world that no
    /// longer exists.
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

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(crate) fn entries(&self) -> &VecDeque<(RobotInstant, T)> {
        &self.entries
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
