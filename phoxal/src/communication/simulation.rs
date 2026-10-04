//! Rust-authored `phoxal.simulation.v1` wire contracts.

/// The `AcquireAuthorityRequest` wire contract.
#[phoxal::message(package = "phoxal.simulation.v1")]
#[derive(Eq)]
pub struct AcquireAuthorityRequest {
    /// `execution_id`.
    #[phoxal(tag = 1)]
    pub execution_id: String,
    /// `model_identity`.
    #[phoxal(tag = 2)]
    pub model_identity: String,
    /// `quantum_ns`.
    #[phoxal(tag = 3)]
    pub quantum_ns: u64,
    /// `providers`.
    #[phoxal(tag = 4)]
    pub providers: Vec<ProviderRequirement>,
    /// `session_id`.
    #[phoxal(tag = 5)]
    pub session_id: Vec<u8>,
    /// `correlation_id`.
    #[phoxal(tag = 6)]
    pub correlation_id: Vec<u8>,
    /// `max_product_bytes`.
    #[phoxal(tag = 7)]
    pub max_product_bytes: u64,
    /// `max_cut_bytes`.
    #[phoxal(tag = 8)]
    pub max_cut_bytes: u64,
}

/// The `ProviderRequirement` wire contract.
#[phoxal::message(package = "phoxal.simulation.v1")]
#[derive(Eq)]
pub struct ProviderRequirement {
    /// `service_instance`.
    #[phoxal(tag = 1)]
    pub service_instance: String,
    /// `port`.
    #[phoxal(tag = 2)]
    pub port: String,
    /// `payload_fqn`.
    #[phoxal(tag = 3)]
    pub payload_fqn: String,
    /// Numeric phoxal.session.v1.MethodShape value.
    #[phoxal(tag = 4)]
    pub shape: i32,
    /// `input_fqn`.
    #[phoxal(tag = 5)]
    pub input_fqn: String,
    /// Immutable source cadence in millionths of one hertz.
    #[phoxal(tag = 6)]
    pub rate_microhertz: u64,
}

/// The `AcquireAuthorityResponse` wire contract.
#[phoxal::message(package = "phoxal.simulation.v1")]
#[derive(Eq)]
pub struct AcquireAuthorityResponse {
    /// `authority_grant`.
    #[phoxal(tag = 1)]
    pub authority_grant: Vec<u8>,
    /// `timeline_id`.
    #[phoxal(tag = 2)]
    pub timeline_id: String,
    /// `boundary`.
    #[phoxal(tag = 3)]
    pub boundary: u64,
    /// `lease_ms`.
    #[phoxal(tag = 4)]
    pub lease_ms: u32,
    /// `session_id`.
    #[phoxal(tag = 5)]
    pub session_id: Vec<u8>,
    /// `execution_id`.
    #[phoxal(tag = 6)]
    pub execution_id: String,
    /// `model_identity`.
    #[phoxal(tag = 7)]
    pub model_identity: String,
    /// `quantum_ns`.
    #[phoxal(tag = 8)]
    pub quantum_ns: u64,
    /// `correlation_id`.
    #[phoxal(tag = 9)]
    pub correlation_id: Vec<u8>,
    /// `max_product_bytes`.
    #[phoxal(tag = 10)]
    pub max_product_bytes: u64,
    /// `max_cut_bytes`.
    #[phoxal(tag = 11)]
    pub max_cut_bytes: u64,
}

/// The `TransitionKey` wire contract.
#[phoxal::message(package = "phoxal.simulation.v1")]
#[derive(Eq)]
pub struct TransitionKey {
    /// `session_id`.
    #[phoxal(tag = 1)]
    pub session_id: Vec<u8>,
    /// `execution_id`.
    #[phoxal(tag = 2)]
    pub execution_id: String,
    /// `timeline_id`.
    #[phoxal(tag = 3)]
    pub timeline_id: String,
    /// `authority_grant`.
    #[phoxal(tag = 4)]
    pub authority_grant: Vec<u8>,
    /// `boundary`.
    #[phoxal(tag = 5)]
    pub boundary: u64,
    /// `operation_sequence`.
    #[phoxal(tag = 6)]
    pub operation_sequence: u64,
}

