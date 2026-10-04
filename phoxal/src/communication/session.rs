//! Rust-authored `phoxal.session.v1` wire contracts.

/// The `OpenSessionRequest` wire contract.
#[phoxal::message(package = "phoxal.session.v1")]
#[derive(Eq)]
pub struct OpenSessionRequest {
    /// `protocol`.
    #[phoxal(tag = 1)]
    pub protocol: String,
}

/// The `OpenSessionResponse` wire contract.
#[phoxal::message(package = "phoxal.session.v1")]
#[derive(Eq)]
pub struct OpenSessionResponse {
    /// `session_id`.
    #[phoxal(tag = 1)]
    pub session_id: Vec<u8>,
    /// `protocol`.
    #[phoxal(tag = 2)]
    pub protocol: String,
    /// `lease_ms`.
    #[phoxal(tag = 3)]
    pub lease_ms: u32,
}

/// The `RenewSessionRequest` wire contract.
#[phoxal::message(package = "phoxal.session.v1")]
#[derive(Eq)]
pub struct RenewSessionRequest {
    /// `session_id`.
    #[phoxal(tag = 1)]
    pub session_id: Vec<u8>,
}

/// The `RenewSessionResponse` wire contract.
#[phoxal::message(package = "phoxal.session.v1")]
#[derive(Eq)]
pub struct RenewSessionResponse {
    /// `lease_ms`.
    #[phoxal(tag = 1)]
    pub lease_ms: u32,
}

/// The `CloseSessionRequest` wire contract.
#[phoxal::message(package = "phoxal.session.v1")]
#[derive(Eq)]
pub struct CloseSessionRequest {
    /// `session_id`.
    #[phoxal(tag = 1)]
    pub session_id: Vec<u8>,
}

/// The `CloseSessionResponse` wire contract.
#[phoxal::message(package = "phoxal.session.v1")]
#[derive(Eq)]
pub struct CloseSessionResponse {}

/// The `SupervisorInfoRequest` wire contract.
#[phoxal::message(package = "phoxal.session.v1")]
#[derive(Eq)]
pub struct SupervisorInfoRequest {
    /// `session_id`.
    #[phoxal(tag = 10)]
    pub session_id: Vec<u8>,
}

/// The `SupervisorInfoResponse` wire contract.
#[phoxal::message(package = "phoxal.session.v1")]
#[derive(Eq)]
pub struct SupervisorInfoResponse {
    /// `supervisor_version`.
    #[phoxal(tag = 1)]
    pub supervisor_version: String,
    /// `framework_version`.
    #[phoxal(tag = 2)]
    pub framework_version: String,
}

/// The `SupervisorStatusRequest` wire contract.
#[phoxal::message(package = "phoxal.session.v1")]
#[derive(Eq)]
pub struct SupervisorStatusRequest {
    /// `session_id`.
    #[phoxal(tag = 10)]
    pub session_id: Vec<u8>,
}

/// The `SupervisorStatusResponse` wire contract.
#[phoxal::message(package = "phoxal.session.v1")]
#[derive(Eq)]
pub struct SupervisorStatusResponse {
    /// `state`.
    #[phoxal(tag = 1)]
    pub state: SupervisorState,
    /// `detail`.
    #[phoxal(tag = 2)]
    pub detail: Option<String>,
}

/// The `SupervisorState` wire contract.
#[phoxal::message(package = "phoxal.session.v1")]
pub enum SupervisorState {
    /// `SUPERVISOR_STATE_UNSPECIFIED`.
    Unspecified = 0,
    /// `SUPERVISOR_STATE_IDLE`.
    Idle = 1,
    /// `SUPERVISOR_STATE_PREPARING`.
    Preparing = 2,
    /// `SUPERVISOR_STATE_READY`.
    Ready = 3,
    /// `SUPERVISOR_STATE_FAILED`.
    Failed = 4,
    /// `SUPERVISOR_STATE_STOPPING`.
    Stopping = 5,
}

