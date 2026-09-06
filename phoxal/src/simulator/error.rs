//! Errors reported by Live simulator sessions.

use super::*;

/// A failure while attaching or operating one Live controller session.
#[derive(Debug, thiserror::Error)]
pub enum SimulatorError {
    #[error(
        "no Phoxal execution is reachable at {connect}; start the supervisor before the simulation"
    )]
    NoExecution { connect: String },
    #[error(
        "{count} Phoxal executions are reachable at {connect}, which must identify exactly one: {executions:?}"
    )]
    MultipleExecutions {
        connect: String,
        count: usize,
        executions: Vec<ExecutionId>,
    },
    #[error(transparent)]
    SourceLabel(#[from] SourceLabelError),
    #[error(transparent)]
    Bus(#[from] BusError),
    #[error(transparent)]
    Query(#[from] QueryError),
    #[error("execution attachment bootstrap failed: {detail}")]
    Bootstrap { detail: String },
    #[error("Live simulation requires an unchanged monotonic execution time domain")]
    NonMonotonicTimeDomain,
    #[error("the host monotonic clock is unavailable")]
    ClockUnavailable,
    #[error("the controller has no Active simulation attachment")]
    AttachmentInactive,
    #[error("attachment is bound to controller {expected}, not this session {observed}")]
    WrongController {
        expected: crate::identity::ProducerId,
        observed: crate::identity::ProducerId,
    },
    #[error("the Live attachment observer failed: {detail}")]
    AttachmentObserver { detail: String },
    #[error("the transition stamp no longer names the current Active attachment")]
    StaleTransition,
    #[error("StepEvent index {observed} does not match transition progress {expected}")]
    StepIndexMismatch { expected: u64, observed: u64 },
    #[error("world progress step {observed} does not immediately follow completed step {previous}")]
    NonMonotonicProgress { previous: u64, observed: u64 },
    #[error(transparent)]
    InvalidProgress(#[from] crate::model::world::WorldProgressError),
    #[error("the supervisor returned an invalid Live attachment: {detail}")]
    AttachmentProtocol { detail: String },
    #[error("the host attachment transaction task stopped: {detail}")]
    AttachmentTask { detail: String },
}

/// The simulator session closed, but a close stage left evidence.
#[derive(Debug, thiserror::Error)]
#[error("the simulator session did not close cleanly: {report}")]
pub struct SimulatorCloseError {
    pub report: BusCloseReport,
}
