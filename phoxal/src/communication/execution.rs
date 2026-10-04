//! Rust-authored `phoxal.execution.v1` wire contracts.

/// Execution admission identity for the typed-enum decoding contract.
pub const PROTOCOL: &str = "phoxal.execution.v1.r1";

/// The `ContractRequirement` wire contract.
#[phoxal::message(package = "phoxal.execution.v1")]
#[derive(Eq)]
pub struct ContractRequirement {
    /// `protocol`.
    #[phoxal(tag = 1)]
    pub protocol: String,
    /// `capabilities`.
    #[phoxal(tag = 2)]
    pub capabilities: Vec<String>,
}

/// The supervisor selects one scheduling owner for the complete execution.
/// Hardware runtimes retain their host-monotonic cadence; controlled runtimes
/// wait for an explicit Invocation at each due boundary.
#[phoxal::message(package = "phoxal.execution.v1")]
pub enum ExecutionMode {
    /// `EXECUTION_MODE_UNSPECIFIED`.
    Unspecified = 0,
    /// `EXECUTION_MODE_HARDWARE`.
    Hardware = 1,
    /// `EXECUTION_MODE_CONTROLLED`.
    Controlled = 2,
}

/// The `AdmitExecutionRequest` wire contract.
#[phoxal::message(package = "phoxal.execution.v1")]
#[derive(Eq)]
pub struct AdmitExecutionRequest {
    /// `execution_id`.
    #[phoxal(tag = 1)]
    pub execution_id: String,
    /// `timeline_id`.
    #[phoxal(tag = 2)]
    pub timeline_id: String,
    /// `required_contracts`.
    #[phoxal(tag = 4)]
    pub required_contracts: Vec<ContractRequirement>,
    /// `mode`.
    #[phoxal(tag = 5)]
    pub mode: ExecutionMode,
    /// `quantum_ns`.
    #[phoxal(tag = 6)]
    pub quantum_ns: u64,
}

/// The `AdmitExecutionResponse` wire contract.
#[phoxal::message(package = "phoxal.execution.v1")]
#[derive(Eq)]
pub struct AdmitExecutionResponse {
    /// `admitted`.
    #[phoxal(tag = 1)]
    pub admitted: bool,
    /// `unsupported_contracts`.
    #[phoxal(tag = 2)]
    pub unsupported_contracts: Vec<String>,
    /// `detail`.
    #[phoxal(tag = 3)]
    pub detail: Option<String>,
}

/// The `Ready` wire contract.
#[phoxal::message(package = "phoxal.execution.v1")]
#[derive(Eq)]
pub struct Ready {
    /// `execution_id`.
    #[phoxal(tag = 1)]
    pub execution_id: String,
    /// `timeline_id`.
    #[phoxal(tag = 2)]
    pub timeline_id: String,
    /// `runtime_instance`.
    #[phoxal(tag = 3)]
    pub runtime_instance: String,
}

/// Publish initialized State only after every controlled receiver is ready.
/// This phase does not invoke Runtime::step or advance an invocation counter.
#[phoxal::message(package = "phoxal.execution.v1")]
#[derive(Eq)]
pub struct InitializeStateRequest {
    /// `execution_id`.
    #[phoxal(tag = 1)]
    pub execution_id: String,
    /// `timeline_id`.
    #[phoxal(tag = 2)]
    pub timeline_id: String,
}

/// The `InitializeStateResponse` wire contract.
#[phoxal::message(package = "phoxal.execution.v1")]
#[derive(Eq)]
pub struct InitializeStateResponse {
    /// `execution_id`.
    #[phoxal(tag = 1)]
    pub execution_id: String,
    /// `timeline_id`.
    #[phoxal(tag = 2)]
    pub timeline_id: String,
    /// `runtime_instance`.
    #[phoxal(tag = 3)]
    pub runtime_instance: String,
    /// `required_products`.
    #[phoxal(tag = 4)]
    pub required_products: Vec<ProductReceipt>,
    /// Exact graph records emitted by this accepted invocation.  ProductReceipt
    /// remains the compact per-port summary used by the supervisor; these
    /// records let it wait for every receiver admission without guessing item
    /// identity from an aggregate count.
    #[phoxal(tag = 5)]
    pub required_deliveries: Vec<DeliveryReceipt>,
}

