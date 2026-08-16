//! The supervisor's single publication authority.
//!
//! Every observation - a startup step finishing, a Ready lease appearing or
//! disappearing - republishes one complete snapshot at the next revision.
//! `revision` is monotonic within the execution and assigned here, so a client
//! that keeps the highest revision it has seen can never install an older
//! document over a newer one.
//!
//! Taking the next revision and installing the snapshot happen under one lock.
//! Split, two callers could take revisions 4 and 5 and then publish them in the
//! other order, leaving the watch channel - and therefore every attached
//! client's `current` answer - resting on revision 4 forever.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use phoxal_protocol::supervisor::execution::{
    Detail, Snapshot, StartupStep, StartupStepKind, StartupStepState,
};
use phoxal_runtime_contract::identity::{ParticipantId, ProducerId};
use tokio::sync::watch;

use super::presence::Presence;

/// Shared handle to one execution's published state. Cloning shares it.
#[derive(Clone)]
pub(crate) struct ExecutionState {
    inner: Arc<Inner>,
}

struct Inner {
    data: Mutex<Data>,
    published: watch::Sender<Snapshot>,
}

struct Data {
    revision: u64,
    presence: Presence,
    startup: Vec<StartupStep>,
    /// When each currently active step began, so a finished step can report how
    /// long it took.
    started: Vec<(StartupStepKind, Instant)>,
}

impl Data {
    fn project(&self) -> Snapshot {
        Snapshot {
            revision: self.revision,
            lifecycle: self.presence.lifecycle(),
            startup: self.startup.clone(),
            processes: self.presence.processes(),
        }
    }
}

impl ExecutionState {
    /// Start an execution from its expected runtime set, before any startup
    /// step has begun.
    pub(crate) fn new(presence: Presence) -> Self {
        let startup = StartupStepKind::ALL
            .into_iter()
            .map(|kind| StartupStep {
                kind,
                state: StartupStepState::Pending,
                detail: None,
                elapsed_ms: None,
            })
            .collect();
        let data = Data {
            revision: 0,
            presence,
            startup,
            started: Vec::new(),
        };
        let (published, _) = watch::channel(data.project());
        Self {
            inner: Arc::new(Inner {
                data: Mutex::new(data),
                published,
            }),
        }
    }

    /// The most recently published snapshot. This is what the `current` query
    /// answers with.
    pub(crate) fn snapshot(&self) -> Snapshot {
        self.inner.published.borrow().clone()
    }

    /// Observe every published snapshot, for the snapshot stream.
    pub(crate) fn subscribe(&self) -> watch::Receiver<Snapshot> {
        self.inner.published.subscribe()
    }

    /// Apply one participant Ready lease change.
    pub(crate) fn record_presence(
        &self,
        participant: &ParticipantId,
        producer: ProducerId,
        ready: bool,
    ) {
        self.publish(|data| data.presence.record(participant, producer, ready));
    }

    pub(crate) fn step_active(&self, kind: StartupStepKind) {
        self.publish(|data| {
            let Some(step) = data.startup.iter_mut().find(|step| step.kind == kind) else {
                return;
            };
            if step.state == StartupStepState::Active {
                return;
            }
            step.state = StartupStepState::Active;
            step.elapsed_ms = None;
            data.started.push((kind, Instant::now()));
        });
    }

    pub(crate) fn step_detail(&self, kind: StartupStepKind, detail: impl AsRef<str>) {
        self.publish(|data| {
            if let Some(step) = data.startup.iter_mut().find(|step| step.kind == kind) {
                step.detail = Some(Detail::new(detail));
            }
        });
    }

    pub(crate) fn step_done(&self, kind: StartupStepKind) {
        self.publish(|data| {
            let position = data.started.iter().position(|(step, _)| *step == kind);
            let elapsed = position.map(|position| data.started.remove(position).1.elapsed());
            let Some(step) = data.startup.iter_mut().find(|step| step.kind == kind) else {
                return;
            };
            if step.state == StartupStepState::Done {
                return;
            }
            step.state = StartupStepState::Done;
            step.elapsed_ms = elapsed
                .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
                .or(step.elapsed_ms);
        });
    }

