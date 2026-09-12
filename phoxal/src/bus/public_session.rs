//! The first public `phoxal.session.v1` Zenoh transport.
//!
//! This module is deliberately owned by the session and supervisor boundary.
//! The Protobuf messages and route/admission rules remain in [`super`], while
//! this file owns only the wire exchange, bounded query collection, and the
//! lifetime of the public queryables.
//!
//! The public transport is not the internal execution bus. In particular, it
//! never exposes a raw Zenoh handle, internal execution key, participant
//! identity, or unrestricted publication capability.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use prost::Message;
use thiserror::Error;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use zenoh::bytes::Encoding;
use zenoh::handlers::FifoChannel;
use zenoh::query::{ConsolidationMode, Query, QueryTarget, Queryable};

use crate::communication::bootstrap::SessionOffers;
use crate::communication::session::{
    BindPortRequest, BindPortResponse, CloseSessionRequest, CloseSessionResponse, ExecutionState,
    ListExecutionsRequest, ListExecutionsResponse, ListPortsRequest, ListPortsResponse,
    OpenSessionRequest, OpenSessionResponse, RenewSessionRequest, RenewSessionResponse,
    SupervisorInfoRequest, SupervisorInfoResponse, SupervisorState, SupervisorStatusRequest,
    SupervisorStatusResponse,
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

const PUBLIC_OPERATION_QUERYABLES: [PublicOperation; 12] = [
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
];

type PublicQueryable = Queryable<zenoh::handlers::FifoChannelHandler<Query>>;

#[derive(Clone)]
struct OperationServerContext {
    target: DeploymentTarget,
    adapter: Arc<Mutex<SupervisorAdapter>>,
    principal_policy: PrincipalPolicy,
    limits: PublicTransportLimits,
    started: Instant,
    shutdown: CancellationToken,
}

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
}

/// A running supervisor-side public query surface.
///
/// The server owns queryables and a presence token, but not the shared Zenoh
/// session. Closing this value undeclares the public surface while leaving the
/// supervisor's internal bus owner in charge of closing the session itself.
pub struct PublicSessionServer {
    shutdown: CancellationToken,
    tasks: Vec<JoinHandle<()>>,
    adapter: Arc<Mutex<SupervisorAdapter>>,
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
                    adapter: adapter.clone(),
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
            adapter,
            _presence: presence,
            _session: session,
        })
    }

    /// Update the supervisor status while retaining the public surface.
    pub(crate) async fn set_status(
        &self,
        state: SupervisorState,
        detail: Option<String>,
    ) -> Result<(), PublicTransportError> {
        self.adapter
            .lock()
            .await
            .set_status(state, detail)
            .map_err(|error| PublicTransportError::Adapter {
                operation: "status".to_owned(),
                detail: error.to_string(),
            })
    }

    /// Update the execution lifecycle after a process graph transition.
    pub(crate) async fn set_execution_state(
        &self,
        execution_id: &str,
        state: ExecutionState,
    ) -> Result<(), PublicTransportError> {
        self.adapter
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
pub struct PublicSessionConfig {
    endpoint: String,
    target: DeploymentTarget,
    principal: String,
    limits: PublicTransportLimits,
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
        let session = zenoh::open(
            crate::bus::session::client_config(&endpoint)
                .map_err(|error| PublicTransportError::Transport(error.to_string()))?,
        )
        .await
        .map_err(|error| PublicTransportError::Transport(error.to_string()))?;
        Ok(Self {
            session,
            endpoint,
            principal,
            limits,
        })
    }

    /// Open one independent logical session on this shared transport.
    pub async fn open(
        &self,
        target: DeploymentTarget,
    ) -> Result<PublicSessionConnection, PublicTransportError> {
        let config = PublicSessionConfig::new(&self.endpoint, target, &self.principal)?
            .with_limits(self.limits.clone())?;
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
        let session = zenoh::open(
            crate::bus::session::client_config(config.endpoint())
                .map_err(|error| PublicTransportError::Transport(error.to_string()))?,
        )
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
            &context.adapter,
            &context.principal_policy,
            &context.limits,
            context.started,
        )
        .await;
    }
}

async fn serve_one_operation(
    query: &Query,
    operation: PublicOperation,
    target: &DeploymentTarget,
    adapter: &Arc<Mutex<SupervisorAdapter>>,
    principal_policy: &PrincipalPolicy,
    limits: &PublicTransportLimits,
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
        PublicOperation::Read
        | PublicOperation::Command
        | PublicOperation::Watch
        | PublicOperation::Subscribe => {
            send_error(
                query,
                &PublicTransportError::Rejected {
                    operation: operation_name.to_owned(),
                    detail: "this fixture does not expose generic service data transport"
                        .to_owned(),
                },
                limits,
            )
            .await;
        }
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
    let lane = match operation.kind() {
        crate::communication::PublicRouteKind::Control => "control",
        crate::communication::PublicRouteKind::Inspection => "inspection",
        crate::communication::PublicRouteKind::Mutation => "mutation",
    };
    format!(
        "{}/clients/*/{lane}/{}",
        target.session_prefix(),
        operation.segment()
    )
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
    let expected_key = key.clone();
    let deadline = tokio::time::Instant::now() + limits.deadline();
    let builder = session
        .get(key)
        .target(QueryTarget::All)
        .consolidation(ConsolidationMode::None)
        .timeout(limits.deadline())
        .with(FifoChannel::new(limits.query_capacity()));
    let builder = match payload {
        Some(payload) => builder
            .payload(payload)
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
    }

    #[test]
    fn principal_policy_only_does_not_turn_a_route_into_authentication() {
        let policy = PrincipalPolicy::only(["operator-a"]);
        assert!(policy.allows("operator-a"));
        assert!(!policy.allows("operator-b"));
    }

    /// Exercise two independently routed supervisor namespaces and two
    /// principal-bound logical sessions over one local Zenoh router.
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
