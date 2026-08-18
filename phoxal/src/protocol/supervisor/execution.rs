//! The supervisor's execution projection, as a wire value.
//!
//! The supervisor observes; it does not run the graph. Every fact below is
//! therefore an *expected versus present* fact and nothing else: which runtimes
//! the manifest says this robot has, which of them currently hold a Ready
//! lease, and under which producer. There is no desired state, no exit status,
//! no restart count and no failure evidence, because the process that would
//! have produced them is not this one - runtimes are launched by the CLI
//! locally and by systemd on a device, and each of those already owns the child
//! facts of what it started.
//!
//! Which robot the projection is about is not here either: it is the manifest
//! `supervisor/info` answers with, and a snapshot that repeated a slice of it
//! on every revision would be a second, smaller answer to the same question.
//!
//! This module owns the shape, the bounds, and the invariants a peer may rely
//! on after a successful decode. What the supervisor does with them is the
//! supervisor's.

use crate::identity::{ParticipantId, ProducerId};
use crate::participant::metadata::ParticipantKind;
use crate::__compat::wire::{DescribeWire, WireField, WireSchema};
use serde::{Deserialize, Deserializer, Serialize};

/// Whether one expected runtime currently holds a Ready lease.
///
/// These are the only two things presence can express. A runtime that never
/// started, one that exited, and one whose host is unreachable are all
/// `Absent` here, and deliberately so: the supervisor watches leases, so it can
/// report that a runtime is not there but never why. Whoever launched the
/// runtime knows why.
#[derive(
    phoxal_macros::DescribeWire, Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    Absent,
    Present,
}

/// One expected runtime of this robot, and whether it is there.
#[derive(phoxal_macros::DescribeWire, Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Process {
    pub participant: ParticipantId,
    /// Derived from where the manifest names this id: `brain`, a `services`
    /// key, or a `components` key with a driver. It is carried so a client can
    /// group rows without re-reading the manifest.
    pub kind: ParticipantKind,
    pub state: ProcessState,
    /// The exact producer holding the Ready lease, present exactly when
    /// [`ProcessState::Present`] is.
    pub producer: Option<ProducerId>,
}

/// Whole-execution lifecycle, derived entirely from presence.
///
/// There is no failed or stopped state. A runtime being absent is not a
/// supervisor failure - it is what `Degraded` says - and the supervisor's own
/// death is observed by every client as the `supervisor/presence` liveliness
/// token disappearing, which is evidence a dying process cannot publish for
/// itself anyway.
#[derive(
    phoxal_macros::DescribeWire, Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    /// At least one expected runtime has never yet been seen present.
    Starting,
    /// Every expected runtime is present.
    Ready,
    /// Every expected runtime has been present at some point, and some is not
    /// present now.
    Degraded,
}

/// Complete supervisor projection at one execution-local revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    /// Strictly increasing within one execution and incomparable across runs.
    pub revision: u64,
    pub lifecycle: Lifecycle,
    /// Ordered by participant id.
    pub processes: Vec<Process>,
}

impl Snapshot {
    /// Verify ordering and presence relationships before publication.
    pub fn validate(&self) -> Result<(), SnapshotError> {
        for (index, pair) in self.processes.windows(2).enumerate() {
            match pair[0].participant.cmp(&pair[1].participant) {
                std::cmp::Ordering::Less => {}
                std::cmp::Ordering::Equal => {
                    return Err(SnapshotError::DuplicateParticipant { index: index + 1 });
                }
                std::cmp::Ordering::Greater => {
                    return Err(SnapshotError::UnorderedParticipants { index: index + 1 });
                }
            }
        }
        for process in &self.processes {
            // Presence *is* the producer holding the lease, so the two can
            // never disagree: a present row without one names no lease, and an
            // absent row with one names a lease that is gone.
            if process.producer.is_some() != (process.state == ProcessState::Present) {
                return Err(SnapshotError::PresenceProducerMismatch {
                    participant: process.participant.clone(),
                    state: process.state,
                });
            }
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for Snapshot {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            revision: u64,
            lifecycle: Lifecycle,
            processes: Vec<Process>,
        }
        let wire = Wire::deserialize(deserializer)?;
        let snapshot = Self {
            revision: wire.revision,
            lifecycle: wire.lifecycle,
            processes: wire.processes,
        };
        snapshot.validate().map_err(serde::de::Error::custom)?;
        Ok(snapshot)
    }
}

impl Serialize for Snapshot {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.validate().map_err(serde::ser::Error::custom)?;
        #[derive(Serialize)]
        #[serde(deny_unknown_fields)]
        struct Wire<'a> {
            revision: u64,
            lifecycle: Lifecycle,
            processes: &'a [Process],
        }
        Wire {
            revision: self.revision,
            lifecycle: self.lifecycle,
            processes: &self.processes,
        }
        .serialize(serializer)
    }
}

impl DescribeWire for Snapshot {
    // Invariant: this states what the `Serialize` above writes through its
    // `Wire` mirror - one map of those three fields. The relational rules
    // `validate` enforces are decode-time admissibility, not shape.
    fn wire_schema() -> WireSchema {
        WireSchema::opaque(
            "Snapshot",
            WireSchema::structure([
                WireField::required("revision", u64::wire_schema()),
                WireField::required("lifecycle", Lifecycle::wire_schema()),
                WireField::required("processes", <Vec<Process>>::wire_schema()),
            ]),
        )
    }
}