/// The `ListExecutionsRequest` wire contract.
#[phoxal::message(package = "phoxal.session.v1")]
#[derive(Eq)]
pub struct ListExecutionsRequest {
    /// `page_size`.
    #[phoxal(tag = 1)]
    pub page_size: u32,
    /// `page_token`.
    #[phoxal(tag = 2)]
    pub page_token: Vec<u8>,
    /// `session_id`.
    #[phoxal(tag = 10)]
    pub session_id: Vec<u8>,
}

/// The `ListExecutionsResponse` wire contract.
#[phoxal::message(package = "phoxal.session.v1")]
#[derive(Eq)]
pub struct ListExecutionsResponse {
    /// `executions`.
    #[phoxal(tag = 1)]
    pub executions: Vec<ExecutionSummary>,
    /// `next_page_token`.
    #[phoxal(tag = 2)]
    pub next_page_token: Vec<u8>,
}

/// The `ExecutionSummary` wire contract.
#[phoxal::message(package = "phoxal.session.v1")]
#[derive(Eq)]
pub struct ExecutionSummary {
    /// `execution_id`.
    #[phoxal(tag = 1)]
    pub execution_id: String,
    /// `timeline_id`.
    #[phoxal(tag = 2)]
    pub timeline_id: String,
    /// `state`.
    #[phoxal(tag = 3)]
    pub state: ExecutionState,
}

/// The `ExecutionState` wire contract.
#[phoxal::message(package = "phoxal.session.v1")]
pub enum ExecutionState {
    /// `EXECUTION_STATE_UNSPECIFIED`.
    Unspecified = 0,
    /// `EXECUTION_STATE_PREPARING`.
    Preparing = 1,
    /// `EXECUTION_STATE_READY`.
    Ready = 2,
    /// `EXECUTION_STATE_ACTIVE`.
    Active = 3,
    /// `EXECUTION_STATE_PAUSED`.
    Paused = 4,
    /// `EXECUTION_STATE_FAILED`.
    Failed = 5,
    /// `EXECUTION_STATE_STOPPED`.
    Stopped = 6,
}

/// The `ListMethodsRequest` wire contract.
#[phoxal::message(package = "phoxal.session.v1")]
#[derive(Eq)]
pub struct ListMethodsRequest {
    /// `execution_id`.
    #[phoxal(tag = 1)]
    pub execution_id: String,
    /// `service_instance`.
    #[phoxal(tag = 2)]
    pub service_instance: String,
    /// `page_size`.
    #[phoxal(tag = 3)]
    pub page_size: u32,
    /// `page_token`.
    #[phoxal(tag = 4)]
    pub page_token: Vec<u8>,
    /// `session_id`.
    #[phoxal(tag = 10)]
    pub session_id: Vec<u8>,
}

/// The `ListMethodsResponse` wire contract.
#[phoxal::message(package = "phoxal.session.v1")]
#[derive(Eq)]
pub struct ListMethodsResponse {
    /// `methods`.
    #[phoxal(tag = 1)]
    pub methods: Vec<MethodMetadata>,
    /// `next_page_token`.
    #[phoxal(tag = 2)]
    pub next_page_token: Vec<u8>,
}

/// The `MethodMetadata` wire contract.
#[phoxal::message(package = "phoxal.session.v1")]
#[derive(Eq)]
pub struct MethodMetadata {
    /// `endpoint`.
    #[phoxal(tag = 1)]
    pub endpoint: String,
    /// `shape`.
    #[phoxal(tag = 2)]
    pub shape: MethodShape,
    /// `input_fqn`.
    #[phoxal(tag = 3)]
    pub input_fqn: String,
    /// `output_fqn`.
    #[phoxal(tag = 4)]
    pub output_fqn: String,
    /// `max_message_bytes`.
    #[phoxal(tag = 5)]
    pub max_message_bytes: u32,
    /// `max_buffered_items`.
    #[phoxal(tag = 6)]
    pub max_buffered_items: u32,
    /// `retained_latest`.
    #[phoxal(tag = 7)]
    pub retained_latest: bool,
    /// `lease_valid_for_ms`.
    #[phoxal(tag = 8)]
    pub lease_valid_for_ms: Option<u64>,
}

