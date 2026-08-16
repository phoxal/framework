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
//! This module owns the shape, the bounds, and the invariants a peer may rely
//! on after a successful decode. What the supervisor does with them is the
//! supervisor's.

use phoxal_runtime_contract::identity::{ParticipantId, ProducerId, RobotId};
use phoxal_runtime_contract::metadata::ParticipantKind;
use phoxal_runtime_contract::wire_schema::{DescribeWire, WireField, WireSchema};
use serde::{Deserialize, Deserializer, Serialize};

/// Maximum process rows in one snapshot.
///
/// One row is one expected runtime, and the expected set is `brain` plus every
/// service and every component instance the manifest declares, so this is a
/// bound on how large a robot a snapshot can describe rather than a bound on
/// anything a peer chooses.
pub const MAX_PROCESSES: usize = 64;
/// Maximum UTF-8 bytes in one short diagnostic detail.
pub const MAX_DETAIL_BYTES: usize = 1024;

/// Bounded diagnostic text. Construction truncates; decoding rejects an
/// over-budget peer so every valid value remains deliverable on the bus.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct DiagnosticText<const MAX: usize>(String);

impl<const MAX: usize> DiagnosticText<MAX> {
    /// Bound a diagnostic string, truncating it deterministically when needed.
    #[must_use]
    pub fn new(value: impl AsRef<str>) -> Self {
        Self::new_with_truncation(value).0
    }

    #[must_use]
    /// Bound a diagnostic string and return the number of source bytes removed.
    pub fn new_with_truncation(value: impl AsRef<str>) -> (Self, u32) {
        let value = value.as_ref();
        if value.len() <= MAX {
            return (Self(value.to_string()), 0);
        }
        const MARKER: &str = "...";
        let budget = MAX.saturating_sub(MARKER.len());
        let mut end = budget.min(value.len());
        while !value.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        let mut bounded = value[..end].to_string();
        if MAX >= MARKER.len() {
            bounded.push_str(MARKER);
        }
        let dropped = value.len().saturating_sub(end);
        (Self(bounded), u32::try_from(dropped).unwrap_or(u32::MAX))
    }

    #[must_use]
    /// Borrow the bounded UTF-8 text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<const MAX: usize> std::fmt::Display for DiagnosticText<MAX> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl<'de, const MAX: usize> Deserialize<'de> for DiagnosticText<MAX> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.len() > MAX {
            return Err(serde::de::Error::custom(format!(
                "diagnostic text is {} bytes; limit is {MAX}",
                value.len()
            )));
        }
        Ok(Self(value))
    }
}

impl<const MAX: usize> DescribeWire for DiagnosticText<MAX> {
    // Invariant: this states what `#[serde(transparent)]` writes above - the
    // text as one string. The byte budget is a decode rule and not a shape, so
    // every bound shares the one wire form, which is why the declaration does
    // not spell `MAX`.
    fn wire_schema() -> WireSchema {
        WireSchema::opaque("DiagnosticText", WireSchema::String)
    }
}

pub type Detail = DiagnosticText<MAX_DETAIL_BYTES>;

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
    /// key, or a `components` key. It is carried so a client can group rows
    /// without re-reading the manifest.
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

/// The fixed supervisor bootstrap sequence.
#[derive(
    phoxal_macros::DescribeWire,
    Clone,
    Copy,
    Debug,
    Deserialize,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
    Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum StartupStepKind {
    /// Read `manifest.json`.
    Bundle,
    /// Open the embedded router and the supervisor's own session on it.
    Router,
}

impl StartupStepKind {
    pub const ALL: [Self; 2] = [Self::Bundle, Self::Router];
}

/// State of one supervisor startup step.
///
/// There is no failed state: both steps complete before the control plane a
/// client attaches through exists, so a step that failed has no supervisor left
/// to publish it. A client that can read a snapshot at all is reading one whose
/// steps are done.
#[derive(
    phoxal_macros::DescribeWire, Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum StartupStepState {
    Pending,
    Active,
    Done,
}

/// Progress for one fixed supervisor startup step.
#[derive(phoxal_macros::DescribeWire, Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StartupStep {
    pub kind: StartupStepKind,
    pub state: StartupStepState,
    pub detail: Option<Detail>,
    pub elapsed_ms: Option<u64>,
}

