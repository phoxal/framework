//! The sealed capability surface a role attribute grants a participant marker.
//!
//! Each trait here is a capability token, not behavior: implementing it is what
//! makes a group of [`SetupContext`](crate::SetupContext) methods, or a
//! scheduled step, exist for one marker type. The role attribute is the only
//! implementor, so the set of capabilities a participant has is fixed by which
//! attribute authored it and cannot be widened from the participant's own
//! crate.
//!
//! [`sealing::Sealed`] is what enforces that: the three context-gating traits
//! require it, and only macro-generated code inside a participant crate can
//! name it, so an author cannot hand-write `impl WorldAuthoritySurface for
//! MyService` to reach world-clock authority. [`SchedulableSurface`] is
//! deliberately unsealed - it gates nothing but the presence of a step
//! cadence, and its `#[diagnostic::on_unimplemented]` message is the whole
//! point of it.

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

/// A scheduled `Participant::step`.
#[doc(hidden)]
#[diagnostic::on_unimplemented(
    message = "`{Self}` has a clockless launch policy and cannot own a scheduled step",
    label = "scheduled steps are available only on services and drivers"
)]
pub trait SchedulableSurface {}

/// World-clock authority: minting a timeline and publishing robot time onto it.
#[doc(hidden)]
pub trait WorldAuthoritySurface: sealing::Sealed {}

#[cfg(test)]
mod tests {
    use super::{ComponentBoundSurface, SchedulableSurface, TypedIoSurface, WorldAuthoritySurface};
    use crate::participant::api::ParticipantSpec;
    use crate::prelude::*;
    use phoxal_runtime_contract::metadata::ParticipantKind;

    #[phoxal::simulator(id = "marker-simulator")]
    struct MarkerSimulator;

    impl Participant for MarkerSimulator {
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
        fn assert_simulator<T: WorldAuthoritySurface + ComponentBoundSurface + TypedIoSurface>() {}

        assert_simulator::<MarkerSimulator>();
    }

    /// The brain is a checked, schedulable participant and nothing more: it
    /// gets typed I/O and a step, never a component binding or world
    /// authority. The negative half is a trybuild case
    /// (`brain_has_no_privileged_capabilities`), since an unimplemented trait
    /// cannot be asserted from inside the crate that defines it.
    #[test]
    fn the_brain_marker_is_checked_and_schedulable_only() {
        fn assert_checked_schedulable<T: TypedIoSurface + SchedulableSurface>() {}

        assert_checked_schedulable::<MarkerBrain>();
        assert_eq!(<MarkerBrain as ParticipantSpec>::ID, "brain");
        assert_eq!(
            <MarkerBrain as ParticipantSpec>::KIND,
            ParticipantKind::Brain
        );
    }
}