    /// Apply one change and publish the complete snapshot that results, at the
    /// next revision.
    fn publish(&self, change: impl FnOnce(&mut Data)) {
        let mut data = self.lock();
        change(&mut data);
        data.revision = data.revision.saturating_add(1);
        let snapshot = data.project();
        // Sent while the lock is held, which is what makes the channel's final
        // value the highest revision rather than whichever writer got there
        // last. A subscriber only ever borrows the channel, never this lock, so
        // there is no cycle to deadlock on.
        self.inner.published.send_replace(snapshot);
    }

    fn lock(&self) -> MutexGuard<'_, Data> {
        self.inner
            .data
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use phoxal_bus::{Codec, MessagePack};
    use phoxal_model::RobotBuilder;
    use phoxal_protocol::supervisor::execution::{Lifecycle, ProcessState};

    use super::*;

    fn state() -> ExecutionState {
        let robot = RobotBuilder::new("rover")
            .service("drive", None)
            .build()
            .expect("a valid robot");
        ExecutionState::new(Presence::for_robot(&robot).expect("an expected set"))
    }

    fn producer(seed: u128) -> ProducerId {
        ProducerId::try_from((1_u128 << 124) | seed).expect("a canonical producer id")
    }

    #[test]
    fn the_startup_sequence_is_the_supervisors_own_and_starts_entirely_pending() {
        let snapshot = state().snapshot();
        assert_eq!(
            snapshot
                .startup
                .iter()
                .map(|step| step.kind)
                .collect::<Vec<_>>(),
            StartupStepKind::ALL
        );
        assert!(
            snapshot
                .startup
                .iter()
                .all(|step| step.state == StartupStepState::Pending)
        );
        assert_eq!(snapshot.revision, 0);
        assert_eq!(snapshot.lifecycle, Lifecycle::Starting);
    }

    #[test]
    fn every_change_publishes_a_strictly_higher_revision() {
        let state = state();
        let mut revisions = vec![state.snapshot().revision];
        state.step_active(StartupStepKind::Bundle);
        revisions.push(state.snapshot().revision);
        state.step_detail(StartupStepKind::Bundle, "manifest.json");
        revisions.push(state.snapshot().revision);
        state.step_done(StartupStepKind::Bundle);
        revisions.push(state.snapshot().revision);
        assert!(
            revisions.windows(2).all(|pair| pair[1] > pair[0]),
            "{revisions:?}"
        );

        let done = state
            .snapshot()
            .startup
            .into_iter()
            .find(|step| step.kind == StartupStepKind::Bundle)
            .expect("the bundle step");
        assert_eq!(done.state, StartupStepState::Done);
        assert_eq!(
            done.detail.as_ref().map(Detail::as_str),
            Some("manifest.json")
        );
        assert!(done.elapsed_ms.is_some(), "a finished step is timed");
    }

    /// A Ready lease reaches the published snapshot, and what is published is
    /// always something the serve path can encode.
    #[test]
    fn a_ready_lease_republishes_an_encodable_snapshot() {
        let state = state();
        let before = state.snapshot().revision;
        let drive = ParticipantId::new("drive").expect("a valid participant id");

        state.record_presence(&drive, producer(5), true);
        let snapshot = state.snapshot();
        assert!(snapshot.revision > before);
        let row = snapshot
            .processes
            .iter()
            .find(|process| process.participant == drive)
            .expect("the service row");
        assert_eq!(row.state, ProcessState::Present);
        assert_eq!(row.producer, Some(producer(5)));
        snapshot.validate().expect("the wire contract accepts it");
        MessagePack::encode(&snapshot).expect("the serve path encodes it");
    }

    /// A subscriber sees every revision the authority published, in order.
    #[tokio::test]
    async fn a_subscriber_never_observes_a_lower_revision_than_it_already_has() {
        let state = state();
        let mut snapshots = state.subscribe();
        let mut seen = vec![snapshots.borrow_and_update().revision];
        for step in StartupStepKind::ALL {
            state.step_active(step);
            snapshots.changed().await.expect("the authority is alive");
            seen.push(snapshots.borrow_and_update().revision);
        }
        assert!(seen.windows(2).all(|pair| pair[1] > pair[0]), "{seen:?}");
    }
}
