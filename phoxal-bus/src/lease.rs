//! Receiver-owned command leases and producer fencing (#952 sections F and G).
//!
//! A command envelope carries execution membership (structurally, through the
//! bus root), producer identity, a monotonic sequence, and the body. It carries
//! no production timestamp, because a command is a request, not an observation.
//!
//! The **owning service** - never the sender - decides what a command is worth:
//! it rejects a non-increasing sequence from a known producer, stamps its own
//! [`LocalInstant`] observation, renews a lease it owns itself, and applies the
//! result at a logical step. Every leased command has two independent expiry
//! conditions, and neither is distorted to serve the other:
//!
//! - a **host-monotonic silence deadline**, which protects human and network
//!   liveness. At 5x simulation speed it still requires operator presence in
//!   human time.
//! - a **logical hold horizon**, which bounds real or simulated travel. At 5x
//!   simulation speed it still bounds how far the machine may move on one
//!   command.
//!
//! [`ProducerFence`] is the identity half. A fresh process is a fresh producer
//! whose sequence starts at zero, so "did the sequence reset" collapses into
//! "is this a different producer". Once a replacement producer is accepted, the
//! superseded one is fenced: it cannot renew a lease or an actuator permit
//! afterwards, even if it is still live.

use std::time::Duration;

use crate::identity::ProducerId;
use crate::time::{LocalInstant, RobotInstant, TimelineMismatch};

/// Why a command was not accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LeaseRejection {
    /// The sequence did not increase within the accepted producer's stream, so
    /// this is a replay or a reorder.
    #[error("sequence {observed} does not follow {accepted} for the accepted producer")]
    StaleSequence {
        /// The highest sequence accepted so far.
        accepted: u64,
        /// The sequence just observed.
        observed: u64,
    },
    /// A producer that has already been superseded tried to renew.
    #[error("producer {superseded} was superseded by {accepted}")]
    SupersededProducer {
        /// The producer that tried to renew.
        superseded: ProducerId,
        /// The producer that replaced it.
        accepted: ProducerId,
    },
}

/// What happened to a lease when a command was offered to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseDecision {
    /// The command was accepted and the lease renewed.
    Renewed,
    /// A replacement producer was accepted; the previous one is now fenced.
    ProducerReplaced {
        /// The producer that was fenced.
        superseded: ProducerId,
    },
    /// The command was rejected and the lease untouched.
    Rejected(LeaseRejection),
}

/// Tracks which producer currently holds authority over one input.
///
/// The replacement policy is deliberately "last accepted wins": a live producer
/// is authoritative until a *different* producer is accepted, at which point the
/// previous one is fenced permanently. That is what makes a restart safe (the
/// replacement takes over immediately) while keeping the old process from
/// re-asserting itself afterwards.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProducerFence {
    accepted: Option<(ProducerId, u64)>,
}

impl ProducerFence {
    /// Nothing accepted yet.
    pub const fn new() -> Self {
        ProducerFence { accepted: None }
    }

    /// The producer currently holding authority, if any.
    pub fn accepted_producer(&self) -> Option<ProducerId> {
        self.accepted.map(|(producer, _)| producer)
    }

    /// Offer a sample from `producer` at `sequence`.
    ///
    /// A different producer replaces the current one and fences it. The same
    /// producer must strictly increase its sequence.
    pub fn admit(&mut self, producer: ProducerId, sequence: u64) -> LeaseDecision {
        match self.accepted {
            None => {
                self.accepted = Some((producer, sequence));
                LeaseDecision::Renewed
            }
            Some((accepted, last)) if accepted == producer => {
                if sequence > last {
                    self.accepted = Some((producer, sequence));
                    LeaseDecision::Renewed
                } else {
                    LeaseDecision::Rejected(LeaseRejection::StaleSequence {
                        accepted: last,
                        observed: sequence,
                    })
                }
            }
            Some((superseded, _)) => {
                self.accepted = Some((producer, sequence));
                LeaseDecision::ProducerReplaced { superseded }
            }
        }
    }

    /// Whether `producer` may still act, i.e. has not been superseded.
    pub fn permits(&self, producer: ProducerId) -> bool {
        self.accepted
            .is_none_or(|(accepted, _)| accepted == producer)
    }

