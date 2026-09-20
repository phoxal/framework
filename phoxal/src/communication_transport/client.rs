//! Client-side public-session transport.
//!
//! Public types: `PublicSessionTransport`, `PublicSessionConfig`,
//! `PublicSessionConnection`, `PublicSubscription`, `DiscoveryEvent`,
//! `SupervisorWatch`, `PublicTlsCredentials`, `PublicTransportSecurity`,
//! `PublicTransportLimits`, `PublicTransportError`.
//!
//! Helpers shared with the server half (`hex_bytes`, `operation_key_expression`,
//! `encode_message`, `decode_message`, `decode_request`,
//! `bounded_error_detail`) live in the parent module and are accessed via
//! `super::` since they are `pub(super)`.
#![allow(unused_imports)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use prost::Message;
use thiserror::Error;
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use zenoh::bytes::Encoding;
use zenoh::handlers::FifoChannel;
use zenoh::query::{ConsolidationMode, QueryTarget};

use crate::communication::bootstrap::SessionOffers;
use crate::communication::route::{PublicOperation, PublicRoute};
use crate::communication::session::{
    BindPortRequest, BindPortResponse, CloseSessionRequest, CloseSessionResponse,
    ListExecutionsRequest, ListExecutionsResponse, ListPortsRequest, ListPortsResponse,
    OpenSessionRequest, OpenSessionResponse, OperationOutcome, OperationRequest, OperationResponse,
    RecordKind, RenewSessionRequest, RenewSessionResponse, SubscriptionAdmission,
    SubscriptionRecord, SubscriptionRequest, SupervisorInfoRequest, SupervisorInfoResponse,
    SupervisorStatusRequest, SupervisorStatusResponse,
};
use crate::communication::validation::{
    BootstrapError, DeploymentTarget, SESSION_PROTOCOL, validate_session_offers,
};

use super::{
    DEFAULT_MAX_DISCOVERED_SUPERVISORS, DEFAULT_PUBLIC_DEADLINE, DEFAULT_PUBLIC_MAX_REQUEST_BYTES,
    DEFAULT_PUBLIC_MAX_RESPONSE_BYTES, DEFAULT_PUBLIC_QUERY_CAPACITY, MAX_PUBLIC_DEADLINE,
    MAX_PUBLIC_ERROR_BYTES, MAX_PUBLIC_SUBSCRIPTION_ID_BYTES, PUBLIC_PROTOBUF_ENCODING,
    bounded_error_detail, decode_message, encode_message, malformed_client,
    operation_key_expression, subscription_key, validate_subscription_admission,
    validate_subscription_request,
};

/// Upper bound for a single simulation observation cut, in bytes. The client
/// uses this as the cap on its own buffer limits; the supervisor uses the
/// same numeric cap as `MAX_CLIENT_SIMULATION_CUT_BYTES` inside the simulation
/// authority. The values must agree — both are the "one bounded native
/// physics cut" envelope — but they are owned by their respective crates.
const MAX_CLIENT_SIMULATION_CUT_BYTES: usize = 8 * 1024 * 1024;

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

    /// Maximum encoded request body for one routed operation.
    #[must_use]
    pub fn request_limit(&self, operation: &str) -> usize {
        if is_simulation_operation(operation) {
            MAX_CLIENT_SIMULATION_CUT_BYTES
        } else {
            self.max_request_bytes
        }
    }

    /// Maximum encoded response body for one routed operation.
    #[must_use]
    pub fn response_limit(&self, operation: &str) -> usize {
        if is_simulation_operation(operation) {
            MAX_CLIENT_SIMULATION_CUT_BYTES
        } else {
            self.max_response_bytes
        }
    }

    pub fn validate(&self) -> Result<(), PublicTransportError> {
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

fn is_simulation_operation(operation: &str) -> bool {
    matches!(
        operation,
        "acquire-authority"
            | "admit-initial-observations"
            | "prepare-boundary"
            | "admit-observations"
            | "reset"
            | "release-authority"
            | "progress"
    )
}

/// The trusted ingress policy applied after exact route parsing.
///
/// `Any` is suitable only when the selected Zenoh router has already enforced
/// the association between an authenticated connection and its
/// `clients/{principal}` namespace. It is not a claim that a route string is
/// itself a credential. `Only` is useful for a supervisor-side allow-list and
/// for tests that exercise spoofed principals without a real ACL router.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
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
        if session_id.len() != 32 {
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
                | PublicOperation::AdmitInitialObservations
                | PublicOperation::PrepareBoundary
                | PublicOperation::AdmitObservations
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
    let payload = encode_message(request, limits.request_limit(operation), operation)?;
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
                        if bytes > limits.response_limit(operation)
                            || response_bytes > limits.response_limit(operation)
                        {
                            return Err(PublicTransportError::BodyTooLarge {
                                operation: operation.to_owned(),
                                bytes: response_bytes,
                                maximum: limits.response_limit(operation),
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
        operation_key_expression(&target, PublicOperation::PrepareBoundary),
        "phoxal/workshop/supervisors/rover-01/simulation/v1/clients/*/prepare-boundary"
    );
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