/// The `ProductDisposition` wire contract.
#[phoxal::message(package = "phoxal.simulation.v1")]
pub enum ProductDisposition {
    /// `PRODUCT_DISPOSITION_UNSPECIFIED`.
    Unspecified = 0,
    /// `PRODUCT_DISPOSITION_PRESENT`.
    Present = 1,
    /// `PRODUCT_DISPOSITION_EMPTY`.
    Empty = 2,
    /// `PRODUCT_DISPOSITION_NOT_DUE`.
    NotDue = 3,
}

/// The `ProductMembership` wire contract.
#[phoxal::message(package = "phoxal.simulation.v1")]
#[derive(Eq)]
pub struct ProductMembership {
    /// `producer`.
    #[phoxal(tag = 1)]
    pub producer: String,
    /// `port`.
    #[phoxal(tag = 2)]
    pub port: String,
    /// `producer_incarnation`.
    #[phoxal(tag = 3)]
    pub producer_incarnation: Vec<u8>,
    /// `sequence`.
    #[phoxal(tag = 4)]
    pub sequence: u64,
    /// `capture_boundary`.
    #[phoxal(tag = 5)]
    pub capture_boundary: u64,
    /// `capture_time_ns`.
    #[phoxal(tag = 6)]
    pub capture_time_ns: u64,
    /// `disposition`.
    #[phoxal(tag = 7)]
    pub disposition: ProductDisposition,
    /// `item_count`.
    #[phoxal(tag = 8)]
    pub item_count: u64,
    /// `encoded_bytes`.
    #[phoxal(tag = 9)]
    pub encoded_bytes: u64,
    /// `payload_digest`.
    #[phoxal(tag = 10)]
    pub payload_digest: Vec<u8>,
}

/// The `Observation` wire contract.
#[phoxal::message(package = "phoxal.simulation.v1")]
#[derive(Eq)]
pub struct Observation {
    /// `membership`.
    #[phoxal(tag = 1)]
    pub membership: Option<ProductMembership>,
    /// `payload`.
    #[phoxal(tag = 2)]
    pub payload: Vec<u8>,
}

/// The `Actuation` wire contract.
#[phoxal::message(package = "phoxal.simulation.v1")]
#[derive(Eq)]
pub struct Actuation {
    /// `membership`.
    #[phoxal(tag = 1)]
    pub membership: Option<ProductMembership>,
    /// `payload`.
    #[phoxal(tag = 2)]
    pub payload: Vec<u8>,
    /// `valid_until_ns`.
    #[phoxal(tag = 3)]
    pub valid_until_ns: u64,
}

/// The `ProductReceipt` wire contract.
#[phoxal::message(package = "phoxal.simulation.v1")]
#[derive(Eq)]
pub struct ProductReceipt {
    /// `membership`.
    #[phoxal(tag = 1)]
    pub membership: Option<ProductMembership>,
}

/// The `PhaseStatus` wire contract.
#[phoxal::message(package = "phoxal.simulation.v1")]
pub enum PhaseStatus {
    /// `PHASE_STATUS_UNSPECIFIED`.
    Unspecified = 0,
    /// `PHASE_STATUS_INITIAL_ADMITTED`.
    InitialAdmitted = 1,
    /// `PHASE_STATUS_PREPARED`.
    Prepared = 2,
    /// `PHASE_STATUS_OBSERVATIONS_ADMITTED`.
    ObservationsAdmitted = 3,
}

