//! Proof that a step happened, and the authority that mints world steps.
//!
//! # These constructors are `pub`, and that is honest, not a seal
//!
//! [`StepToken::mint`], [`WorldStepToken`]'s minter
//! ([`TimelineAuthority::completed_step`]) and [`TimelineAuthority::mint`] are
//! `pub` because their only legitimate callers live in *other* crates - the
//! runner in `phoxal`, and the external client that drives a simulated world
//! and publishes its clock - while the types live here. Rust has no visibility
//! between "this crate" and "the world", so `pub(crate)` cannot express that
//! boundary, and no token-based seal can either: any token this crate could
//! hand a caller is a token every other crate can obtain the same way.
//!
//! So the guarantee is stated exactly as strong as it is. A participant cannot
//! express robot time it did not reach *by accident*, and cannot do it through
//! the documented authoring surface at all - the role markers make handing a
//! token to the wrong publisher a compile error, and `phoxal::prelude` does not
//! name these minters. A participant that deliberately writes
//! `StepToken::mint` can. Closing that would mean merging the api, bus, and
//! runtime crates into one.
//!
//! `StepToken::mint` belongs to `phoxal`'s runner and to nothing else. The
//! world-step minters belong to the one client that drives the simulated world;
//! the `phoxal` facade re-exports neither of them, so a participant has no
//! documented path to either.

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
    /// Mint the token for one released step. Callable only by `phoxal`'s
    /// runner; see the module docs for why it must nonetheless be `pub`.
    #[doc(hidden)]
    pub const fn mint(at: RobotInstant) -> Self {
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
/// runner-minted [`StepToken`] can cover it. A [`TimelineAuthority`] mints this
/// token once per completed world advance, for all outputs of that advance.
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
/// **What the type system closes.** The documented authoring surface has no
/// path to an authority at all: minting a world clock is not a participant
/// capability, so this type is re-exported from neither `phoxal::bus` nor
/// `phoxal::prelude` and no `SetupContext` method hands one out. That closes
/// the accidental route; see the module docs for why [`mint`](Self::mint)
/// cannot close the deliberate one.
pub struct TimelineAuthority {
    timeline: TimelineId,
}

/// One authority per process: the runtime backstop. The cross-process "exactly
/// one authority" rule is a selection-time property of which participants are
/// launched, not something this process can observe.
static TIMELINE_AUTHORITY_HELD: AtomicBool = AtomicBool::new(false);

impl TimelineAuthority {
    /// Take this process's single timeline authority, or fail if it is already
    /// held. Called by the external client that drives the simulated world; see
    /// the module docs for why it must nonetheless be `pub`.
    #[doc(hidden)]
    pub fn mint(timeline: TimelineId) -> Result<Self> {
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