/// Snapshot wire document. The schema tag is a parse-time format
/// discriminator: a reader refuses a tag it does not implement before it looks
/// at any field.
#[derive(phoxal_macros::DescribeWire, Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "schema")]
pub enum SnapshotDocument {
    #[serde(rename = "phoxal/supervisor-snapshot/v0")]
    V0(Snapshot),
}

impl SnapshotDocument {
    #[must_use]
    pub const fn snapshot(&self) -> &Snapshot {
        match self {
            Self::V0(snapshot) => snapshot,
        }
    }

    #[must_use]
    pub fn into_snapshot(self) -> Snapshot {
        match self {
            Self::V0(snapshot) => snapshot,
        }
    }
}

impl<'de> Deserialize<'de> for SnapshotDocument {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "schema")]
        enum Wire {
            #[serde(rename = "phoxal/supervisor-snapshot/v0")]
            V0(Snapshot),
        }
        match Wire::deserialize(deserializer)? {
            Wire::V0(snapshot) => Ok(Self::V0(snapshot)),
        }
    }
}

/// A snapshot that violates the supervisor wire invariants.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum SnapshotError {
    #[error("snapshot repeats participant at index {index}")]
    DuplicateParticipant { index: usize },
    #[error("snapshot processes are unordered at index {index}")]
    UnorderedParticipants { index: usize },
    #[error("participant {participant} is {state:?} but its producer says otherwise")]
    PresenceProducerMismatch {
        participant: ParticipantId,
        state: ProcessState,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn producer(seed: u128) -> ProducerId {
        ProducerId::try_from((1_u128 << 124) | seed).expect("a canonical producer id")
    }

    fn absent(id: &str) -> Process {
        Process {
            participant: ParticipantId::new(id).expect("valid participant id"),
            kind: ParticipantKind::Brain,
            state: ProcessState::Absent,
            producer: None,
        }
    }

    fn present(id: &str, seed: u128) -> Process {
        Process {
            state: ProcessState::Present,
            producer: Some(producer(seed)),
            ..absent(id)
        }
    }

    fn snapshot(processes: Vec<Process>) -> Snapshot {
        Snapshot {
            revision: 1,
            lifecycle: Lifecycle::Starting,
            processes,
        }
    }

    #[test]
    fn process_rows_are_ordered_and_unique() {
        assert_eq!(
            snapshot(vec![absent("brain"), absent("drive")]).validate(),
            Ok(())
        );
        assert_eq!(
            snapshot(vec![absent("drive"), absent("brain")]).validate(),
            Err(SnapshotError::UnorderedParticipants { index: 1 })
        );
        assert_eq!(
            snapshot(vec![absent("drive"), absent("drive")]).validate(),
            Err(SnapshotError::DuplicateParticipant { index: 1 })
        );
    }

    #[test]
    fn snapshot_document_round_trips_and_rejects_unknown_fields() {
        let document = SnapshotDocument::V0(snapshot(vec![present("brain", 7)]));
        let encoded = rmp_serde::to_vec_named(&document).expect("snapshot encodes");
        assert_eq!(
            rmp_serde::from_slice::<SnapshotDocument>(&encoded).expect("snapshot decodes"),
            document
        );
        let malformed = rmp_serde::to_vec_named(&serde_json::json!({
            "schema": "phoxal/supervisor-snapshot/v0",
            "revision": 1,
            "lifecycle": "starting",
            "processes": [],
            "extra": true
        }))
        .expect("malformed fixture encodes");
        assert!(rmp_serde::from_slice::<SnapshotDocument>(&malformed).is_err());
    }

    /// Presence and the producer are one fact written twice, so the contract
    /// refuses either half without the other, on validation and on encoding.
    #[test]
    fn every_snapshot_relation_is_enforced_on_validation_and_encoding() {
        let mut without_producer = present("brain", 3);
        without_producer.producer = None;
        let invalid = snapshot(vec![without_producer]);
        assert!(matches!(
            invalid.validate(),
            Err(SnapshotError::PresenceProducerMismatch {
                state: ProcessState::Present,
                ..
            })
        ));
        assert!(rmp_serde::to_vec_named(&invalid).is_err());

        let mut absent_with_producer = absent("brain");
        absent_with_producer.producer = Some(producer(4));
        assert!(matches!(
            snapshot(vec![absent_with_producer]).validate(),
            Err(SnapshotError::PresenceProducerMismatch {
                state: ProcessState::Absent,
                ..
            })
        ));
    }

    /// The snapshot's serializer is hand-written through a `Wire` mirror, so
    /// its declared shape is checked against the document that mirror actually
    /// produces - including the derived rows underneath it, which reach across
    /// crates for the participant, producer, and kind identities.
    #[test]
    fn the_declared_snapshot_shape_is_the_shape_the_mirror_writes() {
        let mut driver = present("drive", 9);
        driver.kind = ParticipantKind::Driver;

        let mut populated = snapshot(vec![absent("brain"), driver]);
        populated.lifecycle = Lifecycle::Degraded;

        let document = SnapshotDocument::V0(populated);
        let json = serde_json::to_value(&document).expect("the snapshot document serializes");
        assert_eq!(SnapshotDocument::wire_schema().conforms(&json), Ok(()));
        // The tag merges into the same map as the snapshot's own fields, so
        // the declared document has to describe the merged form.
        assert_eq!(json["schema"], "phoxal/supervisor-snapshot/v0");
        assert!(json["processes"].is_array());
    }
}
