//! Receiver-owned authority leases for fixed topology and external sources.
//!
//! A packet never transfers authority merely because it arrived later.  Fixed
//! topology inputs are admitted only when the packet's participant is the
//! expected one and that participant has exactly one currently Ready producer.
//! External inputs use an explicit receiver-owned acquisition lease and do not
//! participate in participant Ready at all.

use std::collections::HashSet;
use std::time::Duration;

use phoxal_runtime_contract::identity::{ParticipantId, ProducerId};

use crate::liveliness::{ParticipantReadyEvent, ParticipantReadyStatus};
use crate::metadata::ParticipantSourceIdentity;
use crate::time::{LocalInstant, RobotInstant, RobotTimeError};

/// The maximum number of simultaneously observed Ready incarnations retained
/// by one fixed-source receiver.  More than one is already a fail-closed
/// conflict, so the cap keeps receiver state bounded no matter how many
/// incarnations the liveliness stream reports.
pub const MAX_READY_PRODUCERS: usize = 16;

/// Why a source was not admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LeaseRejection {
    /// The source identified a different participant than this input expects.
    #[error("participant attribution does not match the fixed source")]
    WrongParticipant,
    /// A participant-attributed source attempted to use an external-only
    /// acquisition lease.
    #[error("participant-attributed source cannot acquire an external lease")]
    ParticipantSource,
    /// No producer currently holds an exact Ready lease for the participant.
    #[error("fixed source has no Ready producer")]
    SourceAbsent,
    /// More than one producer is Ready for the fixed participant.
    #[error("fixed source has a Ready producer conflict")]
    SourceConflict,
    /// The sequence did not strictly increase for the active producer.
    #[error("sequence {observed} does not follow {accepted} for the active producer")]
    StaleSequence { accepted: u64, observed: u64 },
    /// A different external producer owns the receiver-side acquisition lease.
    #[error("external producer {owner} already owns the lease")]
    AuthorityHeld { owner: ProducerId },
    /// A caller attempted to release another producer's external lease.
    #[error("producer {requested} does not own the external lease held by {owner}")]
    NotOwner {
        owner: ProducerId,
        requested: ProducerId,
    },
    /// The bounded Ready observation state overflowed and authority is unknown.
    #[error("Ready observation state overflowed")]
    ReadyStateOverflow,
}

/// What happened when a source offered a body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseDecision {
    /// The current source renewed its receiver-owned lease.
    Renewed,
    /// A source acquired an otherwise unowned receiver-side lease.
    Acquired,
    /// The body was rejected and the held value was left untouched.
    Rejected(LeaseRejection),
}

/// The tracing target used for authority decisions.
pub const LEASE_TRACE_TARGET: &str = "phoxal.lease";

#[derive(Clone, Debug, PartialEq, Eq)]
struct Held<B> {
    body: B,
    observed_at: LocalInstant,
    producer: ProducerId,
    sequence: u64,
    observation: u64,
    accepted_at: Option<RobotInstant>,
}

/// A fixed topology source whose authority follows the exact Ready set for
/// one expected participant.
///
/// The Ready set is current state, not a historical fence.  When the old
/// producer loses Ready and a new producer is the sole Ready source, the new
/// source may start a fresh sequence stream.  While both are Ready, every
/// packet is rejected and any held body is dropped fail-closed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixedSourceLease<B> {
    input: &'static str,
    expected_participant: ParticipantId,
    silence: Duration,
    hold: Duration,
    ready: HashSet<ProducerId>,
    ready_overflow: bool,
    /// The one current source's high-water sequence. It survives body
    /// expiry, but is cleared when that source loses Ready, so no producer
    /// history accumulates across handoffs.
    active: Option<(ProducerId, u64)>,
    held: Option<Held<B>>,
    observations: u64,
}

impl<B> FixedSourceLease<B> {
    /// Construct a fixed-source lease for `expected_participant`.
    pub fn new(
        input: &'static str,
        expected_participant: ParticipantId,
        silence: Duration,
        hold: Duration,
    ) -> Self {
        Self {
            input,
            expected_participant,
            silence,
            hold,
            ready: HashSet::new(),
            ready_overflow: false,
            active: None,
            held: None,
            observations: 0,
        }
    }

    /// The participant this lease accepts.
    pub fn expected_participant(&self) -> &ParticipantId {
        &self.expected_participant
    }

    /// Update one exact participant/producer Ready token.
    pub fn update_ready(
        &mut self,
        source: &ParticipantSourceIdentity,
        status: ParticipantReadyStatus,
    ) {
        if source.participant != self.expected_participant {
            return;
        }
        let producer = source.producer;
        match status {
            ParticipantReadyStatus::Ready => {
                if !self.ready.contains(&producer) && self.ready.len() >= MAX_READY_PRODUCERS {
                    self.ready_overflow = true;
                } else {
                    self.ready.insert(producer);
                }
            }
            ParticipantReadyStatus::Lost => {
                self.ready.remove(&producer);
                if self.active.is_some_and(|(active, _)| active == producer) {
                    self.active = None;
                    self.held = None;
                }
                // A later exact observation can make the state trustworthy
                // again; loss events themselves do not erase an overflow
                // because the dropped identities are still unknown.
            }
        }
        self.reconcile_ready();
    }

