//! The supervisor's single publication authority.
//!
//! Every observation - a Ready lease appearing or disappearing - republishes
//! one complete snapshot at the next revision. `revision` is monotonic within
//! the execution and assigned here, so a client that keeps the highest revision
//! it has seen can never install an older document over a newer one.
//!
//! Taking the next revision and installing the snapshot happen under one lock.
//! Split, two callers could take revisions 4 and 5 and then publish them in the
//! other order, leaving the watch channel - and therefore every attached
//! client's `current` answer - resting on revision 4 forever.

use std::sync::{Arc, Mutex, MutexGuard};

use crate::identity::{ParticipantId, ProducerId};
use crate::supervisor::api::execution::Snapshot;
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
}

impl Data {
    fn project(&self) -> Snapshot {
        Snapshot {
            revision: self.revision,
            lifecycle: self.presence.lifecycle(),
            processes: self.presence.processes(),
        }
    }
}

impl ExecutionState {
    /// Start an execution from its expected runtime set, before any of them
    /// has been seen.
    pub(crate) fn new(presence: Presence) -> Self {
        let data = Data {
            revision: 0,
            presence,
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
    use crate::bus::{Codec, MessagePack};
    use crate::model::RobotBuilder;
    use crate::supervisor::api::execution::{Lifecycle, ProcessState};

    use super::*;

    fn state() -> ExecutionState {
        let robot = RobotBuilder::new("rover")
            .service("drive", None)
            .build()
            .expect("a valid robot");
        ExecutionState::new(Presence::for_robot(&robot))
    }

    fn producer(seed: u128) -> ProducerId {
        ProducerId::try_from((1_u128 << 124) | seed).expect("a canonical producer id")
    }

    #[test]
    fn an_execution_starts_at_revision_zero_with_nothing_seen() {
        let snapshot = state().snapshot();
        assert_eq!(snapshot.revision, 0);
        assert_eq!(snapshot.lifecycle, Lifecycle::Starting);
        assert!(
            snapshot
                .processes
                .iter()
                .all(|process| process.state == ProcessState::Absent)
        );
    }

    /// A Ready lease reaches the published snapshot at a higher revision, and
    /// what is published is always something the serve path can encode.
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
        let brain = ParticipantId::new("brain").expect("a valid participant id");
        for ready in [true, false, true] {
            state.record_presence(&brain, producer(1), ready);
            snapshots.changed().await.expect("the authority is alive");
            seen.push(snapshots.borrow_and_update().revision);
        }
        assert!(seen.windows(2).all(|pair| pair[1] > pair[0]), "{seen:?}");
    }
}