/// The `MethodShape` wire contract.
#[phoxal::message(package = "phoxal.session.v1")]
pub enum MethodShape {
    /// `METHOD_SHAPE_UNSPECIFIED`.
    Unspecified = 0,
    /// `METHOD_SHAPE_CALL`.
    Call = 1,
    /// `METHOD_SHAPE_OBSERVATION`.
    Observation = 2,
}

/// The `BindMethodRequest` wire contract.
#[phoxal::message(package = "phoxal.session.v1")]
#[derive(Eq)]
pub struct BindMethodRequest {
    /// `session_id`.
    #[phoxal(tag = 1)]
    pub session_id: Vec<u8>,
    /// `execution_id`.
    #[phoxal(tag = 2)]
    pub execution_id: String,
    /// `service_instance`.
    #[phoxal(tag = 3)]
    pub service_instance: String,
    /// `expected`.
    #[phoxal(tag = 4)]
    pub expected: Option<MethodMetadata>,
}

/// The `BindMethodResponse` wire contract.
#[phoxal::message(package = "phoxal.session.v1")]
#[derive(Eq)]
pub struct BindMethodResponse {
    /// `binding_id`.
    #[phoxal(tag = 1)]
    pub binding_id: Vec<u8>,
    /// `admitted`.
    #[phoxal(tag = 2)]
    pub admitted: Option<MethodMetadata>,
}

/// The `OperationRequest` wire contract.
#[phoxal::message(package = "phoxal.session.v1")]
#[derive(Eq)]
pub struct OperationRequest {
    /// `session_id`.
    #[phoxal(tag = 1)]
    pub session_id: Vec<u8>,
    /// `binding_id`.
    #[phoxal(tag = 2)]
    pub binding_id: Vec<u8>,
    /// `correlation_id`.
    #[phoxal(tag = 3)]
    pub correlation_id: Vec<u8>,
    /// `execution_id`.
    #[phoxal(tag = 4)]
    pub execution_id: String,
    /// `timeline_id`.
    #[phoxal(tag = 5)]
    pub timeline_id: String,
    /// `payload`.
    #[phoxal(tag = 6)]
    pub payload: Vec<u8>,
    /// `timeout_ms`.
    #[phoxal(tag = 7)]
    pub timeout_ms: u32,
}

/// The `OperationResponse` wire contract.
#[phoxal::message(package = "phoxal.session.v1")]
#[derive(Eq)]
pub struct OperationResponse {
    /// `session_id`.
    #[phoxal(tag = 1)]
    pub session_id: Vec<u8>,
    /// `binding_id`.
    #[phoxal(tag = 2)]
    pub binding_id: Vec<u8>,
    /// `correlation_id`.
    #[phoxal(tag = 3)]
    pub correlation_id: Vec<u8>,
    /// `execution_id`.
    #[phoxal(tag = 4)]
    pub execution_id: String,
    /// `timeline_id`.
    #[phoxal(tag = 5)]
    pub timeline_id: String,
    /// `outcome`.
    #[phoxal(tag = 6)]
    pub outcome: OperationOutcome,
    /// `payload`.
    #[phoxal(tag = 7)]
    pub payload: Vec<u8>,
    /// `detail`.
    #[phoxal(tag = 8)]
    pub detail: Option<String>,
}

/// The `OperationOutcome` wire contract.
#[phoxal::message(package = "phoxal.session.v1")]
pub enum OperationOutcome {
    /// `OPERATION_OUTCOME_UNSPECIFIED`.
    Unspecified = 0,
    /// `OPERATION_OUTCOME_RECEIVED`.
    Received = 1,
    /// `OPERATION_OUTCOME_NOT_SENT`.
    NotSent = 2,
    /// `OPERATION_OUTCOME_REJECTED_BEFORE_ADMISSION`.
    RejectedBeforeAdmission = 3,
    /// `OPERATION_OUTCOME_UNKNOWN`.
    Unknown = 4,
}

