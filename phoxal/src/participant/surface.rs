//! The sealed capability surface a role attribute grants a participant marker.
//!
//! Each trait here is a capability token, not behavior: implementing it is what
//! makes a group of [`SetupContext`](crate::SetupContext) methods, or a
//! scheduled step, exist for one marker type. The role attribute is the only
//! implementor, so the set of capabilities a participant has is fixed by which
//! attribute authored it and cannot be widened from the participant's own
//! crate.
//!
//! [`sealing::Sealed`] is what enforces that: both traits require it, and only
//! macro-generated code inside a participant crate can name it, so an author
//! cannot hand-write `impl ComponentBoundSurface for MyService` to reach a
//! component binding it was not launched for.
//!
//! There is no schedulable marker any more: every remaining role - service,
//! driver, brain - owns a step, so a marker gating one would be satisfied by
//! every participant that can exist. It existed to keep the deleted simulator
//! role out of the scheduler, and it went with that role.

/// The sealing boundary for macro-emitted setup capabilities.
#[doc(hidden)]
pub mod sealing {
    pub trait Sealed {}
}

/// Typed bus IO: publishers, subscribers, queriers, and query registration.
#[doc(hidden)]
pub trait TypedIoSurface: sealing::Sealed {}

/// A bound `robot.components` entry, readable through
/// [`SetupContext::component`](crate::SetupContext::component).
#[doc(hidden)]
pub trait ComponentBoundSurface: sealing::Sealed {}

#[cfg(test)]
mod tests {
    use super::{ComponentBoundSurface, TypedIoSurface};
    use crate::participant::metadata::ParticipantKind;
    use crate::participant::spec::ParticipantSpec;
    use crate::prelude::*;

    #[phoxal::driver(id = "marker-driver")]
    struct MarkerDriver;

    impl Participant for MarkerDriver {
        async fn setup(
            &self,
            _ctx: &mut SetupContext<Self>,
            _config: Self::Config,
        ) -> Result<(Self::State, Self::Api)> {
            Ok(((), ()))
        }
    }

    #[phoxal::brain]
    struct MarkerBrain;

    impl Participant for MarkerBrain {
        async fn setup(
            &self,
            _ctx: &mut SetupContext<Self>,
            _config: Self::Config,
        ) -> Result<(Self::State, Self::Api)> {
            Ok(((), ()))
        }
    }

    /// Each kind macro emits its own marker, which is what gates the
    /// kind-specific `SetupContext` accessors.
    #[test]
    fn kind_macros_emit_their_markers() {
        fn assert_driver<T: ComponentBoundSurface + TypedIoSurface>() {}

        assert_driver::<MarkerDriver>();
    }

    /// The brain is a checked participant and nothing more: typed I/O and a
    /// step, never a component binding. The negative half is a trybuild case
    /// (`brain_has_no_privileged_capabilities`), since an unimplemented trait
    /// cannot be asserted from inside the crate that defines it.
    #[test]
    fn the_brain_marker_is_checked_only() {
        fn assert_checked<T: TypedIoSurface>() {}

        assert_checked::<MarkerBrain>();
        assert_eq!(<MarkerBrain as ParticipantSpec>::ID, "brain");
        assert_eq!(
            <MarkerBrain as ParticipantSpec>::KIND,
            ParticipantKind::Brain
        );
    }
}
