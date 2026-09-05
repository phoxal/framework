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

use crate::bus::{LocalInstant, RobotInstant};
use crate::identity::{ParticipantId, ProducerId, TimelineId};
use crate::supervisor::api::execution::Snapshot;
use crate::supervisor::api::simulation::attach::AttachRequest;
use crate::supervisor::api::simulation::{
    SimulationAttachmentPhase, SimulationAttachmentState, SimulationEndReason,
};
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
    attachment_updates: mpsc::Sender<Option<SimulationAttachmentState>>,
    attachment_receiver: Mutex<Option<mpsc::Receiver<Option<SimulationAttachmentState>>>>,
    attachment_current: watch::Sender<Option<SimulationAttachmentState>>,
}

struct Data {
    revision: u64,
    presence: Presence,
    stopping: bool,
    time_domain: TimeDomain,
    attachment_revision: u64,
    attachment: Option<SimulationAttachmentState>,
    attachment_failure: Option<SimulationEndReason>,
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
            stopping: false,
            time_domain: TimeDomain {
                revision: 0,
                timeline: TimelineId::mint(),
                mode: TimeMode::Monotonic,
            },
            attachment_revision: 0,
            attachment: None,
            attachment_failure: None,
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
        let (attachment_updates, attachment_receiver) = mpsc::channel(32);
        let (attachment_current, _) = watch::channel(None);
        Ok(Self {
            inner: Arc::new(Inner {
                data: Mutex::new(data),
                published,
                time_domain,
                time_domain_updates,
                time_domain_receiver: Mutex::new(Some(receiver)),
                attachment_updates,
                attachment_receiver: Mutex::new(Some(attachment_receiver)),
                attachment_current,
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

    /// The current source-bound Live attachment, if any.
    pub(crate) fn attachment(&self) -> Option<SimulationAttachmentState> {
        self.lock().attachment
    }

    /// Observe current attachment phase for internal liveness enforcement.
    pub(crate) fn subscribe_attachment(
        &self,
    ) -> watch::Receiver<Option<SimulationAttachmentState>> {
        self.inner.attachment_current.subscribe()
    }

    /// Take the one ordered stream of complete attachment replacements.
    pub(crate) fn take_attachment_updates(
        &self,
    ) -> Result<mpsc::Receiver<Option<SimulationAttachmentState>>, AttachmentStateError> {
        self.inner
            .attachment_receiver
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or(AttachmentStateError::StreamAlreadyTaken)
    }

    /// Bind a proposed world and controller in Preparing without changing the
    /// execution time domain.
    pub(crate) fn prepare_attachment(
        &self,
        host: ProducerId,
        request: AttachRequest,
    ) -> Result<(SimulationAttachmentState, TimeDomain), AttachmentStateError> {
        let mut data = self.lock();
        if data.stopping {
            return Err(AttachmentStateError::Stopping);
        }
        if let Some(current) = data.attachment {
            if current.host == host
                && current.world == request.world()
                && current.controller == request.controller()
                && current.attached_at.world == request.progress()
                && current.phase != SimulationAttachmentPhase::Removing
            {
                if current.phase == SimulationAttachmentPhase::Active
                    && !data.presence.admits_live_controller(current.controller)
                {
                    return Err(AttachmentStateError::ControllerNotReady {
                        controller: current.controller,
                    });
                }
                return Ok((current, data.time_domain));
            }
            return Err(AttachmentStateError::AlreadyAttached {
                world: current.world,
                host: current.host,
                controller: current.controller,
            });
        }
        if data.time_domain.mode != TimeMode::Monotonic {
            return Err(AttachmentStateError::NonMonotonic);
        }
        if !data.presence.admits_live_controller(request.controller()) {
            return Err(AttachmentStateError::ControllerNotReady {
                controller: request.controller(),
            });
        }
        let Some(now) = LocalInstant::try_now() else {
            return Err(AttachmentStateError::ClockUnavailable);
        };
        let permit = reserve_attachment(&self.inner.attachment_updates)?;
        let revision = data
            .attachment_revision
            .checked_add(1)
            .ok_or(AttachmentStateError::RevisionExhausted)?;
        let attachment = SimulationAttachmentState {
            revision,
            world: request.world(),
            host,
            controller: request.controller(),
            phase: SimulationAttachmentPhase::Preparing,
            attached_at: crate::model::world::LiveAttachmentBoundary {
                world: request.progress(),
                execution: RobotInstant::new(data.time_domain.timeline, now.boot_ns()),
            },
        };
        data.attachment_revision = revision;
        data.attachment = Some(attachment);
        self.inner.attachment_current.send_replace(Some(attachment));
        permit.send(Some(attachment));
        Ok((attachment, data.time_domain))
    }

    /// Commit one Preparing transaction after its bound controller has
    /// acknowledged the revision.
    pub(crate) fn activate_attachment(
        &self,
        host: ProducerId,
        preparing_revision: u64,
    ) -> Result<(SimulationAttachmentState, TimeDomain), AttachmentStateError> {
        let mut data = self.lock();
        let current = data.attachment.ok_or(AttachmentStateError::NotAttached)?;
        if current.host != host {
            return Err(AttachmentStateError::WrongHost {
                expected: current.host,
                observed: host,
            });
        }
        if current.phase == SimulationAttachmentPhase::Active {
            return Ok((current, data.time_domain));
        }
        if current.phase != SimulationAttachmentPhase::Preparing
            || current.revision != preparing_revision
        {
            return Err(AttachmentStateError::NotPreparing);
        }
        if !data.presence.admits_live_controller(current.controller) {
            return Err(AttachmentStateError::ControllerNotReady {
                controller: current.controller,
            });
        }
        let permit = reserve_attachment(&self.inner.attachment_updates)?;
        let revision = data
            .attachment_revision
            .checked_add(1)
            .ok_or(AttachmentStateError::RevisionExhausted)?;
        let active = SimulationAttachmentState {
            revision,
            phase: SimulationAttachmentPhase::Active,
            ..current
        };
        data.attachment_revision = revision;
        data.attachment = Some(active);
        self.inner.attachment_current.send_replace(Some(active));
        permit.send(Some(active));
        Ok((active, data.time_domain))
    }

    /// Enter Removing from the bound host. The execution retains the terminal
    /// state until its ordinary supervisor shutdown completes.
    pub(crate) fn remove_attachment(
        &self,
        host: ProducerId,
    ) -> Result<SimulationAttachmentState, AttachmentStateError> {
        let mut data = self.lock();
        let current = data.attachment.ok_or(AttachmentStateError::NotAttached)?;
        if current.host != host {
            return Err(AttachmentStateError::WrongHost {
                expected: current.host,
                observed: host,
            });
        }
        if current.phase == SimulationAttachmentPhase::Removing {
            return Ok(current);
        }
        transition_to_removing(&mut data, &self.inner, current)
    }

    /// Abort exactly one still-Preparing transaction. A delayed waiter cannot
    /// use this to remove a later revision.
    pub(crate) fn abort_preparing_attachment(
        &self,
        host: ProducerId,
        preparing_revision: u64,
    ) -> Result<SimulationAttachmentState, AttachmentStateError> {
        let mut data = self.lock();
        let current = data.attachment.ok_or(AttachmentStateError::NotAttached)?;
        if current.host != host {
            return Err(AttachmentStateError::WrongHost {
                expected: current.host,
                observed: host,
            });
        }
        if current.phase != SimulationAttachmentPhase::Preparing
            || current.revision != preparing_revision
        {
            return Err(AttachmentStateError::NotPreparing);
        }
        transition_to_removing(&mut data, &self.inner, current)
    }

    /// Converge an exact Active revision to Removing after a typed liveness
    /// failure. This is supervisor-owned rather than host-attributed.
    pub(crate) fn fail_active_attachment(
        &self,
        active_revision: u64,
        reason: SimulationEndReason,
    ) -> Result<SimulationAttachmentState, AttachmentStateError> {
        let mut data = self.lock();
        let current = data.attachment.ok_or(AttachmentStateError::NotAttached)?;
        if current.phase != SimulationAttachmentPhase::Active
            || current.revision != active_revision
        {
            return Err(AttachmentStateError::NotActiveRevision);
        }
        data.stopping = true;
        data.attachment_failure = Some(reason);
        transition_to_removing(&mut data, &self.inner, current)
    }

    pub(crate) fn attachment_failure(&self) -> Option<SimulationEndReason> {
        self.lock().attachment_failure
    }

    /// Refuse new attachment work and publish Removing before an intentional
    /// supervisor shutdown tears down the transport.
    pub(crate) fn begin_shutdown_attachment(
        &self,
    ) -> Result<Option<SimulationAttachmentState>, AttachmentStateError> {
        let mut data = self.lock();
        data.stopping = true;
        let Some(current) = data.attachment else {
            return Ok(None);
        };
        if current.phase == SimulationAttachmentPhase::Removing {
            return Ok(Some(current));
        }
        transition_to_removing(&mut data, &self.inner, current).map(Some)
    }

    /// Whether the delegated controller still owns any expected Ready row.
    pub(crate) fn producer_is_present(&self, producer: ProducerId) -> bool {
        self.lock().presence.contains_producer(producer)
    }

    /// Whether this producer still exclusively owns every delegated driver
    /// while all non-driver runtime roles remain Ready.
    pub(crate) fn controller_is_exclusive(&self, controller: ProducerId) -> bool {
        self.lock().presence.admits_live_controller(controller)
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

fn reserve_attachment(
    sender: &mpsc::Sender<Option<SimulationAttachmentState>>,
) -> Result<mpsc::Permit<'_, Option<SimulationAttachmentState>>, AttachmentStateError> {
    sender.try_reserve().map_err(|error| match error {
        mpsc::error::TrySendError::Full(()) => AttachmentStateError::StreamFull,
        mpsc::error::TrySendError::Closed(()) => AttachmentStateError::StreamClosed,
    })
}

fn transition_to_removing(
    data: &mut Data,
    inner: &Inner,
    current: SimulationAttachmentState,
) -> Result<SimulationAttachmentState, AttachmentStateError> {
    let permit = reserve_attachment(&inner.attachment_updates)?;
    let revision = data
        .attachment_revision
        .checked_add(1)
        .ok_or(AttachmentStateError::RevisionExhausted)?;
    let removing = SimulationAttachmentState {
        revision,
        phase: SimulationAttachmentPhase::Removing,
        ..current
    };
    data.attachment_revision = revision;
    data.attachment = Some(removing);
    inner.attachment_current.send_replace(Some(removing));
    permit.send(Some(removing));
    Ok(removing)
}

/// An attachment transition could not be admitted without losing serialized
/// state or violating source ownership.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum AttachmentStateError {
    #[error("the execution attachment stream is already being served")]
    StreamAlreadyTaken,
    #[error("the execution attachment stream is saturated")]
    StreamFull,
    #[error("the execution attachment stream is unavailable")]
    StreamClosed,
    #[error("the execution attachment revision is exhausted")]
    RevisionExhausted,
    #[error("Live attachment requires the unchanged monotonic execution time domain")]
    NonMonotonic,
    #[error(
        "controller {controller} does not exclusively hold every delegated driver Ready lease while all non-drivers are Ready"
    )]
    ControllerNotReady { controller: ProducerId },
    #[error("the host monotonic clock is unavailable")]
    ClockUnavailable,
    #[error("this execution has no simulation attachment")]
    NotAttached,
    #[error("the execution is stopping and refuses new simulation attachment work")]
    Stopping,
    #[error("the simulation attachment is not in the expected Preparing revision")]
    NotPreparing,
    #[error("the simulation attachment is not in the expected Active revision")]
    NotActiveRevision,
    #[error(
        "execution is already attached to world {world} by host {host} and controller {controller}"
    )]
    AlreadyAttached {
        world: crate::model::world::WorldInstanceId,
        host: ProducerId,
        controller: ProducerId,
    },
    #[error("attachment is bound to host {expected}, not request source {observed}")]
    WrongHost {
        expected: ProducerId,
        observed: ProducerId,
    },
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