/// Capture the accepted Read views before any runtime invokes this boundary.
#[phoxal::message(package = "phoxal.execution.v1")]
#[derive(Eq)]
pub struct PinReadViewsRequest {
    /// `execution_id`.
    #[phoxal(tag = 1)]
    pub execution_id: String,
    /// `timeline_id`.
    #[phoxal(tag = 2)]
    pub timeline_id: String,
    /// `boundary`.
    #[phoxal(tag = 3)]
    pub boundary: u64,
}

/// The `PinReadViewsResponse` wire contract.
#[phoxal::message(package = "phoxal.execution.v1")]
#[derive(Eq)]
pub struct PinReadViewsResponse {
    /// `execution_id`.
    #[phoxal(tag = 1)]
    pub execution_id: String,
    /// `timeline_id`.
    #[phoxal(tag = 2)]
    pub timeline_id: String,
    /// `boundary`.
    #[phoxal(tag = 3)]
    pub boundary: u64,
    /// `runtime_instance`.
    #[phoxal(tag = 4)]
    pub runtime_instance: String,
    /// `admitted`.
    #[phoxal(tag = 5)]
    pub admitted: bool,
    /// `detail`.
    #[phoxal(tag = 6)]
    pub detail: Option<String>,
}

/// The `Invocation` wire contract.
#[phoxal::message(package = "phoxal.execution.v1")]
#[derive(Eq)]
pub struct Invocation {
    /// `execution_id`.
    #[phoxal(tag = 1)]
    pub execution_id: String,
    /// `timeline_id`.
    #[phoxal(tag = 2)]
    pub timeline_id: String,
    /// `runtime_instance`.
    #[phoxal(tag = 3)]
    pub runtime_instance: String,
    /// `boundary`.
    #[phoxal(tag = 4)]
    pub boundary: u64,
    /// `logical_time_ns`.
    #[phoxal(tag = 5)]
    pub logical_time_ns: u64,
}

/// The `InvocationAccepted` wire contract.
#[phoxal::message(package = "phoxal.execution.v1")]
#[derive(Eq)]
pub struct InvocationAccepted {
    /// `execution_id`.
    #[phoxal(tag = 1)]
    pub execution_id: String,
    /// `timeline_id`.
    #[phoxal(tag = 2)]
    pub timeline_id: String,
    /// `runtime_instance`.
    #[phoxal(tag = 3)]
    pub runtime_instance: String,
    /// `boundary`.
    #[phoxal(tag = 4)]
    pub boundary: u64,
    /// `required_products`.
    #[phoxal(tag = 5)]
    pub required_products: Vec<ProductReceipt>,
    /// `required_inputs`.
    #[phoxal(tag = 6)]
    pub required_inputs: Vec<InputReceipt>,
    /// `actuations`.
    #[phoxal(tag = 7)]
    pub actuations: Vec<Actuation>,
    /// Exact graph records emitted by this accepted invocation.  ProductReceipt
    /// remains the compact per-port summary used by the supervisor; these
    /// records let it wait for every receiver admission without guessing item
    /// identity from an aggregate count.
    #[phoxal(tag = 8)]
    pub required_deliveries: Vec<DeliveryReceipt>,
}

/// The `ProductReceipt` wire contract.
#[phoxal::message(package = "phoxal.execution.v1")]
#[derive(Eq)]
pub struct ProductReceipt {
    /// `port`.
    #[phoxal(tag = 1)]
    pub port: String,
    /// `sequence`.
    #[phoxal(tag = 2)]
    pub sequence: u64,
    /// `items`.
    #[phoxal(tag = 3)]
    pub items: u32,
    /// `bytes`.
    #[phoxal(tag = 4)]
    pub bytes: u64,
}

