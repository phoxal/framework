//! Expected runtimes, and which of them are there.
//!
//! This is the whole of what an observe-only supervisor knows about the robot
//! graph. The expected set comes from the manifest and never changes for the
//! life of the execution; presence comes from participant Ready leases on the
//! bus and changes as processes come and go. Nothing here launches, stops or
//! judges anything.
//!
//! A runtime that is not present is not a failure. It has not started yet, or
//! it stopped, or the machine it runs on is unreachable - the supervisor
//! observes leases and cannot distinguish those, so it reports the one fact it
//! has and lets whoever launched the runtime explain it.

use std::collections::BTreeMap;

use phoxal::identity::{ParticipantId, ProducerId};
use phoxal::model::Robot;
use phoxal::participant::metadata::ParticipantKind;
use phoxal::supervisor::api::execution::{Lifecycle, Process, ProcessState};

/// The expected runtimes of one robot and their observed Ready leases.
#[derive(Clone, Debug)]
pub(crate) struct Presence {
    /// Keyed by participant id, which is also the wire order of the rows.
    rows: BTreeMap<ParticipantId, Row>,
    /// Whether every expected runtime has been present at the same time at
    /// least once. This is what separates a graph that is still coming up from
    /// one that came up and lost something.
    completed: bool,
}

#[derive(Clone, Debug)]
struct Row {
    kind: ParticipantKind,
    /// Live Ready producers, in the order they appeared. Two producers for one
    /// participant means two processes claim the same runtime - a stale
    /// incarnation that has not yet dropped its lease, or an operator who
    /// started a second one. Either way the newest is the one to report and
    /// neither is an error to raise: the supervisor did not start either
    /// process and has nothing to fence.
    producers: Vec<ProducerId>,
}

impl Row {
    fn incumbent(&self) -> Option<ProducerId> {
        self.producers.last().copied()
    }

    fn project(&self, participant: &ParticipantId) -> Process {
        let producer = self.incumbent();
        Process {
            participant: participant.clone(),
            kind: self.kind,
            state: if producer.is_some() {
                ProcessState::Present
            } else {
                ProcessState::Absent
            },
            producer,
        }
    }
}

impl Presence {
    /// The expected runtime set of one compiled robot: `brain`, every service,
    /// and every component instance that declares a `driver` block.
    ///
    /// The manifest is the only input, and this is the same derivation every
    /// launcher performs - a component without a driver launches no process,
    /// so expecting one would leave the graph permanently incomplete. There is
    /// no separate participant list to disagree with the robot.
    #[expect(
        clippy::expect_used,
        reason = "a service and component-instance id are validated `is_topology_token` values and \
                  `brain` is a literal in that same alphabet, which is exactly what a participant \
                  id accepts, so no robot the bundle could have parsed reaches the failure arm"
    )]
    pub(crate) fn for_robot(robot: &Robot) -> Self {
        let mut rows = BTreeMap::new();
        let mut insert = |id: &str, kind: ParticipantKind| {
            let participant = ParticipantId::new(id).expect("a model id is a participant id");
            rows.insert(
                participant,
                Row {
                    kind,
                    producers: Vec::new(),
                },
            );
        };
        insert(BRAIN, ParticipantKind::Brain);
        for (service, _) in robot.services() {
            insert(service.as_str(), ParticipantKind::Service);
        }
        for component in robot.components() {
            if component.instance().driver().is_some() {
                insert(component.id().as_str(), ParticipantKind::Driver);
            }
        }
        Self {
            rows,
            completed: false,
        }
    }

    /// Apply one Ready lease change.
    ///
    /// A participant the manifest does not expect is ignored: the snapshot
    /// answers "is what this robot is made of running", and a process that is
    /// not part of the robot has no row to fill. It is still free to run - the
    /// bundle refuses nobody - and whoever started it knows it is there.
    pub(crate) fn record(
        &mut self,
        participant: &ParticipantId,
        producer: ProducerId,
        ready: bool,
    ) {
        let Some(row) = self.rows.get_mut(participant) else {
            tracing::debug!(
                %participant,
                %producer,
                ready,
                "ignoring a Ready lease for a participant this robot does not expect"
            );
            return;
        };
        row.producers.retain(|held| *held != producer);
        if ready {
            row.producers.push(producer);
        }
        if self.rows.values().all(|row| row.incumbent().is_some()) {
            self.completed = true;
        }
    }

    /// The lifecycle presence implies.
    pub(crate) fn lifecycle(&self) -> Lifecycle {
        if !self.completed {
            return Lifecycle::Starting;
        }
        if self.rows.values().all(|row| row.incumbent().is_some()) {
            Lifecycle::Ready
        } else {
            Lifecycle::Degraded
        }
    }

    /// One row per expected runtime, ordered by participant id as the wire
    /// contract requires.
    pub(crate) fn processes(&self) -> Vec<Process> {
        self.rows
            .iter()
            .map(|(participant, row)| row.project(participant))
            .collect()
    }
}