    #[tokio::test]
    async fn live_attachment_is_source_bound_ordered_and_preserves_the_domain() {
        let state = state();
        let before = state.time_domain();
        let mut updates = state
            .take_attachment_updates()
            .expect("the attachment stream is available once");
        let host = producer(21);
        let controller = producer(22);
        state.record_presence(
            &ParticipantId::new("brain").expect("valid brain id"),
            producer(19),
            true,
        );
        state.record_presence(
            &ParticipantId::new("drive").expect("valid service id"),
            producer(20),
            true,
        );
        let request = AttachRequest::validated(
            crate::model::world::WorldInstanceId::mint(),
            controller,
            crate::model::world::WorldProgress::at(4, 12).expect("valid progress"),
            12,
        )
        .expect("the host validated its progress boundary");

        let (preparing, preparing_domain) = state
            .prepare_attachment(host, request)
            .expect("a monotonic execution accepts preparation");
        assert_eq!(preparing.phase, SimulationAttachmentPhase::Preparing);
        assert_eq!(preparing.revision, 1);
        assert_eq!(preparing.host, host);
        assert_eq!(preparing.controller, controller);
        assert_eq!(preparing.attached_at.world, request.progress());
        assert_eq!(preparing_domain, before);
        assert_eq!(updates.recv().await, Some(Some(preparing)));

        assert_eq!(
            state
                .activate_attachment(producer(23), preparing.revision)
                .expect_err("another producer cannot commit the attachment"),
            AttachmentStateError::WrongHost {
                expected: host,
                observed: producer(23),
            }
        );
        let brain = ParticipantId::new("brain").expect("valid brain id");
        state.record_presence(&brain, producer(19), false);
        assert_eq!(
            state
                .activate_attachment(host, preparing.revision)
                .expect_err("controller exclusivity is rechecked at commit"),
            AttachmentStateError::ControllerNotReady { controller }
        );
        state.record_presence(&brain, producer(19), true);
        let (active, active_domain) = state
            .activate_attachment(host, preparing.revision)
            .expect("the source-bound host commits preparation");
        assert_eq!(active.phase, SimulationAttachmentPhase::Active);
        assert_eq!(active.revision, 2);
        assert_eq!(active_domain, before);
        assert_eq!(state.time_domain(), before);
        assert_eq!(updates.recv().await, Some(Some(active)));

        let (retry, retry_domain) = state
            .prepare_attachment(host, request)
            .expect("an identical lost-reply retry is idempotent");
        assert_eq!(retry, active);
        assert_eq!(retry_domain, before);
        assert!(updates.try_recv().is_err(), "a retry publishes no phase");

        assert_eq!(
            state
                .remove_attachment(producer(24))
                .expect_err("another producer cannot remove the attachment"),
            AttachmentStateError::WrongHost {
                expected: host,
                observed: producer(24),
            }
        );
        let removing = state
            .remove_attachment(host)
            .expect("the bound host enters Removing");
        assert_eq!(removing.phase, SimulationAttachmentPhase::Removing);
        assert_eq!(removing.revision, 3);
        assert_eq!(updates.recv().await, Some(Some(removing)));
        assert_eq!(state.time_domain(), before);
    }

