//! Time-windowed, capacity-bounded ring buffer used to hold each dynamic
//! frame's recent transform samples for nearest-timestamp lookup.

use std::collections::VecDeque;

#[derive(Clone, Debug)]
pub(crate) struct RingBuffer<T> {
    window_ns: u64,
    max_entries: usize,
    entries: VecDeque<(u64, T)>,
}

impl<T> RingBuffer<T> {
    pub(crate) fn new(window_ns: u64, max_entries: usize) -> Self {
        Self {
            window_ns,
            max_entries,
            entries: VecDeque::with_capacity(max_entries.min(256)),
        }
    }

    pub(crate) fn push(&mut self, timestamp_ns: u64, value: T) {
        if self.max_entries == 0 {
            return;
        }
        while self.entries.front().is_some_and(|(entry_timestamp_ns, _)| {
            entry_timestamp_ns.saturating_add(self.window_ns) < timestamp_ns
        }) {
            self.entries.pop_front();
        }
        while self.entries.len() >= self.max_entries {
            self.entries.pop_front();
        }
        self.entries.push_back((timestamp_ns, value));
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(crate) fn entries(&self) -> &VecDeque<(u64, T)> {
        &self.entries
    }
}

impl<T: Clone> RingBuffer<T> {
    pub(crate) fn latest(&self) -> Option<(u64, T)> {
        self.entries
            .back()
            .map(|(timestamp_ns, value)| (*timestamp_ns, value.clone()))
    }

    pub(crate) fn nearest(&self, timestamp_ns: u64) -> Option<(u64, T)> {
        let (oldest_available_ns, _) = self.entries.front()?;
        if timestamp_ns < *oldest_available_ns {
            return None;
        }
        let (newest_available_ns, _) = self.entries.back()?;
        if timestamp_ns > *newest_available_ns {
            return (timestamp_ns.saturating_sub(*newest_available_ns) <= self.window_ns)
                .then(|| self.latest())
                .flatten();
        }
        self.entries
            .iter()
            .min_by_key(|(entry_timestamp_ns, _)| entry_timestamp_ns.abs_diff(timestamp_ns))
            .map(|(entry_timestamp_ns, value)| (*entry_timestamp_ns, value.clone()))
    }
}
