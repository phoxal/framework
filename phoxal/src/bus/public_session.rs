//! The first public `phoxal.session.v1` Zenoh transport.
//!
//! This module is deliberately owned by the session and supervisor boundary.
//! The Protobuf messages and route/admission rules remain in [`crate::communication`], while
//! this file owns only the wire exchange, bounded query collection, and the
//! lifetime of the public queryables.
//!
//! The public transport is not the internal execution bus. In particular, it
//! never exposes a raw Zenoh handle, internal execution key, participant
//! identity, or unrestricted publication capability.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use prost::Message;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use zenoh::bytes::Encoding;
use zenoh::handlers::FifoChannel;
use zenoh::query::{ConsolidationMode, Query, QueryTarget, Queryable};

use crate::communication::bootstrap::SessionOffers;
use crate::communication::session::{
    BindPortRequest, BindPortResponse, CloseSessionRequest, CloseSessionResponse, ExecutionState,
    ListExecutionsRequest, ListExecutionsResponse, ListPortsRequest, ListPortsResponse,
    OpenSessionRequest, OpenSessionResponse, OperationOutcome, OperationRequest, OperationResponse,
    PortMetadata, RecordKind, RenewSessionRequest, RenewSessionResponse, SubscriptionAdmission,
    SubscriptionRecord, SubscriptionRequest, SupervisorInfoRequest, SupervisorInfoResponse,
    SupervisorState, SupervisorStatusRequest, SupervisorStatusResponse,
};
use crate::communication::simulation::{
    AcquireAuthorityRequest, AcquireAuthorityResponse, AdvanceRequest, AdvanceResponse,
    ProgressRequest, ProgressResponse, ReleaseAuthorityRequest, ReleaseAuthorityResponse,
    ResetRequest, ResetResponse,
};
use crate::communication::{
    BootstrapError, DeploymentTarget, SESSION_PROTOCOL, validate_session_offers,
};
use crate::communication::{
    PublicOperation, PublicRoute, SupervisorAdapter, SupervisorAdapterError,
};

/// The fixed standard Protobuf encoding carried by public session exchanges.
pub const PUBLIC_PROTOBUF_ENCODING: &str = "application/protobuf";
/// Default bounded request body size.
pub const DEFAULT_PUBLIC_MAX_REQUEST_BYTES: usize = 16 * 1024;
/// Default bounded response body size.
pub const DEFAULT_PUBLIC_MAX_RESPONSE_BYTES: usize = 16 * 1024;
/// Default bounded query handler capacity per operation.
pub const DEFAULT_PUBLIC_QUERY_CAPACITY: usize = 64;
/// Default finite public request deadline.
pub const DEFAULT_PUBLIC_DEADLINE: Duration = Duration::from_secs(5);
/// Maximum deadline accepted by the public transport configuration.
pub const MAX_PUBLIC_DEADLINE: Duration = Duration::from_secs(30);
/// Maximum supervisors returned by one bounded scope inventory.
pub const DEFAULT_MAX_DISCOVERED_SUPERVISORS: usize = 256;
/// Maximum diagnostic text sent on the native Zenoh error leg.
pub const MAX_PUBLIC_ERROR_BYTES: usize = 4 * 1024;

const PUBLIC_OPERATION_QUERYABLES: [PublicOperation; 17] = [
    PublicOperation::Open,
    PublicOperation::Renew,
    PublicOperation::Close,
    PublicOperation::Info,
    PublicOperation::Status,
    PublicOperation::ListExecutions,
    PublicOperation::ListPorts,
    PublicOperation::Bind,
    PublicOperation::Read,
    PublicOperation::Command,
    PublicOperation::Watch,
    PublicOperation::Subscribe,
    PublicOperation::AcquireAuthority,
    PublicOperation::Advance,
    PublicOperation::Reset,
    PublicOperation::ReleaseAuthority,
    PublicOperation::Progress,
];

type PublicQueryable = Queryable<zenoh::handlers::FifoChannelHandler<Query>>;

#[derive(Clone)]
struct OperationServerContext {
    target: DeploymentTarget,
    session: zenoh::Session,
    adapter: Arc<Mutex<SupervisorAdapter>>,
    backend: Arc<dyn PublicSessionBackend>,
    subscriptions: Arc<Mutex<BTreeMap<String, CancellationToken>>>,
    subscription_tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
    simulation_authority: Arc<Mutex<Option<SimulationAuthority>>>,
    simulation_backend: Arc<dyn PublicSimulationBackend>,
    principal_policy: PrincipalPolicy,
    limits: PublicTransportLimits,
    started: Instant,
    shutdown: CancellationToken,
}

/// One exclusive, lease-bound simulation authority held by this supervisor.
///
/// The grant is opaque and never reused.  Every simulation request rechecks
/// the protected principal, grant, execution, timeline, and lease before it
/// mutates the boundary, so an old grant cannot become valid after release,
/// reset, or a later acquisition.
#[derive(Clone, Debug)]
struct SimulationAuthority {
    principal: String,
    session_id: Vec<u8>,
    grant: Vec<u8>,
    execution_id: String,
    timeline_id: String,
    model_identity: String,
    quantum_ns: u64,
    boundary: u64,
    lease_deadline: Instant,
    advance_results: VecDeque<RetainedAdvance>,
}

#[derive(Clone, Debug)]
struct RetainedAdvance {
    correlation_id: Vec<u8>,
    request_digest: [u8; 32],
    response: AdvanceResponse,
}

const SIMULATION_AUTHORITY_LEASE: Duration = Duration::from_secs(30);
const SIMULATION_GRANT_BYTES: usize = 32;
const MAX_SIMULATION_CORRELATION_BYTES: usize = 64;
const MAX_RETAINED_ADVANCE_RESULTS: usize = 64;
const MAX_PUBLIC_SUBSCRIPTION_ID_BYTES: usize = 32;

/// Bounds applied to every public-session request and reply collection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicTransportLimits {
    max_request_bytes: usize,
    max_response_bytes: usize,
    max_reply_count: usize,
    query_capacity: usize,
    deadline: Duration,
}

impl Default for PublicTransportLimits {
    fn default() -> Self {
        Self {
            max_request_bytes: DEFAULT_PUBLIC_MAX_REQUEST_BYTES,
            max_response_bytes: DEFAULT_PUBLIC_MAX_RESPONSE_BYTES,
            // One success or error is required. A second reply must still be
            // observable so duplicate responders cannot be hidden by the
            // collection bound.
            max_reply_count: 2,
            query_capacity: DEFAULT_PUBLIC_QUERY_CAPACITY,
            deadline: DEFAULT_PUBLIC_DEADLINE,
        }
    }
}

impl PublicTransportLimits {
    /// Construct bounded public transport limits.
    ///
    /// The deadline is deliberately capped so a caller cannot turn one
    /// logical session into an unbounded transport worker.
    pub fn new(
        max_request_bytes: usize,
        max_response_bytes: usize,
        max_reply_count: usize,
        query_capacity: usize,
        deadline: Duration,
    ) -> Result<Self, PublicTransportError> {
        let limits = Self {
            max_request_bytes,
            max_response_bytes,
            max_reply_count,
            query_capacity,
            deadline,
        };
        limits.validate()?;
        Ok(limits)
    }

    /// Maximum encoded request body accepted by a queryable/client.
    #[must_use]
    pub const fn max_request_bytes(&self) -> usize {
        self.max_request_bytes
    }

    /// Maximum encoded response body accepted or sent by a queryable/client.
    #[must_use]
    pub const fn max_response_bytes(&self) -> usize {
        self.max_response_bytes
    }

    /// Maximum replies collected from one query before it is rejected.
    #[must_use]
    pub const fn max_reply_count(&self) -> usize {
        self.max_reply_count
    }

    /// Maximum pending queries held by one operation queryable.
    #[must_use]
    pub const fn query_capacity(&self) -> usize {
        self.query_capacity
    }

    /// Host-monotonic deadline for one exchange.
    #[must_use]
    pub const fn deadline(&self) -> Duration {
        self.deadline
    }

    fn validate(&self) -> Result<(), PublicTransportError> {
        if self.max_request_bytes == 0
            || self.max_response_bytes == 0
            || self.max_reply_count == 0
            || self.query_capacity == 0
            || self.deadline.is_zero()
            || self.deadline > MAX_PUBLIC_DEADLINE
        {
            return Err(PublicTransportError::InvalidLimits);
        }
        Ok(())
    }
}

/// The trusted ingress policy applied after exact route parsing.
///
/// `Any` is suitable only when the selected Zenoh router has already enforced
/// the association between an authenticated connection and its
/// `clients/{principal}` namespace. It is not a claim that a route string is
/// itself a credential. `Only` is useful for a supervisor-side allow-list and
/// for tests that exercise spoofed principals without a real ACL router.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PrincipalPolicy {
    /// Trust the configured router's protected principal namespace.
    Any,
    /// Admit only the listed protected route principals.
    Only(BTreeSet<String>),
}

impl PrincipalPolicy {
    /// Allow exactly the supplied principal identifiers.
    #[must_use]
    pub fn only<I, S>(principals: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::Only(principals.into_iter().map(Into::into).collect())
    }

    fn allows(&self, principal: &str) -> bool {
        match self {
            Self::Any => true,
            Self::Only(principals) => principals.contains(principal),
        }
    }
}

/// The exact binding context passed to a production public-service backend.
///
/// The context is copied from the supervisor adapter only after session,
/// binding, execution, and timeline validation has succeeded.  A backend must
/// treat it as admission evidence, not as a substitute for its own domain
/// authorization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicBindingContext {
    /// Opaque logical-session identifier.
    pub session_id: Vec<u8>,
    /// Opaque binding identifier.
    pub binding_id: Vec<u8>,
    /// Exact execution identity selected by the client.
    pub execution_id: String,
    /// Exact timeline identity selected by the client.
    pub timeline_id: String,
    /// Deployed service instance receiving the operation.
    pub service_instance: String,
    /// Generated descriptor admitted by the adapter.
    pub metadata: PortMetadata,
}

impl From<crate::communication::BindingContext> for PublicBindingContext {
    fn from(binding: crate::communication::BindingContext) -> Self {
        Self {
            session_id: binding.session_id,
            binding_id: binding.binding_id,
            execution_id: binding.execution_id,
            timeline_id: binding.timeline_id,
            service_instance: binding.service_instance,
            metadata: binding.metadata,
        }
    }
}

/// A result from a backend operation after the public adapter has admitted it.
///
/// `OutcomeUnknown` is deliberately available to a backend because a transport
/// or service may have accepted a command before its reply path failed.  The
/// server never changes that result into a retryable ordinary error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublicBackendOutcome {
    /// The operation completed and carries the generated response body.
    Received(Vec<u8>),
    /// Local admission proved that the operation was never transmitted.
    NotSent(String),
    /// The target refused the operation before queue admission.
    RejectedBeforeAdmission(String),
    /// The operation may have reached the target but no definitive result was
    /// observed.
    OutcomeUnknown(String),
}

/// A bounded backend failure that occurred before an operation outcome could
/// be established.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PublicBackendError {
    /// The backend could not admit the operation locally.
    #[error("backend rejected the operation before admission: {0}")]
    RejectedBeforeAdmission(String),
    /// The backend could not allocate its bounded observer resources.
    #[error("backend observer capacity is exhausted")]
    Capacity,
    /// The backend's service transport failed before admission evidence.
    #[error("backend transport failed: {0}")]
    Transport(String),
}

/// One bounded observation source returned by a public-service backend.
pub struct PublicBackendSubscription {
    initial: Option<SubscriptionRecord>,
    records: mpsc::Receiver<Result<SubscriptionRecord, PublicBackendError>>,
}

impl PublicBackendSubscription {
    /// Construct a source with an optional initial record and bounded updates.
    #[must_use]
    pub fn new(
        initial: Option<SubscriptionRecord>,
        records: mpsc::Receiver<Result<SubscriptionRecord, PublicBackendError>>,
    ) -> Self {
        Self { initial, records }
    }

    fn take_initial(&mut self) -> Option<SubscriptionRecord> {
        self.initial.take()
    }
}

/// Identity and boundary evidence passed to a simulation backend after public
/// route and authority checks. A backend must use the grant and session
/// identity as part of its own admission key and must not infer authority from
/// the Zenoh source alone.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicSimulationContext {
    /// Protected principal from the exact simulation route.
    pub principal: String,
    /// Logical public session that acquired the grant.
    pub session_id: Vec<u8>,
    /// Opaque grant currently admitted by the supervisor.
    pub authority_grant: Vec<u8>,
    /// Caller-owned operation correlation retained for delayed-result
    /// reconciliation.
    pub correlation_id: Vec<u8>,
    /// Exact execution identity.
    pub execution_id: String,
    /// Exact execution timeline.
    pub timeline_id: String,
    /// Current completed boundary before this operation.
    pub completed_boundary: u64,
    /// Model identity negotiated during acquisition.
    pub model_identity: String,
    /// Fixed simulation quantum.
    pub quantum_ns: u64,
}

/// Runtime-facing hook for the public simulation authority lane.
///
/// The public server owns route/session/grant fencing and bounded wire
/// exchange. A production implementation owns required robot-boundary
/// admission, exactly-once execution, observation receipts, actuation cut,
/// reset, and authoritative progress. The default implementation refuses all
/// simulation work, so a control-only supervisor cannot claim a fake advance.
pub trait PublicSimulationBackend: Send + Sync {
    /// Validate and reserve one complete model/provider agreement.
    fn acquire(
        &self,
        context: PublicSimulationContext,
        request: AcquireAuthorityRequest,
    ) -> Pin<Box<dyn Future<Output = Result<(), PublicBackendError>> + Send>>;

    /// Admit one current-boundary observation set and run that boundary once.
    fn advance(
        &self,
        context: PublicSimulationContext,
        request: AdvanceRequest,
    ) -> Pin<Box<dyn Future<Output = Result<AdvanceResponse, PublicBackendError>> + Send>>;

    /// Reset the selected execution from its authoritative completed boundary.
    fn reset(
        &self,
        context: PublicSimulationContext,
        request: ResetRequest,
        next_timeline_id: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), PublicBackendError>> + Send>>;

    /// Release the reserved model/provider authority.
    fn release(
        &self,
        context: PublicSimulationContext,
        request: ReleaseAuthorityRequest,
    ) -> Pin<Box<dyn Future<Output = Result<(), PublicBackendError>> + Send>>;

    /// Return authoritative progress after a possibly uncertain exchange.
    fn progress(
        &self,
        context: PublicSimulationContext,
        request: ProgressRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ProgressResponse, PublicBackendError>> + Send>>;
}

#[derive(Debug, Default)]
struct UnavailableSimulationBackend;

impl PublicSimulationBackend for UnavailableSimulationBackend {
    fn acquire(
        &self,
        _context: PublicSimulationContext,
        _request: AcquireAuthorityRequest,
    ) -> Pin<Box<dyn Future<Output = Result<(), PublicBackendError>> + Send>> {
        Box::pin(async {
            Err(PublicBackendError::RejectedBeforeAdmission(
                "the selected supervisor has no simulation boundary backend".to_owned(),
            ))
        })
    }

    fn advance(
        &self,
        _context: PublicSimulationContext,
        _request: AdvanceRequest,
    ) -> Pin<Box<dyn Future<Output = Result<AdvanceResponse, PublicBackendError>> + Send>> {
        Box::pin(async {
            Err(PublicBackendError::RejectedBeforeAdmission(
                "the selected supervisor has no simulation boundary backend".to_owned(),
            ))
        })
    }

    fn reset(
        &self,
        _context: PublicSimulationContext,
        _request: ResetRequest,
        _next_timeline_id: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), PublicBackendError>> + Send>> {
        Box::pin(async {
            Err(PublicBackendError::RejectedBeforeAdmission(
                "the selected supervisor has no simulation boundary backend".to_owned(),
            ))
        })
    }

    fn release(
        &self,
        _context: PublicSimulationContext,
        _request: ReleaseAuthorityRequest,
    ) -> Pin<Box<dyn Future<Output = Result<(), PublicBackendError>> + Send>> {
        Box::pin(async { Ok(()) })
    }

