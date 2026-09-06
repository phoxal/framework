use super::*;

// Native indexed geometry for a current robot occupies several MiB. Keep the source
// bounded independently of its small protocol envelope and check it before mutation.
pub(super) const MAX_ROBOT_SOURCE_BYTES: usize = 16 * 1024 * 1024;

/// Reject an oversized generated robot import before it crosses the native link.
pub fn validate_robot_import(definition: &str, source: &str) -> Result<(), LinkError> {
    let bytes = definition.len().saturating_add(source.len());
    if bytes > MAX_ROBOT_SOURCE_BYTES {
        return Err(LinkError::FrameTooLarge {
            bytes,
            maximum: MAX_ROBOT_SOURCE_BYTES,
        });
    }
    Ok(())
}
pub(super) const EVENT_QUEUE_CAPACITY: usize = 64;
pub(super) const IO_TIMEOUT: Duration = Duration::from_secs(2);

/// The native role opening one private host connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ControllerRole {
    World,
    Robot { execution: ExecutionId },
}

/// The native Webots pacing mode observed at a completed boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ObservedNativeMode {
    Paused,
    RealTime,
    Run,
    Fast,
}

/// The only native motion states a Live host may request.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum NativeMotion {
    Paused,
    RealTime,
}

/// One observation of shared native progress.
///
/// The framework owns the public `WorldProgress` contract.
/// This private record is only the raw Webots observation the host validates before updating that
/// contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NativeProgressObservation {
    pub completed_step: u64,
    pub elapsed_ns: u64,
    pub mode: ObservedNativeMode,
}

/// Why one native controller can no longer be trusted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ControllerFault {
    Device { detail: String },
    InvalidProgress { detail: String },
    UnsupportedMode { observed: ObservedNativeMode },
    Protocol { detail: String },
}

/// One bounded typed record of command admission and native application.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActuationEvidence {
    pub capability: CapabilityRef,
    pub revision: u64,
    /// Monotonic Active boundary where command authority was selected before native entry.
    pub selected_at: RobotInstant,
    /// Last completed world boundary from which this command selection advanced.
    pub selected_from: WorldProgress,
    /// Completed world transition correlated with `instant`, typed outputs, and `StepEvent`.
    pub progress: WorldProgress,
    pub instant: RobotInstant,
    pub offered: Vec<OfferedActuation>,
    pub selected: Option<api::component::motor::Command>,
    pub selection: ActuationSelection,
    pub applied: AppliedActuation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ActuationSelection {
    SelectedNew,
    Reused,
    None { reason: NoActuationReason },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum NoActuationReason {
    Missing,
    SourceAbsent,
    SourceConflict,
    Expired,
    ReceiverClosed,
    Rejected,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OfferedActuation {
    pub producer: Option<ProducerId>,
    pub sequence: u64,
    pub command: api::component::motor::Command,
    pub decision: ActuationDecision,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ActuationDecision {
    Acquired,
    Renewed,
    WrongParticipant,
    ParticipantSource,
    ReadyStateOverflow,
    SourceAbsent,
    SourceConflict,
    StaleSequence {
        accepted: u64,
        observed: u64,
    },
    AuthorityHeld {
        owner: ProducerId,
    },
    NotOwner {
        owner: ProducerId,
        requested: ProducerId,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum AppliedActuation {
    Position(f64),
    Velocity(f64),
    Torque(f64),
    Stop,
}

/// A bounded controller-to-host observation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ControllerEvent {
    /// Poll the current directive while Webots is paused outside `wb_robot_step`.
    Heartbeat,
    WorldReady {
        time_step_ns: u64,
        mode: ObservedNativeMode,
    },
    WorldMode {
        mode: ObservedNativeMode,
    },
    WorldProgress(NativeProgressObservation),
    RobotReady {
        controller: ProducerId,
    },
    /// The controller observed and accepted one exact supervisor Active revision.
    RobotActive {
        revision: u64,
    },
    /// The Robot has completed all work for this boundary and is outside `wb_robot_step`.
    RobotBoundary {
        progress: WorldProgress,
        motion: NativeMotion,
    },
    RobotParked,
    RobotStopping,
    RobotSupervisorLost,
    ActuationEvidence(Vec<ActuationEvidence>),
    /// Native import exists; release its source and start the controller without physics.
    RobotImported {
        transaction: u64,
    },
    MutationCompleted {
        transaction: u64,
        error: Option<String>,
    },
    Stopped,
    Fault(ControllerFault),
}

/// One request on the local private connection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum HostRequest {
    Hello {
        framework: FrameworkVersion,
        role: ControllerRole,
    },
    Event(ControllerEvent),
}

/// The latest host directive each native controller follows at a transition boundary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum HostDirective {
    Continue { motion: NativeMotion },
    Park,
    Mutate(NativeMutation),
    Stop { reason: String },
}

/// One serialized scene mutation performed by the sole Webots supervisor.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum NativeMutation {
    ImportRobot {
        transaction: u64,
        execution: ExecutionId,
        definition: String,
        source: String,
    },
    StartRobotController {
        transaction: u64,
        execution: ExecutionId,
        ready: bool,
    },
    RemoveRobot {
        transaction: u64,
        definition: String,
    },
    /// Idempotent rollback after an import attempt with an uncertain native outcome.
    RollbackRobot {
        transaction: u64,
        definition: String,
    },
}

impl NativeMutation {
    #[must_use]
    pub const fn transaction(&self) -> u64 {
        match self {
            Self::ImportRobot { transaction, .. }
            | Self::StartRobotController { transaction, .. }
            | Self::RemoveRobot { transaction, .. }
            | Self::RollbackRobot { transaction, .. } => *transaction,
        }
    }
}

/// One host response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum HostResponse {
    Accepted {
        directive: HostDirective,
        robot_plan: Option<RobotSimulationPlan>,
    },
    Directive(HostDirective),
    Rejected {
        reason: String,
    },
}

/// A private controller link failure.
#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error("invalid private Webots host endpoint '{endpoint}'")]
    InvalidEndpoint { endpoint: String },
    #[error("failed to connect to the private Webots host at {endpoint}: {source}")]
    Connect {
        endpoint: String,
        #[source]
        source: std::io::Error,
    },
    #[error("private Webots host I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("private Webots host message encoding failed: {0}")]
    Encode(#[from] rmp_serde::encode::Error),
    #[error("private Webots host message decoding failed: {0}")]
    Decode(#[from] rmp_serde::decode::Error),
    #[error("private Webots host message is {bytes} bytes, exceeding the {maximum}-byte bound")]
    FrameTooLarge { bytes: usize, maximum: usize },
    #[error("private Webots host refused this controller: {reason}")]
    Rejected { reason: String },
    #[error("private Webots host returned an invalid handshake response")]
    InvalidHandshake,
    #[error("the bounded private Webots host event queue is full")]
    WouldBlock,
    #[error("the private Webots host link has stopped")]
    Closed,
    #[error("the private Webots host link failed: {detail}")]
    Failed { detail: String },
}
