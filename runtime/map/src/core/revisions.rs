use std::collections::BTreeMap;

use phoxal::api::localize::v1::{LocalizationRevision, LocalizationRevisionId};
use phoxal::api::map::v1::{MapRevisionCause, MapRevisionId};
use tracing::warn;

/// How many completed revisions to retain (current + previous N-1). 3 per BLUEPRINT default.
pub const RETAIN_COMPLETED_REVISIONS: usize = 3;
/// Map epoch is independent of localization epoch; the map process anchors itself to a starting
/// epoch and increments whenever the underlying localize epoch changes.
pub const INITIAL_MAP_EPOCH: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetainedRevision {
    pub map_revision_id: MapRevisionId,
    pub previous_map_revision_id: Option<MapRevisionId>,
    pub built_from_localize_revision: LocalizationRevisionId,
    pub cause: MapRevisionCause,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevisionLookup {
    Found(RetainedRevision),
    Stale {
        current: MapRevisionId,
    },
    Unavailable {
        latest_available: Option<MapRevisionId>,
    },
    WrongEpoch {
        current: MapRevisionId,
    },
}

// Pin mechanism deferred to P2.3.B/C; synchronous query handling prevents mid-read eviction today.
#[derive(Debug, Clone)]
pub struct RevisionStore {
    epoch: u64,
    next_sequence: u64,
    retained: BTreeMap<u64, RetainedRevision>,
    last_localize_revision: Option<LocalizationRevisionId>,
}

impl RevisionStore {
    pub fn new() -> Self {
        Self {
            epoch: INITIAL_MAP_EPOCH,
            next_sequence: 0,
            retained: BTreeMap::new(),
            last_localize_revision: None,
        }
    }

    /// Returns the current (latest retained) revision, if any.
    pub fn current(&self) -> Option<&RetainedRevision> {
        self.retained
            .iter()
            .next_back()
            .map(|(_, revision)| revision)
    }

    /// Returns the lowest sequence currently retained for the current epoch, if any.
    pub fn lowest_retained_sequence(&self) -> Option<u64> {
        self.retained.keys().next().copied()
    }

    /// Returns the current epoch.
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Returns the count of retained revisions.
    pub fn len(&self) -> usize {
        self.retained.len()
    }

    /// Observe a fresh localize revision. Returns Some(newly-recorded RetainedRevision) iff a new
    /// map revision was emitted. Returns None when the localize revision is identical to the last
    /// one seen (idempotent: Stamped duplicates from the bus are common).
    pub fn observe(
        &mut self,
        localize_revision: &LocalizationRevision,
    ) -> Option<RetainedRevision> {
        let revision_id = localize_revision.revision_id;
        let last_localize_epoch = self.last_localize_revision.map(|previous| previous.epoch);

        if let Some(last_epoch) = last_localize_epoch
            && revision_id.epoch < last_epoch
        {
            warn!(
                localize_epoch = revision_id.epoch,
                last_localize_epoch = last_epoch,
                map_epoch = self.epoch(),
                "map runtime skipped stale localization revision from an older epoch"
            );
            return None;
        }

        if let Some(last_epoch) = last_localize_epoch
            && revision_id.epoch != last_epoch
        {
            self.epoch += 1;
            self.retained.clear();
            self.next_sequence = 0;
            return Some(self.record(revision_id, MapRevisionCause::Reset, None));
        }

        if last_localize_epoch.is_none() {
            return Some(self.record(revision_id, MapRevisionCause::SensorIntegration, None));
        }

        if Some(revision_id) == self.last_localize_revision {
            return None;
        }

        let previous = self.current().map(|revision| revision.map_revision_id);
        Some(self.record(
            revision_id,
            MapRevisionCause::LocalizationCorrection,
            previous,
        ))
    }

    /// Pure query — no state change.
    pub fn lookup(&self, requested: MapRevisionId) -> RevisionLookup {
        if requested.epoch != self.epoch() {
            return RevisionLookup::WrongEpoch {
                current: self.current_id(),
            };
        }

        if let Some(retained) = self.retained.get(&requested.sequence) {
            return RevisionLookup::Found(retained.clone());
        }

        if self
            .lowest_retained_sequence()
            .is_some_and(|lowest| requested.sequence < lowest)
        {
            return RevisionLookup::Stale {
                current: self.current_id(),
            };
        }

        RevisionLookup::Unavailable {
            latest_available: self.current().map(|revision| revision.map_revision_id),
        }
    }

    fn record(
        &mut self,
        localize_revision: LocalizationRevisionId,
        cause: MapRevisionCause,
        previous_map_revision_id: Option<MapRevisionId>,
    ) -> RetainedRevision {
        let map_revision_id = MapRevisionId {
            epoch: self.epoch,
            sequence: self.next_sequence,
        };
        self.next_sequence += 1;

        let retained = RetainedRevision {
            map_revision_id,
            previous_map_revision_id,
            built_from_localize_revision: localize_revision,
            cause,
        };
        self.retained
            .insert(map_revision_id.sequence, retained.clone());
        self.last_localize_revision = Some(localize_revision);
        self.evict_completed_revisions();
        retained
    }

    fn evict_completed_revisions(&mut self) {
        while self.len() > RETAIN_COMPLETED_REVISIONS {
            let _ = self.retained.pop_first();
        }
    }

    fn current_id(&self) -> MapRevisionId {
        self.current()
            .map(|revision| revision.map_revision_id)
            .unwrap_or(MapRevisionId {
                epoch: self.epoch(),
                sequence: 0,
            })
    }
}

impl Default for RevisionStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use phoxal::api::frame::v1::FrameId;
    use phoxal::api::localize::v1::{AffectedKeyframeSummary, LocalizationRevisionCause, Region};

    use super::*;

    #[test]
    fn first_observation_seeds_initial_revision() {
        let mut store = RevisionStore::new();

        let observed = store.observe(&localize_revision(1, 0));

        assert_eq!(
            observed,
            Some(RetainedRevision {
                map_revision_id: MapRevisionId {
                    epoch: INITIAL_MAP_EPOCH,
                    sequence: 0
                },
                previous_map_revision_id: None,
                built_from_localize_revision: localize_revision_id(1, 0),
                cause: MapRevisionCause::SensorIntegration
            })
        );
        assert_eq!(store.current(), observed.as_ref());
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn subsequent_observation_bumps_sequence_and_links_previous() {
        let mut store = RevisionStore::new();
        let Some(first) = store.observe(&localize_revision(1, 0)) else {
            panic!("first revision should be recorded");
        };

        let Some(second) = store.observe(&localize_revision(1, 1)) else {
            panic!("second revision should be recorded");
        };

        assert_eq!(second.previous_map_revision_id, Some(first.map_revision_id));
        assert_eq!(second.map_revision_id.sequence, 1);
        assert_eq!(second.cause, MapRevisionCause::LocalizationCorrection);
    }

    #[test]
    fn duplicate_localize_revision_is_idempotent() {
        let mut store = RevisionStore::new();
        let revision = localize_revision(1, 0);

        assert!(store.observe(&revision).is_some());
        assert_eq!(store.observe(&revision), None);

        assert_eq!(store.len(), 1);
    }

    #[test]
    fn retention_evicts_beyond_three() {
        let mut store = RevisionStore::new();

        observe_many(&mut store, 5);

        assert_eq!(store.len(), RETAIN_COMPLETED_REVISIONS);
        assert_eq!(store.lowest_retained_sequence(), Some(2));
        assert_eq!(
            store
                .current()
                .map(|revision| revision.map_revision_id.sequence),
            Some(4)
        );
    }

    #[test]
    fn epoch_change_resets_store() {
        let mut store = RevisionStore::new();
        let previous_epoch = store.epoch();
        assert!(store.observe(&localize_revision(5, 0)).is_some());

        let Some(reset) = store.observe(&localize_revision(6, 0)) else {
            panic!("new localization epoch should reset map revision store");
        };

        assert_eq!(store.epoch(), previous_epoch + 1);
        assert_eq!(store.len(), 1);
        assert_eq!(reset.map_revision_id.epoch, previous_epoch + 1);
        assert_eq!(reset.map_revision_id.sequence, 0);
        assert_eq!(reset.previous_map_revision_id, None);
        assert_eq!(reset.cause, MapRevisionCause::Reset);
        assert_eq!(store.lowest_retained_sequence(), Some(0));
    }

    #[test]
    fn multiple_observations_same_localize_epoch_do_not_reset() {
        let mut store = RevisionStore::new();

        let _ = store.observe(&localize_revision(5, 0));
        let _ = store.observe(&localize_revision(5, 1));
        let _ = store.observe(&localize_revision(5, 2));

        assert_eq!(store.epoch(), INITIAL_MAP_EPOCH);
        assert_eq!(store.len(), 3);
        assert_eq!(
            store.current().map(|revision| revision.cause),
            Some(MapRevisionCause::LocalizationCorrection),
        );
        assert_eq!(
            store
                .current()
                .map(|revision| revision.map_revision_id.sequence),
            Some(2),
        );
    }

    #[test]
    fn stale_localize_epoch_is_warned_and_skipped() {
        let mut store = RevisionStore::new();
        let _ = store.observe(&localize_revision(5, 1));
        let _ = store.observe(&localize_revision(5, 2));
        let len_before = store.len();
        let current_before = store.current().cloned();

        let result = store.observe(&localize_revision(4, 0));

        assert_eq!(result, None);
        assert_eq!(store.len(), len_before);
        assert_eq!(store.current().cloned(), current_before);
    }

    #[test]
    fn lookup_found_for_current_revision() {
        let mut store = RevisionStore::new();
        let Some(retained) = store.observe(&localize_revision(1, 0)) else {
            panic!("revision should be recorded");
        };

        let lookup = store.lookup(retained.map_revision_id);

        assert_eq!(lookup, RevisionLookup::Found(retained));
    }

    #[test]
    fn lookup_stale_for_evicted_revision() {
        let mut store = RevisionStore::new();
        observe_many(&mut store, 5);

        let lookup = store.lookup(MapRevisionId {
            epoch: INITIAL_MAP_EPOCH,
            sequence: 0,
        });

        assert_eq!(
            lookup,
            RevisionLookup::Stale {
                current: MapRevisionId {
                    epoch: INITIAL_MAP_EPOCH,
                    sequence: 4
                }
            }
        );
    }

    #[test]
    fn lookup_unavailable_for_future_sequence() {
        let mut store = RevisionStore::new();
        let Some(retained) = store.observe(&localize_revision(1, 0)) else {
            panic!("revision should be recorded");
        };

        let lookup = store.lookup(MapRevisionId {
            epoch: INITIAL_MAP_EPOCH,
            sequence: 9999,
        });

        assert_eq!(
            lookup,
            RevisionLookup::Unavailable {
                latest_available: Some(retained.map_revision_id)
            }
        );
    }

    #[test]
    fn lookup_unavailable_when_store_empty() {
        let store = RevisionStore::new();

        let lookup = store.lookup(MapRevisionId {
            epoch: INITIAL_MAP_EPOCH,
            sequence: 0,
        });

        assert_eq!(
            lookup,
            RevisionLookup::Unavailable {
                latest_available: None
            }
        );
    }

    #[test]
    fn lookup_wrong_epoch_for_mismatched_epoch() {
        let mut store = RevisionStore::new();
        let Some(retained) = store.observe(&localize_revision(1, 0)) else {
            panic!("revision should be recorded");
        };

        let lookup = store.lookup(MapRevisionId {
            epoch: 99,
            sequence: 0,
        });

        assert_eq!(
            lookup,
            RevisionLookup::WrongEpoch {
                current: retained.map_revision_id
            }
        );
    }

    #[test]
    fn lookup_wrong_epoch_when_store_empty() {
        let store = RevisionStore::new();

        let lookup = store.lookup(MapRevisionId {
            epoch: 99,
            sequence: 0,
        });

        assert_eq!(
            lookup,
            RevisionLookup::WrongEpoch {
                current: MapRevisionId {
                    epoch: INITIAL_MAP_EPOCH,
                    sequence: 0
                }
            }
        );
    }

    fn observe_many(store: &mut RevisionStore, count: u64) {
        for sequence in 0..count {
            let _ = store.observe(&localize_revision(1, sequence));
        }
    }

    fn localize_revision(epoch: u64, sequence: u64) -> LocalizationRevision {
        LocalizationRevision {
            revision_id: localize_revision_id(epoch, sequence),
            previous_revision_id: sequence
                .checked_sub(1)
                .map(|previous| LocalizationRevisionId {
                    epoch,
                    sequence: previous,
                }),
            cause: LocalizationRevisionCause::SensorIntegration,
            affected_keyframes: AffectedKeyframeSummary {
                keyframe_ids: Vec::new(),
                region: Some(Region {
                    frame_id: FrameId::new("map"),
                    min_xyz_m: [0.0, 0.0, 0.0],
                    max_xyz_m: [1.0, 1.0, 1.0],
                }),
            },
            inline_correction_available: false,
            correction_fetch_required: false,
        }
    }

    const fn localize_revision_id(epoch: u64, sequence: u64) -> LocalizationRevisionId {
        LocalizationRevisionId { epoch, sequence }
    }
}