/// The `CutReceipt` wire contract.
#[phoxal::message(package = "phoxal.simulation.v1")]
#[derive(Eq)]
pub struct CutReceipt {
    /// `transition_key`.
    #[phoxal(tag = 1)]
    pub transition_key: Option<TransitionKey>,
    /// `correlation_id`.
    #[phoxal(tag = 2)]
    pub correlation_id: Vec<u8>,
    /// `membership_digest`.
    #[phoxal(tag = 4)]
    pub membership_digest: Vec<u8>,
    /// `products`.
    #[phoxal(tag = 5)]
    pub products: Vec<ProductMembership>,
    /// `prepared_boundary`.
    #[phoxal(tag = 6)]
    pub prepared_boundary: u64,
    /// `admitted_observation_boundary`.
    #[phoxal(tag = 7)]
    pub admitted_observation_boundary: u64,
    /// `status`.
    #[phoxal(tag = 8)]
    pub status: PhaseStatus,
}

/// The `AdmitInitialObservationsRequest` wire contract.
#[phoxal::message(package = "phoxal.simulation.v1")]
#[derive(Eq)]
pub struct AdmitInitialObservationsRequest {
    /// `transition_key`.
    #[phoxal(tag = 1)]
    pub transition_key: Option<TransitionKey>,
    /// `observations`.
    #[phoxal(tag = 2)]
    pub observations: Vec<Observation>,
    /// `membership_digest`.
    #[phoxal(tag = 3)]
    pub membership_digest: Vec<u8>,
    /// `correlation_id`.
    #[phoxal(tag = 4)]
    pub correlation_id: Vec<u8>,
}

/// The `AdmitInitialObservationsResponse` wire contract.
#[phoxal::message(package = "phoxal.simulation.v1")]
#[derive(Eq)]
pub struct AdmitInitialObservationsResponse {
    /// `receipt`.
    #[phoxal(tag = 1)]
    pub receipt: Option<CutReceipt>,
}

/// The `PrepareBoundaryRequest` wire contract.
#[phoxal::message(package = "phoxal.simulation.v1")]
#[derive(Eq)]
pub struct PrepareBoundaryRequest {
    /// `transition_key`.
    #[phoxal(tag = 1)]
    pub transition_key: Option<TransitionKey>,
    /// `correlation_id`.
    #[phoxal(tag = 2)]
    pub correlation_id: Vec<u8>,
}

/// The `PrepareBoundaryResponse` wire contract.
#[phoxal::message(package = "phoxal.simulation.v1")]
#[derive(Eq)]
pub struct PrepareBoundaryResponse {
    /// `receipt`.
    #[phoxal(tag = 1)]
    pub receipt: Option<CutReceipt>,
    /// `actuation`.
    #[phoxal(tag = 2)]
    pub actuation: Vec<Actuation>,
}

/// The `AdmitObservationsRequest` wire contract.
#[phoxal::message(package = "phoxal.simulation.v1")]
#[derive(Eq)]
pub struct AdmitObservationsRequest {
    /// `transition_key`.
    #[phoxal(tag = 1)]
    pub transition_key: Option<TransitionKey>,
    /// `observations`.
    #[phoxal(tag = 2)]
    pub observations: Vec<Observation>,
    /// `membership_digest`.
    #[phoxal(tag = 3)]
    pub membership_digest: Vec<u8>,
    /// `correlation_id`.
    #[phoxal(tag = 4)]
    pub correlation_id: Vec<u8>,
}

/// The `AdmitObservationsResponse` wire contract.
#[phoxal::message(package = "phoxal.simulation.v1")]
#[derive(Eq)]
pub struct AdmitObservationsResponse {
    /// `receipt`.
    #[phoxal(tag = 1)]
    pub receipt: Option<CutReceipt>,
}

/// The `ResetRequest` wire contract.
#[phoxal::message(package = "phoxal.simulation.v1")]
#[derive(Eq)]
pub struct ResetRequest {
    /// `authority_grant`.
    #[phoxal(tag = 1)]
    pub authority_grant: Vec<u8>,
    /// `execution_id`.
    #[phoxal(tag = 2)]
    pub execution_id: String,
    /// `timeline_id`.
    #[phoxal(tag = 3)]
    pub timeline_id: String,
    /// `completed_boundary`.
    #[phoxal(tag = 4)]
    pub completed_boundary: u64,
    /// `session_id`.
    #[phoxal(tag = 5)]
    pub session_id: Vec<u8>,
    /// `correlation_id`.
    #[phoxal(tag = 6)]
    pub correlation_id: Vec<u8>,
}