    /// Apply an exact observer event, ignoring Ready tokens for other
    /// participants in the execution.
    pub fn update_ready_event(&mut self, event: &ParticipantReadyEvent) {
        if event.source.participant == self.expected_participant {
            self.update_ready(&event.source, event.status);
        }
    }

    /// Mark the Ready observer's bounded channel as lossy.  The receiver stays
    /// fail-closed until the process reconstructs a fresh lease/observer.
    pub fn mark_ready_overflow(&mut self) {
        self.ready_overflow = true;
        self.reconcile_ready();
    }

    /// Number of current Ready producers for this participant.
    pub fn ready_count(&self) -> usize {
        self.ready.len()
    }

    /// The active source, if a body is currently held.
    pub fn producer(&self) -> Option<ProducerId> {
        self.held.as_ref().map(|held| held.producer)
    }

    /// Number of receiver observations, useful for diagnostics and tests.
    pub const fn observations(&self) -> u64 {
        self.observations
    }

    /// Offer one fixed-source body with transport provenance.
    pub fn offer(
        &mut self,
        source: Option<&ParticipantSourceIdentity>,
        sequence: u64,
        observed_at: LocalInstant,
        body: B,
    ) -> LeaseDecision {
        let producer = source.map(|source| source.producer);
        let decision =
            if source.map(|source| &source.participant) != Some(&self.expected_participant) {
                LeaseDecision::Rejected(LeaseRejection::WrongParticipant)
            } else if self.ready_overflow {
                LeaseDecision::Rejected(LeaseRejection::ReadyStateOverflow)
            } else if self.ready.is_empty() {
                LeaseDecision::Rejected(LeaseRejection::SourceAbsent)
            } else if self.ready.len() != 1
                || !producer.is_some_and(|producer| self.ready.contains(&producer))
            {
                LeaseDecision::Rejected(LeaseRejection::SourceConflict)
            } else if let Some((active, accepted)) = self.active {
                if Some(active) != producer {
                    // A different producer can only be admitted after the Ready
                    // loss cleared `active`; packet arrival never performs a
                    // takeover.
                    LeaseDecision::Rejected(LeaseRejection::SourceConflict)
                } else if sequence <= accepted {
                    LeaseDecision::Rejected(LeaseRejection::StaleSequence {
                        accepted,
                        observed: sequence,
                    })
                } else {
                    LeaseDecision::Renewed
                }
            } else {
                LeaseDecision::Acquired
            };

        self.observations = self.observations.saturating_add(1);
        let Some(producer) = producer else {
            return decision;
        };
        self.trace(producer, sequence, decision);
        if matches!(decision, LeaseDecision::Renewed | LeaseDecision::Acquired) {
            self.active = Some((producer, sequence));
            self.held = Some(Held {
                body,
                observed_at,
                producer,
                sequence,
                observation: self.observations,
                accepted_at: None,
            });
        }
        decision
    }

    /// Return the held body when both host and logical deadlines are live.
    pub fn live(&mut self, now: LocalInstant, step: RobotInstant) -> Option<&B> {
        self.live_host(now)?;
        let held = self.held.as_mut()?;
        let (producer, sequence, observation) = (held.producer, held.sequence, held.observation);
        let anchor = match held.accepted_at {
            Some(anchor) => anchor,
            None => *held.accepted_at.insert(step),
        };
        match step.duration_since(anchor) {
            Ok(elapsed) if elapsed < self.hold => Some(&self.held.as_ref()?.body),
            Ok(_) => {
                self.trace_expiry(producer, sequence, observation, "expired_hold");
                self.held = None;
                None
            }
            Err(RobotTimeError::TimelineMismatch(_)) => {
                self.trace_expiry(producer, sequence, observation, "timeline_replaced");
                self.held = None;
                None
            }
            Err(RobotTimeError::Reversed { .. }) => {
                self.trace_expiry(producer, sequence, observation, "time_reversed");
                self.held = None;
                None
            }
        }
    }

    /// Return the held body when the receiver's host-clock silence deadline is
    /// live.  Webots motor control has no framework robot-step token before it
    /// advances the world, so it uses this host-only part of the same lease.
    pub fn live_host(&mut self, now: LocalInstant) -> Option<&B> {
        let held = self.held.as_ref()?;
        let expired = now.saturating_duration_since(held.observed_at) >= self.silence;
        if !expired {
            return self.held.as_ref().map(|held| &held.body);
        }
        let held = self.held.take()?;
        let (producer, sequence, observation) = (held.producer, held.sequence, held.observation);
        self.trace_expiry(producer, sequence, observation, "expired_silence");
        None
    }