    /// Forget the accepted producer, so the next sample of any producer starts
    /// a fresh stream. Used when the owning service resets its derived state.
    pub fn clear(&mut self) {
        self.accepted = None;
    }
}

/// A receiver-owned command lease with two independent expiry conditions.
///
/// The lease holds the last accepted command body plus both deadlines. It is
/// the owning service's own state, renewed only by commands the service itself
/// admitted, and read at a logical step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lease<B> {
    silence: Duration,
    hold: Duration,
    fence: ProducerFence,
    held: Option<Held<B>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Held<B> {
    body: B,
    observed_at: LocalInstant,
    accepted_at: Option<RobotInstant>,
}

impl<B> Lease<B> {
    /// A lease that expires after `silence` of host-monotonic quiet, or after
    /// `hold` of robot time since the command was applied.
    pub const fn new(silence: Duration, hold: Duration) -> Self {
        Lease {
            silence,
            hold,
            fence: ProducerFence::new(),
            held: None,
        }
    }

    /// The host-monotonic silence deadline this lease enforces.
    pub const fn silence(&self) -> Duration {
        self.silence
    }

    /// The logical hold horizon this lease enforces.
    pub const fn hold(&self) -> Duration {
        self.hold
    }

    /// The producer currently holding this lease.
    pub fn producer(&self) -> Option<ProducerId> {
        self.fence.accepted_producer()
    }

    /// Offer an observed command to the lease.
    ///
    /// `observed_at` is the receiver's own stamp - the one the bus took before
    /// decode - never anything the sender supplied.
    pub fn offer(
        &mut self,
        producer: ProducerId,
        sequence: u64,
        observed_at: LocalInstant,
        body: B,
    ) -> LeaseDecision {
        let decision = self.fence.admit(producer, sequence);
        match decision {
            LeaseDecision::Renewed | LeaseDecision::ProducerReplaced { .. } => {
                self.held = Some(Held {
                    body,
                    observed_at,
                    // A renewal restarts the logical horizon at the next step
                    // that applies it; until then it has not been applied at
                    // any robot instant.
                    accepted_at: None,
                });
            }
            LeaseDecision::Rejected(_) => {}
        }
        decision
    }

    /// The live command at this step, or `None` if either expiry condition has
    /// been met.
    ///
    /// The first call after a renewal anchors the logical hold horizon at
    /// `now`, so the horizon measures time since the command was *applied*, not
    /// since it was sent - which is what makes it survive a paused simulation
    /// correctly.
    ///
    /// A cross-timeline anchor is not a silently wrong number: it means the
    /// world was replaced under a held command, so the command is dropped.
    pub fn live(&mut self, now: LocalInstant, step: RobotInstant) -> Option<&B> {
        let held = self.held.as_mut()?;
        if now.saturating_duration_since(held.observed_at) > self.silence {
            self.held = None;
            return None;
        }
        let anchor = *held.accepted_at.get_or_insert(step);
        match step.duration_since(anchor) {
            Ok(elapsed) if elapsed <= self.hold => Some(&self.held.as_ref()?.body),
            Ok(_) | Err(TimelineMismatch { .. }) => {
                self.held = None;
                None
            }
        }
    }