    fn progress(
        &self,
        _context: PublicSimulationContext,
        _request: ProgressRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ProgressResponse, PublicBackendError>> + Send>> {
        Box::pin(async {
            Err(PublicBackendError::RejectedBeforeAdmission(
                "the selected supervisor has no simulation boundary backend".to_owned(),
            ))
        })
    }
}

/// Backend hook used by the production supervisor and independent fixtures.
///
/// The public transport owns routing, Protobuf framing, admission, correlation,
/// and bounded subscription cleanup.  A backend owns the service-specific
/// exchange after that boundary.  Returning a boxed future keeps the public
/// crate independent of an async-trait compatibility layer.
pub trait PublicSessionBackend: Send + Sync {
    /// Execute one admitted Read or Commands operation.
    fn call(
        &self,
        operation: PublicOperation,
        binding: PublicBindingContext,
        payload: Vec<u8>,
        timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<PublicBackendOutcome, PublicBackendError>> + Send>>;

    /// Establish one admitted State, Sample, Event, or Stream observation.
    fn subscribe(
        &self,
        operation: PublicOperation,
        binding: PublicBindingContext,
        request: SubscriptionRequest,
        capacity: usize,
    ) -> Result<PublicBackendSubscription, PublicBackendError>;
}

#[derive(Debug, Default)]
struct UnavailableBackend;

impl PublicSessionBackend for UnavailableBackend {
    fn call(
        &self,
        _operation: PublicOperation,
        _binding: PublicBindingContext,
        _payload: Vec<u8>,
        _timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<PublicBackendOutcome, PublicBackendError>> + Send>>
    {
        Box::pin(async {
            Ok(PublicBackendOutcome::NotSent(
                "the selected service has no public data backend".to_owned(),
            ))
        })
    }

    fn subscribe(
        &self,
        _operation: PublicOperation,
        _binding: PublicBindingContext,
        _request: SubscriptionRequest,
        _capacity: usize,
    ) -> Result<PublicBackendSubscription, PublicBackendError> {
        Err(PublicBackendError::RejectedBeforeAdmission(
            "the selected service has no public observation backend".to_owned(),
        ))
    }
}

/// Public session transport and protocol failures.
#[derive(Debug, Error)]
pub enum PublicTransportError {
    /// Zenoh could not open, declare, query, or close the transport.
    #[error("public session transport failed: {0}")]
    Transport(String),
    /// Bounds or deadline configuration was not finite and nonzero.
    #[error("public session transport limits must be finite and nonzero")]
    InvalidLimits,
    /// A request or response exceeded its encoded byte bound.
    #[error("public session {operation} body is too large: {bytes} bytes exceeds {maximum}")]
    BodyTooLarge {
        /// Operation whose body exceeded the bound.
        operation: String,
        /// Observed encoded bytes.
        bytes: usize,
        /// Configured encoded bound.
        maximum: usize,
    },
    /// A query waited beyond its host-monotonic deadline.
    #[error("public session {operation} exchange timed out")]
    Timeout {
        /// Operation whose exchange timed out.
        operation: String,
    },
    /// A query delivered a reply but did not complete before its deadline.
    #[error("public session {operation} query did not complete before its deadline")]
    IncompleteQuery {
        /// Operation whose query remained incomplete.
        operation: String,
    },
    /// No responder completed the exact query.
    #[error("public session {operation} query received no reply")]
    NoReply {
        /// Operation that had no responder.
        operation: String,
    },
    /// More than one reply arrived for an operation requiring one authority.
    #[error("public session {operation} query received multiple replies")]
    TooManyReplies {
        /// Operation with ambiguous responders.
        operation: String,
    },
    /// A responder replied on a key other than the exact request key.
    #[error("public session {operation} reply key mismatch: expected {expected}, got {actual}")]
    WrongReplyKey {
        /// Exact requested key.
        expected: String,
        /// Key carried by the reply.
        actual: String,
        /// Operation being exchanged.
        operation: String,
    },
    /// A success reply did not use the standard Protobuf encoding.
    #[error("public session {operation} reply is not encoded as standard Protobuf")]
    WrongEncoding {
        /// Operation whose reply encoding was wrong.
        operation: String,
    },
    /// A responder rejected a request on Zenoh's native error leg.
    #[error("public session {operation} was rejected: {detail}")]
    Rejected {
        /// Operation that was rejected.
        operation: String,
        /// Bounded server diagnostic.
        detail: String,
    },
    /// A request was malformed before adapter admission.
    #[error("public session {operation} request is malformed: {detail}")]
    Malformed {
        /// Operation whose request was malformed.
        operation: String,
        /// Bounded server diagnostic.
        detail: String,
    },
    /// The incoming route was not an exact route for the selected operation.
    #[error("public session {operation} route is not authorized: {detail}")]
    Unauthorized {
        /// Operation whose route was refused.
        operation: String,
        /// Bounded server diagnostic.
        detail: String,
    },
    /// The transport-independent adapter refused a validly decoded request.
    #[error("public session {operation} adapter rejected the request: {detail}")]
    Adapter {
        /// Operation whose domain admission failed.
        operation: String,
        /// Adapter diagnostic.
        detail: String,
    },
    /// A local client attempted an operation after its conservative lease.
    #[error("public session lease has expired locally")]
    LeaseExpired,
    /// The server returned an invalid bootstrap document.
    #[error("public session bootstrap is invalid: {0}")]
    Bootstrap(#[from] BootstrapError),
    /// The server returned an invalid Protobuf body.
    #[error("public session {operation} Protobuf body is invalid: {detail}")]
    Decode {
        /// Operation whose body could not be decoded.
        operation: String,
        /// Decoder diagnostic.
        detail: String,
    },
    /// A routed scope contained more supervisors than the caller's bound.
    #[error("public supervisor inventory exceeds its {maximum}-target bound")]
    InventoryOverflow {
        /// Maximum number of targets the caller allowed.
        maximum: usize,
    },
    /// A liveliness reply did not name one exact supervisor presence key.
    #[error("public supervisor inventory contains malformed presence key '{key}'")]
    MalformedPresence {
        /// Key returned by the trusted router.
        key: String,
    },
    /// A bounded client observation queue overflowed.
    #[error("public session {operation} observation queue overflowed")]
    SubscriptionOverflow {
        /// Operation whose observer queue was full.
        operation: String,
    },
}

/// A running supervisor-side public query surface.
///
/// The server owns queryables and a presence token, but not the shared Zenoh
/// session. Closing this value undeclares the public surface while leaving the
/// supervisor's internal bus owner in charge of closing the session itself.
pub struct PublicSessionServer {
    shutdown: CancellationToken,
    tasks: Vec<JoinHandle<()>>,
    _adapter: Arc<Mutex<SupervisorAdapter>>,
    subscriptions: Arc<Mutex<BTreeMap<String, CancellationToken>>>,
    subscription_tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
    simulation_authority: Arc<Mutex<Option<SimulationAuthority>>>,
    simulation_backend: Arc<dyn PublicSimulationBackend>,
    _presence: zenoh::liveliness::LivelinessToken,
    _session: zenoh::Session,
}

impl std::fmt::Debug for PublicSessionServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PublicSessionServer")
            .field("tasks", &self.tasks.len())
            .finish_non_exhaustive()
    }
}

impl PublicSessionServer {
    /// Declare the exact bootstrap and public-session operation queryables.
    ///
    /// The supplied session may be a clone of the supervisor's owner session.
    /// The `Any` policy requires an authenticated router to protect the
    /// principal segment; use [`PrincipalPolicy::only`] when this process has
    /// an additional local allow-list.
    pub async fn start(
        session: zenoh::Session,
        adapter: SupervisorAdapter,
        principal_policy: PrincipalPolicy,
        limits: PublicTransportLimits,
    ) -> Result<Self, PublicTransportError> {
        Self::start_with_backend(
            session,
            adapter,
            Arc::new(UnavailableBackend),
            principal_policy,
            limits,
        )
        .await
    }

    /// Declare the public surface with a service-owned data backend.
    ///
    /// The backend is called only after the route, principal, lease, exact
    /// binding, execution, and timeline have been checked by the adapter.
    /// This is the production entry point for a supervisor that has a live
    /// runtime graph, while [`Self::start`] remains useful for control-only
    /// supervisors and protocol fixtures.
    pub async fn start_with_backend(
        session: zenoh::Session,
        adapter: SupervisorAdapter,
        backend: Arc<dyn PublicSessionBackend>,
        principal_policy: PrincipalPolicy,
        limits: PublicTransportLimits,
    ) -> Result<Self, PublicTransportError> {
        Self::start_with_backends(
            session,
            adapter,
            backend,
            Arc::new(UnavailableSimulationBackend),
            principal_policy,
            limits,
        )
        .await
    }

    /// Declare the public surface with service and simulation backends.
    ///
    /// The simulation backend is the only component permitted to perform a
    /// robot boundary. Without one, authority acquisition and advance are
    /// refused before a grant is installed, making the absence of runtime
    /// integration explicit instead of exposing a counter-only fixture.
    pub async fn start_with_backends(
        session: zenoh::Session,
        adapter: SupervisorAdapter,
        backend: Arc<dyn PublicSessionBackend>,
        simulation_backend: Arc<dyn PublicSimulationBackend>,
        principal_policy: PrincipalPolicy,
        limits: PublicTransportLimits,
    ) -> Result<Self, PublicTransportError> {
        limits.validate()?;
        let target = adapter.target().clone();
        let presence = session
            .liveliness()
            .declare_token(format!("{}/presence", target.prefix()))
            .await
            .map_err(|error| PublicTransportError::Transport(error.to_string()))?;
        let bootstrap = session
            .declare_queryable(target.bootstrap_key())
            .complete(true)
            .with(FifoChannel::new(limits.query_capacity()))
            .await
            .map_err(|error| PublicTransportError::Transport(error.to_string()))?;
        let mut operation_queryables = Vec::with_capacity(PUBLIC_OPERATION_QUERYABLES.len());
        for operation in PUBLIC_OPERATION_QUERYABLES {
            let key = operation_key_expression(&target, operation);
            let queryable = session
                .declare_queryable(key)
                .complete(true)
                .with(FifoChannel::new(limits.query_capacity()))
                .await
                .map_err(|error| PublicTransportError::Transport(error.to_string()))?;
            operation_queryables.push((operation, queryable));
        }

        let shutdown = CancellationToken::new();
        let started = Instant::now();
        let adapter = Arc::new(Mutex::new(adapter));
        let subscriptions = Arc::new(Mutex::new(BTreeMap::new()));
        let subscription_tasks = Arc::new(Mutex::new(Vec::new()));
        let simulation_authority = Arc::new(Mutex::new(None));
        let mut tasks = Vec::with_capacity(1 + operation_queryables.len());
        tasks.push(tokio::spawn(serve_bootstrap(
            bootstrap,
            target.clone(),
            limits.clone(),
            started,
            shutdown.clone(),
        )));
        for (operation, queryable) in operation_queryables {
            tasks.push(tokio::spawn(serve_operation(
                queryable,
                operation,
                OperationServerContext {
                    target: target.clone(),
                    session: session.clone(),
                    adapter: adapter.clone(),
                    backend: backend.clone(),
                    subscriptions: subscriptions.clone(),
                    subscription_tasks: subscription_tasks.clone(),
                    simulation_authority: simulation_authority.clone(),
                    simulation_backend: simulation_backend.clone(),
                    principal_policy: principal_policy.clone(),
                    limits: limits.clone(),
                    started,
                    shutdown: shutdown.clone(),
                },
            )));
        }
        Ok(Self {
            shutdown,
            tasks,
            _adapter: adapter,
            subscriptions,
            subscription_tasks,
            simulation_authority,
            simulation_backend,
            _presence: presence,
            _session: session,
        })
    }

    /// Update the supervisor status while retaining the public surface.
    #[allow(
        dead_code,
        reason = "the session profile owns the public client while the supervisor profile owns server status publication"
    )]
    pub(crate) async fn set_status(
        &self,
        state: SupervisorState,
        detail: Option<String>,
    ) -> Result<(), PublicTransportError> {
        self._adapter
            .lock()
            .await
            .set_status(state, detail)
            .map_err(|error| PublicTransportError::Adapter {
                operation: "status".to_owned(),
                detail: error.to_string(),
            })
    }