/// The `InputReceipt` wire contract.
#[phoxal::message(package = "phoxal.execution.v1")]
#[derive(Eq)]
pub struct InputReceipt {
    /// Consumer field and producer port are distinct graph identities.
    #[phoxal(tag = 6)]
    pub input: String,
    /// `source`.
    #[phoxal(tag = 1)]
    pub source: String,
    /// `port`.
    #[phoxal(tag = 2)]
    pub port: String,
    /// `sequence`.
    #[phoxal(tag = 3)]
    pub sequence: u64,
    /// `items`.
    #[phoxal(tag = 4)]
    pub items: u32,
    /// `bytes`.
    #[phoxal(tag = 5)]
    pub bytes: u64,
}

/// The `Actuation` wire contract.
#[phoxal::message(package = "phoxal.execution.v1")]
#[derive(Eq)]
pub struct Actuation {
    /// `port`.
    #[phoxal(tag = 1)]
    pub port: String,
    /// `payload`.
    #[phoxal(tag = 2)]
    pub payload: Vec<u8>,
    /// `valid_until_ns`.
    #[phoxal(tag = 3)]
    pub valid_until_ns: u64,
}

/// One output record that must be admitted by a graph receiver.  The target is
/// optional for ordinary publications: the supervisor expands that record over
/// the immutable graph fan-out.  Requests carry their exact target because the
/// generated activation already resolved it before publication. Reply targets
/// name the exact caller instance and input field; request targets name the
/// server instance, whose public method resolves to one admitted handler field.
#[phoxal::message(package = "phoxal.execution.v1")]
#[derive(Eq)]
pub struct DeliveryReceipt {
    /// `port`.
    #[phoxal(tag = 1)]
    pub port: String,
    /// `direction`.
    #[phoxal(tag = 2)]
    pub direction: String,
    /// Exact receiving instance and private field, separated by a dot.
    #[phoxal(tag = 3)]
    pub target: String,
    /// `sequence`.
    #[phoxal(tag = 4)]
    pub sequence: u64,
    /// `item`.
    #[phoxal(tag = 5)]
    pub item: u32,
    /// `bytes`.
    #[phoxal(tag = 6)]
    pub bytes: u64,
}

/// Receiver-owned acknowledgement for one exact graph record.  This message
/// is carried on a private execution leg and is never part of a public method
/// or session contract.
#[phoxal::message(package = "phoxal.execution.v1")]
#[derive(Eq)]
pub struct DeliveryAck {
    /// `execution_id`.
    #[phoxal(tag = 1)]
    pub execution_id: String,
    /// `timeline_id`.
    #[phoxal(tag = 2)]
    pub timeline_id: String,
    /// `boundary`.
    #[phoxal(tag = 3)]
    pub boundary: u64,
    /// `source`.
    #[phoxal(tag = 4)]
    pub source: String,
    /// Exact receiving instance and private field, separated by a dot.
    #[phoxal(tag = 5)]
    pub target: String,
    /// `port`.
    #[phoxal(tag = 6)]
    pub port: String,
    /// `direction`.
    #[phoxal(tag = 7)]
    pub direction: String,
    /// `sequence`.
    #[phoxal(tag = 8)]
    pub sequence: u64,
    /// `item`.
    #[phoxal(tag = 9)]
    pub item: u32,
    /// `bytes`.
    #[phoxal(tag = 10)]
    pub bytes: u64,
    /// `admitted`.
    #[phoxal(tag = 11)]
    pub admitted: bool,
    /// `detail`.
    #[phoxal(tag = 12)]
    pub detail: Option<String>,
}

/// The `ResetExecutionRequest` wire contract.
#[phoxal::message(package = "phoxal.execution.v1")]
#[derive(Eq)]
pub struct ResetExecutionRequest {
    /// `execution_id`.
    #[phoxal(tag = 1)]
    pub execution_id: String,
    /// `retired_timeline_id`.
    #[phoxal(tag = 2)]
    pub retired_timeline_id: String,
    /// `next_timeline_id`.
    #[phoxal(tag = 3)]
    pub next_timeline_id: String,
    /// `completed_boundary`.
    #[phoxal(tag = 4)]
    pub completed_boundary: u64,
}