    #[test]
    fn aborting_preparing_prevents_a_delayed_active_commit() {
        let state = state();
        let host = producer(41);
        let controller = producer(42);
        state.record_presence(
            &ParticipantId::new("brain").expect("valid brain id"),
            producer(39),
            true,
        );
        state.record_presence(
            &ParticipantId::new("drive").expect("valid service id"),
            producer(40),
            true,
        );
        let request = AttachRequest::validated(
            crate::model::world::WorldInstanceId::mint(),
            controller,
            crate::model::world::WorldProgress::at(1, 12).expect("valid progress"),
            12,
        )
        .expect("valid attachment request");
        let (preparing, _) = state
            .prepare_attachment(host, request)
            .expect("preparation starts");

        let removing = state
            .abort_preparing_attachment(host, preparing.revision)
            .expect("the exact Preparing revision rolls back");
        assert_eq!(removing.phase, SimulationAttachmentPhase::Removing);
        assert_eq!(
            state
                .activate_attachment(host, preparing.revision)
                .expect_err("a delayed acknowledgement cannot commit after rollback"),
            AttachmentStateError::NotPreparing
        );
    }

    #[test]
    fn active_authority_loss_records_a_typed_reason_before_removing() {
        let state = state();
        let host = producer(51);
        let controller = producer(52);
        state.record_presence(
            &ParticipantId::new("brain").expect("valid brain id"),
            producer(49),
            true,
        );
        state.record_presence(
            &ParticipantId::new("drive").expect("valid service id"),
            producer(50),
            true,
        );
        let request = AttachRequest::validated(
            crate::model::world::WorldInstanceId::mint(),
            controller,
            crate::model::world::WorldProgress::at(1, 12).expect("valid progress"),
            12,
        )
        .expect("valid attachment request");
        let (preparing, _) = state
            .prepare_attachment(host, request)
            .expect("preparation starts");
        let (active, _) = state
            .activate_attachment(host, preparing.revision)
            .expect("preparation commits");

        let removing = state
            .fail_active_attachment(active.revision, SimulationEndReason::HostLost)
            .expect("the exact Active revision converges to Removing");
        assert_eq!(removing.phase, SimulationAttachmentPhase::Removing);
        assert_eq!(state.attachment_failure(), Some(SimulationEndReason::HostLost));
        assert_eq!(
            state
                .fail_active_attachment(active.revision, SimulationEndReason::ControllerLost)
                .expect_err("a stale liveness callback cannot rewrite terminal evidence"),
            AttachmentStateError::NotActiveRevision
        );
        assert_eq!(state.attachment_failure(), Some(SimulationEndReason::HostLost));
    }