    /// Drop the held body while retaining the current source's sequence fence.
    /// Ready state is retained so a later packet cannot bypass the fixed-source
    /// policy or replay an earlier sequence.
    pub fn clear(&mut self) {
        self.held = None;
    }

    fn reconcile_ready(&mut self) {
        let sole = (!self.ready_overflow && self.ready.len() == 1)
            .then(|| self.ready.iter().next().copied())
            .flatten();
        if let Some((active, _)) = self.active
            && !self.ready.contains(&active)
            && sole != Some(active)
        {
            self.active = None;
        }
        let still_owned = self
            .held
            .as_ref()
            .is_some_and(|held| sole == Some(held.producer));
        if !still_owned {
            self.held = None;
        }
    }

    /// Trace one admission decision.
    ///
    /// One record either way, at two levels: acquiring and renewing are what
    /// every packet on a live input does, and at a setpoint's cadence an
    /// info-level line per packet buries everything else a run has to say. A
    /// rejection is the opposite - a packet that did *not* reach the receiver -
    /// so it stays where an operator sees it without asking.
    fn trace(&self, producer: ProducerId, sequence: u64, decision: LeaseDecision) {
        match decision {
            LeaseDecision::Rejected(_) => tracing::info!(
                target: LEASE_TRACE_TARGET,
                input = self.input,
                expected_participant = %self.expected_participant,
                producer = %producer,
                sequence,
                observation = self.observations,
                decision = ?decision,
                ready_count = self.ready.len(),
                "fixed-source authority decision"
            ),
            LeaseDecision::Acquired | LeaseDecision::Renewed => tracing::debug!(
                target: LEASE_TRACE_TARGET,
                input = self.input,
                expected_participant = %self.expected_participant,
                producer = %producer,
                sequence,
                observation = self.observations,
                decision = ?decision,
                ready_count = self.ready.len(),
                "fixed-source authority decision"
            ),
        }
    }

    /// An expiring hold is the ordinary end of a source that stopped sending,
    /// not a fault, so it is traced at the same level as the admissions that
    /// preceded it.
    fn trace_expiry(&self, producer: ProducerId, sequence: u64, observation: u64, decision: &str) {
        tracing::debug!(
            target: LEASE_TRACE_TARGET,
            input = self.input,
            expected_participant = %self.expected_participant,
            producer = %producer,
            sequence,
            observation,
            decision,
            "fixed-source authority expired"
        );
    }
}

/// A bodyless fixed-source admission gate for state observations.
///
/// Unlike [`FixedSourceLease`], this type does not own or expire a body.  It
/// only decides whether a participant-attributed observation may enter a
/// receiver's keep-last slot.  The receiver retains an accepted value until
/// that value's own domain validity (for example `expires_at`) ends, even if
/// the source later loses Ready.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixedSourceAdmission {
    expected_participant: ParticipantId,
    ready: HashSet<ProducerId>,
    ready_overflow: bool,
    active: Option<(ProducerId, u64)>,
    ready_generation: u64,
}

impl FixedSourceAdmission {
    /// Construct an admission gate for one participant.
    pub fn new(expected_participant: ParticipantId) -> Self {
        Self {
            expected_participant,
            ready: HashSet::new(),
            ready_overflow: false,
            active: None,
            ready_generation: 0,
        }
    }

    /// The participant this gate accepts.
    pub fn expected_participant(&self) -> &ParticipantId {
        &self.expected_participant
    }

    /// Update one exact participant/producer Ready token.
    pub fn update_ready(
        &mut self,
        source: &ParticipantSourceIdentity,
        status: ParticipantReadyStatus,
    ) {
        if source.participant != self.expected_participant {
            return;
        }
        match status {
            ParticipantReadyStatus::Ready => {
                if !self.ready.contains(&source.producer) && self.ready.len() >= MAX_READY_PRODUCERS
                {
                    self.ready_overflow = true;
                } else {
                    self.ready.insert(source.producer);
                }
            }
            ParticipantReadyStatus::Lost => {
                self.ready.remove(&source.producer);
                let Some(next_generation) = self.ready_generation.checked_add(1) else {
                    self.ready_overflow = true;
                    self.active = None;
                    return;
                };
                self.ready_generation = next_generation;
                if self
                    .active
                    .is_some_and(|(producer, _)| producer == source.producer)
                {
                    self.active = None;
                }
            }
        }
    }

    /// Apply an exact Ready observer event.
    pub fn update_ready_event(&mut self, event: &ParticipantReadyEvent) {
        self.update_ready(&event.source, event.status);
    }