/// The `ResetResponse` wire contract.
#[phoxal::message(package = "phoxal.simulation.v1")]
#[derive(Eq)]
pub struct ResetResponse {
    /// `next_timeline_id`.
    #[phoxal(tag = 1)]
    pub next_timeline_id: String,
    /// `boundary`.
    #[phoxal(tag = 2)]
    pub boundary: u64,
    /// `session_id`.
    #[phoxal(tag = 3)]
    pub session_id: Vec<u8>,
    /// `execution_id`.
    #[phoxal(tag = 4)]
    pub execution_id: String,
    /// `authority_grant`.
    #[phoxal(tag = 5)]
    pub authority_grant: Vec<u8>,
    /// `correlation_id`.
    #[phoxal(tag = 6)]
    pub correlation_id: Vec<u8>,
    /// `previous_timeline_id`.
    #[phoxal(tag = 7)]
    pub previous_timeline_id: String,
    /// `requested_boundary`.
    #[phoxal(tag = 8)]
    pub requested_boundary: u64,
}

/// The `ReleaseAuthorityRequest` wire contract.
#[phoxal::message(package = "phoxal.simulation.v1")]
#[derive(Eq)]
pub struct ReleaseAuthorityRequest {
    /// `authority_grant`.
    #[phoxal(tag = 1)]
    pub authority_grant: Vec<u8>,
    /// `session_id`.
    #[phoxal(tag = 2)]
    pub session_id: Vec<u8>,
    /// `correlation_id`.
    #[phoxal(tag = 3)]
    pub correlation_id: Vec<u8>,
}

/// The `ReleaseAuthorityResponse` wire contract.
#[phoxal::message(package = "phoxal.simulation.v1")]
#[derive(Eq)]
pub struct ReleaseAuthorityResponse {
    /// `session_id`.
    #[phoxal(tag = 1)]
    pub session_id: Vec<u8>,
    /// `authority_grant`.
    #[phoxal(tag = 2)]
    pub authority_grant: Vec<u8>,
    /// `correlation_id`.
    #[phoxal(tag = 3)]
    pub correlation_id: Vec<u8>,
    /// `execution_id`.
    #[phoxal(tag = 4)]
    pub execution_id: String,
    /// `timeline_id`.
    #[phoxal(tag = 5)]
    pub timeline_id: String,
    /// `completed_boundary`.
    #[phoxal(tag = 6)]
    pub completed_boundary: u64,
}

/// The `ProgressRequest` wire contract.
#[phoxal::message(package = "phoxal.simulation.v1")]
#[derive(Eq)]
pub struct ProgressRequest {
    /// `authority_grant`.
    #[phoxal(tag = 1)]
    pub authority_grant: Vec<u8>,
    /// `session_id`.
    #[phoxal(tag = 2)]
    pub session_id: Vec<u8>,
    /// `correlation_id`.
    #[phoxal(tag = 3)]
    pub correlation_id: Vec<u8>,
}

/// The `ProgressResponse` wire contract.
#[phoxal::message(package = "phoxal.simulation.v1")]
#[derive(Eq)]
pub struct ProgressResponse {
    /// `execution_id`.
    #[phoxal(tag = 1)]
    pub execution_id: String,
    /// `timeline_id`.
    #[phoxal(tag = 2)]
    pub timeline_id: String,
    /// `completed_boundary`.
    #[phoxal(tag = 3)]
    pub completed_boundary: u64,
    /// `failed`.
    #[phoxal(tag = 4)]
    pub failed: bool,
    /// `detail`.
    #[phoxal(tag = 5)]
    pub detail: Option<String>,
    /// `session_id`.
    #[phoxal(tag = 6)]
    pub session_id: Vec<u8>,
    /// `authority_grant`.
    #[phoxal(tag = 7)]
    pub authority_grant: Vec<u8>,
    /// `correlation_id`.
    #[phoxal(tag = 8)]
    pub correlation_id: Vec<u8>,
}