    #[tokio::test]
    async fn shutdown_publishes_removing_once_and_refuses_new_attachment_work() {
        let state = state();
        let mut updates = state
            .take_attachment_updates()
            .expect("the attachment stream is available once");
        let host = producer(31);
        let controller = producer(32);
        state.record_presence(
            &ParticipantId::new("brain").expect("valid brain id"),
            producer(29),
            true,
        );
        state.record_presence(
            &ParticipantId::new("drive").expect("valid service id"),
            producer(30),
            true,
        );
        let request = AttachRequest::validated(
            crate::model::world::WorldInstanceId::mint(),
            controller,
            crate::model::world::WorldProgress::at(3, 12).expect("valid progress"),
            12,
        )
        .expect("the host validated its progress boundary");
        let (preparing, _) = state
            .prepare_attachment(host, request)
            .expect("preparation starts before shutdown");
        assert_eq!(updates.recv().await, Some(Some(preparing)));

        let removing = state
            .begin_shutdown_attachment()
            .expect("shutdown publishes terminal attachment evidence")
            .expect("the attachment exists");
        assert_eq!(removing.phase, SimulationAttachmentPhase::Removing);
        assert!(removing.revision > preparing.revision);
        assert_eq!(updates.recv().await, Some(Some(removing)));
        assert_eq!(
            state.begin_shutdown_attachment().unwrap(),
            Some(removing),
            "repeated shutdown does not mint another revision"
        );
        assert!(updates.try_recv().is_err());
        assert_eq!(
            state
                .prepare_attachment(host, request)
                .expect_err("a stopping execution refuses attachment work"),
            AttachmentStateError::Stopping
        );
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