    /// Update the execution lifecycle after a process graph transition.
    #[allow(
        dead_code,
        reason = "the session profile owns the public client while the supervisor profile owns execution-state publication"
    )]
    pub(crate) async fn set_execution_state(
        &self,
        execution_id: &str,
        state: ExecutionState,
    ) -> Result<(), PublicTransportError> {
        self._adapter
            .lock()
            .await
            .set_execution_state(execution_id, state)
            .map_err(|error| PublicTransportError::Adapter {
                operation: "execution-state".to_owned(),
                detail: error.to_string(),
            })
    }

    /// Stop every public queryable and wait for its bounded receive loop.
    pub async fn close(mut self) -> Result<(), PublicTransportError> {
        self.shutdown.cancel();
        for token in self.subscriptions.lock().await.values() {
            token.cancel();
        }
        if let Some(authority) = self.simulation_authority.lock().await.take() {
            release_backend_authority(authority, &self.simulation_backend).await;
        }
        let mut failure = None;
        for task in self.tasks.drain(..) {
            if let Err(error) = task.await {
                failure.get_or_insert_with(|| {
                    PublicTransportError::Transport(format!(
                        "public session server task failed: {error}"
                    ))
                });
            }
        }
        for task in self.subscription_tasks.lock().await.drain(..) {
            if let Err(error) = task.await {
                failure.get_or_insert_with(|| {
                    PublicTransportError::Transport(format!(
                        "public subscription task failed: {error}"
                    ))
                });
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl Drop for PublicSessionServer {
    fn drop(&mut self) {
        self.shutdown.cancel();
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Client configuration for one target and one protected principal route.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicTlsCredentials {
    root_ca_certificate: PathBuf,
    client_certificate: PathBuf,
    client_private_key: PathBuf,
    server_name: String,
}

impl PublicTlsCredentials {
    /// Configure file-backed mutual-TLS credentials and strict server-name
    /// verification.
    pub fn from_files(
        root_ca_certificate: impl Into<PathBuf>,
        client_certificate: impl Into<PathBuf>,
        client_private_key: impl Into<PathBuf>,
        server_name: impl Into<String>,
    ) -> Result<Self, PublicTransportError> {
        let credentials = Self {
            root_ca_certificate: root_ca_certificate.into(),
            client_certificate: client_certificate.into(),
            client_private_key: client_private_key.into(),
            server_name: server_name.into(),
        };
        if credentials.root_ca_certificate.as_os_str().is_empty()
            || credentials.client_certificate.as_os_str().is_empty()
            || credentials.client_private_key.as_os_str().is_empty()
            || credentials.server_name.is_empty()
            || !credentials.server_name.is_ascii()
            || credentials
                .server_name
                .bytes()
                .any(|byte| byte.is_ascii_whitespace())
        {
            return Err(PublicTransportError::Malformed {
                operation: "connect".to_owned(),
                detail: "mutual-TLS credentials are incomplete".to_owned(),
            });
        }
        Ok(credentials)
    }

    /// Root CA certificate file.
    #[must_use]
    pub fn root_ca_certificate(&self) -> &std::path::Path {
        &self.root_ca_certificate
    }

    /// Client certificate chain file.
    #[must_use]
    pub fn client_certificate(&self) -> &std::path::Path {
        &self.client_certificate
    }

    /// Client private key file.
    #[must_use]
    pub fn client_private_key(&self) -> &std::path::Path {
        &self.client_private_key
    }

    /// TLS server name used for certificate verification.
    #[must_use]
    pub fn server_name(&self) -> &str {
        &self.server_name
    }
}

/// Public transport security profile.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum PublicTransportSecurity {
    /// Plain loopback TCP or a local Unix socket.
    #[default]
    Plaintext,
    /// Mutual TLS with caller-supplied certificate files.
    Tls(PublicTlsCredentials),
}

/// Configure a Zenoh client from an explicit public security profile.
fn public_client_config(endpoint: &str) -> Result<zenoh::Config, PublicTransportError> {
    let mut config = zenoh::Config::default();
    for (key, value) in [
        ("transport/link/tx/lease", "3000"),
        ("transport/link/tx/keep_alive", "4"),
        ("scouting/multicast/enabled", "false"),
        ("mode", "\"client\""),
    ] {
        config
            .insert_json5(key, value)
            .map_err(|error| PublicTransportError::Transport(error.to_string()))?;
    }
    let endpoints = serde_json::to_string(std::slice::from_ref(&endpoint))
        .map_err(|error| PublicTransportError::Transport(error.to_string()))?;
    config
        .insert_json5("connect/endpoints", &endpoints)
        .map_err(|error| PublicTransportError::Transport(error.to_string()))?;
    Ok(config)
}

fn client_config_with_security(
    endpoint: &str,
    security: &PublicTransportSecurity,
) -> Result<zenoh::Config, PublicTransportError> {
    if endpoint.is_empty() {
        return Err(PublicTransportError::Malformed {
            operation: "connect".to_owned(),
            detail: "router endpoint is empty".to_owned(),
        });
    }
    let mut config = public_client_config(endpoint)?;
    let insert = |config: &mut zenoh::Config, key: &str, value: &str| {
        config
            .insert_json5(key, value)
            .map_err(|error| PublicTransportError::Transport(error.to_string()))
    };
    insert(
        &mut config,
        "transport/unicast/compression/enabled",
        "false",
    )?;
    match security {
        PublicTransportSecurity::Plaintext => {
            if !is_local_endpoint(endpoint) || endpoint.starts_with("tls/") {
                return Err(PublicTransportError::Malformed {
                    operation: "connect".to_owned(),
                    detail: "plaintext public sessions require loopback TCP or a local Unix socket"
                        .to_owned(),
                });
            }
            let protocol = if endpoint.starts_with("unixsock-stream/") {
                "[\"unixsock-stream\"]"
            } else {
                "[\"tcp\"]"
            };
            insert(&mut config, "transport/link/protocols", protocol)?;
        }
        PublicTransportSecurity::Tls(credentials) => {
            if !endpoint.starts_with("tls/") {
                return Err(PublicTransportError::Malformed {
                    operation: "connect".to_owned(),
                    detail: "mutual-TLS credentials require a tls/ endpoint".to_owned(),
                });
            }
            let endpoint_name =
                tls_endpoint_name(endpoint).ok_or_else(|| PublicTransportError::Malformed {
                    operation: "connect".to_owned(),
                    detail: "tls endpoint must contain a host and port".to_owned(),
                })?;
            if endpoint_name != credentials.server_name {
                return Err(PublicTransportError::Malformed {
                    operation: "connect".to_owned(),
                    detail: "TLS server_name must exactly match the tls endpoint host".to_owned(),
                });
            }
            insert(&mut config, "transport/link/protocols", "[\"tls\"]")?;
            insert(
                &mut config,
                "transport/link/tls/root_ca_certificate",
                &json_string(credentials.root_ca_certificate.to_string_lossy().as_ref())?,
            )?;
            insert(
                &mut config,
                "transport/link/tls/connect_certificate",
                &json_string(credentials.client_certificate.to_string_lossy().as_ref())?,
            )?;
            insert(
                &mut config,
                "transport/link/tls/connect_private_key",
                &json_string(credentials.client_private_key.to_string_lossy().as_ref())?,
            )?;
            insert(&mut config, "transport/link/tls/enable_mtls", "true")?;
            insert(
                &mut config,
                "transport/link/tls/verify_name_on_connect",
                "true",
            )?;
        }
    }
    Ok(config)
}

fn json_string(value: &str) -> Result<String, PublicTransportError> {
    serde_json::to_string(value)
        .map_err(|error| PublicTransportError::Transport(format!("TLS path is invalid: {error}")))
}

fn is_local_endpoint(endpoint: &str) -> bool {
    endpoint.starts_with("tcp/127.0.0.1:")
        || endpoint.starts_with("tcp/localhost:")
        || endpoint.starts_with("tcp/[::1]:")
        || endpoint.starts_with("unixsock-stream/")
}

fn tls_endpoint_name(endpoint: &str) -> Option<&str> {
    let address = endpoint.strip_prefix("tls/")?;
    if let Some(address) = address.strip_prefix('[') {
        let (host, _) = address.split_once(']')?;
        return (!host.is_empty()).then_some(host);
    }
    let (host, port) = address.rsplit_once(':')?;
    (!host.is_empty()
        && !port.is_empty()
        && port.chars().all(|character| character.is_ascii_digit()))
    .then_some(host)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicSessionConfig {
    endpoint: String,
    target: DeploymentTarget,
    principal: String,
    limits: PublicTransportLimits,
    security: PublicTransportSecurity,
}

impl PublicSessionConfig {
    /// Build a client configuration for one exact router endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error when the endpoint is empty, the principal cannot be a
    /// protected route segment, or the supplied limits are invalid.
    pub fn new(
        endpoint: impl Into<String>,
        target: DeploymentTarget,
        principal: impl Into<String>,
    ) -> Result<Self, PublicTransportError> {
        let endpoint = endpoint.into();
        if endpoint.is_empty() {
            return Err(PublicTransportError::Malformed {
                operation: "connect".to_owned(),
                detail: "router endpoint is empty".to_owned(),
            });
        }
        let principal = principal.into();
        PublicRoute::for_operation(&target, &principal, PublicOperation::Open).map_err(
            |error| PublicTransportError::Malformed {
                operation: "connect".to_owned(),
                detail: error.to_string(),
            },
        )?;
        Ok(Self {
            endpoint,
            target,
            principal,
            limits: PublicTransportLimits::default(),
            security: PublicTransportSecurity::default(),
        })
    }

    /// Replace the default limits with a validated finite set.
    pub fn with_limits(
        mut self,
        limits: PublicTransportLimits,
    ) -> Result<Self, PublicTransportError> {
        limits.validate()?;
        self.limits = limits;
        Ok(self)
    }

    /// Select the explicit transport security profile.
    pub fn with_security(mut self, security: PublicTransportSecurity) -> Self {
        self.security = security;
        self
    }

    /// Router endpoint used by the client owner.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Selected deployment target.
    #[must_use]
    pub fn target(&self) -> &DeploymentTarget {
        &self.target
    }

    /// Protected route principal.
    #[must_use]
    pub fn principal(&self) -> &str {
        &self.principal
    }

    /// Bounded exchange limits.
    #[must_use]
    pub const fn limits(&self) -> &PublicTransportLimits {
        &self.limits
    }

    /// Transport security profile.
    #[must_use]
    pub fn security(&self) -> &PublicTransportSecurity {
        &self.security
    }
}

/// One shared Zenoh transport for independently routed logical sessions.
///
/// A client that needs more than one robot should open this owner once and
/// call [`Self::open`] for each target. Closing one returned logical session
/// never closes this transport or invalidates another target.
pub struct PublicSessionTransport {
    session: zenoh::Session,
    endpoint: String,
    principal: String,
    limits: PublicTransportLimits,
    security: PublicTransportSecurity,
}

/// One bounded change in an authorized supervisor inventory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiscoveryEvent {
    /// The merged initial observation.  `complete=false` means the bounded
    /// query or its race buffer overflowed and the set must not be treated as
    /// an atomic fleet snapshot.
    Snapshot {
        /// Targets observed during the initial query and race merge.
        targets: Vec<DeploymentTarget>,
        /// Whether the initial observation completed without uncertainty.
        complete: bool,
    },
    /// A validated supervisor presence token appeared.
    Appeared(DeploymentTarget),
    /// A previously observed supervisor presence token disappeared.
    Disappeared(DeploymentTarget),
    /// The bounded change queue overflowed and the inventory needs a fresh
    /// bounded observation.
    Gap {
        /// Number of changes that could not be retained, when known.
        dropped: usize,
    },
    /// The trusted liveliness observation became unavailable.
    Unavailable {
        /// Bounded diagnostic detail.
        detail: String,
    },
}

/// A bounded race-safe supervisor inventory watch.
pub struct SupervisorWatch {
    events: mpsc::Receiver<DiscoveryEvent>,
    task: JoinHandle<()>,
}

impl SupervisorWatch {
    /// Receive the next inventory snapshot or change.
    pub async fn recv(&mut self) -> Option<DiscoveryEvent> {
        self.events.recv().await
    }
}

impl Drop for SupervisorWatch {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl std::fmt::Debug for PublicSessionTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PublicSessionTransport")
            .field("endpoint", &self.endpoint)
            .field("principal", &self.principal)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl PublicSessionTransport {
    /// Open one client transport to a router endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error when the endpoint or principal is malformed, the
    /// limits are invalid, or Zenoh cannot open the configured connection.
    pub async fn connect(
        endpoint: impl Into<String>,
        principal: impl Into<String>,
    ) -> Result<Self, PublicTransportError> {
        Self::connect_with_limits(endpoint, principal, PublicTransportLimits::default()).await
    }

    /// Open one client transport with caller-selected finite limits.
    pub async fn connect_with_limits(
        endpoint: impl Into<String>,
        principal: impl Into<String>,
        limits: PublicTransportLimits,
    ) -> Result<Self, PublicTransportError> {
        Self::connect_with_security(
            endpoint,
            principal,
            PublicTransportSecurity::Plaintext,
            limits,
        )
        .await
    }

    /// Open one public transport using an explicit plaintext or mTLS profile.
    pub async fn connect_with_security(
        endpoint: impl Into<String>,
        principal: impl Into<String>,
        security: PublicTransportSecurity,
        limits: PublicTransportLimits,
    ) -> Result<Self, PublicTransportError> {
        limits.validate()?;
        let endpoint = endpoint.into();
        if endpoint.is_empty() {
            return Err(PublicTransportError::Malformed {
                operation: "connect".to_owned(),
                detail: "router endpoint is empty".to_owned(),
            });
        }
        let principal = principal.into();
        let validation_target = DeploymentTarget::new("local", "local")?;
        PublicRoute::for_operation(&validation_target, &principal, PublicOperation::Open)
            .map_err(|error| malformed_client("connect", error))?;
        let session = zenoh::open(client_config_with_security(&endpoint, &security)?)
            .await
            .map_err(|error| PublicTransportError::Transport(error.to_string()))?;
        Ok(Self {
            session,
            endpoint,
            principal,
            limits,
            security,
        })
    }

    /// Open one independent logical session on this shared transport.
    pub async fn open(
        &self,
        target: DeploymentTarget,
    ) -> Result<PublicSessionConnection, PublicTransportError> {
        let config = PublicSessionConfig::new(&self.endpoint, target, &self.principal)?
            .with_limits(self.limits.clone())?
            .with_security(self.security.clone());
        PublicSessionConnection::connect_on_session(self.session.clone(), config, false).await
    }

    /// Discover the current bounded supervisor inventory in one authorized scope.
    ///
    /// This is a point-in-time liveliness query. A supervisor can disappear
    /// immediately after it is returned, so opening the logical session and
    /// obtaining its information remain authoritative.
    pub async fn discover(
        &self,
        scope: &str,
    ) -> Result<Vec<DeploymentTarget>, PublicTransportError> {
        self.discover_bounded(scope, DEFAULT_MAX_DISCOVERED_SUPERVISORS)
            .await
    }

    /// Discover a scope with an explicit nonzero result bound.
    pub async fn discover_bounded(
        &self,
        scope: &str,
        maximum: usize,
    ) -> Result<Vec<DeploymentTarget>, PublicTransportError> {
        if maximum == 0 {
            return Err(PublicTransportError::InvalidLimits);
        }
        DeploymentTarget::new(scope, "validation")?;
        let prefix = format!("phoxal/{scope}/supervisors/");
        let suffix = "/presence";
        let selector = format!("{prefix}*{suffix}");
        let replies = self
            .session
            .liveliness()
            .get(selector)
            .timeout(self.limits.deadline())
            .with(FifoChannel::new(maximum.saturating_add(1)))
            .await
            .map_err(|error| PublicTransportError::Transport(error.to_string()))?;
        let mut supervisors = BTreeSet::new();
        while let Ok(reply) = replies.recv_async().await {
            let sample = reply
                .result()
                .map_err(|error| PublicTransportError::Rejected {
                    operation: "discover".to_owned(),
                    detail: bounded_error_detail(
                        error
                            .payload()
                            .try_to_string()
                            .as_deref()
                            .unwrap_or("liveliness query failed"),
                    ),
                })?;
            let key = sample.key_expr().as_str();
            let supervisor = key
                .strip_prefix(&prefix)
                .and_then(|value| value.strip_suffix(suffix))
                .filter(|value| !value.contains('/'))
                .ok_or_else(|| PublicTransportError::MalformedPresence {
                    key: key.to_owned(),
                })?;
            let target = DeploymentTarget::new(scope, supervisor).map_err(|_| {
                PublicTransportError::MalformedPresence {
                    key: key.to_owned(),
                }
            })?;
            supervisors.insert(target.supervisor().to_owned());
            if supervisors.len() > maximum {
                return Err(PublicTransportError::InventoryOverflow { maximum });
            }
        }
        supervisors
            .into_iter()
            .map(|supervisor| DeploymentTarget::new(scope, supervisor).map_err(Into::into))
            .collect()
    }

    /// Watch one authorized scope with the default inventory and change
    /// bounds.
    pub async fn watch(&self, scope: &str) -> Result<SupervisorWatch, PublicTransportError> {
        self.watch_bounded(scope, DEFAULT_MAX_DISCOVERED_SUPERVISORS, 1024)
            .await
    }

    /// Start a bounded, race-safe scope watch.
    ///
    /// The liveliness subscriber is declared before the initial query starts.
    /// Changes observed during that query are retained in a finite merge
    /// buffer and replayed after the initial snapshot.  Overflow is an
    /// explicit gap, never a silently complete inventory.
    pub async fn watch_bounded(
        &self,
        scope: &str,
        maximum: usize,
        change_capacity: usize,
    ) -> Result<SupervisorWatch, PublicTransportError> {
        if maximum == 0 || change_capacity == 0 {
            return Err(PublicTransportError::InvalidLimits);
        }
        DeploymentTarget::new(scope, "validation")?;
        let prefix = format!("phoxal/{scope}/supervisors/");
        let suffix = "/presence";
        let selector = format!("{prefix}*{suffix}");
        let (change_sender, mut change_receiver) = mpsc::channel(change_capacity);
        let change_overflow = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let change_overflow_callback = change_overflow.clone();
        let subscriber = self
            .session
            .liveliness()
            .declare_subscriber(selector)
            .history(true)
            .callback(move |sample| {
                if change_sender
                    .try_send((sample.kind(), sample.key_expr().as_str().to_owned()))
                    .is_err()
                {
                    change_overflow_callback.fetch_add(1, std::sync::atomic::Ordering::Release);
                }
            })
            .await
            .map_err(|error| PublicTransportError::Transport(error.to_string()))?;
        let replies = self
            .session
            .liveliness()
            .get(format!("{prefix}*{suffix}"))
            .timeout(self.limits.deadline())
            .with(FifoChannel::new(maximum.saturating_add(1)))
            .await
            .map_err(|error| PublicTransportError::Transport(error.to_string()))?;
        let (sender, events) = mpsc::channel(change_capacity);
        let scope_owned = scope.to_owned();
        let task = tokio::spawn(async move {
            let mut targets = BTreeSet::new();
            let mut pending = Vec::new();
            let mut query_error = None;
            let mut query_complete = false;
            while !query_complete {
                tokio::select! {
                    reply = replies.recv_async() => match reply {
                        Ok(reply) => match reply.result() {
                            Ok(sample) => match parse_presence_target(&scope_owned, &prefix, suffix, sample.key_expr().as_str()) {
                                Ok(supervisor) => {
                                    targets.insert(supervisor);
                                    if targets.len() > maximum {
                                        query_error = Some(PublicTransportError::InventoryOverflow { maximum });
                                        query_complete = true;
                                    }
                                }
                                Err(error) => {
                                    query_error = Some(error);
                                    query_complete = true;
                                }
                            },
                            Err(error) => {
                                query_error = Some(PublicTransportError::Rejected {
                                    operation: "discover".to_owned(),
                                    detail: bounded_error_detail(&format!("{error:?}")),
                                });
                                query_complete = true;
                            }
                        },
                        Err(_) => query_complete = true,
                    },
                    change = change_receiver.recv() => match change {
                        Some(change) => {
                            if pending.len() < change_capacity {
                                pending.push(change);
                            } else {
                                change_overflow.fetch_add(1, std::sync::atomic::Ordering::Release);
                            }
                        }
                        None => query_complete = true,
                    }
                }
            }
            let mut snapshot = Vec::with_capacity(targets.len());
            for supervisor in targets {
                if let Ok(target) = DeploymentTarget::new(&scope_owned, supervisor) {
                    snapshot.push(target);
                }
            }
            let dropped = change_overflow.swap(0, std::sync::atomic::Ordering::AcqRel);
            let incomplete = query_error.is_some() || dropped != 0;
            if sender
                .send(DiscoveryEvent::Snapshot {
                    targets: snapshot.clone(),
                    complete: !incomplete,
                })
                .await
                .is_err()
            {
                drop(subscriber);
                return;
            }
            if let Some(error) = query_error
                && sender
                    .send(DiscoveryEvent::Unavailable {
                        detail: bounded_error_detail(&error.to_string()),
                    })
                    .await
                    .is_err()
            {
                drop(subscriber);
                return;
            }
            if dropped != 0 && sender.send(DiscoveryEvent::Gap { dropped }).await.is_err() {
                drop(subscriber);
                return;
            }
            let mut current = snapshot
                .iter()
                .map(|target| target.supervisor().to_owned())
                .collect::<BTreeSet<_>>();
            for (kind, key) in pending {
                if let Ok(supervisor) = parse_presence_target(&scope_owned, &prefix, suffix, &key)
                    && let Some(event) =
                        discovery_change(&mut current, &scope_owned, supervisor, kind)
                    && sender.send(event).await.is_err()
                {
                    drop(subscriber);
                    return;
                }
            }
            loop {
                let dropped = change_overflow.swap(0, std::sync::atomic::Ordering::AcqRel);
                if dropped != 0 && sender.send(DiscoveryEvent::Gap { dropped }).await.is_err() {
                    drop(subscriber);
                    return;
                }
                let Some((kind, key)) = change_receiver.recv().await else {
                    break;
                };
                let Ok(supervisor) = parse_presence_target(&scope_owned, &prefix, suffix, &key)
                else {
                    if sender
                        .send(DiscoveryEvent::Unavailable {
                            detail: format!("malformed presence key '{key}'"),
                        })
                        .await
                        .is_err()
                    {
                        break;
                    }
                    continue;
                };
                if let Some(event) = discovery_change(&mut current, &scope_owned, supervisor, kind)
                    && sender.send(event).await.is_err()
                {
                    break;
                }
            }
            drop(subscriber);
        });
        Ok(SupervisorWatch { events, task })
    }

    /// Close the shared physical transport.
    ///
    /// Callers should first close every logical session. Closing the transport
    /// deliberately invalidates all remaining handles because it is the
    /// single owner of their shared router connection.
    pub async fn close(self) -> Result<(), PublicTransportError> {
        self.session
            .close()
            .await
            .map_err(|error| PublicTransportError::Transport(error.to_string()))
    }

    /// Close this shared Zenoh transport while retaining a borrowed owner.
    ///
    /// `Connection` uses this method after closing its logical sessions so one
    /// physical transport remains the sole lifecycle authority.
    pub async fn shutdown(&self) -> Result<(), PublicTransportError> {
        self.session
            .close()
            .await
            .map_err(|error| PublicTransportError::Transport(error.to_string()))
    }
}

/// One established logical session over either a dedicated or shared Zenoh transport.
pub struct PublicSessionConnection {
    session: zenoh::Session,
    target: DeploymentTarget,
    principal: String,
    session_id: Vec<u8>,
    lease_deadline: Instant,
    info: SupervisorInfoResponse,
    limits: PublicTransportLimits,
    close_transport: bool,
}

/// A bounded public observation returned by a Watch or Subscribe exchange.
///
/// The admission response acknowledges the bounded observation cursor and
/// carries an optional initial record only for a State watch.  Later records
/// arrive on a dedicated Zenoh subscriber.  Declaring that subscriber before
/// issuing the admission request closes the enumeration race between the
/// initial value and the first publication.
pub struct PublicSubscription {
    initial: Option<SubscriptionRecord>,
    records: mpsc::Receiver<Result<SubscriptionRecord, PublicTransportError>>,
    task: JoinHandle<()>,
}

impl PublicSubscription {
    /// Take the initial record, if the selected port has one.
    pub fn take_initial(&mut self) -> Option<SubscriptionRecord> {
        self.initial.take()
    }

    /// Receive the next bounded publication record.
    pub async fn recv(&mut self) -> Option<Result<SubscriptionRecord, PublicTransportError>> {
        self.records.recv().await
    }
}

impl Drop for PublicSubscription {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl std::fmt::Debug for PublicSessionConnection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PublicSessionConnection")
            .field("target", &self.target)
            .field("principal", &self.principal)
            .field("session_id", &"<redacted>")
            .field("lease_deadline", &self.lease_deadline)
            .finish_non_exhaustive()
    }
}

impl PublicSessionConnection {
    /// Perform offer-only bootstrap, exact protocol open, and the mandatory
    /// initial information exchange.
    pub async fn connect(config: PublicSessionConfig) -> Result<Self, PublicTransportError> {
        let session = zenoh::open(client_config_with_security(
            config.endpoint(),
            config.security(),
        )?)
        .await
        .map_err(|error| PublicTransportError::Transport(error.to_string()))?;
        match Self::connect_on_session(session.clone(), config, true).await {
            Ok(connection) => Ok(connection),
            Err(error) => {
                let _ = session.close().await;
                Err(error)
            }
        }
    }

    async fn connect_on_session(
        session: zenoh::Session,
        config: PublicSessionConfig,
        close_transport: bool,
    ) -> Result<Self, PublicTransportError> {
        let bootstrap_key = config.target.bootstrap_key();
        let bootstrap_bytes =
            query_one(&session, bootstrap_key, None, "bootstrap", &config.limits).await?;
        let offers =
            decode_message::<SessionOffers>(&bootstrap_bytes, "bootstrap", &config.limits)?;
        let selected = validate_session_offers(&config.target, bootstrap_bytes.len(), &offers)?;
        if selected.protocol != SESSION_PROTOCOL {
            return Err(PublicTransportError::Bootstrap(
                BootstrapError::UnsupportedSessionProtocol,
            ));
        }

        let route =
            PublicRoute::for_operation(&config.target, &config.principal, PublicOperation::Open)
                .map_err(|error| malformed_client("open", error))?;
        let open_request = OpenSessionRequest {
            protocol: selected.protocol,
        };
        let open_started = Instant::now();
        let open_response = query_proto::<OpenSessionRequest, OpenSessionResponse>(
            &session,
            &route,
            &open_request,
            "open",
            &config.limits,
        )
        .await?;
        if open_response.protocol != SESSION_PROTOCOL || open_response.lease_ms == 0 {
            return Err(PublicTransportError::Malformed {
                operation: "open".to_owned(),
                detail: "server did not confirm the selected protocol and lease".to_owned(),
            });
        }
        let session_id = open_response.session_id;
        if crate::communication::SessionId::from_bytes(&session_id).is_err() {
            return Err(PublicTransportError::Malformed {
                operation: "open".to_owned(),
                detail: "server returned an invalid session identifier".to_owned(),
            });
        }
        let lease_deadline = open_started
            .checked_add(Duration::from_millis(u64::from(open_response.lease_ms)))
            .ok_or_else(|| PublicTransportError::Malformed {
                operation: "open".to_owned(),
                detail: "session lease overflows the local clock".to_owned(),
            })?;
        if Instant::now() >= lease_deadline {
            return Err(PublicTransportError::LeaseExpired);
        }

        let info_route =
            PublicRoute::for_operation(&config.target, &config.principal, PublicOperation::Info)
                .map_err(|error| malformed_client("info", error))?;
        let info_request = SupervisorInfoRequest {
            session_id: session_id.clone(),
        };
        let info = query_proto::<SupervisorInfoRequest, SupervisorInfoResponse>(
            &session,
            &info_route,
            &info_request,
            "info",
            &config.limits,
        )
        .await?;
        if info.supervisor_version.is_empty() || info.framework_version.is_empty() {
            return Err(PublicTransportError::Malformed {
                operation: "info".to_owned(),
                detail: "server returned empty diagnostic version information".to_owned(),
            });
        }

        Ok(Self {
            session,
            target: config.target,
            principal: config.principal,
            session_id,
            lease_deadline,
            info,
            limits: config.limits,
            close_transport,
        })
    }

    /// Selected deployment target.
    #[must_use]
    pub fn target(&self) -> &DeploymentTarget {
        &self.target
    }

    /// Protected principal bound to this logical session.
    #[must_use]
    pub fn principal(&self) -> &str {
        &self.principal
    }

    /// Opaque session identifier. The returned bytes are not credentials for
    /// another principal or deployment target.
    #[must_use]
    pub fn session_id(&self) -> &[u8] {
        &self.session_id
    }

    /// Cached information obtained before this handle was returned.
    #[must_use]
    pub fn info(&self) -> &SupervisorInfoResponse {
        &self.info
    }

    /// Bounded exchange limits used by this logical session.
    #[must_use]
    pub const fn limits(&self) -> &PublicTransportLimits {
        &self.limits
    }

    /// Renew this logical session with the fixed server lease.
    pub async fn renew(&mut self) -> Result<RenewSessionResponse, PublicTransportError> {
        self.ensure_lease()?;
        let route = self.route(PublicOperation::Renew)?;
        let request_started = Instant::now();
        let response = query_proto::<RenewSessionRequest, RenewSessionResponse>(
            &self.session,
            &route,
            &RenewSessionRequest {
                session_id: self.session_id.clone(),
            },
            "renew",
            &self.limits,
        )
        .await?;
        if response.lease_ms == 0 {
            return Err(PublicTransportError::Malformed {
                operation: "renew".to_owned(),
                detail: "server returned a zero lease".to_owned(),
            });
        }
        let deadline = request_started
            .checked_add(Duration::from_millis(u64::from(response.lease_ms)))
            .ok_or_else(|| PublicTransportError::Malformed {
                operation: "renew".to_owned(),
                detail: "renewed lease overflows the local clock".to_owned(),
            })?;
        if Instant::now() >= deadline {
            return Err(PublicTransportError::LeaseExpired);
        }
        self.lease_deadline = deadline;
        Ok(response)
    }

    /// Refresh the diagnostic information for this active session.
    pub async fn refresh_info(&mut self) -> Result<&SupervisorInfoResponse, PublicTransportError> {
        self.ensure_lease()?;
        let route = self.route(PublicOperation::Info)?;
        let response = query_proto::<SupervisorInfoRequest, SupervisorInfoResponse>(
            &self.session,
            &route,
            &SupervisorInfoRequest {
                session_id: self.session_id.clone(),
            },
            "info",
            &self.limits,
        )
        .await?;
        self.info = response;
        Ok(&self.info)
    }

    /// Read current supervisor lifecycle status.
    pub async fn status(&self) -> Result<SupervisorStatusResponse, PublicTransportError> {
        self.ensure_lease()?;
        let route = self.route(PublicOperation::Status)?;
        query_proto::<SupervisorStatusRequest, SupervisorStatusResponse>(
            &self.session,
            &route,
            &SupervisorStatusRequest {
                session_id: self.session_id.clone(),
            },
            "status",
            &self.limits,
        )
        .await
    }

    /// Read one bounded page of execution summaries.
    pub async fn list_executions(
        &self,
        request: ListExecutionsRequest,
    ) -> Result<ListExecutionsResponse, PublicTransportError> {
        self.ensure_lease()?;
        let route = self.route(PublicOperation::ListExecutions)?;
        query_proto::<ListExecutionsRequest, ListExecutionsResponse>(
            &self.session,
            &route,
            &request,
            "list-executions",
            &self.limits,
        )
        .await
    }

    /// Read one bounded page of generated public port metadata.
    pub async fn list_ports(
        &self,
        request: ListPortsRequest,
    ) -> Result<ListPortsResponse, PublicTransportError> {
        self.ensure_lease()?;
        let route = self.route(PublicOperation::ListPorts)?;
        query_proto::<ListPortsRequest, ListPortsResponse>(
            &self.session,
            &route,
            &request,
            "list-ports",
            &self.limits,
        )
        .await
    }

    /// Bind one exact generated public port descriptor.
    pub async fn bind(
        &self,
        request: BindPortRequest,
    ) -> Result<BindPortResponse, PublicTransportError> {
        self.ensure_lease()?;
        let route = self.route(PublicOperation::Bind)?;
        query_proto::<BindPortRequest, BindPortResponse>(
            &self.session,
            &route,
            &request,
            "bind",
            &self.limits,
        )
        .await
    }

    /// Execute one admitted public Read or Commands operation.
    pub async fn operation(
        &self,
        operation: PublicOperation,
        request: OperationRequest,
    ) -> Result<OperationResponse, PublicTransportError> {
        if !matches!(operation, PublicOperation::Read | PublicOperation::Command) {
            return Err(PublicTransportError::Malformed {
                operation: operation.segment().to_owned(),
                detail: "operation must be read or command".to_owned(),
            });
        }
        self.ensure_lease()?;
        let route = self.route(operation)?;
        let response = query_proto::<OperationRequest, OperationResponse>(
            &self.session,
            &route,
            &request,
            operation.segment(),
            &self.limits,
        )
        .await?;
        validate_operation_response(&response, &request, operation)?;
        Ok(response)
    }

    /// Exchange one generated simulation authority/boundary message on the
    /// dedicated `simulation/v1` route family.
    pub async fn simulation<Request, Response>(
        &self,
        operation: PublicOperation,
        request: Request,
    ) -> Result<Response, PublicTransportError>
    where
        Request: Message,
        Response: Message + Default,
    {
        if !matches!(
            operation,
            PublicOperation::AcquireAuthority
                | PublicOperation::Advance
                | PublicOperation::Reset
                | PublicOperation::ReleaseAuthority
                | PublicOperation::Progress
        ) {
            return Err(PublicTransportError::Malformed {
                operation: operation.segment().to_owned(),
                detail: "operation is not in the simulation lane".to_owned(),
            });
        }
        self.ensure_lease()?;
        let route = self.route(operation)?;
        query_proto(
            &self.session,
            &route,
            &request,
            operation.segment(),
            &self.limits,
        )
        .await
    }

    /// Establish one admitted public observation.
    pub async fn subscribe(
        &self,
        operation: PublicOperation,
        request: SubscriptionRequest,
    ) -> Result<PublicSubscription, PublicTransportError> {
        self.subscribe_with_cancel(operation, request, CancellationToken::new())
            .await
    }

    /// Establish one observation whose task is additionally cancelled when
    /// its owning logical supervisor session is replaced or lost.
    pub async fn subscribe_with_cancel(
        &self,
        operation: PublicOperation,
        request: SubscriptionRequest,
        cancellation: CancellationToken,
    ) -> Result<PublicSubscription, PublicTransportError> {
        if !matches!(
            operation,
            PublicOperation::Watch | PublicOperation::Subscribe
        ) {
            return Err(PublicTransportError::Malformed {
                operation: operation.segment().to_owned(),
                detail: "operation must be watch or subscribe".to_owned(),
            });
        }
        self.ensure_lease()?;
        if request.session_id != self.session_id {
            return Err(PublicTransportError::Malformed {
                operation: operation.segment().to_owned(),
                detail: "subscription session is invalid".to_owned(),
            });
        }
        let capacity = validate_subscription_request(&request, &self.limits)?;
        let key = subscription_key(&self.target, &self.principal, &request.subscription_id);
        let subscriber = self
            .session
            .declare_subscriber(key)
            .with(FifoChannel::new(capacity))
            .await
            .map_err(|error| PublicTransportError::Transport(error.to_string()))?;
        let (sender, receiver) = mpsc::channel(capacity);
        let limits = self.limits.clone();
        let operation_name = operation.segment().to_owned();
        let task_cancellation = cancellation;
        let task = tokio::spawn(async move {
            loop {
                let sample = tokio::select! {
                    () = task_cancellation.cancelled() => break,
                    sample = subscriber.recv_async() => match sample {
                        Ok(sample) => sample,
                        Err(_) => break,
                    },
                };
                let result =
                    if sample.encoding() != &Encoding::from(PUBLIC_PROTOBUF_ENCODING.to_owned()) {
                        Err(PublicTransportError::WrongEncoding {
                            operation: operation_name.clone(),
                        })
                    } else if sample.payload().len() > limits.max_response_bytes() {
                        Err(PublicTransportError::BodyTooLarge {
                            operation: operation_name.clone(),
                            bytes: sample.payload().len(),
                            maximum: limits.max_response_bytes(),
                        })
                    } else {
                        decode_message::<SubscriptionRecord>(
                            &sample.payload().to_bytes(),
                            &operation_name,
                            &limits,
                        )
                    };
                if sender.try_send(result).is_err() {
                    // Preserve the fact of loss. Waiting for one bounded slot
                    // is preferable to silently turning queue saturation into
                    // a normal end-of-stream; the explicit error terminates
                    // this observer and lets the caller resubscribe.
                    let overflow = Err(PublicTransportError::SubscriptionOverflow {
                        operation: operation_name.clone(),
                    });
                    if sender.send(overflow).await.is_err() {
                        break;
                    }
                    break;
                }
            }
        });
        let route = self.route(operation)?;
        let admission = match query_proto::<SubscriptionRequest, SubscriptionAdmission>(
            &self.session,
            &route,
            &request,
            operation.segment(),
            &self.limits,
        )
        .await
        {
            Ok(initial) => initial,
            Err(error) => {
                task.abort();
                return Err(error);
            }
        };
        let initial =
            match validate_subscription_admission(&admission, &request, &self.limits, operation) {
                Ok(initial) => initial,
                Err(error) => {
                    task.abort();
                    return Err(error);
                }
            };
        Ok(PublicSubscription {
            initial,
            records: receiver,
            task,
        })
    }

    /// Close this logical session and then close its owned Zenoh transport.
    pub async fn close(self) -> Result<CloseSessionResponse, PublicTransportError> {
        let PublicSessionConnection {
            session,
            target,
            principal,
            session_id,
            lease_deadline,
            limits,
            close_transport,
            ..
        } = self;
        let close_result = if Instant::now() < lease_deadline {
            let route = PublicRoute::for_operation(&target, &principal, PublicOperation::Close)
                .map_err(|error| malformed_client("close", error));
            match route {
                Ok(route) => {
                    query_proto::<CloseSessionRequest, CloseSessionResponse>(
                        &session,
                        &route,
                        &CloseSessionRequest { session_id },
                        "close",
                        &limits,
                    )
                    .await
                }
                Err(error) => Err(error),
            }
        } else {
            Err(PublicTransportError::LeaseExpired)
        };
        let transport_result = if close_transport {
            session
                .close()
                .await
                .map_err(|error| PublicTransportError::Transport(error.to_string()))
        } else {
            Ok(())
        };
        match (close_result, transport_result) {
            (Ok(response), Ok(())) => Ok(response),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(error)) | (Err(_), Err(error)) => Err(error),
        }
    }

    fn route(&self, operation: PublicOperation) -> Result<PublicRoute, PublicTransportError> {
        PublicRoute::for_operation(&self.target, &self.principal, operation)
            .map_err(|error| malformed_client(operation.segment(), error))
    }

    fn ensure_lease(&self) -> Result<(), PublicTransportError> {
        if Instant::now() >= self.lease_deadline {
            Err(PublicTransportError::LeaseExpired)
        } else {
            Ok(())
        }
    }
}

async fn serve_bootstrap(
    queryable: PublicQueryable,
    target: DeploymentTarget,
    limits: PublicTransportLimits,
    started: Instant,
    shutdown: CancellationToken,
) {
    loop {
        let query = tokio::select! {
            () = shutdown.cancelled() => return,
            received = queryable.recv_async() => match received {
                Ok(query) => query,
                Err(_) => return,
            },
        };
        let key = query.key_expr().to_string();
        let operation = "bootstrap";
        if key != target.bootstrap_key() {
            send_error(
                &query,
                &PublicTransportError::WrongReplyKey {
                    expected: target.bootstrap_key(),
                    actual: key,
                    operation: operation.to_owned(),
                },
                &limits,
            )
            .await;
            continue;
        }
        if query.payload().is_some_and(|payload| !payload.is_empty()) {
            send_error(
                &query,
                &PublicTransportError::Malformed {
                    operation: operation.to_owned(),
                    detail: "bootstrap request body must be empty".to_owned(),
                },
                &limits,
            )
            .await;
            continue;
        }
        if query.encoding().is_some() {
            send_error(
                &query,
                &PublicTransportError::Malformed {
                    operation: operation.to_owned(),
                    detail: "bootstrap request must not carry an encoding".to_owned(),
                },
                &limits,
            )
            .await;
            continue;
        }
        let offers = target.session_offers();
        let response = match encode_message(&offers, limits.max_response_bytes(), operation) {
            Ok(response) => response,
            Err(error) => {
                send_error(&query, &error, &limits).await;
                continue;
            }
        };
        if let Err(error) = query
            .reply(query.key_expr(), response)
            .encoding(Encoding::from(PUBLIC_PROTOBUF_ENCODING.to_owned()))
            .await
        {
            tracing::debug!(error = %error, "public bootstrap reply failed");
        }
        let _ = started;
    }
}

async fn serve_operation(
    queryable: PublicQueryable,
    operation: PublicOperation,
    context: OperationServerContext,
) {
    loop {
        let query = tokio::select! {
            () = context.shutdown.cancelled() => return,
            received = queryable.recv_async() => match received {
                Ok(query) => query,
                Err(_) => return,
            },
        };
        serve_one_operation(
            &query,
            operation,
            &context.target,
            &context.session,
            &context.adapter,
            &context.backend,
            &context.subscriptions,
            &context.subscription_tasks,
            &context.simulation_authority,
            &context.simulation_backend,
            &context.principal_policy,
            &context.limits,
            &context.shutdown,
            context.started,
        )
        .await;
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the operation loop receives one immutable bounded server context"
)]
async fn serve_one_operation(
    query: &Query,
    operation: PublicOperation,
    target: &DeploymentTarget,
    session: &zenoh::Session,
    adapter: &Arc<Mutex<SupervisorAdapter>>,
    backend: &Arc<dyn PublicSessionBackend>,
    subscriptions: &Arc<Mutex<BTreeMap<String, CancellationToken>>>,
    subscription_tasks: &Arc<Mutex<Vec<JoinHandle<()>>>>,
    simulation_authority: &Arc<Mutex<Option<SimulationAuthority>>>,
    simulation_backend: &Arc<dyn PublicSimulationBackend>,
    principal_policy: &PrincipalPolicy,
    limits: &PublicTransportLimits,
    shutdown: &CancellationToken,
    started: Instant,
) {
    let operation_name = operation.segment();
    let key = query.key_expr().to_string();
    let route = match PublicRoute::parse(target, &key) {
        Ok(route) if route.operation() == operation => route,
        Ok(route) => {
            send_error(
                query,
                &PublicTransportError::Unauthorized {
                    operation: operation_name.to_owned(),
                    detail: format!(
                        "route operation is {:?}, expected {:?}",
                        route.operation(),
                        operation
                    ),
                },
                limits,
            )
            .await;
            return;
        }
        Err(error) => {
            send_error(
                query,
                &PublicTransportError::Unauthorized {
                    operation: operation_name.to_owned(),
                    detail: error.to_string(),
                },
                limits,
            )
            .await;
            return;
        }
    };
    if !principal_policy.allows(route.principal()) {
        send_error(
            query,
            &PublicTransportError::Unauthorized {
                operation: operation_name.to_owned(),
                detail: "principal is not admitted by the trusted ingress policy".to_owned(),
            },
            limits,
        )
        .await;
        return;
    }
    let request_bytes = match query.payload() {
        Some(payload) if payload.len() <= limits.max_request_bytes() => payload.to_bytes(),
        Some(payload) => {
            send_error(
                query,
                &PublicTransportError::BodyTooLarge {
                    operation: operation_name.to_owned(),
                    bytes: payload.len(),
                    maximum: limits.max_request_bytes(),
                },
                limits,
            )
            .await;
            return;
        }
        None => {
            send_error(
                query,
                &PublicTransportError::Malformed {
                    operation: operation_name.to_owned(),
                    detail: "operation request body is missing".to_owned(),
                },
                limits,
            )
            .await;
            return;
        }
    };
    if query.encoding() != Some(&Encoding::from(PUBLIC_PROTOBUF_ENCODING.to_owned())) {
        send_error(
            query,
            &PublicTransportError::Malformed {
                operation: operation_name.to_owned(),
                detail: "operation request must use standard Protobuf encoding".to_owned(),
            },
            limits,
        )
        .await;
        return;
    }

    let now_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
    match operation {
        PublicOperation::Open => {
            let request = match decode_request::<OpenSessionRequest>(
                &request_bytes,
                operation_name,
                limits,
            ) {
                Ok(request) => request,
                Err(error) => {
                    send_error(query, &error, limits).await;
                    return;
                }
            };
            let result = adapter.lock().await.open(&route, &request, now_ms);
            reply_adapter(query, result, operation_name, limits).await;
        }
        PublicOperation::Renew => {
            let request =
                match decode_request::<RenewSessionRequest>(&request_bytes, operation_name, limits)
                {
                    Ok(request) => request,
                    Err(error) => {
                        send_error(query, &error, limits).await;
                        return;
                    }
                };
            let result = adapter.lock().await.renew(&route, &request, now_ms);
            reply_adapter(query, result, operation_name, limits).await;
        }
        PublicOperation::Close => {
            let request =
                match decode_request::<CloseSessionRequest>(&request_bytes, operation_name, limits)
                {
                    Ok(request) => request,
                    Err(error) => {
                        send_error(query, &error, limits).await;
                        return;
                    }
                };
            let result = adapter.lock().await.close(&route, &request, now_ms);
            if result.is_ok() {
                cancel_session_subscriptions(&route, &request.session_id, subscriptions).await;
                revoke_simulation_for_session(
                    &route,
                    &request.session_id,
                    adapter,
                    simulation_authority,
                    simulation_backend,
                )
                .await;
            }
            reply_adapter(query, result, operation_name, limits).await;
        }
        PublicOperation::Info => {
            let request = match decode_request::<SupervisorInfoRequest>(
                &request_bytes,
                operation_name,
                limits,
            ) {
                Ok(request) => request,
                Err(error) => {
                    send_error(query, &error, limits).await;
                    return;
                }
            };
            let session_id = request.session_id.clone();
            let result = adapter
                .lock()
                .await
                .info(&route, &session_id, &request, now_ms);
            reply_adapter(query, result, operation_name, limits).await;
        }
        PublicOperation::Status => {
            let request = match decode_request::<SupervisorStatusRequest>(
                &request_bytes,
                operation_name,
                limits,
            ) {
                Ok(request) => request,
                Err(error) => {
                    send_error(query, &error, limits).await;
                    return;
                }
            };
            let session_id = request.session_id.clone();
            let result = adapter
                .lock()
                .await
                .status(&route, &session_id, &request, now_ms);
            reply_adapter(query, result, operation_name, limits).await;
        }
        PublicOperation::ListExecutions => {
            let request = match decode_request::<ListExecutionsRequest>(
                &request_bytes,
                operation_name,
                limits,
            ) {
                Ok(request) => request,
                Err(error) => {
                    send_error(query, &error, limits).await;
                    return;
                }
            };
            let session_id = request.session_id.clone();
            let result =
                adapter
                    .lock()
                    .await
                    .list_executions(&route, &session_id, &request, now_ms);
            reply_adapter(query, result, operation_name, limits).await;
        }
        PublicOperation::ListPorts => {
            let request =
                match decode_request::<ListPortsRequest>(&request_bytes, operation_name, limits) {
                    Ok(request) => request,
                    Err(error) => {
                        send_error(query, &error, limits).await;
                        return;
                    }
                };
            let session_id = request.session_id.clone();
            let result = adapter
                .lock()
                .await
                .list_ports(&route, &session_id, &request, now_ms);
            reply_adapter(query, result, operation_name, limits).await;
        }
        PublicOperation::Bind => {
            let request =
                match decode_request::<BindPortRequest>(&request_bytes, operation_name, limits) {
                    Ok(request) => request,
                    Err(error) => {
                        send_error(query, &error, limits).await;
                        return;
                    }
                };
            let result = adapter.lock().await.bind(&route, &request, now_ms);
            reply_adapter(query, result, operation_name, limits).await;
        }
        PublicOperation::Read | PublicOperation::Command => {
            let request =
                match decode_request::<OperationRequest>(&request_bytes, operation_name, limits) {
                    Ok(request) => request,
                    Err(error) => {
                        send_error(query, &error, limits).await;
                        return;
                    }
                };
            let binding = match adapter
                .lock()
                .await
                .validate_operation_binding(&route, &request, operation, now_ms)
            {
                Ok(binding) => binding,
                Err(error) => {
                    send_error(
                        query,
                        &PublicTransportError::Adapter {
                            operation: operation_name.to_owned(),
                            detail: error.to_string(),
                        },
                        limits,
                    )
                    .await;
                    return;
                }
            };
            if request.correlation_id.is_empty() {
                send_error(
                    query,
                    &PublicTransportError::Malformed {
                        operation: operation_name.to_owned(),
                        detail: "operation correlation_id must not be empty".to_owned(),
                    },
                    limits,
                )
                .await;
                return;
            }
            let timeout = bounded_timeout(request.timeout_ms, limits.deadline());
            let outcome = match tokio::time::timeout(
                timeout,
                backend.call(operation, binding.into(), request.payload.clone(), timeout),
            )
            .await
            {
                Ok(outcome) => outcome,
                Err(_) => Ok(PublicBackendOutcome::OutcomeUnknown(
                    "public service backend deadline elapsed".to_owned(),
                )),
            };
            let response = match outcome {
                Ok(PublicBackendOutcome::Received(payload))
                    if payload.len() <= limits.max_response_bytes() =>
                {
                    OperationResponse {
                        session_id: request.session_id.clone(),
                        binding_id: request.binding_id.clone(),
                        correlation_id: request.correlation_id.clone(),
                        execution_id: request.execution_id.clone(),
                        timeline_id: request.timeline_id.clone(),
                        outcome: OperationOutcome::Received as i32,
                        payload,
                        detail: None,
                    }
                }
                Ok(PublicBackendOutcome::Received(_payload)) => OperationResponse {
                    session_id: request.session_id.clone(),
                    binding_id: request.binding_id.clone(),
                    correlation_id: request.correlation_id.clone(),
                    execution_id: request.execution_id.clone(),
                    timeline_id: request.timeline_id.clone(),
                    outcome: OperationOutcome::Unknown as i32,
                    payload: Vec::new(),
                    detail: Some(
                        "public service returned a response larger than the wire bound".to_owned(),
                    ),
                },
                Ok(PublicBackendOutcome::NotSent(detail)) => OperationResponse {
                    session_id: request.session_id.clone(),
                    binding_id: request.binding_id.clone(),
                    correlation_id: request.correlation_id.clone(),
                    execution_id: request.execution_id.clone(),
                    timeline_id: request.timeline_id.clone(),
                    outcome: OperationOutcome::NotSent as i32,
                    payload: Vec::new(),
                    detail: Some(bounded_error_detail(&detail)),
                },
                Ok(PublicBackendOutcome::RejectedBeforeAdmission(detail)) => OperationResponse {
                    session_id: request.session_id.clone(),
                    binding_id: request.binding_id.clone(),
                    correlation_id: request.correlation_id.clone(),
                    execution_id: request.execution_id.clone(),
                    timeline_id: request.timeline_id.clone(),
                    outcome: OperationOutcome::RejectedBeforeAdmission as i32,
                    payload: Vec::new(),
                    detail: Some(bounded_error_detail(&detail)),
                },
                Ok(PublicBackendOutcome::OutcomeUnknown(detail))
                | Err(PublicBackendError::Transport(detail)) => OperationResponse {
                    session_id: request.session_id.clone(),
                    binding_id: request.binding_id.clone(),
                    correlation_id: request.correlation_id.clone(),
                    execution_id: request.execution_id.clone(),
                    timeline_id: request.timeline_id.clone(),
                    outcome: OperationOutcome::Unknown as i32,
                    payload: Vec::new(),
                    detail: Some(bounded_error_detail(&detail)),
                },
                Err(error) => OperationResponse {
                    session_id: request.session_id.clone(),
                    binding_id: request.binding_id.clone(),
                    correlation_id: request.correlation_id.clone(),
                    execution_id: request.execution_id.clone(),
                    timeline_id: request.timeline_id.clone(),
                    outcome: OperationOutcome::RejectedBeforeAdmission as i32,
                    payload: Vec::new(),
                    detail: Some(bounded_error_detail(&error.to_string())),
                },
            };
            reply_message(query, &response, operation_name, limits).await;
        }
        PublicOperation::Watch | PublicOperation::Subscribe => {
            let request =
                match decode_request::<SubscriptionRequest>(&request_bytes, operation_name, limits)
                {
                    Ok(request) => request,
                    Err(error) => {
                        send_error(query, &error, limits).await;
                        return;
                    }
                };
            if let Err(error) = validate_subscription_request(&request, limits) {
                send_error(query, &error, limits).await;
                return;
            }
            let operation_request = OperationRequest {
                session_id: request.session_id.clone(),
                binding_id: request.binding_id.clone(),
                correlation_id: request.subscription_id.clone(),
                execution_id: request.execution_id.clone(),
                timeline_id: request.timeline_id.clone(),
                payload: Vec::new(),
                timeout_ms: 0,
            };
            let binding = match adapter.lock().await.validate_operation_binding(
                &route,
                &operation_request,
                operation,
                now_ms,
            ) {
                Ok(binding) => binding,
                Err(error) => {
                    send_error(
                        query,
                        &PublicTransportError::Adapter {
                            operation: operation_name.to_owned(),
                            detail: error.to_string(),
                        },
                        limits,
                    )
                    .await;
                    return;
                }
            };
            let subscription_id = subscription_map_key(
                route.principal(),
                &request.session_id,
                &request.subscription_id,
            );
            let cancellation = CancellationToken::new();
            let duplicate = {
                let mut active = subscriptions.lock().await;
                if active.contains_key(&subscription_id) {
                    true
                } else {
                    active.insert(subscription_id.clone(), cancellation.clone());
                    false
                }
            };
            if duplicate {
                send_error(
                    query,
                    &PublicTransportError::Rejected {
                        operation: operation_name.to_owned(),
                        detail: "subscription identifier is already active for this session"
                            .to_owned(),
                    },
                    limits,
                )
                .await;
                return;
            }
            let capacity = match validate_subscription_request(&request, limits) {
                Ok(capacity) => capacity,
                Err(error) => {
                    subscriptions.lock().await.remove(&subscription_id);
                    send_error(query, &error, limits).await;
                    return;
                }
            };
            let mut source =
                match backend.subscribe(operation, binding.into(), request.clone(), capacity) {
                    Ok(source) => source,
                    Err(error) => {
                        subscriptions.lock().await.remove(&subscription_id);
                        send_error(
                            query,
                            &PublicTransportError::Rejected {
                                operation: operation_name.to_owned(),
                                detail: bounded_error_detail(&error.to_string()),
                            },
                            limits,
                        )
                        .await;
                        return;
                    }
                };
            let source_initial = source.take_initial();
            if operation == PublicOperation::Subscribe && source_initial.is_some() {
                subscriptions.lock().await.remove(&subscription_id);
                send_error(
                    query,
                    &PublicTransportError::Rejected {
                        operation: operation_name.to_owned(),
                        detail: "non-State subscriptions cannot replay an initial value".to_owned(),
                    },
                    limits,
                )
                .await;
                return;
            }
            let initial = if operation == PublicOperation::Watch {
                Some(source_initial.unwrap_or_else(|| SubscriptionRecord {
                    session_id: request.session_id.clone(),
                    binding_id: request.binding_id.clone(),
                    subscription_id: request.subscription_id.clone(),
                    execution_id: request.execution_id.clone(),
                    timeline_id: request.timeline_id.clone(),
                    revision: 0,
                    // State watches use an explicit control marker when the
                    // admitted state has no value.  This marker is carried
                    // by the admission response, never by the data stream.
                    kind: RecordKind::InitialAbsent as i32,
                    payload: Vec::new(),
                    dropped: 0,
                    detail: None,
                }))
            } else {
                // Event, Sample, and Stream subscriptions begin at the
                // admitted boundary with no initial record.
                None
            };
            if let Some(initial) = initial.as_ref()
                && let Err(error) = validate_state_initial_record(initial, &request, limits)
            {
                subscriptions.lock().await.remove(&subscription_id);
                send_error(query, &error, limits).await;
                return;
            }
            let key = subscription_key(target, route.principal(), &request.subscription_id);
            let key_for_task = key.clone();
            let session = session.clone();
            let limits_for_task = limits.clone();
            let cancellation_for_task = cancellation.clone();
            let request_for_task = request.clone();
            let shutdown_for_task = shutdown.clone();
            let subscriptions_for_task = subscriptions.clone();
            let subscription_id_for_task = subscription_id.clone();
            let adapter_for_task = adapter.clone();
            let route_for_task = route.clone();
            let operation_request_for_task = operation_request.clone();
            let started_for_task = started;
            let task = tokio::spawn(async move {
                let mut health_check = tokio::time::interval(Duration::from_secs(1));
                health_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    let next = tokio::select! {
                        () = cancellation_for_task.cancelled() => break,
                        () = shutdown_for_task.cancelled() => break,
                        _ = health_check.tick() => {
                            let now_ms = started_for_task.elapsed().as_millis().min(u64::MAX as u128) as u64;
                            let valid = adapter_for_task
                                .lock()
                                .await
                                .validate_operation_binding(
                                    &route_for_task,
                                    &operation_request_for_task,
                                    operation,
                                    now_ms,
                                )
                                .is_ok();
                            if !valid { break; }
                            continue;
                        }
                        record = source.records.recv() => match record {
                            Some(record) => record,
                            None => break,
                        },
                    };
                    match next {
                        Ok(record) => {
                            if validate_subscription_record(
                                &record,
                                &request_for_task,
                                &limits_for_task,
                                false,
                            )
                            .is_err()
                            {
                                continue;
                            }
                            let Ok(payload) = encode_message(
                                &record,
                                limits_for_task.max_response_bytes(),
                                "subscription",
                            ) else {
                                continue;
                            };
                            let _ = session
                                .put(key_for_task.clone(), payload)
                                .encoding(Encoding::from(PUBLIC_PROTOBUF_ENCODING.to_owned()))
                                .await;
                        }
                        Err(error) => {
                            let record = SubscriptionRecord {
                                session_id: request_for_task.session_id.clone(),
                                binding_id: request_for_task.binding_id.clone(),
                                subscription_id: request_for_task.subscription_id.clone(),
                                execution_id: request_for_task.execution_id.clone(),
                                timeline_id: request_for_task.timeline_id.clone(),
                                revision: 0,
                                kind: RecordKind::Failed as i32,
                                payload: Vec::new(),
                                dropped: 0,
                                detail: Some(bounded_error_detail(&error.to_string())),
                            };
                            if let Ok(payload) = encode_message(
                                &record,
                                limits_for_task.max_response_bytes(),
                                "subscription",
                            ) {
                                let _ = session
                                    .put(key_for_task.clone(), payload)
                                    .encoding(Encoding::from(PUBLIC_PROTOBUF_ENCODING.to_owned()))
                                    .await;
                            }
                            break;
                        }
                    }
                }
                subscriptions_for_task
                    .lock()
                    .await
                    .remove(&subscription_id_for_task);
            });
            subscription_tasks.lock().await.push(task);
            reply_message(
                query,
                &SubscriptionAdmission {
                    session_id: request.session_id,
                    binding_id: request.binding_id,
                    subscription_id: request.subscription_id,
                    execution_id: request.execution_id,
                    timeline_id: request.timeline_id,
                    initial,
                },
                operation_name,
                limits,
            )
            .await;
        }
        PublicOperation::AcquireAuthority
        | PublicOperation::Advance
        | PublicOperation::Reset
        | PublicOperation::ReleaseAuthority
        | PublicOperation::Progress => {
            serve_simulation_operation(
                query,
                operation,
                &route,
                &request_bytes,
                adapter,
                subscriptions,
                simulation_authority,
                simulation_backend,
                now_ms,
                limits,
            )
            .await;
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the operation loop receives one immutable bounded server context"
)]
async fn serve_simulation_operation(
    query: &Query,
    operation: PublicOperation,
    route: &PublicRoute,
    request_bytes: &[u8],
    adapter: &Arc<Mutex<SupervisorAdapter>>,
    subscriptions: &Arc<Mutex<BTreeMap<String, CancellationToken>>>,
    simulation_authority: &Arc<Mutex<Option<SimulationAuthority>>>,
    simulation_backend: &Arc<dyn PublicSimulationBackend>,
    now_ms: u64,
    limits: &PublicTransportLimits,
) {
    let operation_name = operation.segment();
    match operation {
        PublicOperation::AcquireAuthority => {
            let request = match decode_request::<AcquireAuthorityRequest>(
                request_bytes,
                operation_name,
                limits,
            ) {
                Ok(request) => request,
                Err(error) => {
                    send_error(query, &error, limits).await;
                    return;
                }
            };
            let result = acquire_simulation_authority(
                route,
                request,
                adapter,
                simulation_authority,
                simulation_backend,
                now_ms,
            )
            .await;
            reply_simulation_result(query, result, operation_name, limits).await;
        }
        PublicOperation::Advance => {
            let request =
                match decode_request::<AdvanceRequest>(request_bytes, operation_name, limits) {
                    Ok(request) => request,
                    Err(error) => {
                        send_error(query, &error, limits).await;
                        return;
                    }
                };
            let result = advance_simulation(
                route,
                request,
                adapter,
                simulation_authority,
                simulation_backend,
                now_ms,
            )
            .await;
            reply_simulation_result(query, result, operation_name, limits).await;
        }
        PublicOperation::Reset => {
            let request =
                match decode_request::<ResetRequest>(request_bytes, operation_name, limits) {
                    Ok(request) => request,
                    Err(error) => {
                        send_error(query, &error, limits).await;
                        return;
                    }
                };
            let result = reset_simulation(
                route,
                request,
                adapter,
                subscriptions,
                simulation_authority,
                simulation_backend,
                now_ms,
            )
            .await;
            reply_simulation_result(query, result, operation_name, limits).await;
        }
        PublicOperation::ReleaseAuthority => {
            let request = match decode_request::<ReleaseAuthorityRequest>(
                request_bytes,
                operation_name,
                limits,
            ) {
                Ok(request) => request,
                Err(error) => {
                    send_error(query, &error, limits).await;
                    return;
                }
            };
            let result = release_simulation(
                route,
                request,
                adapter,
                simulation_authority,
                simulation_backend,
                now_ms,
            )
            .await;
            reply_simulation_result(query, result, operation_name, limits).await;
        }
        PublicOperation::Progress => {
            let request =
                match decode_request::<ProgressRequest>(request_bytes, operation_name, limits) {
                    Ok(request) => request,
                    Err(error) => {
                        send_error(query, &error, limits).await;
                        return;
                    }
                };
            let result = progress_simulation(
                route,
                request,
                adapter,
                simulation_authority,
                simulation_backend,
                now_ms,
            )
            .await;
            reply_simulation_result(query, result, operation_name, limits).await;
        }
        _ => unreachable!("serve_simulation_operation received a non-simulation operation"),
    }
}

async fn acquire_simulation_authority(
    route: &PublicRoute,
    request: AcquireAuthorityRequest,
    adapter: &Arc<Mutex<SupervisorAdapter>>,
    authority: &Arc<Mutex<Option<SimulationAuthority>>>,
    simulation_backend: &Arc<dyn PublicSimulationBackend>,
    now_ms: u64,
) -> Result<AcquireAuthorityResponse, PublicTransportError> {
    if request.execution_id.is_empty()
        || request.model_identity.is_empty()
        || !request.model_identity.is_ascii()
        || request.quantum_ns == 0
        || request.providers.is_empty()
        || request.session_id.is_empty()
        || request.correlation_id.is_empty()
        || request.correlation_id.len() > MAX_SIMULATION_CORRELATION_BYTES
    {
        return Err(simulation_rejected(
            "acquire-authority requires execution, model, quantum, and providers",
        ));
    }
    let mut adapter_guard = adapter.lock().await;
    adapter_guard
        .authorize_simulation_session(route, &request.session_id, now_ms)
        .map_err(|error| simulation_adapter_error("acquire-authority", error))?;
    let summary = adapter_guard
        .simulation_execution(&request.execution_id)
        .map_err(|error| simulation_adapter_error("acquire-authority", error))?;
    let definition = adapter_guard
        .simulation_definition(&request.execution_id)
        .map_err(|error| simulation_adapter_error("acquire-authority", error))?;
    if request.model_identity != definition.model_identity()
        || request.quantum_ns != definition.quantum_ns()
        || request.providers.len() != definition.providers().len()
    {
        return Err(simulation_rejected(
            "simulation request does not match the supervisor-owned definition",
        ));
    }
    let definition_model_identity = definition.model_identity().to_owned();
    let definition_quantum_ns = definition.quantum_ns();
    let state = crate::communication::session::ExecutionState::try_from(summary.state)
        .map_err(|_| simulation_rejected("execution state is unknown"))?;
    if !matches!(
        state,
        crate::communication::session::ExecutionState::Ready
            | crate::communication::session::ExecutionState::Active
    ) {
        return Err(simulation_rejected(
            "simulation authority requires a ready or active execution",
        ));
    }
    let mut providers = BTreeSet::new();
    for provider in &request.providers {
        if provider.service_instance.is_empty()
            || provider.port.is_empty()
            || provider.payload_fqn.is_empty()
            || !provider.payload_fqn.is_ascii()
            || !providers.insert((provider.service_instance.as_str(), provider.port.as_str()))
        {
            return Err(simulation_rejected(
                "simulation providers must be complete and unique",
            ));
        }
        adapter_guard
            .validate_simulation_provider(
                &request.execution_id,
                &provider.service_instance,
                &provider.port,
                provider.kind,
                &provider.input_fqn,
                &provider.payload_fqn,
            )
            .map_err(|error| simulation_adapter_error("acquire-authority", error))?;
    }
    drop(adapter_guard);
    let mut authority_guard = authority.lock().await;
    if let Some(current) = authority_guard.as_ref() {
        let mut adapter_guard = adapter.lock().await;
        let current_is_live = Instant::now() < current.lease_deadline
            && adapter_guard.simulation_session_active(
                &current.principal,
                &current.session_id,
                now_ms,
            )
            && adapter_guard
                .simulation_execution(&current.execution_id)
                .is_ok_and(|summary| summary.timeline_id == current.timeline_id);
        drop(adapter_guard);
        if current_is_live {
            return Err(simulation_rejected(
                "simulation authority is already held by another client",
            ));
        }
        if let Some(stale) = authority_guard.take() {
            release_backend_authority(stale, simulation_backend).await;
        }
    }
    let mut grant = vec![0_u8; SIMULATION_GRANT_BYTES];
    getrandom::fill(&mut grant).map_err(|_| {
        PublicTransportError::Transport("cannot obtain simulation authority entropy".to_owned())
    })?;
    let lease_deadline = Instant::now()
        .checked_add(SIMULATION_AUTHORITY_LEASE)
        .ok_or_else(|| simulation_rejected("simulation authority lease overflows the clock"))?;
    let context = PublicSimulationContext {
        principal: route.principal().to_owned(),
        session_id: request.session_id.clone(),
        authority_grant: grant.clone(),
        correlation_id: request.correlation_id.clone(),
        execution_id: request.execution_id.clone(),
        timeline_id: summary.timeline_id.clone(),
        completed_boundary: 0,
        model_identity: definition_model_identity.clone(),
        quantum_ns: definition_quantum_ns,
    };
    let acquire = tokio::time::timeout(
        DEFAULT_PUBLIC_DEADLINE,
        simulation_backend.acquire(context, request.clone()),
    )
    .await
    .map_err(|_| PublicTransportError::Timeout {
        operation: "acquire-authority".to_owned(),
    })?;
    acquire.map_err(|error| simulation_backend_error("acquire-authority", error))?;
    *authority_guard = Some(SimulationAuthority {
        principal: route.principal().to_owned(),
        session_id: request.session_id.clone(),
        grant: grant.clone(),
        execution_id: request.execution_id.clone(),
        timeline_id: summary.timeline_id.clone(),
        model_identity: definition_model_identity.clone(),
        quantum_ns: definition_quantum_ns,
        boundary: 0,
        lease_deadline,
        advance_results: VecDeque::new(),
    });
    Ok(AcquireAuthorityResponse {
        authority_grant: grant,
        timeline_id: summary.timeline_id,
        boundary: 0,
        lease_ms: SIMULATION_AUTHORITY_LEASE.as_millis() as u32,
        session_id: request.session_id,
        execution_id: request.execution_id,
        model_identity: definition_model_identity,
        quantum_ns: definition_quantum_ns,
        correlation_id: request.correlation_id,
    })
}

async fn revoke_simulation_for_session(
    route: &PublicRoute,
    session_id: &[u8],
    _adapter: &Arc<Mutex<SupervisorAdapter>>,
    authority: &Arc<Mutex<Option<SimulationAuthority>>>,
    simulation_backend: &Arc<dyn PublicSimulationBackend>,
) {
    let mut guard = authority.lock().await;
    let Some(current) = guard.as_ref() else {
        return;
    };
    if current.principal != route.principal() || current.session_id != session_id {
        return;
    }
    let Some(current) = guard.take() else {
        return;
    };
    drop(guard);
    release_backend_authority(current, simulation_backend).await;
}

async fn revoke_simulation_authority(
    authority: &Arc<Mutex<Option<SimulationAuthority>>>,
    simulation_backend: &Arc<dyn PublicSimulationBackend>,
) {
    let current = authority.lock().await.take();
    if let Some(current) = current {
        release_backend_authority(current, simulation_backend).await;
    }
}

async fn release_backend_authority(
    current: SimulationAuthority,
    simulation_backend: &Arc<dyn PublicSimulationBackend>,
) {
    let context = PublicSimulationContext {
        principal: current.principal.clone(),
        session_id: current.session_id.clone(),
        authority_grant: current.grant.clone(),
        correlation_id: Vec::new(),
        execution_id: current.execution_id.clone(),
        timeline_id: current.timeline_id.clone(),
        completed_boundary: current.boundary,
        model_identity: current.model_identity.clone(),
        quantum_ns: current.quantum_ns,
    };
    let _ = tokio::time::timeout(
        DEFAULT_PUBLIC_DEADLINE,
        simulation_backend.release(
            context,
            ReleaseAuthorityRequest {
                authority_grant: current.grant,
                session_id: current.session_id,
                correlation_id: Vec::new(),
            },
        ),
    )
    .await;
}

async fn cancel_session_subscriptions(
    route: &PublicRoute,
    session_id: &[u8],
    subscriptions: &Arc<Mutex<BTreeMap<String, CancellationToken>>>,
) {
    let prefix = format!("{}/{}/", route.principal(), hex_bytes(session_id));
    let mut active = subscriptions.lock().await;
    let keys = active
        .keys()
        .filter(|key| key.starts_with(&prefix))
        .cloned()
        .collect::<Vec<_>>();
    for key in keys {
        if let Some(token) = active.remove(&key) {
            token.cancel();
        }
    }
}

async fn advance_simulation(
    route: &PublicRoute,
    request: AdvanceRequest,
    adapter: &Arc<Mutex<SupervisorAdapter>>,
    authority: &Arc<Mutex<Option<SimulationAuthority>>>,
    simulation_backend: &Arc<dyn PublicSimulationBackend>,
    now_ms: u64,
) -> Result<AdvanceResponse, PublicTransportError> {
    validate_simulation_correlation(&request.correlation_id)?;
    let mut guard = authority.lock().await;
    let current = guard
        .as_mut()
        .ok_or_else(|| simulation_rejected("simulation authority is not active"))?;
    authorize_simulation_grant(route, &request.authority_grant, current)?;
    if Instant::now() >= current.lease_deadline {
        let expired = guard.take();
        drop(guard);
        if let Some(expired) = expired {
            release_backend_authority(expired, simulation_backend).await;
        }
        return Err(simulation_rejected("simulation authority lease expired"));
    }
    let request_digest: [u8; 32] = Sha256::digest(request.encode_to_vec()).into();
    if let Some(retained) = current
        .advance_results
        .iter()
        .find(|retained| retained.correlation_id == request.correlation_id)
    {
        if retained.request_digest == request_digest {
            return Ok(retained.response.clone());
        }
        return Err(simulation_rejected(
            "simulation correlation_id was reused with different inputs",
        ));
    }
    if request.execution_id != current.execution_id || request.timeline_id != current.timeline_id {
        return Err(simulation_rejected(
            "advance execution or timeline does not match authority",
        ));
    }
    if request.session_id != current.session_id {
        return Err(simulation_rejected(
            "advance session does not match authority",
        ));
    }
    if request.boundary != current.boundary {
        return Err(simulation_rejected(
            "advance boundary is not the current completed boundary",
        ));
    }
    let mut adapter_guard = adapter.lock().await;
    if !adapter_guard.simulation_session_active(route.principal(), &request.session_id, now_ms) {
        drop(adapter_guard);
        let expired = guard.take();
        drop(guard);
        if let Some(expired) = expired {
            release_backend_authority(expired, simulation_backend).await;
        }
        return Err(simulation_rejected(
            "simulation session is no longer active",
        ));
    }
    adapter_guard
        .authorize_simulation_session(route, &request.session_id, now_ms)
        .map_err(|error| simulation_adapter_error("advance", error))?;
    let summary = match adapter_guard.simulation_execution(&current.execution_id) {
        Ok(summary) => summary,
        Err(error) => {
            drop(adapter_guard);
            drop(guard);
            revoke_simulation_authority(authority, simulation_backend).await;
            return Err(simulation_adapter_error("advance", error));
        }
    };
    let definition = match adapter_guard.simulation_definition(&current.execution_id) {
        Ok(definition) => definition,
        Err(error) => {
            drop(adapter_guard);
            drop(guard);
            revoke_simulation_authority(authority, simulation_backend).await;
            return Err(simulation_adapter_error("advance", error));
        }
    };
    if summary.timeline_id != current.timeline_id {
        drop(adapter_guard);
        let invalidated = guard.take();
        drop(guard);
        if let Some(invalidated) = invalidated {
            release_backend_authority(invalidated, simulation_backend).await;
        }
        return Err(simulation_rejected(
            "advance timeline was invalidated by the supervisor",
        ));
    }
    let mut observations = BTreeSet::new();
    for observation in &request.observations {
        if observation.service_instance.is_empty()
            || observation.port.is_empty()
            || !observations.insert((
                observation.service_instance.as_str(),
                observation.port.as_str(),
            ))
        {
            return Err(simulation_rejected(
                "advance observations must identify unique provider ports",
            ));
        }
        let metadata = adapter_guard
            .validate_simulation_observation(
                &current.execution_id,
                &observation.service_instance,
                &observation.port,
            )
            .map_err(|error| simulation_adapter_error("advance", error))?;
        if observation.payload.len()
            > usize::try_from(metadata.max_message_bytes).unwrap_or(usize::MAX)
        {
            return Err(simulation_rejected(
                "simulation observation exceeds its generated port byte bound",
            ));
        }
    }
    let required = definition
        .providers()
        .iter()
        .map(|provider| (provider.service_instance(), provider.port()))
        .collect::<BTreeSet<_>>();
    if observations != required {
        return Err(simulation_rejected(
            "advance observations do not contain the complete required provider set",
        ));
    }
    let completed_boundary = current
        .boundary
        .checked_add(1)
        .ok_or_else(|| simulation_rejected("simulation boundary overflow"))?;
    let context = PublicSimulationContext {
        principal: current.principal.clone(),
        session_id: current.session_id.clone(),
        authority_grant: current.grant.clone(),
        correlation_id: request.correlation_id.clone(),
        execution_id: current.execution_id.clone(),
        timeline_id: current.timeline_id.clone(),
        completed_boundary: current.boundary,
        model_identity: current.model_identity.clone(),
        quantum_ns: current.quantum_ns,
    };
    drop(adapter_guard);
    let response = tokio::time::timeout(
        DEFAULT_PUBLIC_DEADLINE,
        simulation_backend.advance(context, request.clone()),
    )
    .await
    .map_err(|_| PublicTransportError::Timeout {
        operation: "advance".to_owned(),
    })?
    .map_err(|error| simulation_backend_error("advance", error))?;
    let mut response = response;
    validate_advance_response(&response, current, &request, completed_boundary)?;
    response.session_id = request.session_id.clone();
    response.execution_id = current.execution_id.clone();
    response.timeline_id = current.timeline_id.clone();
    response.requested_boundary = request.boundary;
    response.correlation_id = request.correlation_id.clone();
    response.authority_grant = current.grant.clone();
    for receipt in &mut response.observation_receipts {
        receipt.boundary = request.boundary;
        receipt.correlation_id = request.correlation_id.clone();
    }
    current.boundary = response.completed_boundary;
    current.lease_deadline = Instant::now()
        .checked_add(SIMULATION_AUTHORITY_LEASE)
        .ok_or_else(|| simulation_rejected("simulation authority lease overflows the clock"))?;
    current.advance_results.push_back(RetainedAdvance {
        correlation_id: request.correlation_id,
        request_digest,
        response: response.clone(),
    });
    while current.advance_results.len() > MAX_RETAINED_ADVANCE_RESULTS {
        current.advance_results.pop_front();
    }
    Ok(response)
}

async fn reset_simulation(
    route: &PublicRoute,
    request: ResetRequest,
    adapter: &Arc<Mutex<SupervisorAdapter>>,
    subscriptions: &Arc<Mutex<BTreeMap<String, CancellationToken>>>,
    authority: &Arc<Mutex<Option<SimulationAuthority>>>,
    simulation_backend: &Arc<dyn PublicSimulationBackend>,
    now_ms: u64,
) -> Result<ResetResponse, PublicTransportError> {
    validate_simulation_correlation(&request.correlation_id)?;
    let mut guard = authority.lock().await;
    let current = guard
        .as_mut()
        .ok_or_else(|| simulation_rejected("simulation authority is not active"))?;
    authorize_simulation_grant(route, &request.authority_grant, current)?;
    if Instant::now() >= current.lease_deadline {
        let expired = guard.take();
        drop(guard);
        if let Some(expired) = expired {
            release_backend_authority(expired, simulation_backend).await;
        }
        return Err(simulation_rejected("simulation authority lease expired"));
    }
    if request.execution_id != current.execution_id || request.timeline_id != current.timeline_id {
        return Err(simulation_rejected(
            "reset execution or timeline does not match authority",
        ));
    }
    if request.session_id != current.session_id {
        return Err(simulation_rejected(
            "reset session does not match authority",
        ));
    }
    if request.completed_boundary != current.boundary {
        return Err(simulation_rejected(
            "reset boundary does not match authoritative progress",
        ));
    }
    let mut adapter_guard = adapter.lock().await;
    if !adapter_guard.simulation_session_active(route.principal(), &request.session_id, now_ms) {
        drop(adapter_guard);
        drop(guard);
        revoke_simulation_authority(authority, simulation_backend).await;
        return Err(simulation_rejected(
            "simulation session is no longer active",
        ));
    }
    adapter_guard
        .authorize_simulation_session(route, &request.session_id, now_ms)
        .map_err(|error| simulation_adapter_error("reset", error))?;
    let summary = match adapter_guard.simulation_execution(&current.execution_id) {
        Ok(summary) => summary,
        Err(error) => {
            drop(adapter_guard);
            drop(guard);
            revoke_simulation_authority(authority, simulation_backend).await;
            return Err(simulation_adapter_error("reset", error));
        }
    };
    if summary.timeline_id != current.timeline_id {
        drop(adapter_guard);
        drop(guard);
        revoke_simulation_authority(authority, simulation_backend).await;
        return Err(simulation_rejected(
            "reset timeline was invalidated by the supervisor",
        ));
    }
    drop(adapter_guard);
    let next_timeline_id = crate::identity::TimelineId::mint().to_string();
    let context = PublicSimulationContext {
        principal: current.principal.clone(),
        session_id: current.session_id.clone(),
        authority_grant: current.grant.clone(),
        correlation_id: request.correlation_id.clone(),
        execution_id: current.execution_id.clone(),
        timeline_id: current.timeline_id.clone(),
        completed_boundary: current.boundary,
        model_identity: current.model_identity.clone(),
        quantum_ns: current.quantum_ns,
    };
    let reset = tokio::time::timeout(
        DEFAULT_PUBLIC_DEADLINE,
        simulation_backend.reset(context, request.clone(), next_timeline_id.clone()),
    )
    .await
    .map_err(|_| PublicTransportError::Timeout {
        operation: "reset".to_owned(),
    })?;
    reset.map_err(|error| simulation_backend_error("reset", error))?;
    let mut adapter_guard = adapter.lock().await;
    if let Err(error) = adapter_guard.reset_timeline(&current.execution_id, &next_timeline_id) {
        drop(adapter_guard);
        drop(guard);
        revoke_simulation_authority(authority, simulation_backend).await;
        return Err(simulation_adapter_error("reset", error));
    }
    drop(adapter_guard);
    current.timeline_id = next_timeline_id.clone();
    current.boundary = 0;
    current.advance_results.clear();
    current.lease_deadline = Instant::now()
        .checked_add(SIMULATION_AUTHORITY_LEASE)
        .ok_or_else(|| simulation_rejected("simulation authority lease overflows the clock"))?;
    cancel_session_subscriptions(route, &request.session_id, subscriptions).await;
    Ok(ResetResponse {
        next_timeline_id,
        boundary: 0,
        session_id: request.session_id.clone(),
        execution_id: current.execution_id.clone(),
        authority_grant: current.grant.clone(),
        correlation_id: request.correlation_id.clone(),
        previous_timeline_id: request.timeline_id,
        requested_boundary: request.completed_boundary,
    })
}

async fn release_simulation(
    route: &PublicRoute,
    request: ReleaseAuthorityRequest,
    adapter: &Arc<Mutex<SupervisorAdapter>>,
    authority: &Arc<Mutex<Option<SimulationAuthority>>>,
    simulation_backend: &Arc<dyn PublicSimulationBackend>,
    now_ms: u64,
) -> Result<ReleaseAuthorityResponse, PublicTransportError> {
    validate_simulation_correlation(&request.correlation_id)?;
    let mut guard = authority.lock().await;
    let current = guard
        .as_ref()
        .ok_or_else(|| simulation_rejected("simulation authority is not active"))?;
    authorize_simulation_grant(route, &request.authority_grant, current)?;
    if request.session_id != current.session_id {
        return Err(simulation_rejected(
            "release session does not match authority",
        ));
    }
    if Instant::now() >= current.lease_deadline {
        let expired = guard.take();
        drop(guard);
        if let Some(expired) = expired {
            release_backend_authority(expired, simulation_backend).await;
        }
        return Err(simulation_rejected("simulation authority lease expired"));
    }
    let session_active = adapter.lock().await.simulation_session_active(
        route.principal(),
        &request.session_id,
        now_ms,
    );
    if !session_active {
        drop(guard);
        revoke_simulation_authority(authority, simulation_backend).await;
        return Err(simulation_rejected("release session is no longer active"));
    }
    let context = PublicSimulationContext {
        principal: current.principal.clone(),
        session_id: current.session_id.clone(),
        authority_grant: current.grant.clone(),
        correlation_id: request.correlation_id.clone(),
        execution_id: current.execution_id.clone(),
        timeline_id: current.timeline_id.clone(),
        completed_boundary: current.boundary,
        model_identity: current.model_identity.clone(),
        quantum_ns: current.quantum_ns,
    };
    let release = tokio::time::timeout(
        DEFAULT_PUBLIC_DEADLINE,
        simulation_backend.release(context, request.clone()),
    )
    .await
    .map_err(|_| PublicTransportError::Timeout {
        operation: "release-authority".to_owned(),
    })?;
    release.map_err(|error| simulation_backend_error("release-authority", error))?;
    let session_id = request.session_id.clone();
    let authority_grant = request.authority_grant.clone();
    let correlation_id = request.correlation_id.clone();
    let execution_id = current.execution_id.clone();
    let timeline_id = current.timeline_id.clone();
    let completed_boundary = current.boundary;
    guard.take();
    Ok(ReleaseAuthorityResponse {
        session_id,
        authority_grant,
        correlation_id,
        execution_id,
        timeline_id,
        completed_boundary,
    })
}

async fn progress_simulation(
    route: &PublicRoute,
    request: ProgressRequest,
    adapter: &Arc<Mutex<SupervisorAdapter>>,
    authority: &Arc<Mutex<Option<SimulationAuthority>>>,
    simulation_backend: &Arc<dyn PublicSimulationBackend>,
    now_ms: u64,
) -> Result<ProgressResponse, PublicTransportError> {
    validate_simulation_correlation(&request.correlation_id)?;
    let mut guard = authority.lock().await;
    let current = guard
        .as_mut()
        .ok_or_else(|| simulation_rejected("simulation authority is not active"))?;
    authorize_simulation_grant(route, &request.authority_grant, current)?;
    if Instant::now() >= current.lease_deadline {
        let expired = guard.take();
        drop(guard);
        if let Some(expired) = expired {
            release_backend_authority(expired, simulation_backend).await;
        }
        return Err(simulation_rejected("simulation authority lease expired"));
    }
    if request.session_id != current.session_id {
        return Err(simulation_rejected(
            "progress session does not match authority",
        ));
    }
    let mut adapter_guard = adapter.lock().await;
    if !adapter_guard.simulation_session_active(route.principal(), &request.session_id, now_ms) {
        drop(adapter_guard);
        drop(guard);
        revoke_simulation_authority(authority, simulation_backend).await;
        return Err(simulation_rejected(
            "simulation session is no longer active",
        ));
    }
    adapter_guard
        .authorize_simulation_session(route, &request.session_id, now_ms)
        .map_err(|error| simulation_adapter_error("progress", error))?;
    let summary = match adapter_guard.simulation_execution(&current.execution_id) {
        Ok(summary) => summary,
        Err(error) => {
            drop(adapter_guard);
            drop(guard);
            revoke_simulation_authority(authority, simulation_backend).await;
            return Err(simulation_adapter_error("progress", error));
        }
    };
    if summary.timeline_id != current.timeline_id {
        drop(adapter_guard);
        drop(guard);
        revoke_simulation_authority(authority, simulation_backend).await;
        return Err(simulation_rejected(
            "simulation timeline was invalidated by the supervisor",
        ));
    }
    let context = PublicSimulationContext {
        principal: current.principal.clone(),
        session_id: current.session_id.clone(),
        authority_grant: current.grant.clone(),
        correlation_id: request.correlation_id.clone(),
        execution_id: current.execution_id.clone(),
        timeline_id: current.timeline_id.clone(),
        completed_boundary: current.boundary,
        model_identity: current.model_identity.clone(),
        quantum_ns: current.quantum_ns,
    };
    drop(adapter_guard);
    let response = tokio::time::timeout(
        DEFAULT_PUBLIC_DEADLINE,
        simulation_backend.progress(context, request.clone()),
    )
    .await
    .map_err(|_| PublicTransportError::Timeout {
        operation: "progress".to_owned(),
    })?
    .map_err(|error| simulation_backend_error("progress", error))?;
    let mut response = response;
    if (!response.execution_id.is_empty() && response.execution_id != current.execution_id)
        || (!response.timeline_id.is_empty() && response.timeline_id != current.timeline_id)
        || (!response.session_id.is_empty() && response.session_id != current.session_id)
        || response.completed_boundary < current.boundary
    {
        drop(guard);
        revoke_simulation_authority(authority, simulation_backend).await;
        return Err(simulation_rejected(
            "simulation backend returned inconsistent progress identity",
        ));
    }
    response.execution_id = current.execution_id.clone();
    response.timeline_id = current.timeline_id.clone();
    response.session_id = current.session_id.clone();
    response.authority_grant = current.grant.clone();
    response.correlation_id = request.correlation_id;
    current.boundary = response.completed_boundary;
    if response.failed {
        drop(guard);
        revoke_simulation_authority(authority, simulation_backend).await;
    } else {
        current.lease_deadline = Instant::now()
            .checked_add(SIMULATION_AUTHORITY_LEASE)
            .ok_or_else(|| simulation_rejected("simulation authority lease overflows the clock"))?;
    }
    Ok(response)
}

fn validate_advance_response(
    response: &AdvanceResponse,
    authority: &SimulationAuthority,
    request: &AdvanceRequest,
    expected_boundary: u64,
) -> Result<(), PublicTransportError> {
    if response.completed_boundary != expected_boundary {
        return Err(simulation_rejected(
            "simulation backend returned a non-sequential completed boundary",
        ));
    }
    if (!response.session_id.is_empty() && response.session_id != authority.session_id)
        || (!response.execution_id.is_empty() && response.execution_id != authority.execution_id)
        || (!response.timeline_id.is_empty() && response.timeline_id != authority.timeline_id)
        || (response.requested_boundary != 0 && response.requested_boundary != request.boundary)
        || (!response.correlation_id.is_empty()
            && response.correlation_id != request.correlation_id)
        || (!response.authority_grant.is_empty() && response.authority_grant != authority.grant)
    {
        return Err(simulation_rejected(
            "simulation backend returned inconsistent advance identity",
        ));
    }
    let mut expected = BTreeSet::new();
    for observation in &request.observations {
        expected.insert((
            observation.service_instance.as_str(),
            observation.port.as_str(),
        ));
    }
    let mut received = BTreeSet::new();
    for receipt in &response.observation_receipts {
        if receipt.service_instance.is_empty()
            || receipt.port.is_empty()
            || (!receipt.correlation_id.is_empty()
                && receipt.correlation_id != request.correlation_id)
            || (receipt.boundary != 0 && receipt.boundary != request.boundary)
            || !received.insert((receipt.service_instance.as_str(), receipt.port.as_str()))
        {
            return Err(simulation_rejected(
                "simulation backend returned an invalid observation receipt",
            ));
        }
    }
    if received != expected {
        return Err(simulation_rejected(
            "simulation backend did not return complete observation receipts",
        ));
    }
    Ok(())
}

fn authorize_simulation_grant(
    route: &PublicRoute,
    grant: &[u8],
    authority: &SimulationAuthority,
) -> Result<(), PublicTransportError> {
    if authority.principal != route.principal()
        || grant.len() != SIMULATION_GRANT_BYTES
        || !constant_time_eq(grant, &authority.grant)
    {
        return Err(simulation_rejected(
            "simulation authority grant or principal is invalid",
        ));
    }
    Ok(())
}

fn validate_simulation_correlation(correlation_id: &[u8]) -> Result<(), PublicTransportError> {
    if correlation_id.is_empty() || correlation_id.len() > MAX_SIMULATION_CORRELATION_BYTES {
        return Err(simulation_rejected(
            "simulation correlation_id must contain 1..=64 bytes",
        ));
    }
    Ok(())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn simulation_rejected(detail: &str) -> PublicTransportError {
    PublicTransportError::Rejected {
        operation: "simulation".to_owned(),
        detail: detail.to_owned(),
    }
}

fn simulation_adapter_error(
    operation: &str,
    error: SupervisorAdapterError,
) -> PublicTransportError {
    PublicTransportError::Adapter {
        operation: operation.to_owned(),
        detail: error.to_string(),
    }
}

fn simulation_backend_error(operation: &str, error: PublicBackendError) -> PublicTransportError {
    match error {
        PublicBackendError::RejectedBeforeAdmission(detail) => PublicTransportError::Rejected {
            operation: operation.to_owned(),
            detail: bounded_error_detail(&detail),
        },
        PublicBackendError::Capacity => PublicTransportError::Rejected {
            operation: operation.to_owned(),
            detail: "simulation backend observer capacity is exhausted".to_owned(),
        },
        PublicBackendError::Transport(detail) => PublicTransportError::Transport(detail),
    }
}

async fn reply_simulation_result<M: Message>(
    query: &Query,
    result: Result<M, PublicTransportError>,
    operation: &str,
    limits: &PublicTransportLimits,
) {
    match result {
        Ok(response) => reply_message(query, &response, operation, limits).await,
        Err(error) => send_error(query, &error, limits).await,
    }
}

async fn reply_adapter<M: Message>(
    query: &Query,
    result: Result<M, SupervisorAdapterError>,
    operation: &str,
    limits: &PublicTransportLimits,
) {
    match result {
        Ok(response) => match encode_message(&response, limits.max_response_bytes(), operation) {
            Ok(payload) => {
                if let Err(error) = query
                    .reply(query.key_expr(), payload)
                    .encoding(Encoding::from(PUBLIC_PROTOBUF_ENCODING.to_owned()))
                    .await
                {
                    tracing::debug!(operation, error = %error, "public session reply failed");
                }
            }
            Err(error) => send_error(query, &error, limits).await,
        },
        Err(error) => {
            send_error(
                query,
                &PublicTransportError::Adapter {
                    operation: operation.to_owned(),
                    detail: error.to_string(),
                },
                limits,
            )
            .await;
        }
    }
}

async fn reply_message<M: Message>(
    query: &Query,
    message: &M,
    operation: &str,
    limits: &PublicTransportLimits,
) {
    match encode_message(message, limits.max_response_bytes(), operation) {
        Ok(payload) => {
            if let Err(error) = query
                .reply(query.key_expr(), payload)
                .encoding(Encoding::from(PUBLIC_PROTOBUF_ENCODING.to_owned()))
                .await
            {
                tracing::debug!(operation, error = %error, "public session reply failed");
            }
        }
        Err(error) => send_error(query, &error, limits).await,
    }
}

fn bounded_timeout(timeout_ms: u32, default: Duration) -> Duration {
    if timeout_ms == 0 {
        default
    } else {
        Duration::from_millis(u64::from(timeout_ms)).min(default)
    }
}

fn subscription_map_key(principal: &str, session_id: &[u8], subscription_id: &[u8]) -> String {
    format!(
        "{principal}/{}/{}",
        hex_bytes(session_id),
        hex_bytes(subscription_id)
    )
}

fn parse_presence_target(
    scope: &str,
    prefix: &str,
    suffix: &str,
    key: &str,
) -> Result<String, PublicTransportError> {
    let supervisor = key
        .strip_prefix(prefix)
        .and_then(|value| value.strip_suffix(suffix))
        .filter(|value| !value.contains('/'))
        .ok_or_else(|| PublicTransportError::MalformedPresence {
            key: key.to_owned(),
        })?;
    let target = DeploymentTarget::new(scope, supervisor).map_err(|_| {
        PublicTransportError::MalformedPresence {
            key: key.to_owned(),
        }
    })?;
    Ok(target.supervisor().to_owned())
}

fn discovery_change(
    current: &mut BTreeSet<String>,
    scope: &str,
    supervisor: String,
    kind: zenoh::sample::SampleKind,
) -> Option<DiscoveryEvent> {
    let target = match DeploymentTarget::new(scope, &supervisor) {
        Ok(target) => target,
        Err(_) => return None,
    };
    match kind {
        zenoh::sample::SampleKind::Put if current.insert(supervisor.clone()) => {
            Some(DiscoveryEvent::Appeared(target))
        }
        zenoh::sample::SampleKind::Delete if current.remove(&supervisor) => {
            Some(DiscoveryEvent::Disappeared(target))
        }
        _ => None,
    }
}

fn subscription_key(target: &DeploymentTarget, principal: &str, subscription_id: &[u8]) -> String {
    format!(
        "{}/clients/{principal}/observations/{}",
        target.session_prefix(),
        hex_bytes(subscription_id)
    )
}

fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        text.push(HEX[usize::from(byte >> 4)] as char);
        text.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    text
}

fn validate_subscription_record(
    record: &SubscriptionRecord,
    request: &SubscriptionRequest,
    limits: &PublicTransportLimits,
    allow_initial_absent: bool,
) -> Result<(), PublicTransportError> {
    if record.session_id != request.session_id
        || record.binding_id != request.binding_id
        || record.subscription_id != request.subscription_id
        || record.execution_id != request.execution_id
        || record.timeline_id != request.timeline_id
    {
        return Err(PublicTransportError::Malformed {
            operation: "subscription".to_owned(),
            detail: "subscription record context does not match its request".to_owned(),
        });
    }
    if record.payload.len() > limits.max_response_bytes() {
        return Err(PublicTransportError::BodyTooLarge {
            operation: "subscription".to_owned(),
            bytes: record.payload.len(),
            maximum: limits.max_response_bytes(),
        });
    }
    let kind = RecordKind::try_from(record.kind).map_err(|_| PublicTransportError::Malformed {
        operation: "subscription".to_owned(),
        detail: "subscription record kind is unspecified or unknown".to_owned(),
    })?;
    if kind == RecordKind::Unspecified {
        return Err(PublicTransportError::Malformed {
            operation: "subscription".to_owned(),
            detail: "subscription record kind is unspecified or unknown".to_owned(),
        });
    }
    match kind {
        RecordKind::InitialAbsent if !allow_initial_absent => {
            return Err(PublicTransportError::Malformed {
                operation: "subscription".to_owned(),
                detail: "initial-absence is valid only for a State watch".to_owned(),
            });
        }
        RecordKind::InitialAbsent | RecordKind::Gap | RecordKind::End | RecordKind::Failed
            if !record.payload.is_empty() =>
        {
            return Err(PublicTransportError::Malformed {
                operation: "subscription".to_owned(),
                detail: "control subscription records must not carry a payload".to_owned(),
            });
        }
        _ => {}
    }
    Ok(())
}

fn validate_subscription_request(
    request: &SubscriptionRequest,
    limits: &PublicTransportLimits,
) -> Result<usize, PublicTransportError> {
    if request.session_id.is_empty()
        || request.binding_id.is_empty()
        || request.subscription_id.is_empty()
        || request.subscription_id.len() > MAX_PUBLIC_SUBSCRIPTION_ID_BYTES
        || request.execution_id.is_empty()
        || request.timeline_id.is_empty()
        || request.max_buffered_items == 0
        || request.max_buffered_bytes == 0
    {
        return Err(PublicTransportError::Malformed {
            operation: "subscription".to_owned(),
            detail: format!(
                "subscription identity and finite item/byte bounds are required; subscription_id must contain 1..={MAX_PUBLIC_SUBSCRIPTION_ID_BYTES} bytes"
            ),
        });
    }
    Ok(usize::try_from(request.max_buffered_items)
        .unwrap_or(usize::MAX)
        .min(limits.query_capacity())
        .max(1))
}

fn validate_subscription_admission(
    admission: &SubscriptionAdmission,
    request: &SubscriptionRequest,
    limits: &PublicTransportLimits,
    operation: PublicOperation,
) -> Result<Option<SubscriptionRecord>, PublicTransportError> {
    if admission.session_id != request.session_id
        || admission.binding_id != request.binding_id
        || admission.subscription_id != request.subscription_id
        || admission.execution_id != request.execution_id
        || admission.timeline_id != request.timeline_id
    {
        return Err(PublicTransportError::Malformed {
            operation: operation.segment().to_owned(),
            detail: "subscription admission context does not match its request".to_owned(),
        });
    }
    let Some(initial) = admission.initial.clone() else {
        if operation == PublicOperation::Watch {
            return Err(PublicTransportError::Malformed {
                operation: operation.segment().to_owned(),
                detail: "State watch admission omitted its initial cursor".to_owned(),
            });
        }
        return Ok(None);
    };
    if operation != PublicOperation::Watch {
        return Err(PublicTransportError::Malformed {
            operation: operation.segment().to_owned(),
            detail: "non-State subscription admission carried an initial record".to_owned(),
        });
    }
    validate_state_initial_record(&initial, request, limits)?;
    Ok(Some(initial))
}

fn validate_state_initial_record(
    record: &SubscriptionRecord,
    request: &SubscriptionRequest,
    limits: &PublicTransportLimits,
) -> Result<(), PublicTransportError> {
    validate_subscription_record(record, request, limits, true)?;
    let kind = RecordKind::try_from(record.kind).map_err(|_| PublicTransportError::Malformed {
        operation: "subscription".to_owned(),
        detail: "subscription admission initial kind is unknown".to_owned(),
    })?;
    if !matches!(kind, RecordKind::InitialAbsent | RecordKind::Value) {
        return Err(PublicTransportError::Malformed {
            operation: "subscription".to_owned(),
            detail: "State watch admission initial kind is not a value or absence marker"
                .to_owned(),
        });
    }
    Ok(())
}

fn validate_operation_response(
    response: &OperationResponse,
    request: &OperationRequest,
    operation: PublicOperation,
) -> Result<(), PublicTransportError> {
    if response.session_id != request.session_id
        || response.binding_id != request.binding_id
        || response.correlation_id != request.correlation_id
        || response.execution_id != request.execution_id
        || response.timeline_id != request.timeline_id
    {
        return Err(PublicTransportError::Malformed {
            operation: operation.segment().to_owned(),
            detail: "operation response context does not match its request".to_owned(),
        });
    }
    let outcome = OperationOutcome::try_from(response.outcome).map_err(|_| {
        PublicTransportError::Malformed {
            operation: operation.segment().to_owned(),
            detail: "operation response outcome is unspecified or unknown".to_owned(),
        }
    })?;
    if outcome == OperationOutcome::Received && response.detail.is_some() {
        return Err(PublicTransportError::Malformed {
            operation: operation.segment().to_owned(),
            detail: "received operation response contains a refusal detail".to_owned(),
        });
    }
    Ok(())
}

async fn send_error(query: &Query, error: &PublicTransportError, limits: &PublicTransportLimits) {
    let mut payload = error.to_string().into_bytes();
    if payload.len() > MAX_PUBLIC_ERROR_BYTES.min(limits.max_response_bytes()) {
        payload.truncate(MAX_PUBLIC_ERROR_BYTES.min(limits.max_response_bytes()));
    }
    if let Err(send_error) = query.reply_err(payload).await {
        tracing::debug!(error = %send_error, "public session error reply failed");
    }
}

fn operation_key_expression(target: &DeploymentTarget, operation: PublicOperation) -> String {
    if operation.kind() == crate::communication::PublicRouteKind::Simulation {
        format!(
            "{}/clients/*/{}",
            target.simulation_prefix(),
            operation.segment()
        )
    } else {
        let lane = match operation.kind() {
            crate::communication::PublicRouteKind::Control => "control",
            crate::communication::PublicRouteKind::Inspection => "inspection",
            crate::communication::PublicRouteKind::Mutation => "mutation",
            crate::communication::PublicRouteKind::Simulation => {
                unreachable!("simulation routes are handled by the dedicated path above")
            }
        };
        format!(
            "{}/clients/*/{lane}/{}",
            target.session_prefix(),
            operation.segment()
        )
    }
}

fn encode_message<M: Message>(
    message: &M,
    maximum: usize,
    operation: &str,
) -> Result<Vec<u8>, PublicTransportError> {
    let encoded_len = message.encoded_len();
    if encoded_len > maximum {
        return Err(PublicTransportError::BodyTooLarge {
            operation: operation.to_owned(),
            bytes: encoded_len,
            maximum,
        });
    }
    let mut payload = Vec::with_capacity(encoded_len);
    message
        .encode(&mut payload)
        .map_err(|error| PublicTransportError::Malformed {
            operation: operation.to_owned(),
            detail: format!("failed to encode Protobuf response: {error}"),
        })?;
    Ok(payload)
}

fn decode_request<M: Message + Default>(
    payload: &[u8],
    operation: &str,
    limits: &PublicTransportLimits,
) -> Result<M, PublicTransportError> {
    decode_message(payload, operation, limits)
}

fn decode_message<M: Message + Default>(
    payload: &[u8],
    operation: &str,
    limits: &PublicTransportLimits,
) -> Result<M, PublicTransportError> {
    if payload.len() > limits.max_response_bytes().max(limits.max_request_bytes()) {
        return Err(PublicTransportError::BodyTooLarge {
            operation: operation.to_owned(),
            bytes: payload.len(),
            maximum: limits.max_request_bytes(),
        });
    }
    M::decode(payload).map_err(|error| PublicTransportError::Decode {
        operation: operation.to_owned(),
        detail: error.to_string(),
    })
}

async fn query_proto<Request, Response>(
    session: &zenoh::Session,
    route: &PublicRoute,
    request: &Request,
    operation: &str,
    limits: &PublicTransportLimits,
) -> Result<Response, PublicTransportError>
where
    Request: Message,
    Response: Message + Default,
{
    let payload = encode_message(request, limits.max_request_bytes(), operation)?;
    let bytes = query_one(session, route.key(), Some(payload), operation, limits).await?;
    decode_message(&bytes, operation, limits)
}

async fn query_one(
    session: &zenoh::Session,
    key: String,
    payload: Option<Vec<u8>>,
    operation: &str,
    limits: &PublicTransportLimits,
) -> Result<Vec<u8>, PublicTransportError> {
    let deadline = tokio::time::Instant::now() + limits.deadline();
    loop {
        let result = query_one_attempt(
            session,
            &key,
            payload.as_deref(),
            operation,
            limits,
            deadline,
        )
        .await;
        match result {
            Err(PublicTransportError::NoReply { .. }) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep_until(
                    (tokio::time::Instant::now() + Duration::from_millis(20)).min(deadline),
                )
                .await;
            }
            result => return result,
        }
    }
}

async fn query_one_attempt(
    session: &zenoh::Session,
    key: &str,
    payload: Option<&[u8]>,
    operation: &str,
    limits: &PublicTransportLimits,
    deadline: tokio::time::Instant,
) -> Result<Vec<u8>, PublicTransportError> {
    let expected_key = key.to_owned();
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
        return Err(PublicTransportError::NoReply {
            operation: operation.to_owned(),
        });
    }
    let builder = session
        .get(key)
        .target(QueryTarget::All)
        .consolidation(ConsolidationMode::None)
        .timeout(remaining)
        .with(FifoChannel::new(limits.query_capacity()));
    let builder = match payload {
        Some(payload) => builder
            .payload(payload.to_vec())
            .encoding(Encoding::from(PUBLIC_PROTOBUF_ENCODING.to_owned())),
        None => builder,
    };
    let replies = tokio::time::timeout_at(deadline, builder)
        .await
        .map_err(|_| PublicTransportError::Timeout {
            operation: operation.to_owned(),
        })?
        .map_err(|error| PublicTransportError::Transport(error.to_string()))?;
    let mut reply_count = 0_usize;
    let mut response_bytes = 0_usize;
    let mut result = None;
    loop {
        match tokio::time::timeout_at(deadline, replies.recv_async()).await {
            Ok(Ok(reply)) => {
                reply_count = reply_count.saturating_add(1);
                if reply_count > limits.max_reply_count() {
                    return Err(PublicTransportError::TooManyReplies {
                        operation: operation.to_owned(),
                    });
                }
                if result.is_some() {
                    return Err(PublicTransportError::TooManyReplies {
                        operation: operation.to_owned(),
                    });
                }
                match reply.into_result() {
                    Ok(sample) => {
                        let actual_key = sample.key_expr().to_string();
                        if actual_key != expected_key {
                            return Err(PublicTransportError::WrongReplyKey {
                                expected: expected_key.clone(),
                                actual: actual_key,
                                operation: operation.to_owned(),
                            });
                        }
                        if sample.encoding() != &Encoding::from(PUBLIC_PROTOBUF_ENCODING.to_owned())
                        {
                            return Err(PublicTransportError::WrongEncoding {
                                operation: operation.to_owned(),
                            });
                        }
                        let bytes = sample.payload().len();
                        response_bytes = response_bytes.saturating_add(bytes);
                        if bytes > limits.max_response_bytes()
                            || response_bytes > limits.max_response_bytes()
                        {
                            return Err(PublicTransportError::BodyTooLarge {
                                operation: operation.to_owned(),
                                bytes: response_bytes,
                                maximum: limits.max_response_bytes(),
                            });
                        }
                        result = Some(Ok(sample.payload().to_bytes().to_vec()));
                    }
                    Err(error) => {
                        if error.payload().len() > MAX_PUBLIC_ERROR_BYTES {
                            return Err(PublicTransportError::BodyTooLarge {
                                operation: operation.to_owned(),
                                bytes: error.payload().len(),
                                maximum: MAX_PUBLIC_ERROR_BYTES,
                            });
                        }
                        let detail =
                            String::from_utf8_lossy(&error.payload().to_bytes()).into_owned();
                        result = Some(Err(PublicTransportError::Rejected {
                            operation: operation.to_owned(),
                            detail,
                        }));
                    }
                }
            }
            Ok(Err(_)) => break,
            Err(_) => {
                return if result.is_some() {
                    Err(PublicTransportError::IncompleteQuery {
                        operation: operation.to_owned(),
                    })
                } else {
                    Err(PublicTransportError::Timeout {
                        operation: operation.to_owned(),
                    })
                };
            }
        }
    }
    match result {
        Some(result) => result,
        None => Err(PublicTransportError::NoReply {
            operation: operation.to_owned(),
        }),
    }
}

fn malformed_client(operation: &str, error: SupervisorAdapterError) -> PublicTransportError {
    PublicTransportError::Malformed {
        operation: operation.to_owned(),
        detail: error.to_string(),
    }
}

fn bounded_error_detail(detail: &str) -> String {
    if detail.len() <= MAX_PUBLIC_ERROR_BYTES {
        return detail.to_owned();
    }
    let mut end = MAX_PUBLIC_ERROR_BYTES;
    while !detail.is_char_boundary(end) {
        end -= 1;
    }
    detail[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_limits_are_finite_and_bounded() {
        let limits = PublicTransportLimits::default();
        limits.validate().expect("default limits validate");
        assert_eq!(limits.deadline(), Duration::from_secs(5));
        assert_eq!(limits.max_reply_count(), 2);
    }

    #[test]
    fn operation_keys_are_explicit_and_lane_separated() {
        let target = DeploymentTarget::new("workshop", "rover-01").expect("target");
        assert_eq!(
            operation_key_expression(&target, PublicOperation::Open),
            "phoxal/workshop/supervisors/rover-01/session/v1/clients/*/control/open"
        );
        assert_eq!(
            operation_key_expression(&target, PublicOperation::Status),
            "phoxal/workshop/supervisors/rover-01/session/v1/clients/*/inspection/status"
        );
        assert_eq!(
            operation_key_expression(&target, PublicOperation::Command),
            "phoxal/workshop/supervisors/rover-01/session/v1/clients/*/mutation/command"
        );
        assert_eq!(
            operation_key_expression(&target, PublicOperation::Advance),
            "phoxal/workshop/supervisors/rover-01/simulation/v1/clients/*/advance"
        );
    }

    #[test]
    fn principal_policy_only_does_not_turn_a_route_into_authentication() {
        let policy = PrincipalPolicy::only(["operator-a"]);
        assert!(policy.allows("operator-a"));
        assert!(!policy.allows("operator-b"));
    }

    #[test]
    fn subscription_admission_keeps_non_state_initial_absence_out_of_data() {
        let request = SubscriptionRequest {
            session_id: b"session".to_vec(),
            binding_id: b"binding".to_vec(),
            subscription_id: b"subscription".to_vec(),
            execution_id: "execution".to_owned(),
            timeline_id: "timeline".to_owned(),
            max_buffered_items: 1,
            max_buffered_bytes: 1,
        };
        let admission = SubscriptionAdmission {
            session_id: request.session_id.clone(),
            binding_id: request.binding_id.clone(),
            subscription_id: request.subscription_id.clone(),
            execution_id: request.execution_id.clone(),
            timeline_id: request.timeline_id.clone(),
            initial: None,
        };
        assert_eq!(
            validate_subscription_admission(
                &admission,
                &request,
                &PublicTransportLimits::default(),
                PublicOperation::Subscribe,
            )
            .expect("Subscribe admission"),
            None
        );

        let invalid = SubscriptionAdmission {
            initial: Some(SubscriptionRecord {
                session_id: request.session_id.clone(),
                binding_id: request.binding_id.clone(),
                subscription_id: request.subscription_id.clone(),
                execution_id: request.execution_id.clone(),
                timeline_id: request.timeline_id.clone(),
                revision: 0,
                kind: RecordKind::InitialAbsent as i32,
                payload: Vec::new(),
                dropped: 0,
                detail: None,
            }),
            ..admission
        };
        assert!(matches!(
            validate_subscription_admission(
                &invalid,
                &request,
                &PublicTransportLimits::default(),
                PublicOperation::Subscribe,
            ),
            Err(PublicTransportError::Malformed { .. })
        ));
    }

    #[test]
    fn state_watch_admission_accepts_only_a_value_or_initial_absence() {
        let request = SubscriptionRequest {
            session_id: b"session".to_vec(),
            binding_id: b"binding".to_vec(),
            subscription_id: b"subscription".to_vec(),
            execution_id: "execution".to_owned(),
            timeline_id: "timeline".to_owned(),
            max_buffered_items: 1,
            max_buffered_bytes: 1,
        };
        let admission = SubscriptionAdmission {
            session_id: request.session_id.clone(),
            binding_id: request.binding_id.clone(),
            subscription_id: request.subscription_id.clone(),
            execution_id: request.execution_id.clone(),
            timeline_id: request.timeline_id.clone(),
            initial: Some(SubscriptionRecord {
                session_id: request.session_id.clone(),
                binding_id: request.binding_id.clone(),
                subscription_id: request.subscription_id.clone(),
                execution_id: request.execution_id.clone(),
                timeline_id: request.timeline_id.clone(),
                revision: 0,
                kind: RecordKind::InitialAbsent as i32,
                payload: Vec::new(),
                dropped: 0,
                detail: None,
            }),
        };
        assert!(matches!(
            validate_subscription_admission(
                &admission,
                &request,
                &PublicTransportLimits::default(),
                PublicOperation::Watch,
            ),
            Ok(Some(SubscriptionRecord {
                kind,
                ..
            })) if kind == RecordKind::InitialAbsent as i32
        ));
    }

    #[test]
    fn tls_server_name_must_match_the_endpoint_host() {
        let credentials = PublicTlsCredentials::from_files(
            "root-ca.pem",
            "client.pem",
            "client-key.pem",
            "router.example",
        )
        .expect("TLS credentials");
        assert!(
            client_config_with_security(
                "tls/other.example:7447",
                &PublicTransportSecurity::Tls(credentials.clone()),
            )
            .is_err()
        );
        assert!(
            client_config_with_security(
                "tls/router.example:7447",
                &PublicTransportSecurity::Tls(credentials),
            )
            .is_ok()
        );
        assert!(
            client_config_with_security(
                "tcp/router.example:7447",
                &PublicTransportSecurity::Plaintext,
            )
            .is_err()
        );
        assert!(
            client_config_with_security(
                "unixsock-stream//tmp/phoxal-supervisor.sock",
                &PublicTransportSecurity::Plaintext,
            )
            .is_ok()
        );
    }

    /// Exercise two independently routed supervisor namespaces and two
    /// principal-bound logical sessions over one local Zenoh router.
    #[cfg(feature = "supervisor")]
    #[serial_test::serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn routed_supervisors_keep_bootstrap_and_sessions_isolated() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("test port");
        let endpoint = format!("tcp/{}", listener.local_addr().expect("test address"));
        drop(listener);

        let router = crate::router::Router::open(
            crate::identity::ExecutionId::mint(),
            std::slice::from_ref(&endpoint),
        )
        .await
        .expect("router");
        let target_a = DeploymentTarget::new("workshop", "rover-a").expect("target a");
        let target_b = DeploymentTarget::new("workshop", "rover-b").expect("target b");
        let (owner_a, bus_a) =
            crate::bus::session::BusOwner::open(crate::bus::session::BusConfig::for_external(
                crate::identity::ExecutionId::mint(),
                None,
                vec![endpoint.clone()],
            ))
            .await
            .expect("supervisor a bus");
        let (owner_b, bus_b) =
            crate::bus::session::BusOwner::open(crate::bus::session::BusConfig::for_external(
                crate::identity::ExecutionId::mint(),
                None,
                vec![endpoint.clone()],
            ))
            .await
            .expect("supervisor b bus");
        let adapter_a =
            SupervisorAdapter::with_defaults(target_a.clone(), "supervisor-a", "framework-a")
                .expect("adapter a");
        let adapter_b =
            SupervisorAdapter::with_defaults(target_b.clone(), "supervisor-b", "framework-b")
                .expect("adapter b");
        let server_a = PublicSessionServer::start(
            bus_a.session().expect("a session").clone(),
            adapter_a,
            PrincipalPolicy::only(["operator-a"]),
            PublicTransportLimits::default(),
        )
        .await
        .expect("server a");
        let server_b = PublicSessionServer::start(
            bus_b.session().expect("b session").clone(),
            adapter_b,
            PrincipalPolicy::only(["operator-a"]),
            PublicTransportLimits::default(),
        )
        .await
        .expect("server b");

        let transport_a = PublicSessionTransport::connect(&endpoint, "operator-a")
            .await
            .expect("transport a");
        let discovered = transport_a
            .discover("workshop")
            .await
            .expect("bounded supervisor inventory");
        assert_eq!(
            discovered
                .iter()
                .map(DeploymentTarget::supervisor)
                .collect::<Vec<_>>(),
            ["rover-a", "rover-b"]
        );
        assert!(matches!(
            transport_a.discover_bounded("workshop", 1).await,
            Err(PublicTransportError::InventoryOverflow { maximum: 1 })
        ));
        let connection_a = transport_a.open(target_a).await.expect("session a");
        let connection_b = transport_a.open(target_b).await.expect("session b");
        assert_eq!(connection_a.info().supervisor_version, "supervisor-a");
        assert_eq!(connection_b.info().supervisor_version, "supervisor-b");
        assert_eq!(connection_a.status().await.expect("status a").state, 1);
        assert_eq!(connection_b.status().await.expect("status b").state, 1);

        let spoofed = PublicSessionConnection::connect(
            PublicSessionConfig::new(
                &endpoint,
                DeploymentTarget::new("workshop", "rover-a").expect("target"),
                "operator-b",
            )
            .expect("spoofed client"),
        )
        .await
        .expect_err("router policy must refuse a principal spoof");
        assert!(matches!(spoofed, PublicTransportError::Rejected { .. }));

        let connection_b = connection_b;
        connection_a.close().await.expect("close a");
        assert_eq!(
            connection_b
                .status()
                .await
                .expect("session b survives")
                .state,
            1
        );
        connection_b.close().await.expect("close b");
        transport_a.close().await.expect("transport a close");
        server_a.close().await.expect("server a close");
        server_b.close().await.expect("server b close");
        let _ = owner_a.close().await;
        let _ = owner_b.close().await;
        router.close().await.expect("router close");
    }
}