    /// Mark the Ready observer as lossy; admission remains fail-closed.
    pub fn mark_ready_overflow(&mut self) {
        self.ready_overflow = true;
    }

    /// Number of currently Ready producers for the participant.
    pub fn ready_count(&self) -> usize {
        self.ready.len()
    }

    /// The Ready incarnation generation used to fence retained observations.
    pub fn ready_generation(&self) -> u64 {
        self.ready_generation
    }

    /// Whether this exact observation is the current admitted value.
    ///
    /// This is intentionally a read-only check. A retained state sample must
    /// not re-enter authority merely because Ready disappeared and later
    /// returned; only a fresh ingress call to [`Self::offer`] may establish a
    /// new active observation.
    pub fn is_current(&self, source: Option<&ParticipantSourceIdentity>, sequence: u64) -> bool {
        let Some(source) = source else {
            return false;
        };
        !self.ready_overflow
            && self.ready.len() == 1
            && self.ready.contains(&source.producer)
            && self.active == Some((source.producer, sequence))
    }

    /// Admit a participant-attributed observation before keep-last coalescing.
    pub fn offer(
        &mut self,
        source: Option<&ParticipantSourceIdentity>,
        sequence: u64,
    ) -> LeaseDecision {
        let Some(source) = source else {
            return LeaseDecision::Rejected(LeaseRejection::WrongParticipant);
        };
        if source.participant != self.expected_participant {
            return LeaseDecision::Rejected(LeaseRejection::WrongParticipant);
        }
        if self.ready_overflow {
            return LeaseDecision::Rejected(LeaseRejection::ReadyStateOverflow);
        }
        if self.ready.is_empty() {
            return LeaseDecision::Rejected(LeaseRejection::SourceAbsent);
        }
        if self.ready.len() != 1 || !self.ready.contains(&source.producer) {
            return LeaseDecision::Rejected(LeaseRejection::SourceConflict);
        }
        let decision = match self.active {
            Some((producer, _accepted)) if producer != source.producer => {
                LeaseDecision::Rejected(LeaseRejection::SourceConflict)
            }
            Some((_producer, accepted)) if sequence <= accepted => {
                LeaseDecision::Rejected(LeaseRejection::StaleSequence {
                    accepted,
                    observed: sequence,
                })
            }
            Some(_) => LeaseDecision::Renewed,
            None => LeaseDecision::Acquired,
        };
        if matches!(decision, LeaseDecision::Acquired | LeaseDecision::Renewed) {
            self.active = Some((source.producer, sequence));
        }
        decision
    }
}

/// A receiver-owned lease for an external/operator producer.
///
/// It has no participant field and never consults Ready.  The first admissible
/// producer acquires an unowned lease; another producer cannot steal it by
/// sending a later sequence.  Expiry or explicit release makes acquisition
/// available again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExclusiveProducerLease<B> {
    input: &'static str,
    silence: Duration,
    hold: Duration,
    owner: Option<(ProducerId, u64)>,
    held: Option<Held<B>>,
    observations: u64,
}

impl<B> ExclusiveProducerLease<B> {
    /// Construct an external acquisition lease.
    pub fn new(input: &'static str, silence: Duration, hold: Duration) -> Self {
        Self {
            input,
            silence,
            hold,
            owner: None,
            held: None,
            observations: 0,
        }
    }

    /// The external producer currently holding the lease.
    pub fn producer(&self) -> Option<ProducerId> {
        self.owner.map(|(producer, _)| producer)
    }

    /// Offer a body, acquiring the lease when it is free.
    pub fn offer(
        &mut self,
        source: &crate::metadata::SourceAttribution,
        sequence: u64,
        observed_at: LocalInstant,
        body: B,
    ) -> LeaseDecision {
        let producer = source.producer();
        let decision = if source.participant_source().is_some() {
            LeaseDecision::Rejected(LeaseRejection::ParticipantSource)
        } else {
            match self.owner {
                None => LeaseDecision::Acquired,
                Some((owner, _accepted)) if owner != producer => {
                    LeaseDecision::Rejected(LeaseRejection::AuthorityHeld { owner })
                }
                Some((_owner, accepted)) if sequence <= accepted => {
                    LeaseDecision::Rejected(LeaseRejection::StaleSequence {
                        accepted,
                        observed: sequence,
                    })
                }
                Some(_) => LeaseDecision::Renewed,
            }
        };
        self.observations = self.observations.saturating_add(1);
        // Same split as the fixed-source lease: an operator's setpoint renews
        // this lease on every packet, so admission is debug and only a refused
        // packet is worth an ordinary run's attention.
        match decision {
            LeaseDecision::Rejected(_) => tracing::info!(
                target: LEASE_TRACE_TARGET,
                input = self.input,
                producer = %producer,
                sequence,
                observation = self.observations,
                decision = ?decision,
                "external authority decision"
            ),
            LeaseDecision::Acquired | LeaseDecision::Renewed => tracing::debug!(
                target: LEASE_TRACE_TARGET,
                input = self.input,
                producer = %producer,
                sequence,
                observation = self.observations,
                decision = ?decision,
                "external authority decision"
            ),
        }
        if matches!(decision, LeaseDecision::Acquired | LeaseDecision::Renewed) {
            self.owner = Some((producer, sequence));
            self.held = Some(Held {
                body,
                observed_at,
                producer,
                sequence,
                observation: self.observations,
                accepted_at: None,
            });
        }
        decision
    }

