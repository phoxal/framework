//! Supervisor-owned Live simulation attachment state and control.
//!
//! Attachment is execution-local state. It correlates one independently
//! progressing world with the execution's unchanged monotonic time domain and
//! binds both the host transaction and the controller producer that may emit
//! simulator data.

crate::nodes! {
    attachment;
    attach;
    end;
}

pub use crate::model::world::{LiveAttachmentBoundary, WorldInstanceId, WorldProgress};

use crate::identity::ProducerId;

/// The phase of the serialized attachment transaction.
#[derive(
    phoxal_macros::DescribeWire,
    Clone,
    Copy,
    Debug,
    Eq,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SimulationAttachmentPhase {
    /// The supervisor has bound the proposed sources, but simulator traffic is
    /// not yet admissible.
    Preparing,
    /// The controller may publish outputs and receive revision-bound commands.
    Active,
    /// The attachment is being removed and no simulator traffic is admissible.
    Removing,
}

/// The complete execution-local binding to one Live world.
#[derive(
    phoxal_macros::DescribeWire,
    Clone,
    Copy,
    Debug,
    Eq,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct SimulationAttachmentState {
    /// Strictly increasing attachment-state revision within this execution.
    pub revision: u64,
    /// The independently hosted world session.
    pub world: WorldInstanceId,
    /// The exact external producer that owns this transaction.
    pub host: ProducerId,
    /// The exact per-Robot controller producer admitted for simulator data.
    pub controller: ProducerId,
    /// The current serialized transaction phase.
    pub phase: SimulationAttachmentPhase,
    /// The immutable world-progress to monotonic-execution correlation captured
    /// at attachment.
    pub attached_at: LiveAttachmentBoundary,
}

impl SimulationAttachmentState {
    /// The revision that setpoint metadata must carry while this attachment is
    /// active. Preparing and Removing deliberately admit no revision.
    #[must_use]
    pub const fn active_revision(self) -> Option<u64> {
        match self.phase {
            SimulationAttachmentPhase::Active => Some(self.revision),
            SimulationAttachmentPhase::Preparing | SimulationAttachmentPhase::Removing => None,
        }
    }
}

/// Why a world host ended one execution's simulation attachment.
#[derive(
    phoxal_macros::DescribeWire,
    Clone,
    Copy,
    Debug,
    Eq,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SimulationEndReason {
    WorldStopped,
    HostLost,
    SimulatorLost,
    WorldControllerLost,
    ControllerLost,
    MutationFailed,
    RemovalFailed,
    UnsupportedNativeMode,
    InvalidProgress,
    ProtocolViolation,
}

#[allow(
    dead_code,
    reason = "attachment lease keys are consumed only by simulator and supervisor profiles"
)]
pub(crate) fn preparation_liveliness_key(revision: u64, controller: ProducerId) -> String {
    format!("simulation/attachment/prepared/{revision}/{controller}")
}

#[allow(
    dead_code,
    reason = "attachment lease keys are consumed only by simulator and supervisor profiles"
)]
pub(crate) fn host_liveliness_key(host: ProducerId) -> String {
    format!("simulation/attachment/host/{host}")
}

#[allow(
    dead_code,
    reason = "attachment lease keys are consumed only by simulator and supervisor profiles"
)]
pub(crate) fn transaction_liveliness_key(
    world: WorldInstanceId,
    host: ProducerId,
    controller: ProducerId,
) -> String {
    format!("simulation/attachment/transaction/{world}/{host}/{controller}")
}

#[allow(
    dead_code,
    reason = "attachment lease keys are consumed only by simulator and supervisor profiles"
)]
pub(crate) fn removal_liveliness_key(revision: u64, host: ProducerId) -> String {
    format!("simulation/attachment/removed/{revision}/{host}")
}
