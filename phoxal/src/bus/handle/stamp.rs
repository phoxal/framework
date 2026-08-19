//! Proof that a step happened, and the authority that mints world steps.
//!
//! # Every minter is `pub(crate)`
//!
//! `StepToken::mint`, the timeline authority and its world-step minter have
//! exactly two legitimate callers, and both are in this crate: the participant
//! runner, which releases a scheduled step, and `phoxal::simulator`'s world
//! time, which completes a world advance. Nothing outside `phoxal` may express
//! a robot instant it did not reach, because nothing outside `phoxal` can name
//! a minter.
//!
//! That is a change of kind, not of degree. While the framework was six
//! packages these constructors had to be `pub` for the runner and the
//! simulator client to reach them across a crate boundary, and the guarantee
//! was stated as "not by accident". One crate makes `pub(crate)` say exactly
//! what was meant, so the deliberate route is closed too.
//!
//! [`WorldStepToken`] itself stays public: `phoxal::simulator` hands one to
//! the world adapter for every completed step, which is how that adapter
//! stamps the step's outputs. Holding one is proof, never authority - there is
//! no public way to make one.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::identity::TimelineId;

use crate::bus::error::{BusError, Result};
use crate::bus::time::RobotInstant;

mod sealed {
    pub trait Sealed {}
}

/// The robot instant a completed step stamps its outputs with.
///
/// Implemented only by [`StepToken`] and [`WorldStepToken`], and sealed, so no
/// other type can ever stamp a checked publication.
pub trait StepStamp: sealed::Sealed {
    /// The instant this step completed at.
    fn instant(&self) -> RobotInstant;
}

/// Proof that the runner released a scheduled `Participant::step` at a robot
/// instant.
///
/// Handing it to
/// [`StatePublisher::publish`](crate::bus::handle::publisher::StatePublisher::publish)
/// is the sole way a service expresses robot time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StepToken {
    at: RobotInstant,
}

impl StepToken {
    /// Mint the token for one released step. The participant runner is its
    /// only caller.
    pub(crate) const fn mint(at: RobotInstant) -> Self {
        StepToken { at }
    }
}

impl sealed::Sealed for StepToken {}

impl StepStamp for StepToken {
    fn instant(&self) -> RobotInstant {
        self.at
    }
}

/// Proof that the world authority completed one world step.
///
/// The externally driven simulation controller has no framework
/// `Participant::step`: it is driven by the simulator's own advance call, so no
/// runner-minted [`StepToken`] can cover it. The crate-private timeline
/// authority behind `phoxal::simulator`'s world time mints this token once per
/// completed world advance, for all outputs of that advance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorldStepToken {
    at: RobotInstant,
}

impl sealed::Sealed for WorldStepToken {}

impl StepStamp for WorldStepToken {
    fn instant(&self) -> RobotInstant {
        self.at
    }
}

/// Ownership of exactly one timeline's coordinate.
///
/// This is the narrowly scoped answer to "who may say what time it is in a
/// world nobody schedules". A second authority in one process is rejected at
/// mint (a per-process runtime backstop). Across processes the invariant is a
/// selection-time one: exactly one world-authority client is attached to a
/// simulated execution, and that selection is enforced by whatever launches the
/// run, not by anything this process can observe.
///
/// **What the type system closes.** Nothing outside this crate can name an
/// authority, let alone mint one: minting a world clock is not a participant
/// capability and not an SDK capability either. The one holder is
/// [`crate::simulator`]'s world time, which hands its owner completed steps
/// and never the authority behind them.
#[allow(
    dead_code,
    reason = "compiled in every profile because a domain module never asks which profile it is in; its only consumer is a module one profile declares"
)]
pub(crate) struct TimelineAuthority {
    timeline: TimelineId,
}

/// One authority per process: the runtime backstop. The cross-process "exactly
/// one authority" rule is a selection-time property of which participants are
/// launched, not something this process can observe.
#[allow(
    dead_code,
    reason = "compiled in every profile because a domain module never asks which profile it is in; its only consumer is a module one profile declares"
)]
static TIMELINE_AUTHORITY_HELD: AtomicBool = AtomicBool::new(false);

#[allow(
    dead_code,
    reason = "compiled in every profile because a domain module never asks which profile it is in; its only consumer is a module one profile declares"
)]
impl TimelineAuthority {
    /// Take this process's single timeline authority, or fail if it is already
    /// held. [`crate::simulator`] is its only caller.
    pub(crate) fn mint(timeline: TimelineId) -> Result<Self> {
        if TIMELINE_AUTHORITY_HELD.swap(true, Ordering::AcqRel) {
            return Err(BusError::DuplicateTimelineAuthority);
        }
        Ok(TimelineAuthority { timeline })
    }

    /// The timeline this authority owns.
    pub const fn timeline(&self) -> TimelineId {
        self.timeline
    }

    /// Begin a new world history on this authority (a reset or replay branch).
    ///
    /// The authority itself is unique for the process; the *timeline* it owns
    /// is replaced, which is exactly the "simulation reset creates a new
    /// timeline within the same execution" lifecycle rule.
    pub fn replace_timeline(&mut self, timeline: TimelineId) {
        self.timeline = timeline;
    }

    /// Mint the token for one completed world step at `ticks` on this
    /// authority's timeline.
    pub const fn completed_step(&self, ticks: u64) -> WorldStepToken {
        WorldStepToken {
            at: RobotInstant::new(self.timeline, ticks),
        }
    }
}

impl Drop for TimelineAuthority {
    fn drop(&mut self) {
        TIMELINE_AUTHORITY_HELD.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::test_support::timeline;

    #[test]
    fn only_one_timeline_authority_exists_at_a_time() {
        let first = TimelineAuthority::mint(timeline(1)).expect("first authority should mint");
        assert!(
            TimelineAuthority::mint(timeline(2)).is_err(),
            "a second authority must be rejected at startup"
        );
        assert_eq!(first.completed_step(50).instant().ticks(), 50);
        drop(first);
        TimelineAuthority::mint(timeline(3)).expect("the slot is released on drop");
    }
}
