//! The supervisor role a participant plays in an execution graph.

/// What role a process plays in a robot's contract graph: the one mandatory
/// root brain, a bus service, a component driver, or a simulator controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ParticipantKind {
    /// The one mandatory root brain: the robot project's
    /// composition root, built from the root Cargo package and staged as
    /// `bin/brain`. A checked, clocked robot-graph participant, distinct from
    /// a user service and never collapsed into one.
    Brain,
    Service,
    Driver,
    Simulator,
}