    /// Explicitly release the receiver-owned lease.
    pub fn release(&mut self, producer: ProducerId) -> Result<(), LeaseRejection> {
        let Some((owner, _)) = self.owner else {
            return Ok(());
        };
        if owner != producer {
            return Err(LeaseRejection::NotOwner {
                owner,
                requested: producer,
            });
        }
        self.owner = None;
        self.held = None;
        Ok(())
    }

    /// Return the held body while both expiry conditions remain live.
    pub fn live(&mut self, now: LocalInstant, step: RobotInstant) -> Option<&B> {
        self.expire_before_offer(now, step);
        let held = self.held.as_mut()?;
        let (producer, sequence, observation) = (held.producer, held.sequence, held.observation);
        let anchor = match held.accepted_at {
            Some(anchor) => anchor,
            None => *held.accepted_at.insert(step),
        };
        match step.duration_since(anchor) {
            Ok(elapsed) if elapsed < self.hold => Some(&self.held.as_ref()?.body),
            Ok(_) => {
                self.expire(producer, sequence, observation, "expired_hold");
                None
            }
            Err(RobotTimeError::TimelineMismatch(_)) => {
                self.expire(producer, sequence, observation, "timeline_replaced");
                None
            }
            Err(RobotTimeError::Reversed { .. }) => {
                self.expire(producer, sequence, observation, "time_reversed");
                None
            }
        }
    }

    /// Drop any external authority and body.
    pub fn clear(&mut self) {
        self.owner = None;
        self.held = None;
    }

    /// Expire an external lease using the receiver's host clock and logical
    /// clock before admitting the next queued body.
    ///
    /// Callers that drain a command queue should invoke this before offering
    /// the queue's next body so a silent or logically expired owner cannot
    /// block a new producer's first command. A body that has not yet been
    /// applied has no logical anchor and is anchored by the first `live` call.
    pub fn expire_before_offer(&mut self, now: LocalInstant, step: RobotInstant) {
        self.expire_host(now);
        let Some(held) = self.held.as_ref() else {
            return;
        };
        let Some(anchor) = held.accepted_at else {
            return;
        };
        let reason = match step.duration_since(anchor) {
            Ok(elapsed) if elapsed >= self.hold => Some("expired_hold"),
            Err(RobotTimeError::TimelineMismatch(_)) => Some("timeline_replaced"),
            Err(RobotTimeError::Reversed { .. }) => Some("time_reversed"),
            _ => None,
        };
        if let Some(reason) = reason {
            let (producer, sequence, observation) =
                (held.producer, held.sequence, held.observation);
            self.expire(producer, sequence, observation, reason);
        }
    }

    /// Expire an external lease using only the receiver's host clock.
    pub fn expire_host(&mut self, now: LocalInstant) {
        let Some(held) = self.held.as_ref() else {
            return;
        };
        if now.saturating_duration_since(held.observed_at) >= self.silence {
            let (producer, sequence, observation) =
                (held.producer, held.sequence, held.observation);
            self.expire(producer, sequence, observation, "expired_silence");
        }
    }