/// The `ResetExecutionResponse` wire contract.
#[phoxal::message(package = "phoxal.execution.v1")]
#[derive(Eq)]
pub struct ResetExecutionResponse {
    /// `accepted`.
    #[phoxal(tag = 1)]
    pub accepted: bool,
    /// `detail`.
    #[phoxal(tag = 2)]
    pub detail: Option<String>,
}

/// The `RuntimeFailure` wire contract.
#[phoxal::message(package = "phoxal.execution.v1")]
#[derive(Eq)]
pub struct RuntimeFailure {
    /// `execution_id`.
    #[phoxal(tag = 1)]
    pub execution_id: String,
    /// `timeline_id`.
    #[phoxal(tag = 2)]
    pub timeline_id: String,
    /// `runtime_instance`.
    #[phoxal(tag = 3)]
    pub runtime_instance: String,
    /// `boundary`.
    #[phoxal(tag = 4)]
    pub boundary: u64,
    /// `reason`.
    #[phoxal(tag = 5)]
    pub reason: String,
}

/// Delivery facts attached to one exact generated payload.
///
/// This is metadata, not a second payload envelope.  Decoders hand the body
/// directly to the generated descriptor codec after validating the attachment.
#[phoxal::message(package = "phoxal.execution.v1")]
#[derive(Eq)]
pub struct RuntimeWireMetadata {
    /// Per-producer publication sequence, when one exists.
    #[phoxal(tag = 1)]
    pub sequence: Option<u64>,
    /// Logical execution time associated with the record.
    #[phoxal(tag = 2)]
    pub logical_time_nanos: Option<u64>,
    /// Original source identity for measured/forwarded observations.
    #[phoxal(tag = 3)]
    pub source: Option<String>,
    /// Immediate graph publisher, distinct from an observation's original source.
    #[phoxal(tag = 17)]
    pub producer: Option<String>,
    /// Original observation revision, when the source exposes one.
    #[phoxal(tag = 4)]
    pub revision: Option<u64>,
    /// Target command correlation, present for commands and replies.
    #[phoxal(tag = 5)]
    pub command_id: Option<u64>,
    /// Deterministic command eligibility boundary.
    #[phoxal(tag = 6)]
    pub eligible_boundary: Option<u64>,
    /// Deterministic caller rank used to merge controlled commands.
    #[phoxal(tag = 7)]
    pub caller_rank: Option<u64>,
    /// Stream control value.  Zero is ordinary data.
    #[phoxal(tag = 8)]
    pub control: u32,
    /// Setpoint expiry in logical execution time, when this is a setpoint
    /// renewal or withdrawal.
    #[phoxal(tag = 9)]
    pub expires_at_nanos: Option<u64>,
    /// Stable graph identity of the caller for a Commands request, in the
    /// form `{runtime-instance}.{input-field}`.
    #[phoxal(tag = 10)]
    pub caller: Option<String>,
    /// Target refusal or source failure detail, when supplied.
    #[phoxal(tag = 11)]
    pub reason: Option<String>,
    /// Supervisor ingress sequence for an external command.
    #[phoxal(tag = 12)]
    pub ingress_sequence: Option<u64>,
    /// Execution identity for a required controlled-delivery record.
    #[phoxal(tag = 13)]
    pub execution_id: Option<String>,
    /// Timeline identity for a required controlled-delivery record.
    #[phoxal(tag = 14)]
    pub timeline_id: Option<String>,
    /// Controlled boundary at which this record was produced.
    #[phoxal(tag = 15)]
    pub boundary: Option<u64>,
    /// Zero-based item index within the output port/direction cut.
    #[phoxal(tag = 16)]
    pub item: Option<u32>,
    /// Selected host-monotonic transfer budget for this managed request.
    #[phoxal(tag = 18)]
    pub request_timeout_ms: Option<u64>,
}