/// Complete supervisor projection at one execution-local revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    /// The robot this execution is running. It is fixed for the life of the
    /// execution - the supervisor is handed one bundle root and never reopens
    /// it - so every revision repeats the same value.
    ///
    /// It rides the snapshot because every client already subscribes to the
    /// snapshot: carrying the identity here costs no endpoint, no second query,
    /// and no addition to the frozen attachment bootstrap, which stays exactly
    /// what it was.
    pub robot: RobotId,
    /// Strictly increasing within one execution and incomparable across runs.
    pub revision: u64,
    pub lifecycle: Lifecycle,
    pub startup: Vec<StartupStep>,
    /// Ordered by participant id.
    pub processes: Vec<Process>,
}

impl Snapshot {
    /// Verify ordering, bounds, and presence relationships before publication.
    pub fn validate(&self) -> Result<(), SnapshotError> {
        if self.processes.len() > MAX_PROCESSES {
            return Err(SnapshotError::TooManyProcesses {
                count: self.processes.len(),
            });
        }
        if self.startup.len() > StartupStepKind::ALL.len() {
            return Err(SnapshotError::TooManyStartupSteps {
                count: self.startup.len(),
            });
        }
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
        for (index, step) in self.startup.iter().enumerate() {
            let expected = StartupStepKind::ALL[index];
            if step.kind != expected {
                if self.startup[..index]
                    .iter()
                    .any(|previous| previous.kind == step.kind)
                {
                    return Err(SnapshotError::DuplicateStartupStep { kind: step.kind });
                }
                return Err(SnapshotError::UnorderedStartupStep {
                    index,
                    expected,
                    actual: step.kind,
                });
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
            robot: RobotId,
            revision: u64,
            lifecycle: Lifecycle,
            startup: Vec<StartupStep>,
            processes: Vec<Process>,
        }
        let wire = Wire::deserialize(deserializer)?;
        let snapshot = Self {
            robot: wire.robot,
            revision: wire.revision,
            lifecycle: wire.lifecycle,
            startup: wire.startup,
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
            robot: &'a RobotId,
            revision: u64,
            lifecycle: Lifecycle,
            startup: &'a [StartupStep],
            processes: &'a [Process],
        }
        Wire {
            robot: &self.robot,
            revision: self.revision,
            lifecycle: self.lifecycle,
            startup: &self.startup,
            processes: &self.processes,
        }
        .serialize(serializer)
    }
}

impl DescribeWire for Snapshot {
    // Invariant: this states what the `Serialize` above writes through its
    // `Wire` mirror - one map of those five fields. The relational rules
    // `validate` enforces are decode-time admissibility, not shape.
    fn wire_schema() -> WireSchema {
        WireSchema::opaque(
            "Snapshot",
            WireSchema::structure([
                WireField::required("robot", RobotId::wire_schema()),
                WireField::required("revision", u64::wire_schema()),
                WireField::required("lifecycle", Lifecycle::wire_schema()),
                WireField::required("startup", <Vec<StartupStep>>::wire_schema()),
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
    #[error("snapshot has {count} processes; limit is {MAX_PROCESSES}")]
    TooManyProcesses { count: usize },
    #[error("snapshot has {count} startup steps; limit is {}", StartupStepKind::ALL.len())]
    TooManyStartupSteps { count: usize },
    #[error("snapshot repeats participant at index {index}")]
    DuplicateParticipant { index: usize },
    #[error("snapshot processes are unordered at index {index}")]
    UnorderedParticipants { index: usize },
    #[error("snapshot repeats startup step {kind:?}")]
    DuplicateStartupStep { kind: StartupStepKind },
    #[error("snapshot startup step {index} is {actual:?}; expected {expected:?}")]
    UnorderedStartupStep {
        index: usize,
        expected: StartupStepKind,
        actual: StartupStepKind,
    },
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
            robot: RobotId::new("rover").expect("a canonical robot id"),
            revision: 1,
            lifecycle: Lifecycle::Starting,
            startup: Vec::new(),
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
            "robot": "rover",
            "revision": 1,
            "lifecycle": "starting",
            "startup": [],
            "processes": [],
            "extra": true
        }))
        .expect("malformed fixture encodes");
        assert!(rmp_serde::from_slice::<SnapshotDocument>(&malformed).is_err());
    }

