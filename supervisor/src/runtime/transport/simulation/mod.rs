//! Public simulation authority and three-phase admission, independent of native physics.
mod authority;
mod phases;
mod products;

use phoxal::communication_transport::{
    cancel_session_subscriptions, DEFAULT_PUBLIC_DEADLINE,
    PublicTransportError, bounded_error_detail,
};

use super::server::{PublicBackendError, PublicSimulationBackend, PublicSimulationContext};
use phoxal::communication::simulation::{
    AcquireAuthorityRequest, AcquireAuthorityResponse, AdmitInitialObservationsRequest,
    AdmitInitialObservationsResponse, AdmitObservationsRequest, AdmitObservationsResponse,
    PrepareBoundaryRequest, PrepareBoundaryResponse, ProgressRequest, ProgressResponse,
    ReleaseAuthorityRequest, ReleaseAuthorityResponse, ResetRequest, ResetResponse, TransitionKey,
};
use phoxal::communication::{PublicOperation, PublicRoute};
use crate::runtime::adapter::{SupervisorAdapter, SupervisorAdapterError};
use prost::Message;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub(crate) use authority::{
    acquire_simulation_authority, progress_simulation, release_backend_authority,
    release_simulation, reset_simulation, revoke_simulation_for_session,
};
pub(crate) use phases::{admit_initial_observations, admit_observations, prepare_boundary};

/// One exclusive, lease-bound simulation authority held by this supervisor.
///
/// The grant is opaque and never reused.  Every simulation request rechecks
/// the protected principal, grant, execution, timeline, and lease before it
/// mutates the boundary, so an old grant cannot become valid after release,
/// reset, or a later acquisition.
#[derive(Clone, Debug)]
pub(crate) struct SimulationAuthority {
    principal: String,
    session_id: Vec<u8>,
    grant: Vec<u8>,
    execution_id: String,
    timeline_id: String,
    model_identity: String,
    quantum_ns: u64,
    boundary: u64,
    lease_deadline: Instant,
    /// Retained phase receipts, bounded by the negotiated byte cap.
    phase_receipts: VecDeque<RetainedSimulationPhase>,
    /// The currently executing transition, if a phase is awaiting a backend
    /// result.  A second transition cannot overtake it.
    in_flight: Option<TransitionKey>,
    /// Highest accepted transition sequence, retained after receipt eviction.
    accepted_sequence_watermark: u64,
    max_product_bytes: usize,
    max_cut_bytes: usize,
    receipt_byte_cap: usize,
    active_phase: Option<PublicOperation>,
    failure: Option<String>,
    resetting: bool,
    retained_reset: Option<(ResetRequest, ResetResponse)>,
}

#[derive(Clone, Debug)]
struct RetainedSimulationPhase {
    operation: PublicOperation,
    transition_key: TransitionKey,
    correlation_id: Vec<u8>,
    request_digest: [u8; 32],
    response: Vec<u8>,
    status: phoxal::communication::simulation::PhaseStatus,
    bytes: usize,
}

const SIMULATION_AUTHORITY_LEASE: Duration = Duration::from_secs(30);
const SIMULATION_GRANT_BYTES: usize = 32;
const MAX_SIMULATION_CORRELATION_BYTES: usize = 64;
const MAX_RETAINED_SIMULATION_PHASES: usize = 64;
const MAX_SIMULATION_PRODUCT_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_SIMULATION_CUT_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_SIMULATION_RECEIPT_BYTE_CAP: usize = 512 * 1024;
#[derive(Clone, Debug)]
struct SimulationPhaseAdmission {
    adapter: Arc<Mutex<SupervisorAdapter>>,
    route: PublicRoute,
    authorized_at_ms: u64,
    started: Instant,
    context: PublicSimulationContext,
    transition_key: TransitionKey,
    request_digest: [u8; 32],
    definition: crate::runtime::adapter::SimulationDefinition,
    operation: PublicOperation,
}

struct PhaseAdmissionContext<'a> {
    adapter: &'a Arc<Mutex<SupervisorAdapter>>,
    authority: &'a Arc<Mutex<Option<SimulationAuthority>>>,
    backend: &'a Arc<dyn PublicSimulationBackend>,
}

#[cfg(test)]
mod tests;
