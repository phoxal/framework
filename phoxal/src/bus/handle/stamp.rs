//! Proof that a participant step happened.
//!
//! # Every minter is `pub(crate)`
//!
//! `StepToken::mint` has exactly one legitimate caller: the participant runner,
//! which releases a scheduled step. Nothing outside `phoxal` may express a
//! participant step instant it did not reach, because nothing outside `phoxal`
//! can name the minter.
//!
//! That is a change of kind, not of degree. While the framework was six
//! packages these constructors had to be `pub` for the runner and the
//! simulator client to reach them across a crate boundary, and the guarantee
//! was stated as "not by accident". One crate makes `pub(crate)` say exactly
//! what was meant, so the deliberate route is closed too.
//!
use crate::bus::time::RobotInstant;

pub(crate) mod sealed {
    pub trait Sealed {}
}

/// The robot instant a completed step stamps its outputs with.
///
/// Implemented only by framework-issued transition stamps, and sealed, so no
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