    #[test]
    fn bounds_and_relational_failures_are_enforced() {
        let mut too_many = snapshot(
            (0..=MAX_PROCESSES)
                .map(|i| absent(&format!("p{i}")))
                .collect(),
        );
        too_many
            .processes
            .sort_by(|a, b| a.participant.cmp(&b.participant));
        assert!(matches!(
            too_many.validate(),
            Err(SnapshotError::TooManyProcesses { .. })
        ));

        let mut duplicate_steps = snapshot(Vec::new());
        duplicate_steps.startup = vec![
            StartupStep {
                kind: StartupStepKind::Bundle,
                state: StartupStepState::Done,
                detail: None,
                elapsed_ms: None,
            },
            StartupStep {
                kind: StartupStepKind::Bundle,
                state: StartupStepState::Done,
                detail: None,
                elapsed_ms: None,
            },
        ];
        assert!(matches!(
            duplicate_steps.validate(),
            Err(SnapshotError::DuplicateStartupStep { .. })
        ));
    }

    #[test]
    fn diagnostic_text_is_bounded_and_reports_truncation() {
        let source = "é".repeat(MAX_DETAIL_BYTES);
        let (bounded, dropped) = Detail::new_with_truncation(&source);
        assert!(bounded.as_str().len() <= MAX_DETAIL_BYTES);
        assert!(bounded.as_str().ends_with("..."));
        assert_eq!(
            usize::try_from(dropped).unwrap(),
            source.len() - (MAX_DETAIL_BYTES - 4)
        );

        let encoded = rmp_serde::to_vec_named(&source).unwrap();
        assert!(rmp_serde::from_slice::<Detail>(&encoded).is_err());
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

        let mut startup = snapshot(Vec::new());
        startup.startup.push(StartupStep {
            kind: StartupStepKind::Router,
            state: StartupStepState::Pending,
            detail: None,
            elapsed_ms: None,
        });
        assert!(matches!(
            startup.validate(),
            Err(SnapshotError::UnorderedStartupStep { .. })
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
        populated.startup.push(StartupStep {
            kind: StartupStepKind::Bundle,
            state: StartupStepState::Done,
            detail: Some(Detail::new("read")),
            elapsed_ms: Some(12),
        });

        let document = SnapshotDocument::V0(populated);
        let json = serde_json::to_value(&document).expect("the snapshot document serializes");
        assert_eq!(SnapshotDocument::wire_schema().conforms(&json), Ok(()));
        // The tag merges into the same map as the snapshot's own fields, so
        // the declared document has to describe the merged form.
        assert_eq!(json["schema"], "phoxal/supervisor-snapshot/v0");
        assert_eq!(json["robot"], "rover");
        assert!(json["processes"].is_array());
    }

    /// The robot id is the identity a client reads the whole projection
    /// against, so a snapshot without one, or with a token that is not a robot
    /// id, is not a snapshot at all.
    #[test]
    fn a_snapshot_without_a_valid_robot_id_is_refused_on_decode() {
        for malformed in [
            serde_json::json!({
                "schema": "phoxal/supervisor-snapshot/v0",
                "revision": 1,
                "lifecycle": "starting",
                "startup": [],
                "processes": [],
            }),
            serde_json::json!({
                "schema": "phoxal/supervisor-snapshot/v0",
                "robot": "Rover One",
                "revision": 1,
                "lifecycle": "starting",
                "startup": [],
                "processes": [],
            }),
        ] {
            let encoded =
                rmp_serde::to_vec_named(&malformed).expect("messagepack permits the input");
            assert!(
                rmp_serde::from_slice::<SnapshotDocument>(&encoded).is_err(),
                "{malformed}"
            );
        }
    }

    /// A bounded diagnostic is one string on the wire whatever its budget is.
    #[test]
    fn a_diagnostic_budget_declares_the_one_text_shape() {
        let json = serde_json::to_value(Detail::new("bounded")).expect("a detail serializes");
        assert_eq!(Detail::wire_schema(), WireSchema::opaque("DiagnosticText", WireSchema::String));
        assert_eq!(Detail::wire_schema().conforms(&json), Ok(()));
    }

    #[test]
    fn startup_all_is_exhaustive() {
        for kind in StartupStepKind::ALL {
            match kind {
                StartupStepKind::Bundle | StartupStepKind::Router => {}
            }
        }
    }
}