/// The `SubscriptionRequest` wire contract.
#[phoxal::message(package = "phoxal.session.v1")]
#[derive(Eq)]
pub struct SubscriptionRequest {
    /// `session_id`.
    #[phoxal(tag = 1)]
    pub session_id: Vec<u8>,
    /// `binding_id`.
    #[phoxal(tag = 2)]
    pub binding_id: Vec<u8>,
    /// `subscription_id`.
    #[phoxal(tag = 3)]
    pub subscription_id: Vec<u8>,
    /// `execution_id`.
    #[phoxal(tag = 4)]
    pub execution_id: String,
    /// `timeline_id`.
    #[phoxal(tag = 5)]
    pub timeline_id: String,
    /// `max_buffered_items`.
    #[phoxal(tag = 6)]
    pub max_buffered_items: u32,
    /// `max_buffered_bytes`.
    #[phoxal(tag = 7)]
    pub max_buffered_bytes: u32,
}

/// Admission acknowledges the bounded observation cursor before publications
/// are emitted on the dedicated observation key.  `initial` is present only
/// for a State watch.  Event, Sample, and Stream subscriptions begin at this
/// admitted boundary and therefore carry no synthetic initial record.
#[phoxal::message(package = "phoxal.session.v1")]
#[derive(Eq)]
pub struct SubscriptionAdmission {
    /// `session_id`.
    #[phoxal(tag = 1)]
    pub session_id: Vec<u8>,
    /// `binding_id`.
    #[phoxal(tag = 2)]
    pub binding_id: Vec<u8>,
    /// `subscription_id`.
    #[phoxal(tag = 3)]
    pub subscription_id: Vec<u8>,
    /// `execution_id`.
    #[phoxal(tag = 4)]
    pub execution_id: String,
    /// `timeline_id`.
    #[phoxal(tag = 5)]
    pub timeline_id: String,
    /// `initial`.
    #[phoxal(tag = 6)]
    pub initial: Option<SubscriptionRecord>,
}

/// The `SubscriptionRecord` wire contract.
#[phoxal::message(package = "phoxal.session.v1")]
#[derive(Eq)]
pub struct SubscriptionRecord {
    /// `session_id`.
    #[phoxal(tag = 1)]
    pub session_id: Vec<u8>,
    /// `binding_id`.
    #[phoxal(tag = 2)]
    pub binding_id: Vec<u8>,
    /// `subscription_id`.
    #[phoxal(tag = 3)]
    pub subscription_id: Vec<u8>,
    /// `execution_id`.
    #[phoxal(tag = 4)]
    pub execution_id: String,
    /// `timeline_id`.
    #[phoxal(tag = 5)]
    pub timeline_id: String,
    /// `revision`.
    #[phoxal(tag = 6)]
    pub revision: u64,
    /// `kind`.
    #[phoxal(tag = 7)]
    pub kind: RecordKind,
    /// `payload`.
    #[phoxal(tag = 8)]
    pub payload: Vec<u8>,
    /// `dropped`.
    #[phoxal(tag = 9)]
    pub dropped: u64,
    /// `detail`.
    #[phoxal(tag = 10)]
    pub detail: Option<String>,
}

/// The `RecordKind` wire contract.
#[phoxal::message(package = "phoxal.session.v1")]
pub enum RecordKind {
    /// `RECORD_KIND_UNSPECIFIED`.
    Unspecified = 0,
    /// `RECORD_KIND_INITIAL_ABSENT`.
    InitialAbsent = 1,
    /// `RECORD_KIND_VALUE`.
    Value = 2,
    /// `RECORD_KIND_GAP`.
    Gap = 3,
    /// `RECORD_KIND_END`.
    End = 4,
    /// `RECORD_KIND_FAILED`.
    Failed = 5,
}