    fn expire(&mut self, producer: ProducerId, sequence: u64, observation: u64, decision: &str) {
        tracing::debug!(
            target: LEASE_TRACE_TARGET,
            input = self.input,
            producer = %producer,
            sequence,
            observation,
            decision,
            "external authority expired"
        );
        self.owner = None;
        self.held = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoxal_runtime_contract::identity::TimelineId;

    fn producer(value: u128) -> ProducerId {
        ProducerId::try_from((1_u128 << 124) | value).expect("canonical test producer")
    }

    fn participant(name: &str) -> ParticipantId {
        ParticipantId::new(name).expect("valid test participant")
    }

    fn source_identity(
        participant: &ParticipantId,
        producer: ProducerId,
    ) -> ParticipantSourceIdentity {
        ParticipantSourceIdentity::new(participant.clone(), producer)
    }

    fn external(producer: ProducerId) -> crate::metadata::SourceAttribution {
        crate::metadata::SourceAttribution::External {
            producer,
            label: None,
        }
    }

    fn step(line: TimelineId, ticks: u64) -> RobotInstant {
        RobotInstant::new(line, ticks)
    }

    #[test]
    fn fixed_source_requires_exact_ready_source_and_participant() {
        let expected = participant("motion");
        let wrong = participant("drive");
        let source = producer(1);
        let mut lease = FixedSourceLease::new(
            "drive/target",
            expected.clone(),
            Duration::from_secs(1),
            Duration::from_secs(1),
        );
        let at = LocalInstant::from_boot_ns(0);
        assert_eq!(
            lease.offer(Some(&source_identity(&expected, source)), 1, at, "blocked"),
            LeaseDecision::Rejected(LeaseRejection::SourceAbsent)
        );
        lease.update_ready(
            &source_identity(&wrong, source),
            ParticipantReadyStatus::Ready,
        );
        assert_eq!(lease.ready_count(), 0);
        assert_eq!(
            lease.offer(Some(&source_identity(&wrong, source)), 2, at, "wrong"),
            LeaseDecision::Rejected(LeaseRejection::WrongParticipant)
        );
        lease.update_ready(
            &source_identity(&expected, source),
            ParticipantReadyStatus::Ready,
        );
        assert_eq!(
            lease.offer(Some(&source_identity(&expected, source)), 1, at, "ok"),
            LeaseDecision::Acquired
        );
    }

    #[test]
    fn fixed_source_conflict_never_allows_arrival_order_takeover() {
        let expected = participant("motion");
        let first = producer(2);
        let second = producer(3);
        let at = LocalInstant::from_boot_ns(0);
        let mut lease = FixedSourceLease::new(
            "drive/target",
            expected.clone(),
            Duration::from_secs(1),
            Duration::from_secs(1),
        );
        lease.update_ready(
            &source_identity(&expected, first),
            ParticipantReadyStatus::Ready,
        );
        assert_eq!(
            lease.offer(Some(&source_identity(&expected, first)), 7, at, "old"),
            LeaseDecision::Acquired
        );
        lease.update_ready(
            &source_identity(&expected, second),
            ParticipantReadyStatus::Ready,
        );
        assert_eq!(lease.producer(), None);
        assert_eq!(
            lease.offer(Some(&source_identity(&expected, second)), 0, at, "new"),
            LeaseDecision::Rejected(LeaseRejection::SourceConflict)
        );
        lease.update_ready(
            &source_identity(&expected, first),
            ParticipantReadyStatus::Lost,
        );
        assert_eq!(
            lease.offer(Some(&source_identity(&expected, second)), 0, at, "new"),
            LeaseDecision::Acquired
        );
        assert_eq!(lease.producer(), Some(second));
    }

    #[test]
    fn fixed_source_only_tracks_current_ready_state_and_rejects_replays() {
        let expected = participant("motion");
        let source = producer(4);
        let at = LocalInstant::from_boot_ns(0);
        let mut lease = FixedSourceLease::new(
            "drive/target",
            expected.clone(),
            Duration::from_secs(1),
            Duration::from_secs(1),
        );
        lease.update_ready(
            &source_identity(&expected, source),
            ParticipantReadyStatus::Ready,
        );
        assert_eq!(
            lease.offer(Some(&source_identity(&expected, source)), 3, at, "first"),
            LeaseDecision::Acquired
        );
        assert_eq!(
            lease.offer(Some(&source_identity(&expected, source)), 3, at, "replay"),
            LeaseDecision::Rejected(LeaseRejection::StaleSequence {
                accepted: 3,
                observed: 3,
            })
        );
        assert!(
            lease
                .live(
                    at.saturating_add(Duration::from_secs(1)),
                    step(TimelineId::mint(), 0),
                )
                .is_none()
        );
        assert_eq!(
            lease.offer(
                Some(&source_identity(&expected, source)),
                1,
                at,
                "expired replay"
            ),
            LeaseDecision::Rejected(LeaseRejection::StaleSequence {
                accepted: 3,
                observed: 1,
            })
        );
        lease.update_ready(
            &source_identity(&expected, source),
            ParticipantReadyStatus::Lost,
        );
        lease.update_ready(
            &source_identity(&expected, source),
            ParticipantReadyStatus::Ready,
        );
        assert_eq!(
            lease.offer(
                Some(&source_identity(&expected, source)),
                0,
                at,
                "new incarnation"
            ),
            LeaseDecision::Acquired
        );
    }

    #[test]
    fn fixed_source_admission_rejects_a_mismatched_clear_before_keep_last() {
        let expected = participant("safety");
        let mismatched = participant("operator");
        let authoritative = source_identity(&expected, producer(14));
        let mismatched = source_identity(&mismatched, producer(15));
        let mut admission = FixedSourceAdmission::new(expected);
        admission.update_ready(&authoritative, ParticipantReadyStatus::Ready);

        let mut keep_last = None;
        if matches!(
            admission.offer(Some(&authoritative), 1),
            LeaseDecision::Acquired | LeaseDecision::Renewed
        ) {
            keep_last = Some("authoritative stop");
        }
        assert_eq!(
            admission.offer(Some(&mismatched), 99),
            LeaseDecision::Rejected(LeaseRejection::WrongParticipant)
        );
        assert_eq!(keep_last, Some("authoritative stop"));
    }

    #[test]
    fn fixed_source_admission_rejects_after_ready_loss_before_step_and_keeps_a() {
        let expected = participant("safety");
        let identity = source_identity(&expected, producer(16));
        let mut admission = FixedSourceAdmission::new(expected);
        admission.update_ready(&identity, ParticipantReadyStatus::Ready);
        assert_eq!(admission.offer(Some(&identity), 1), LeaseDecision::Acquired);
        let keep_last = Some("product A");
        admission.update_ready(&identity, ParticipantReadyStatus::Lost);
        assert_eq!(admission.ready_count(), 0);
        assert_eq!(
            admission.offer(Some(&identity), 2),
            LeaseDecision::Rejected(LeaseRejection::SourceAbsent)
        );
        assert_eq!(keep_last, Some("product A"));
    }

    #[test]
    fn fixed_source_admission_rejects_sustained_mismatched_offers_before_keep_last() {
        let expected = participant("navigation");
        let source = source_identity(&expected, producer(18));
        let mismatched = source_identity(&participant("operator"), producer(19));
        let mut admission = FixedSourceAdmission::new(expected);
        admission.update_ready(&source, ParticipantReadyStatus::Ready);

        let keep_last = Some("candidate A");
        assert_eq!(admission.offer(Some(&source), 1), LeaseDecision::Acquired);
        for sequence in 2..=100 {
            assert_eq!(
                admission.offer(Some(&mismatched), sequence),
                LeaseDecision::Rejected(LeaseRejection::WrongParticipant)
            );
        }
        assert_eq!(keep_last, Some("candidate A"));
    }

    #[test]
    fn fixed_source_admission_requires_fresh_publication_after_ready_reappears() {
        let expected = participant("navigation");
        let identity = source_identity(&expected, producer(20));
        let mut admission = FixedSourceAdmission::new(expected);
        admission.update_ready(&identity, ParticipantReadyStatus::Ready);
        assert_eq!(admission.offer(Some(&identity), 1), LeaseDecision::Acquired);
        assert!(admission.is_current(Some(&identity), 1));

        admission.update_ready(&identity, ParticipantReadyStatus::Lost);
        admission.update_ready(&identity, ParticipantReadyStatus::Ready);
        assert!(!admission.is_current(Some(&identity), 1));
        assert_eq!(
            admission.offer(Some(&identity), 1),
            LeaseDecision::Acquired,
            "only a fresh ingress observation may establish the new incarnation"
        );
    }

    #[test]
    fn fixed_source_admission_generation_exhaustion_fails_closed() {
        let expected = participant("navigation");
        let identity = source_identity(&expected, producer(23));
        let mut admission = FixedSourceAdmission::new(expected);
        admission.update_ready(&identity, ParticipantReadyStatus::Ready);
        assert_eq!(admission.offer(Some(&identity), 1), LeaseDecision::Acquired);

        admission.ready_generation = u64::MAX;
        admission.update_ready(&identity, ParticipantReadyStatus::Lost);

        assert!(!admission.is_current(Some(&identity), 1));
        assert_eq!(
            admission.offer(Some(&identity), 2),
            LeaseDecision::Rejected(LeaseRejection::ReadyStateOverflow)
        );
    }

    #[test]
    fn fixed_source_admission_and_liveness_lease_promote_t2_after_reset() {
        let expected = participant("navigation");
        let identity = source_identity(&expected, producer(21));
        let at = LocalInstant::from_boot_ns(0);
        let line = TimelineId::mint();
        let mut admission = FixedSourceAdmission::new(expected.clone());
        let mut lease = FixedSourceLease::new(
            "navigation/candidate",
            expected,
            Duration::from_secs(1),
            Duration::from_secs(1),
        );
        admission.update_ready(&identity, ParticipantReadyStatus::Ready);
        lease.update_ready(&identity, ParticipantReadyStatus::Ready);

        assert_eq!(admission.offer(Some(&identity), 1), LeaseDecision::Acquired);
        assert_eq!(
            lease.offer(Some(&identity), 1, at, "T1 candidate"),
            LeaseDecision::Acquired
        );

        // T2 is admitted into a replacement-timeline quarantine, while the
        // body-holding lease still owns the active T1 value.
        assert_eq!(admission.offer(Some(&identity), 2), LeaseDecision::Renewed);
        lease.clear();
        assert_eq!(
            lease.offer(Some(&identity), 2, at, "T2 candidate"),
            LeaseDecision::Renewed
        );
        assert_eq!(
            lease.live(at, RobotInstant::new(line, 0)),
            Some(&"T2 candidate")
        );
    }

    #[test]
    fn fixed_source_admission_overflow_blocks_new_values_without_body_state() {
        let expected = participant("safety");
        let identity = source_identity(&expected, producer(17));
        let mut admission = FixedSourceAdmission::new(expected);
        admission.update_ready(&identity, ParticipantReadyStatus::Ready);
        admission.mark_ready_overflow();
        assert_eq!(
            admission.offer(Some(&identity), 1),
            LeaseDecision::Rejected(LeaseRejection::ReadyStateOverflow)
        );
    }

    #[test]
    fn external_control_has_no_participant_or_ready_prerequisite_and_no_steal() {
        let first = producer(5);
        let second = producer(6);
        let at = LocalInstant::from_boot_ns(0);
        let mut lease = ExclusiveProducerLease::new(
            "motion/manual",
            Duration::from_secs(1),
            Duration::from_secs(1),
        );
        assert_eq!(
            lease.offer(&external(first), 0, at, "first"),
            LeaseDecision::Acquired
        );
        assert_eq!(
            lease.offer(&external(second), 99, at, "steal"),
            LeaseDecision::Rejected(LeaseRejection::AuthorityHeld { owner: first })
        );
        assert_eq!(lease.producer(), Some(first));
        assert_eq!(
            lease.release(second),
            Err(LeaseRejection::NotOwner {
                owner: first,
                requested: second,
            })
        );
        assert!(lease.release(first).is_ok());
        assert_eq!(
            lease.offer(&external(second), 0, at, "second"),
            LeaseDecision::Acquired
        );
    }

    #[test]
    fn external_lease_rejects_participant_attribution() {
        let source = ParticipantSourceIdentity::new(participant("motion"), producer(13));
        let at = LocalInstant::from_boot_ns(0);
        let mut lease = ExclusiveProducerLease::new(
            "motion/manual",
            Duration::from_secs(1),
            Duration::from_secs(1),
        );

        assert_eq!(
            lease.offer(
                &crate::metadata::SourceAttribution::Participant(source),
                0,
                at,
                "participant body",
            ),
            LeaseDecision::Rejected(LeaseRejection::ParticipantSource)
        );
        assert_eq!(lease.producer(), None);
    }

    #[test]
    fn external_expiry_releases_authority() {
        let first = producer(7);
        let second = producer(8);
        let at = LocalInstant::from_boot_ns(0);
        let line = TimelineId::mint();
        let mut lease = ExclusiveProducerLease::new(
            "motion/manual",
            Duration::from_millis(10),
            Duration::from_secs(1),
        );
        lease.offer(&external(first), 0, at, "first");
        assert!(
            lease
                .live(at.saturating_add(Duration::from_millis(10)), step(line, 0))
                .is_none()
        );
        assert_eq!(
            lease.offer(&external(second), 0, at, "second"),
            LeaseDecision::Acquired
        );
    }

    #[test]
    fn a_silent_owner_is_replaced_by_the_first_next_step_offer() {
        let first = producer(9);
        let second = producer(10);
        let start = LocalInstant::from_boot_ns(0);
        let after_silence = start.saturating_add(Duration::from_millis(10));
        let mut lease = ExclusiveProducerLease::new(
            "motion/manual",
            Duration::from_millis(10),
            Duration::from_secs(1),
        );

        assert_eq!(
            lease.offer(&external(first), 0, start, "first"),
            LeaseDecision::Acquired
        );
        lease.expire_host(after_silence);
        assert_eq!(
            lease.offer(&external(second), 0, after_silence, "second"),
            LeaseDecision::Acquired
        );
        assert_eq!(lease.producer(), Some(second));
        let line = TimelineId::mint();
        assert_eq!(lease.live(after_silence, step(line, 0)), Some(&"second"));
    }

    #[test]
    fn a_logically_expired_owner_is_replaced_before_the_next_offer() {
        let first = producer(11);
        let second = producer(12);
        let host_start = LocalInstant::from_boot_ns(0);
        let line = TimelineId::mint();
        let hold = Duration::from_millis(10);
        let mut lease = ExclusiveProducerLease::new("motion/manual", Duration::from_secs(1), hold);

        assert_eq!(
            lease.offer(&external(first), 0, host_start, "first"),
            LeaseDecision::Acquired
        );
        assert_eq!(lease.live(host_start, step(line, 0)), Some(&"first"));
        let after_hold = step(line, u64::try_from(hold.as_nanos()).unwrap() + 1);
        lease.expire_before_offer(
            host_start.saturating_add(Duration::from_millis(1)),
            after_hold,
        );
        assert_eq!(
            lease.offer(
                &external(second),
                0,
                host_start.saturating_add(Duration::from_millis(1)),
                "second"
            ),
            LeaseDecision::Acquired
        );
        assert_eq!(lease.producer(), Some(second));
    }
}
