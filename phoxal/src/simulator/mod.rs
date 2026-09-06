//! Narrow SDK for one per-Robot Live simulator controller.
//!
//! A controller joins one supervised execution, observes its source-bound
//! world attachment, stands in for simulated component drivers, and uses the
//! ordinary typed bus lanes for device IO. It never owns or replaces execution
//! time. Every Live transition is stamped from the execution's existing
//! monotonic timeline, and `simulation/step` is passive progress published
//! after that transition's outputs.

mod attachment;
mod bootstrap;
mod controller;
mod error;
mod host;
mod io;
mod observation;
mod transition;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::bus::session::{BusConfig, BusOwner};
use crate::bus::{
    BusCloseReport, BusError, BusHandle, DEFAULT_QUERY_TIMEOUT, Endpoint, EventPublisher,
    FixedSourceLease, KeyLivelinessToken, LocalInstant, Observed, ParticipantReadyEvents,
    ParticipantReadyToken, Publish, Querier, QueryError, ReceiveTerminal, RobotEndpoint,
    RobotInstant, Sample, SamplePublisher, Setpoint, SetpointReceiver, SourceLabel,
    SourceLabelError, State, StatePublisher, Subscribe, Topic,
};
use crate::identity::{ExecutionId, ParticipantId, ProducerId};
use crate::model::world::{LiveAttachmentBoundary, WorldInstanceId, WorldProgress};
use crate::simulation::api::StepEvent;
use crate::supervisor::api::simulation::attach::{AttachRequest, AttachResponse};
use crate::supervisor::api::simulation::end::{EndRequest, EndResponse};
use crate::supervisor::api::simulation::{
    SimulationAttachmentPhase, SimulationAttachmentState, SimulationEndReason,
};
use crate::supervisor::api::time_domain::{TimeDomain, TimeMode};

pub use attachment::SimulationAttachTransaction;
use bootstrap::{LiveBootstrap, open_live_bootstrap};
pub use controller::{SimulatorConnectOptions, SimulatorSession};
pub use error::{SimulatorCloseError, SimulatorError};
pub use host::{SimulationHostConnectOptions, SimulationHostSession};
pub use io::{LiveSamplePublisher, LiveSetpointReceiver, LiveStatePublisher};
pub use transition::{ActiveBoundaryStamp, LiveTransitionStamp};

use attachment::{AttachTransactionResponse, validate_attach_response};
use observation::{install_active_controller_binding, observe_attachment};
use transition::{admit_step_event, ensure_live_publication, validate_next_progress};

const ATTACHMENT_TRANSITION_CAPACITY: usize = 32;

#[cfg(test)]
mod live_contract_tests;