/// The one mandatory runtime every robot has: its composition root, staged as
/// `bin/brain` and launched under this id.
const BRAIN: &str = "brain";

#[cfg(test)]
mod tests {
    use phoxal::model::RobotBuilder;

    use super::*;

    fn producer(seed: u128) -> ProducerId {
        ProducerId::try_from((1_u128 << 124) | seed).expect("a canonical producer id")
    }

    fn participant(id: &str) -> ParticipantId {
        ParticipantId::new(id).expect("a valid participant id")
    }

    /// A robot with one service, one driven component instance and one
    /// driverless one, so the expected set covers all three kinds and the one
    /// component that launches nothing.
    fn presence() -> Presence {
        let robot = RobotBuilder::new("rover")
            .service("drive", None)
            .component_type("motor", |motor| motor.motor("spin", "axle"))
            .component_with("left", "motor", |mounted| {
                mounted.driver(serde_json::json!({ "connection": "can0" }))
            })
            .component("simulated_only", "motor")
            .build()
            .expect("a valid robot");
        Presence::for_robot(&robot)
    }

    /// A component instance without a driver block launches no process, so it
    /// is not a row: expecting one would hold the graph in `Starting` forever.
    #[test]
    fn the_expected_set_is_the_brain_plus_the_services_and_the_driven_components() {
        let rows = presence().processes();
        assert_eq!(
            rows.iter()
                .map(|row| (row.participant.as_str().to_owned(), row.kind))
                .collect::<Vec<_>>(),
            vec![
                ("brain".to_owned(), ParticipantKind::Brain),
                ("drive".to_owned(), ParticipantKind::Service),
                ("left".to_owned(), ParticipantKind::Driver),
            ]
        );
        assert!(
            rows.iter()
                .all(|row| row.state == ProcessState::Absent && row.producer.is_none())
        );
    }

    /// Starting until the graph has been complete once, Ready while it is, and
    /// Degraded once something that was there is gone - never failed.
    #[test]
    fn the_lifecycle_follows_presence_and_never_fails() {
        let mut presence = presence();
        assert_eq!(presence.lifecycle(), Lifecycle::Starting);

        presence.record(&participant("brain"), producer(1), true);
        presence.record(&participant("drive"), producer(2), true);
        assert_eq!(
            presence.lifecycle(),
            Lifecycle::Starting,
            "one expected runtime has still never been seen"
        );

        presence.record(&participant("left"), producer(3), true);
        assert_eq!(
            presence.lifecycle(),
            Lifecycle::Ready,
            "the driverless instance launches no process, so nothing waits for it"
        );

        presence.record(&participant("left"), producer(3), false);
        assert_eq!(presence.lifecycle(), Lifecycle::Degraded);
        let row = presence
            .processes()
            .into_iter()
            .find(|row| row.participant.as_str() == "left")
            .expect("the component row");
        assert_eq!(row.state, ProcessState::Absent);
        assert_eq!(row.producer, None);

        presence.record(&participant("left"), producer(4), true);
        assert_eq!(presence.lifecycle(), Lifecycle::Ready);
    }

    /// Two live producers for one participant is an ordinary transient, not an
    /// error: the newest is reported, and losing the older one changes nothing.
    #[test]
    fn a_second_producer_takes_the_row_and_losing_the_older_one_is_a_no_op() {
        let mut presence = presence();
        let drive = participant("drive");
        presence.record(&drive, producer(1), true);
        presence.record(&drive, producer(2), true);

        let row = |presence: &Presence| {
            presence
                .processes()
                .into_iter()
                .find(|row| row.participant == drive)
                .expect("the service row")
        };
        assert_eq!(row(&presence).producer, Some(producer(2)));

        presence.record(&drive, producer(1), false);
        let row = row(&presence);
        assert_eq!(row.producer, Some(producer(2)));
        assert_eq!(row.state, ProcessState::Present);
    }

    /// A participant the manifest never mentions is free to run; it simply has
    /// no row, and it can neither complete nor degrade the expected graph.
    #[test]
    fn an_unexpected_participant_has_no_row_and_no_effect() {
        let mut presence = presence();
        presence.record(&participant("webots"), producer(9), true);
        assert_eq!(presence.processes().len(), 3);
        assert_eq!(presence.lifecycle(), Lifecycle::Starting);
    }

    /// Every projected row satisfies the wire contract's presence relation, so
    /// the serve path can always encode what this produces.
    #[test]
    fn the_projection_is_always_a_publishable_snapshot() {
        use phoxal::supervisor::api::execution::Snapshot;

        let mut presence = presence();
        presence.record(&participant("brain"), producer(1), true);
        let snapshot = Snapshot {
            revision: 1,
            lifecycle: presence.lifecycle(),
            processes: presence.processes(),
        };
        snapshot.validate().expect("the projection is publishable");
    }
}