    /// Drop any held command and forget the accepted producer.
    pub fn clear(&mut self) {
        self.held = None;
        self.fence.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::TimelineId;

    const SILENCE: Duration = Duration::from_millis(150);
    const HOLD: Duration = Duration::from_millis(500);

    fn step(line: TimelineId, ticks: u64) -> RobotInstant {
        RobotInstant::new(line, ticks)
    }

    fn ms(value: u64) -> u64 {
        value * 1_000_000
    }

    #[test]
    fn a_sequence_that_does_not_increase_is_rejected_without_touching_the_lease() {
        let producer = ProducerId::mint();
        let start = LocalInstant::from_boot_ns(1_000);
        let mut lease = Lease::new(SILENCE, HOLD);

        assert_eq!(
            lease.offer(producer, 1, start, "go"),
            LeaseDecision::Renewed
        );
        assert_eq!(
            lease.offer(producer, 1, start, "replay"),
            LeaseDecision::Rejected(LeaseRejection::StaleSequence {
                accepted: 1,
                observed: 1
            })
        );
        assert_eq!(
            lease.offer(producer, 0, start, "reorder"),
            LeaseDecision::Rejected(LeaseRejection::StaleSequence {
                accepted: 1,
                observed: 0
            })
        );

        let line = TimelineId::mint();
        assert_eq!(lease.live(start, step(line, 0)), Some(&"go"));
    }

    #[test]
    fn a_fresh_producer_starting_at_zero_supersedes_and_fences_the_previous_one() {
        let first = ProducerId::mint();
        let second = ProducerId::mint();
        let start = LocalInstant::from_boot_ns(1_000);
        let mut lease = Lease::new(SILENCE, HOLD);

        lease.offer(first, 9, start, "old");
        // A restarted publisher is a fresh producer whose sequence restarts at
        // zero; that must not look like a replay.
        assert_eq!(
            lease.offer(second, 0, start, "new"),
            LeaseDecision::ProducerReplaced { superseded: first }
        );
        assert!(!lease.fence.permits(first));
        assert!(lease.fence.permits(second));

        // The superseded producer cannot renew afterwards, even while live.
        assert_eq!(
            lease.offer(first, 10, start, "zombie"),
            LeaseDecision::ProducerReplaced { superseded: second },
            "a third takeover is itself a replacement, not a silent accept"
        );
    }

    #[test]
    fn a_superseded_producer_cannot_renew_an_actuator_permit() {
        let first = ProducerId::mint();
        let second = ProducerId::mint();
        let mut fence = ProducerFence::new();
        fence.admit(first, 1);
        fence.admit(second, 0);
        assert_eq!(fence.accepted_producer(), Some(second));
        assert!(
            !fence.permits(first),
            "the fenced authority must not be able to hold a permit open"
        );
    }

    #[test]
    fn host_silence_expires_the_lease_independently_of_logical_time() {
        let producer = ProducerId::mint();
        let line = TimelineId::mint();
        let start = LocalInstant::from_boot_ns(0);
        let mut lease = Lease::new(SILENCE, HOLD);
        lease.offer(producer, 1, start, "go");

        // Logical time is frozen (a paused or slowed simulation), yet host
        // silence still expires the command: operator presence is a human-time
        // requirement.
        let quiet = start.saturating_add(SILENCE + Duration::from_millis(1));
        assert_eq!(lease.live(quiet, step(line, 0)), None);
        assert_eq!(
            lease.live(start, step(line, 0)),
            None,
            "an expired lease stays expired until a new command renews it"
        );
    }

    #[test]
    fn the_logical_horizon_expires_the_lease_independently_of_host_time() {
        let producer = ProducerId::mint();
        let line = TimelineId::mint();
        let start = LocalInstant::from_boot_ns(0);
        let mut lease = Lease::new(SILENCE, HOLD);
        lease.offer(producer, 1, start, "go");

        // Host time is still (an accelerated simulation), yet simulated travel
        // is bounded.
        assert_eq!(lease.live(start, step(line, 0)), Some(&"go"));
        assert_eq!(lease.live(start, step(line, ms(500))), Some(&"go"));
        assert_eq!(lease.live(start, step(line, ms(501))), None);
    }

    #[test]
    fn a_lease_that_expired_while_paused_does_not_apply_on_the_first_resumed_step() {
        let producer = ProducerId::mint();
        let line = TimelineId::mint();
        let start = LocalInstant::from_boot_ns(0);
        let mut lease = Lease::new(SILENCE, HOLD);
        lease.offer(producer, 1, start, "go");
        assert_eq!(lease.live(start, step(line, 0)), Some(&"go"));

        // The world is paused: no steps happen, but host time keeps running
        // past the silence deadline. The first resumed arbitration step must
        // not apply the retained command.
        let resumed = start.saturating_add(SILENCE + Duration::from_millis(1));
        assert_eq!(lease.live(resumed, step(line, 1)), None);
    }

    #[test]
    fn a_replacement_timeline_drops_a_held_command_rather_than_comparing_across_worlds() {
        let producer = ProducerId::mint();
        let first_line = TimelineId::mint();
        let second_line = TimelineId::mint();
        let start = LocalInstant::from_boot_ns(0);
        let mut lease = Lease::new(SILENCE, HOLD);
        lease.offer(producer, 1, start, "go");
        assert_eq!(lease.live(start, step(first_line, 0)), Some(&"go"));
        assert_eq!(lease.live(start, step(second_line, 0)), None);
    }
}
