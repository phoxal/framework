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

use crate::identity::{ParticipantId, ProducerId, TimelineId};
use crate::supervisor::api::execution::Snapshot;
use crate::supervisor::api::time_domain::{TimeDomain, TimeMode};
use tokio::sync::{mpsc, watch};

use super::presence::Presence;

/// Shared handle to one execution's published state. Cloning shares it.
#[derive(Clone)]
pub(crate) struct ExecutionState {
    inner: Arc<Inner>,
}

struct Inner {
    data: Mutex<Data>,
    published: watch::Sender<Snapshot>,
    time_domain: watch::Sender<TimeDomain>,
    time_domain_updates: mpsc::Sender<TimeDomain>,
    time_domain_receiver: Mutex<Option<mpsc::Receiver<TimeDomain>>>,
}

struct Data {
    revision: u64,
    presence: Presence,
    time_domain: TimeDomain,
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
    pub(crate) fn new(presence: Presence) -> Result<Self, TimeDomainReplacementError> {
        let data = Data {
            revision: 0,
            presence,
            time_domain: TimeDomain {
                revision: 0,
                timeline: TimelineId::mint(),
                mode: TimeMode::Monotonic,
            },
        };
        let (published, _) = watch::channel(data.project());
        let (time_domain, _) = watch::channel(data.time_domain);
        // A watch channel deliberately coalesces values, which is right for a
        // current query but wrong for history replacement. The served stream
        // must expose every replacement in revision order, so it owns this
        // bounded queue for the lifetime of the execution.
        let (time_domain_updates, receiver) = mpsc::channel(32);
        time_domain_updates
            .try_send(data.time_domain)
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => TimeDomainReplacementError::StreamFull,
                mpsc::error::TrySendError::Closed(_) => TimeDomainReplacementError::StreamClosed,
            })?;
        Ok(Self {
            inner: Arc::new(Inner {
                data: Mutex::new(data),
                published,
                time_domain,
                time_domain_updates,
                time_domain_receiver: Mutex::new(Some(receiver)),
            }),
        })
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

    /// The supervisor's current execution time authority.
    pub(crate) fn time_domain(&self) -> TimeDomain {
        *self.inner.time_domain.borrow()
    }

    /// Take the one ordered stream of time-domain replacements.
    ///
    /// An execution has exactly one serving endpoint for this authority.
    /// Taking the receiver rather than cloning a broadcast receiver gives that
    /// endpoint an explicit backpressure limit instead of silently dropping
    /// revisions when a client or transport is slow.
    pub(crate) fn take_time_domain_updates(
        &self,
    ) -> Result<mpsc::Receiver<TimeDomain>, TimeDomainReplacementError> {
        self.inner
            .time_domain_receiver
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or(TimeDomainReplacementError::StreamAlreadyTaken)
    }

    /// Replace the current execution history with one freshly minted timeline.
    ///
    /// The caller owns lifecycle admission before invoking this method. This
    /// state owner only makes the replacement indivisible and publishes it in
    /// the order clients reconcile by `revision`.
    #[allow(
        dead_code,
        reason = "the production simulation lifecycle calls this authority in the combined campaign"
    )]
    pub(crate) fn replace_time_domain(
        &self,
        mode: TimeMode,
    ) -> Result<TimeDomain, TimeDomainReplacementError> {
        let mut data = self.lock();
        // Reserve capacity while holding the revision lock. This makes a full
        // stream a visible lifecycle fault rather than a state change whose
        // publication was lost, and preserves queue order with revision order.
        let permit = self
            .inner
            .time_domain_updates
            .try_reserve()
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(()) => TimeDomainReplacementError::StreamFull,
                mpsc::error::TrySendError::Closed(()) => TimeDomainReplacementError::StreamClosed,
            })?;
        let domain = TimeDomain {
            revision: data
                .time_domain
                .revision
                .checked_add(1)
                .ok_or(TimeDomainReplacementError::RevisionExhausted)?,
            timeline: TimelineId::mint(),
            mode,
        };
        data.time_domain = domain;
        self.inner.time_domain.send_replace(domain);
        permit.send(domain);
        Ok(domain)
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

/// A time-domain transition could not be published exactly once.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum TimeDomainReplacementError {
    /// The host attempted to serve the ordered authority more than once.
    #[error("the execution time-domain stream is already being served")]
    StreamAlreadyTaken,
    /// An admitted transition would overflow the bounded publication queue.
    #[error("the execution time-domain stream is saturated")]
    StreamFull,
    /// The serving endpoint stopped before a new transition was admitted.
    #[error("the execution time-domain stream is unavailable")]
    StreamClosed,
    /// The execution has published the largest representable domain revision.
    #[error("the execution time-domain revision is exhausted")]
    RevisionExhausted,
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
            .expect("a fresh execution state accepts its initial time domain")
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
        let domain = state().time_domain();
        assert_eq!(domain.revision, 0);
        assert_eq!(domain.mode, TimeMode::Monotonic);
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

    #[tokio::test]
    async fn every_time_domain_replacement_has_a_fresh_timeline_and_revision() {
        let state = state();
        let initial = state.time_domain();
        let mut domains = state
            .take_time_domain_updates()
            .expect("the one serving stream is available");
        assert_eq!(domains.recv().await, Some(initial));
        let replacement = state
            .replace_time_domain(TimeMode::Simulated)
            .expect("the serving stream has capacity");

        assert_eq!(domains.recv().await, Some(replacement));
        assert!(replacement.revision > initial.revision);
        assert_ne!(replacement.timeline, initial.timeline);
        assert_eq!(replacement.mode, TimeMode::Simulated);
    }

    #[tokio::test]
    async fn time_domain_replacements_never_coalesce() {
        let state = state();
        let mut domains = state
            .take_time_domain_updates()
            .expect("the one serving stream is available");
        let initial = domains.recv().await.expect("the initial domain");
        let simulated = state
            .replace_time_domain(TimeMode::Simulated)
            .expect("the serving stream has capacity");
        let monotonic = state
            .replace_time_domain(TimeMode::Monotonic)
            .expect("the serving stream has capacity");

        assert_eq!(domains.recv().await, Some(simulated));
        assert_eq!(domains.recv().await, Some(monotonic));
        assert_eq!(
            [initial.revision, simulated.revision, monotonic.revision],
            [0, 1, 2]
        );
        assert_ne!(initial.timeline, simulated.timeline);
        assert_ne!(simulated.timeline, monotonic.timeline);
    }

    #[test]
    fn a_exhausted_time_domain_revision_refuses_the_replacement() {
        let state = state();
        {
            let mut data = state.lock();
            data.time_domain.revision = u64::MAX;
        }

        assert_eq!(
            state
                .replace_time_domain(TimeMode::Simulated)
                .expect_err("an exhausted revision cannot identify a newer timeline"),
            TimeDomainReplacementError::RevisionExhausted
        );
        assert_eq!(state.lock().time_domain.revision, u64::MAX);
    }
}
