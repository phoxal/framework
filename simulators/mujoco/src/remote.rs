//! Public-session MuJoCo simulation authority and native scene ownership.
//!
//! The simulator is an application, not a robot runtime process.  It owns the immutable
//! MuJoCo model and the one mutable [`Scene`], while a public
//! [`phoxal::session::Simulation`] owns the authenticated supervisor boundary.
//! The only operation that can move either boundary is [`RemoteSceneRun::step`]:
//! it submits one complete typed observation cut, admits one actuation cut, and
//! then performs exactly one native quantum.
//!
//! This module deliberately does not import an execution runner, supervisor
//! host, or robot implementation crate.  The small [`SimulationTransport`] trait keeps the
//! lifecycle state machine testable with a transport fake while the blanket
//! implementation below uses the public session client in production.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use phoxal::communication::session::PortKind;
use phoxal::communication::simulation::{
    AcquireAuthorityRequest, AcquireAuthorityResponse, Actuation, AdvanceRequest, AdvanceResponse,
    Observation, ProgressRequest, ProgressResponse, ReleaseAuthorityRequest,
    ReleaseAuthorityResponse, ResetRequest, ResetResponse,
};
use phoxal::session::{SessionError, Simulation as SessionSimulation};
#[cfg(feature = "native")]
use prost::{Message, Name};

#[cfg(feature = "native")]
use phoxal_mujoco::{Model, PhysicsQuantum, Scene, SceneError, StateSnapshot};

/// A boxed future used by the public simulation transport boundary.
pub type SimulationFuture<'a, T, E> =
    Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>;

/// The exact public simulation operations needed by the native coordinator.
///
/// A simulator implementation only needs this transport-shaped surface.  In
/// production it is implemented by [`phoxal::session::Simulation`]; tests use
/// a deterministic fake to exercise duplicate, delay, fencing, and recovery
/// behavior without a router.
pub trait SimulationTransport: Clone + Send + Sync + 'static {
    /// Transport or public-session failure.
    type Error: fmt::Display + Send + Sync + 'static;

    /// Acquire exclusive authority for one immutable execution contract.
    fn acquire_authority<'a>(
        &'a self,
        request: AcquireAuthorityRequest,
    ) -> SimulationFuture<'a, AcquireAuthorityResponse, Self::Error>;

    /// Submit one current-boundary observation cut.
    fn advance<'a>(
        &'a self,
        request: AdvanceRequest,
    ) -> SimulationFuture<'a, AdvanceResponse, Self::Error>;

    /// Reset the current timeline to a fresh timeline.
    fn reset<'a>(
        &'a self,
        request: ResetRequest,
    ) -> SimulationFuture<'a, ResetResponse, Self::Error>;

    /// Release exclusive authority.
    fn release_authority<'a>(
        &'a self,
        request: ReleaseAuthorityRequest,
    ) -> SimulationFuture<'a, ReleaseAuthorityResponse, Self::Error>;

    /// Read authoritative completed progress.
    fn progress<'a>(
        &'a self,
        request: ProgressRequest,
    ) -> SimulationFuture<'a, ProgressResponse, Self::Error>;
}

impl SimulationTransport for SessionSimulation {
    type Error = SessionError;

    fn acquire_authority<'a>(
        &'a self,
        request: AcquireAuthorityRequest,
    ) -> SimulationFuture<'a, AcquireAuthorityResponse, Self::Error> {
        Box::pin(async move { self.acquire_authority(request).await })
    }

    fn advance<'a>(
        &'a self,
        request: AdvanceRequest,
    ) -> SimulationFuture<'a, AdvanceResponse, Self::Error> {
        Box::pin(async move { self.advance(request).await })
    }

    fn reset<'a>(
        &'a self,
        request: ResetRequest,
    ) -> SimulationFuture<'a, ResetResponse, Self::Error> {
        Box::pin(async move { self.reset(request).await })
    }

    fn release_authority<'a>(
        &'a self,
        request: ReleaseAuthorityRequest,
    ) -> SimulationFuture<'a, ReleaseAuthorityResponse, Self::Error> {
        Box::pin(async move { self.release_authority(request).await })
    }

    fn progress<'a>(
        &'a self,
        request: ProgressRequest,
    ) -> SimulationFuture<'a, ProgressResponse, Self::Error> {
        Box::pin(async move { self.progress(request).await })
    }
}

const MAX_PROVIDER_REQUIREMENTS: usize = 256;
const MAX_PROVIDER_ID_BYTES: usize = 64;
const MAX_FQN_BYTES: usize = 512;
const MAX_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;
const AUTHORITY_GRANT_BYTES: usize = 32;
const CORRELATION_BYTES: usize = 16;

/// Public simulation protocol selected by this application coordinator.
pub const SIMULATION_PROTOCOL: &str = "phoxal.simulation.v1";

/// One canonical, immutable provider set selected by the robot bundle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderSet {
    requirements: Vec<phoxal::communication::simulation::ProviderRequirement>,
}

/// One exact typed actuation output admitted for a native scene.
///
/// Actuation ports are separate from observation providers.  They are owned by
/// the simulator's immutable scene configuration and are matched by the
/// service instance, port, and payload FQN before any native control is set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActuationBinding {
    service_instance: String,
    port: String,
    payload_fqn: String,
    actuator_ids: Vec<String>,
}

impl ActuationBinding {
    /// Construct one exact output binding.
    pub fn new(
        service_instance: impl Into<String>,
        port: impl Into<String>,
        payload_fqn: impl Into<String>,
        mut actuator_ids: Vec<String>,
    ) -> Result<Self, ProviderSetError> {
        actuator_ids.sort();
        let binding = Self {
            service_instance: service_instance.into(),
            port: port.into(),
            payload_fqn: payload_fqn.into(),
            actuator_ids,
        };
        validate_identifier(&binding.service_instance, "actuation service instance")?;
        validate_identifier(&binding.port, "actuation port")?;
        validate_fqn(&binding.payload_fqn, "actuation payload FQN", false)?;
        if binding.actuator_ids.is_empty() {
            return Err(ProviderSetError::ActuatorMembership {
                service_instance: binding.service_instance.clone(),
                port: binding.port.clone(),
            });
        }
        let mut actuator_ids = BTreeSet::new();
        for actuator_id in &binding.actuator_ids {
            validate_native_name(actuator_id, "actuator id")?;
            if !actuator_ids.insert(actuator_id.as_str()) {
                return Err(ProviderSetError::ActuatorDuplicate {
                    service_instance: binding.service_instance.clone(),
                    port: binding.port.clone(),
                    actuator_id: actuator_id.clone(),
                });
            }
        }
        Ok(binding)
    }

    /// Service instance that owns this output port.
    #[must_use]
    pub fn service_instance(&self) -> &str {
        &self.service_instance
    }

    /// Generated output port name.
    #[must_use]
    pub fn port(&self) -> &str {
        &self.port
    }

    /// Fully-qualified typed payload name.
    #[must_use]
    pub fn payload_fqn(&self) -> &str {
        &self.payload_fqn
    }

    /// Native actuator identities covered by this output payload.
    #[must_use]
    pub fn actuator_ids(&self) -> &[String] {
        &self.actuator_ids
    }
}

impl ProviderSet {
    /// Validate and canonicalize the provider requirements from the bundle.
    pub fn new(
        requirements: Vec<phoxal::communication::simulation::ProviderRequirement>,
    ) -> Result<Self, ProviderSetError> {
        if requirements.is_empty() || requirements.len() > MAX_PROVIDER_REQUIREMENTS {
            return Err(ProviderSetError::Count {
                count: requirements.len(),
            });
        }

        let mut requirements = requirements;
        for requirement in &requirements {
            validate_identifier(&requirement.service_instance, "service instance")?;
            validate_identifier(&requirement.port, "provider port")?;
            validate_fqn(&requirement.payload_fqn, "payload FQN", false)?;
            validate_fqn(&requirement.input_fqn, "input FQN", true)?;
            let kind = PortKind::try_from(requirement.kind).map_err(|_| {
                ProviderSetError::InvalidKind {
                    service_instance: requirement.service_instance.clone(),
                    port: requirement.port.clone(),
                }
            })?;
            if !matches!(kind, PortKind::State | PortKind::Sample | PortKind::Event | PortKind::Stream)
            {
                return Err(ProviderSetError::InvalidKind {
                    service_instance: requirement.service_instance.clone(),
                    port: requirement.port.clone(),
                });
            }
        }
        requirements.sort_by(|left, right| {
            left.service_instance
                .cmp(&right.service_instance)
                .then_with(|| left.port.cmp(&right.port))
                .then_with(|| left.kind.cmp(&right.kind))
                .then_with(|| left.input_fqn.cmp(&right.input_fqn))
                .then_with(|| left.payload_fqn.cmp(&right.payload_fqn))
        });
        for pair in requirements.windows(2) {
            if pair[0].service_instance == pair[1].service_instance
                && pair[0].port == pair[1].port
            {
                return Err(ProviderSetError::Duplicate {
                    service_instance: pair[0].service_instance.clone(),
                    port: pair[0].port.clone(),
                });
            }
        }
        Ok(Self { requirements })
    }

    /// Build a provider set from the framework-owned immutable definition.
    pub fn from_definition(
        definition: &phoxal::communication::SimulationDefinition,
    ) -> Result<Self, ProviderSetError> {
        Self::new(
            definition
                .providers()
                .iter()
                .map(|provider| {
                    phoxal::communication::simulation::ProviderRequirement {
                        service_instance: provider.service_instance().to_owned(),
                        port: provider.port().to_owned(),
                        payload_fqn: provider.payload_fqn().to_owned(),
                        kind: provider.kind() as i32,
                        input_fqn: provider.input_fqn().to_owned(),
                    }
                })
                .collect(),
        )
    }

    /// The canonical wire requirements.
    #[must_use]
    pub fn requirements(
        &self,
    ) -> &[phoxal::communication::simulation::ProviderRequirement] {
        &self.requirements
    }

    /// Return one provider's exact metadata.
    #[must_use]
    pub fn get(
        &self,
        service_instance: &str,
        port: &str,
    ) -> Option<&phoxal::communication::simulation::ProviderRequirement> {
        self.requirements.iter().find(|requirement| {
            requirement.service_instance == service_instance && requirement.port == port
        })
    }

    /// Require the complete provider set for one advance request.
    pub fn validate_observations(
        &self,
        observations: &[Observation],
        boundary: u64,
        quantum_ns: u64,
    ) -> Result<(), ProviderSetError> {
        if observations.len() != self.requirements.len() {
            return Err(ProviderSetError::Incomplete {
                expected: self.requirements.len(),
                actual: observations.len(),
            });
        }
        let expected_capture_ns = boundary
            .checked_mul(quantum_ns)
            .ok_or(ProviderSetError::CaptureTimeOverflow)?;
        let mut seen = BTreeSet::new();
        for observation in observations {
            if observation.payload.len() > MAX_PAYLOAD_BYTES {
                return Err(ProviderSetError::PayloadTooLarge {
                    service_instance: observation.service_instance.clone(),
                    port: observation.port.clone(),
                    bytes: observation.payload.len(),
                });
            }
            if observation.capture_time_ns != expected_capture_ns {
                return Err(ProviderSetError::CaptureTime {
                    expected: expected_capture_ns,
                    actual: observation.capture_time_ns,
                });
            }
            let key = (observation.service_instance.as_str(), observation.port.as_str());
            if !seen.insert(key) {
                return Err(ProviderSetError::Duplicate {
                    service_instance: observation.service_instance.clone(),
                    port: observation.port.clone(),
                });
            }
            let Some(requirement) = self.get(key.0, key.1) else {
                return Err(ProviderSetError::Unknown {
                    service_instance: observation.service_instance.clone(),
                    port: observation.port.clone(),
                });
            };
            if requirement.payload_fqn.is_empty() {
                return Err(ProviderSetError::InvalidFqn {
                    field: "payload FQN",
                });
            }
        }
        Ok(())
    }
}

/// A provider-set admission failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderSetError {
    /// The set is empty or exceeds the finite bound.
    Count { count: usize },
    /// A provider identity is empty, oversized, or not a route identifier.
    InvalidIdentifier { field: &'static str },
    /// A provider FQN is not a valid Protobuf name.
    InvalidFqn { field: &'static str },
    /// A provider uses an unsupported or unspecified semantic kind.
    InvalidKind {
        service_instance: String,
        port: String,
    },
    /// A service/port pair appears more than once.
    Duplicate {
        service_instance: String,
        port: String,
    },
    /// An advance omitted one or more immutable providers.
    Incomplete { expected: usize, actual: usize },
    /// The boundary-to-capture-time conversion overflowed.
    CaptureTimeOverflow,
    /// An observation timestamp is not the exact current-boundary timestamp.
    CaptureTime { expected: u64, actual: u64 },
    /// An observation is not in the immutable provider set.
    Unknown {
        service_instance: String,
        port: String,
    },
    /// A typed observation exceeded the bounded exchange payload.
    PayloadTooLarge {
        service_instance: String,
        port: String,
        bytes: usize,
    },
    /// An actuation binding omitted every native actuator.
    ActuatorMembership {
        service_instance: String,
        port: String,
    },
    /// An actuator identity is repeated within an actuation binding.
    ActuatorDuplicate {
        service_instance: String,
        port: String,
        actuator_id: String,
    },
}

impl fmt::Display for ProviderSetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Count { count } => write!(formatter, "provider set has invalid size {count}"),
            Self::InvalidIdentifier { field } => write!(formatter, "{field} is invalid"),
            Self::InvalidFqn { field } => write!(formatter, "{field} is invalid"),
            Self::InvalidKind {
                service_instance,
                port,
            } => write!(formatter, "provider kind is invalid for {service_instance}/{port}"),
            Self::Duplicate {
                service_instance,
                port,
            } => write!(formatter, "provider {service_instance}/{port} is duplicated"),
            Self::Incomplete { expected, actual } => write!(
                formatter,
                "advance has {actual} observations, expected the complete set of {expected}"
            ),
            Self::CaptureTimeOverflow => write!(formatter, "capture time overflows nanoseconds"),
            Self::CaptureTime { expected, actual } => write!(
                formatter,
                "observation capture time {actual} does not equal current boundary time {expected}"
            ),
            Self::Unknown {
                service_instance,
                port,
            } => write!(formatter, "observation {service_instance}/{port} is not a provider"),
            Self::PayloadTooLarge {
                service_instance,
                port,
                bytes,
            } => write!(
                formatter,
                "observation {service_instance}/{port} payload is {bytes} bytes"
            ),
            Self::ActuatorMembership {
                service_instance,
                port,
            } => write!(formatter, "actuation {service_instance}/{port} has no native actuators"),
            Self::ActuatorDuplicate {
                service_instance,
                port,
                actuator_id,
            } => write!(
                formatter,
                "actuation {service_instance}/{port} repeats actuator {actuator_id}"
            ),
        }
    }
}

impl std::error::Error for ProviderSetError {}

fn validate_identifier(value: &str, field: &'static str) -> Result<(), ProviderSetError> {
    if value.is_empty()
        || value.len() > MAX_PROVIDER_ID_BYTES
        || !value.is_ascii()
        || value.bytes().any(|byte| byte.is_ascii_whitespace())
        || value
            .bytes()
            .next()
            .is_none_or(|byte| !(byte.is_ascii_lowercase() || byte.is_ascii_digit()))
        || !value
            .bytes()
            .skip(1)
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-')
    {
        return Err(ProviderSetError::InvalidIdentifier { field });
    }
    Ok(())
}

fn validate_native_name(value: &str, field: &'static str) -> Result<(), ProviderSetError> {
    if value.is_empty()
        || value.len() > MAX_PROVIDER_ID_BYTES
        || !value.is_ascii()
        || value.bytes().any(|byte| byte.is_ascii_whitespace() || byte == 0)
    {
        return Err(ProviderSetError::InvalidIdentifier { field });
    }
    Ok(())
}

fn validate_fqn(
    value: &str,
    field: &'static str,
    allow_empty: bool,
) -> Result<(), ProviderSetError> {
    if value.is_empty() && allow_empty {
        return Ok(());
    }
    if value.is_empty()
        || value.len() > MAX_FQN_BYTES
        || !value.split('.').all(|segment| {
            !segment.is_empty()
                && segment.bytes().next().is_some_and(|byte| {
                    byte.is_ascii_alphabetic() || byte == b'_'
                })
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
    {
        return Err(ProviderSetError::InvalidFqn { field });
    }
    Ok(())
}

/// Lifecycle state of one simulator-owned authority client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorityState {
    /// No authority has been acquired yet.
    Disconnected,
    /// The client owns a live exclusive authority grant.
    Acquired,
    /// The client deliberately abandoned the authority after application loss.
    Lost,
    /// The authority or its remote execution reached a terminal failure.
    Failed,
    /// The authority was released normally.
    Released,
}

/// A public simulation authority lifecycle failure.
#[derive(Debug)]
pub enum AuthorityClientError<E> {
    /// The operation cannot run in the current local lifecycle state.
    InvalidState {
        operation: &'static str,
        state: AuthorityState,
    },
    /// A request or immutable identity was not admissible locally.
    InvalidRequest(String),
    /// The immutable provider set rejected a request.
    ProviderSet(ProviderSetError),
    /// The public transport returned an error before a usable response.
    Transport(E),
    /// An advance result is uncertain.  The exact request is retained and may
    /// be recovered with [`AuthorityClient::retry_uncertain`].
    UncertainAdvance {
        /// The transport error or timeout reported by the caller's transport.
        source: E,
        /// The boundary the retained request attempted to advance.
        boundary: u64,
        /// The retained idempotency correlation identity.
        correlation_id: Vec<u8>,
    },
    /// The peer returned a response that cannot be fenced to this authority.
    Protocol(String),
    /// The remote authority reports a terminal failure.
    RemoteFailure(String),
    /// The local authority lease has elapsed before a request was sent.
    LeaseExpired,
    /// A second advance was attempted while its first result remains uncertain.
    PendingAdvance,
    /// Recovery was requested without a retained uncertain advance.
    NoPendingAdvance,
}

impl<E: fmt::Display> fmt::Display for AuthorityClientError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidState { operation, state } => {
                write!(formatter, "cannot {operation} in authority state {state:?}")
            }
            Self::InvalidRequest(detail) => formatter.write_str(detail),
            Self::ProviderSet(error) => error.fmt(formatter),
            Self::Transport(error) => write!(formatter, "public simulation transport failed: {error}"),
            Self::UncertainAdvance {
                source,
                boundary,
                correlation_id,
            } => write!(
                formatter,
                "advance at boundary {boundary} is uncertain ({source}); recover correlation {correlation_id:?} with progress"
            ),
            Self::Protocol(detail) => write!(formatter, "public simulation protocol violation: {detail}"),
            Self::RemoteFailure(detail) => write!(formatter, "remote simulation failed: {detail}"),
            Self::LeaseExpired => formatter.write_str("simulation authority lease has expired"),
            Self::PendingAdvance => formatter.write_str("an earlier simulation advance is still pending"),
            Self::NoPendingAdvance => formatter.write_str("there is no uncertain simulation advance to recover"),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for AuthorityClientError<E> {}

#[derive(Clone, Debug)]
struct PendingAdvance {
    request: AdvanceRequest,
}

/// A lease-bound public simulation authority client.
pub struct AuthorityClient<T> {
    transport: T,
    providers: ProviderSet,
    execution_id: String,
    model_identity: String,
    quantum_ns: u64,
    session_id: Vec<u8>,
    authority_grant: Option<Vec<u8>>,
    timeline_id: Option<String>,
    boundary: u64,
    state: AuthorityState,
    generation: u64,
    lease_deadline: Option<Instant>,
    lease_duration: Option<Duration>,
    next_correlation: u64,
    pending: Option<PendingAdvance>,
    retained: BTreeMap<Vec<u8>, AdvanceResponse>,
}

impl<T> fmt::Debug for AuthorityClient<T>
where
    T: SimulationTransport,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthorityClient")
            .field("execution_id", &self.execution_id)
            .field("model_identity", &self.model_identity)
            .field("quantum_ns", &self.quantum_ns)
            .field("timeline_id", &self.timeline_id)
            .field("boundary", &self.boundary)
            .field("state", &self.state)
            .field("generation", &self.generation)
            .field("has_pending", &self.pending.is_some())
            .finish_non_exhaustive()
    }
}

impl<T> AuthorityClient<T>
where
    T: SimulationTransport,
{
    /// Construct an authority client from exact bundle facts.
    pub fn new(
        transport: T,
        execution_id: impl Into<String>,
        model_identity: impl Into<String>,
        quantum_ns: u64,
        providers: ProviderSet,
    ) -> Result<Self, AuthorityClientError<T::Error>> {
        let execution_id = execution_id.into();
        let model_identity = model_identity.into();
        validate_authority_identity(&execution_id, "execution id")?;
        validate_authority_identity(&model_identity, "model identity")?;
        if quantum_ns == 0 {
            return Err(AuthorityClientError::InvalidRequest(
                "simulation quantum must be positive".to_owned(),
            ));
        }
        Ok(Self {
            transport,
            providers,
            execution_id,
            model_identity,
            quantum_ns,
            session_id: Vec::new(),
            authority_grant: None,
            timeline_id: None,
            boundary: 0,
            state: AuthorityState::Disconnected,
            generation: 0,
            lease_deadline: None,
            lease_duration: None,
            next_correlation: 1,
            pending: None,
            retained: BTreeMap::new(),
        })
    }

    /// The immutable provider set this client will present on acquisition.
    #[must_use]
    pub fn providers(&self) -> &ProviderSet {
        &self.providers
    }

    /// The execution identity selected by the bundle.
    #[must_use]
    pub fn execution_id(&self) -> &str {
        &self.execution_id
    }

    /// The immutable model/resource identity selected by the bundle.
    #[must_use]
    pub fn model_identity(&self) -> &str {
        &self.model_identity
    }

    /// The source-authored common simulation quantum.
    #[must_use]
    pub const fn quantum_ns(&self) -> u64 {
        self.quantum_ns
    }

    /// Current authority lifecycle state.
    #[must_use]
    pub const fn state(&self) -> AuthorityState {
        self.state
    }

    /// Reset-scoped local generation fence.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// The currently admitted timeline identity.
    #[must_use]
    pub fn timeline_id(&self) -> Option<&str> {
        self.timeline_id.as_deref()
    }

    /// The last completed boundary acknowledged by the remote authority.
    #[must_use]
    pub const fn boundary(&self) -> u64 {
        self.boundary
    }

    /// The authenticated session identity returned during acquisition.
    #[must_use]
    pub fn session_id(&self) -> &[u8] {
        &self.session_id
    }

    /// The opaque authority grant, when this client owns one.
    #[must_use]
    pub fn authority_grant(&self) -> Option<&[u8]> {
        self.authority_grant.as_deref()
    }

    /// Whether the local lease watchdog considers the authority expired.
    #[must_use]
    pub fn lease_expired(&self) -> bool {
        self.lease_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
    }

    /// Acquire the one exclusive authority grant for this execution.
    pub async fn acquire(&mut self) -> Result<(), AuthorityClientError<T::Error>> {
        if self.state != AuthorityState::Disconnected {
            return Err(self.invalid_state("acquire"));
        }
        let correlation_id = self.next_correlation_id();
        let response = self
            .transport
            .acquire_authority(AcquireAuthorityRequest {
                execution_id: self.execution_id.clone(),
                model_identity: self.model_identity.clone(),
                quantum_ns: self.quantum_ns,
                providers: self.providers.requirements.clone(),
                session_id: Vec::new(),
                correlation_id: correlation_id.clone(),
            })
            .await
            .map_err(AuthorityClientError::Transport)?;
        if response.authority_grant.len() != AUTHORITY_GRANT_BYTES {
            return Err(self.protocol("authority grant has the wrong length"));
        }
        if response.timeline_id.is_empty()
            || response.lease_ms == 0
            || response.execution_id != self.execution_id
            || response.model_identity != self.model_identity
            || response.quantum_ns != self.quantum_ns
            || response.correlation_id != correlation_id
        {
            return Err(self.protocol("authority acquisition response is not fenced to the request"));
        }
        self.session_id = response.session_id;
        if self.session_id.is_empty() {
            return Err(self.protocol("authority acquisition returned an empty session"));
        }
        self.authority_grant = Some(response.authority_grant);
        self.timeline_id = Some(response.timeline_id);
        self.boundary = response.boundary;
        self.generation = self.generation.checked_add(1).ok_or_else(|| {
            AuthorityClientError::Protocol("authority generation overflow".to_owned())
        })?;
        self.lease_deadline = Some(Instant::now() + Duration::from_millis(response.lease_ms as u64));
        self.lease_duration = Some(Duration::from_millis(response.lease_ms as u64));
        self.state = AuthorityState::Acquired;
        Ok(())
    }

    /// Submit one exact current-boundary observation cut.
    ///
    /// A transport error does not cause a replay.  The request is retained
    /// verbatim and the caller must use [`Self::retry_uncertain`], which first
    /// checks authoritative progress and then retries the same correlation only
    /// to recover an idempotent retained result.
    pub async fn advance(
        &mut self,
        observations: Vec<Observation>,
    ) -> Result<AdvanceResponse, AuthorityClientError<T::Error>> {
        self.ensure_live("advance")?;
        if self.pending.is_some() {
            return Err(AuthorityClientError::PendingAdvance);
        }
        self.providers
            .validate_observations(&observations, self.boundary, self.quantum_ns)
            .map_err(AuthorityClientError::ProviderSet)?;
        let timeline_id = self.timeline_id.clone().ok_or_else(|| {
            AuthorityClientError::Protocol("acquired authority has no timeline".to_owned())
        })?;
        let authority_grant = self.authority_grant.clone().ok_or_else(|| {
            AuthorityClientError::Protocol("acquired authority has no grant".to_owned())
        })?;
        let correlation_id = self.next_correlation_id();
        let request = AdvanceRequest {
            authority_grant,
            execution_id: self.execution_id.clone(),
            timeline_id,
            boundary: self.boundary,
            observations,
            session_id: self.session_id.clone(),
            correlation_id,
        };
        let pending = PendingAdvance {
            request: request.clone(),
        };
        self.pending = Some(pending);
        match self.transport.advance(request.clone()).await {
            Ok(response) => {
                self.accept_advance(&request, response)
            }
            Err(source) => Err(AuthorityClientError::UncertainAdvance {
                source,
                boundary: request.boundary,
                correlation_id: request.correlation_id,
            }),
        }
    }

    /// Recover an uncertain advance using authoritative progress and the same
    /// idempotency key.  No new observation cut or blind replay is generated.
    pub async fn retry_uncertain(
        &mut self,
    ) -> Result<AdvanceResponse, AuthorityClientError<T::Error>> {
        self.ensure_live("retry uncertain advance")?;
        let Some(pending) = self.pending.clone() else {
            return Err(AuthorityClientError::NoPendingAdvance);
        };
        let progress = self.query_progress().await?;
        if progress.failed {
            self.state = AuthorityState::Failed;
            return Err(AuthorityClientError::RemoteFailure(
                progress
                    .detail
                    .unwrap_or_else(|| "remote simulation failed".to_owned()),
            ));
        }
        let expected = pending
            .request
            .boundary
            .checked_add(1)
            .ok_or_else(|| self.protocol("advance boundary overflow"))?;
        if progress.completed_boundary != pending.request.boundary
            && progress.completed_boundary != expected
        {
            self.state = AuthorityState::Failed;
            return Err(self.protocol("authoritative progress cannot fence the pending advance"));
        }
        // When the remote side already crossed the boundary, this retry asks
        // only for the retained result under the same correlation.  If that
        // result is unavailable, the public contract rejects the request and
        // the client fails closed instead of replaying a new observation cut.
        let response = self
            .transport
            .advance(pending.request.clone())
            .await
            .map_err(|source| AuthorityClientError::UncertainAdvance {
                source,
                boundary: pending.request.boundary,
                correlation_id: pending.request.correlation_id.clone(),
            })?;
        self.accept_advance(&pending.request, response)
    }

    /// Query and retain authoritative progress as a lease-renewing watchdog.
    pub async fn watchdog_tick(&mut self) -> Result<ProgressResponse, AuthorityClientError<T::Error>> {
        self.ensure_live("renew simulation authority")?;
        self.query_progress().await
    }

    /// Reset the remote timeline and fence all subsequent requests to it.
    pub async fn reset(&mut self) -> Result<ResetResponse, AuthorityClientError<T::Error>> {
        self.ensure_live("reset")?;
        if self.pending.is_some() {
            return Err(AuthorityClientError::PendingAdvance);
        }
        let timeline_id = self.timeline_id.clone().ok_or_else(|| {
            AuthorityClientError::Protocol("acquired authority has no timeline".to_owned())
        })?;
        let authority_grant = self.authority_grant.clone().ok_or_else(|| {
            AuthorityClientError::Protocol("acquired authority has no grant".to_owned())
        })?;
        let correlation_id = self.next_correlation_id();
        let response = self
            .transport
            .reset(ResetRequest {
                authority_grant: authority_grant.clone(),
                execution_id: self.execution_id.clone(),
                timeline_id: timeline_id.clone(),
                completed_boundary: self.boundary,
                session_id: self.session_id.clone(),
                correlation_id: correlation_id.clone(),
            })
            .await
            .map_err(AuthorityClientError::Transport)?;
        if response.next_timeline_id.is_empty()
            || response.next_timeline_id == timeline_id
            || response.boundary != 0
            || response.session_id != self.session_id
            || response.execution_id != self.execution_id
            || response.authority_grant != authority_grant
            || response.correlation_id != correlation_id
            || response.previous_timeline_id != timeline_id
            || response.requested_boundary != self.boundary
        {
            return Err(self.protocol("reset response is not fenced to the current timeline"));
        }
        self.timeline_id = Some(response.next_timeline_id.clone());
        self.boundary = 0;
        self.generation = self.generation.checked_add(1).ok_or_else(|| {
            AuthorityClientError::Protocol("authority generation overflow".to_owned())
        })?;
        self.retained.clear();
        self.refresh_lease();
        Ok(response)
    }

    /// Release the current exclusive authority.
    pub async fn release(
        &mut self,
    ) -> Result<ReleaseAuthorityResponse, AuthorityClientError<T::Error>> {
        self.ensure_live("release")?;
        if self.pending.is_some() {
            return Err(AuthorityClientError::PendingAdvance);
        }
        let authority_grant = self.authority_grant.clone().ok_or_else(|| {
            AuthorityClientError::Protocol("acquired authority has no grant".to_owned())
        })?;
        let timeline_id = self.timeline_id.clone().ok_or_else(|| {
            AuthorityClientError::Protocol("acquired authority has no timeline".to_owned())
        })?;
        let correlation_id = self.next_correlation_id();
        let response = self
            .transport
            .release_authority(ReleaseAuthorityRequest {
                authority_grant: authority_grant.clone(),
                session_id: self.session_id.clone(),
                correlation_id: correlation_id.clone(),
            })
            .await
            .map_err(AuthorityClientError::Transport)?;
        if response.session_id != self.session_id
            || response.authority_grant != authority_grant
            || response.correlation_id != correlation_id
            || response.execution_id != self.execution_id
            || response.timeline_id != timeline_id
            || response.completed_boundary != self.boundary
        {
            return Err(self.protocol("release response is not fenced to the authority"));
        }
        self.authority_grant = None;
        self.timeline_id = None;
        self.lease_deadline = None;
        self.lease_duration = None;
        self.pending = None;
        self.state = AuthorityState::Released;
        self.generation = self.generation.checked_add(1).ok_or_else(|| {
            AuthorityClientError::Protocol("authority generation overflow".to_owned())
        })?;
        Ok(response)
    }

    /// Fail closed after the simulator process loses its native owner.
    ///
    /// The opaque grant is dropped locally.  The supervisor will revoke it by
    /// lease expiry, and no later call can accidentally continue an execution
    /// whose native scene state is no longer known.
    pub fn mark_application_lost(&mut self) {
        self.authority_grant = None;
        self.pending = None;
        self.lease_deadline = None;
        self.state = AuthorityState::Lost;
        self.generation = self.generation.saturating_add(1);
    }

    fn query_progress(&mut self) -> ResultFuture<'_, T, ProgressResponse> {
        Box::pin(async move {
            let authority_grant = self.authority_grant.clone().ok_or_else(|| {
                AuthorityClientError::Protocol("acquired authority has no grant".to_owned())
            })?;
            let correlation_id = self.next_correlation_id();
            let response = self
                .transport
                .progress(ProgressRequest {
                    authority_grant: authority_grant.clone(),
                    session_id: self.session_id.clone(),
                    correlation_id: correlation_id.clone(),
                })
                .await
                .map_err(AuthorityClientError::Transport)?;
            if response.session_id != self.session_id
                || response.authority_grant != authority_grant
                || response.correlation_id != correlation_id
                || response.execution_id != self.execution_id
                || response.timeline_id != self.timeline_id.as_deref().unwrap_or_default()
            {
                return Err(self.protocol("progress response is not fenced to the authority"));
            }
            if response.failed {
                self.state = AuthorityState::Failed;
                return Err(AuthorityClientError::RemoteFailure(
                    response
                        .detail
                        .clone()
                        .unwrap_or_else(|| "remote simulation failed".to_owned()),
                ));
            }
            if self.pending.is_none() && response.completed_boundary != self.boundary {
                self.state = AuthorityState::Failed;
                return Err(self.protocol("authoritative progress diverged from local boundary"));
            }
            self.refresh_lease();
            Ok(response)
        })
    }

    fn accept_advance(
        &mut self,
        request: &AdvanceRequest,
        response: AdvanceResponse,
    ) -> Result<AdvanceResponse, AuthorityClientError<T::Error>> {
        let expected = request
            .boundary
            .checked_add(1)
            .ok_or_else(|| self.protocol("advance boundary overflow"))?;
        if response.session_id != self.session_id
            || response.authority_grant != request.authority_grant
            || response.execution_id != request.execution_id
            || response.timeline_id != request.timeline_id
            || response.requested_boundary != request.boundary
            || response.completed_boundary != expected
            || response.correlation_id != request.correlation_id
        {
            return Err(self.protocol("advance response is not fenced to the request"));
        }
        validate_receipts(request, &response)?;
        for actuation in &response.actuation {
            if actuation.service_instance.is_empty()
                || actuation.port.is_empty()
                || actuation.payload.len() > MAX_PAYLOAD_BYTES
            {
                return Err(self.protocol("advance returned invalid actuation metadata"));
            }
        }
        self.pending = None;
        self.boundary = expected;
        self.refresh_lease();
        if self.retained.len() >= 64 {
            let Some(oldest) = self.retained.keys().next().cloned() else {
                return Err(self.protocol("advance result retention is inconsistent"));
            };
            self.retained.remove(&oldest);
        }
        self.retained
            .insert(request.correlation_id.clone(), response.clone());
        Ok(response)
    }

    fn ensure_live(&mut self, operation: &'static str) -> Result<(), AuthorityClientError<T::Error>> {
        if self.state != AuthorityState::Acquired {
            return Err(self.invalid_state(operation));
        }
        if self.lease_expired() {
            self.state = AuthorityState::Lost;
            self.authority_grant = None;
            self.pending = None;
            self.generation = self.generation.saturating_add(1);
            return Err(AuthorityClientError::LeaseExpired);
        }
        Ok(())
    }

    fn refresh_lease(&mut self) {
        // A public progress/advance/reset response is an authority heartbeat.
        // The server intentionally returns no new duration, so retain the
        // exact lease interval supplied during acquisition.
        if let Some(duration) = self.lease_duration {
            self.lease_deadline = Some(Instant::now() + duration);
        }
    }

    fn next_correlation_id(&mut self) -> Vec<u8> {
        let sequence = self.next_correlation;
        self.next_correlation = self.next_correlation.saturating_add(1);
        let mut id = [0_u8; CORRELATION_BYTES];
        id[..8].copy_from_slice(&self.generation.to_be_bytes());
        id[8..].copy_from_slice(&sequence.to_be_bytes());
        id.to_vec()
    }

    fn invalid_state(&self, operation: &'static str) -> AuthorityClientError<T::Error> {
        AuthorityClientError::InvalidState {
            operation,
            state: self.state,
        }
    }

    fn protocol(&self, detail: impl Into<String>) -> AuthorityClientError<T::Error> {
        AuthorityClientError::Protocol(detail.into())
    }
}

type ResultFuture<'a, T, V> =
    Pin<Box<dyn Future<Output = Result<V, AuthorityClientError<<T as SimulationTransport>::Error>>> + 'a>>;

fn validate_authority_identity<E>(
    value: &str,
    field: &'static str,
) -> Result<(), AuthorityClientError<E>> {
    if value.is_empty()
        || value.len() > MAX_FQN_BYTES
        || !value.is_ascii()
        || value.bytes().any(|byte| byte.is_ascii_whitespace())
    {
        return Err(AuthorityClientError::InvalidRequest(format!(
            "{field} is invalid"
        )));
    }
    Ok(())
}

fn validate_receipts<E>(
    request: &AdvanceRequest,
    response: &AdvanceResponse,
) -> Result<(), AuthorityClientError<E>> {
    if response.observation_receipts.len() != request.observations.len() {
        return Err(AuthorityClientError::Protocol(
            "advance response omitted an observation receipt".to_owned(),
        ));
    }
    let expected = request
        .observations
        .iter()
        .map(|observation| {
            (
                observation.service_instance.as_str(),
                observation.port.as_str(),
            )
        })
        .collect::<BTreeSet<_>>();
    let mut actual = BTreeSet::new();
    for receipt in &response.observation_receipts {
        if receipt.sequence == 0 {
            return Err(AuthorityClientError::Protocol(
                "advance receipt has zero sequence".to_owned(),
            ));
        }
        if receipt.boundary != request.boundary || receipt.correlation_id != request.correlation_id {
            return Err(AuthorityClientError::Protocol(
                "advance receipt is not fenced to the request".to_owned(),
            ));
        }
        if !actual.insert((receipt.service_instance.as_str(), receipt.port.as_str())) {
            return Err(AuthorityClientError::Protocol(
                "advance response duplicated an observation receipt".to_owned(),
            ));
        }
    }
    if actual != expected {
        return Err(AuthorityClientError::Protocol(
            "advance response receipts do not cover the observation cut".to_owned(),
        ));
    }
    Ok(())
}

/// A native-side typed provider owned by the simulator application.
///
/// Implementations translate model-backed sensor values into the exact
/// Protobuf payloads named by [`ProviderSet`] and translate the robot's
/// returned actuation payloads into the complete MuJoCo control vector.  No
/// provider implementation is allowed to infer a missing port or actuator.
#[cfg(feature = "native")]
pub trait NativeProvider {
    /// Provider encoding/decoding failure.
    type Error: fmt::Display;

    /// The exact immutable provider set this implementation serves.
    fn providers(&self) -> &ProviderSet;

    /// The exact set of typed actuation ports that may reach this scene.
    fn actuation_bindings(&self) -> &[ActuationBinding];

    /// Encode one complete current-boundary sensor cut.
    fn observations(
        &mut self,
        model: &Model,
        state: &StateSnapshot,
        quantum_ns: u64,
    ) -> Result<Vec<Observation>, Self::Error>;

    /// Decode the complete returned actuation cut into native scalar controls.
    fn controls(
        &mut self,
        model: &Model,
        actuation: &[Actuation],
    ) -> Result<Vec<f64>, Self::Error>;

    /// Reset provider-local cursors after the native scene has reset.
    fn reset(&mut self, _model: &Model, _state: &StateSnapshot) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Errors returned by the common typed MuJoCo payload helpers.
#[cfg(feature = "native")]
#[derive(Debug, thiserror::Error)]
pub enum NativeProviderError {
    /// A model-local native binding could not be resolved.
    #[error("native model binding failed: {0}")]
    Model(#[from] phoxal_mujoco::ModelError),
    /// A Protobuf payload could not be decoded.
    #[error("typed provider payload could not be decoded: {0}")]
    Decode(#[from] prost::DecodeError),
    /// A typed payload failed domain validation.
    #[error("typed provider payload is invalid: {0}")]
    InvalidPayload(String),
    /// A returned actuation does not identify a configured native actuator.
    #[error("returned actuation is invalid: {0}")]
    InvalidActuation(String),
}

/// Bundle-selected and caller-selected provenance facts required for a run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProvenanceInput {
    /// Immutable robot bundle identity or digest obtained from the selected bundle.
    robot_bundle_identity: String,
    /// Caller-selected finite run identity.
    run_id: String,
}

impl ProvenanceInput {
    /// Construct run facts from the exact immutable robot bundle selected by
    /// cargo-phoxal and the caller's run identity.
    pub fn new(
        robot_bundle_identity: impl Into<String>,
        run_id: impl Into<String>,
    ) -> Result<Self, ProvenanceError> {
        let input = Self {
            robot_bundle_identity: robot_bundle_identity.into(),
            run_id: run_id.into(),
        };
        for (field, value) in [
            ("robot bundle identity", &input.robot_bundle_identity),
            ("run identity", &input.run_id),
        ] {
            if value.is_empty() || value.len() > MAX_FQN_BYTES || value.chars().any(char::is_whitespace)
            {
                return Err(ProvenanceError::InvalidField { field });
            }
        }
        Ok(input)
    }

    /// Exact immutable robot bundle identity retained for the run.
    #[must_use]
    pub fn robot_bundle_identity(&self) -> &str {
        &self.robot_bundle_identity
    }

    /// Caller-selected run identity retained for the run.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }
}

#[cfg(feature = "native")]
const APPLICATION_IDENTITY: &str = concat!(env!("CARGO_PKG_NAME"), "@", env!("CARGO_PKG_VERSION"));

/// Exact app/native/scene/model/robot-bundle/run evidence retained by a run.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct SimulationProvenance {
    /// Public simulation protocol used for the run.
    pub protocol: &'static str,
    /// Simulator application identity.
    pub app_identity: String,
    /// Exact native MuJoCo version linked by the app.
    pub native_version: String,
    /// Source-authored scene identity.
    pub scene_identity: String,
    /// Closed model/resource identity.
    pub model_identity: String,
    /// Common source-authored quantum in nanoseconds.
    pub quantum_ns: u64,
    /// Immutable robot bundle identity.
    pub robot_bundle_identity: String,
    /// Finite run identity.
    pub run_id: String,
    /// Supervisor execution identity selected for the run.
    pub execution_id: String,
    /// Supervisor timeline identity at acquisition.
    pub timeline_id: String,
}

#[cfg(feature = "native")]
impl SimulationProvenance {
    fn from_input(
        input: ProvenanceInput,
        model: &Model,
        quantum_ns: u64,
        execution_id: &str,
        timeline_id: &str,
    ) -> Result<Self, ProvenanceError> {
        if quantum_ns == 0 {
            return Err(ProvenanceError::InvalidQuantum);
        }
        for (field, value) in [("execution identity", execution_id), ("timeline identity", timeline_id)] {
            if value.is_empty() || value.len() > MAX_FQN_BYTES || value.chars().any(char::is_whitespace) {
                return Err(ProvenanceError::InvalidField { field });
            }
        }
        let native_version = Model::native_version().to_owned();
        if native_version.is_empty() {
            return Err(ProvenanceError::InvalidField {
                field: "native version",
            });
        }
        Ok(Self {
            protocol: SIMULATION_PROTOCOL,
            app_identity: APPLICATION_IDENTITY.to_owned(),
            native_version,
            scene_identity: model.identity().to_hex(),
            model_identity: model.identity().to_hex(),
            quantum_ns,
            robot_bundle_identity: input.robot_bundle_identity,
            run_id: input.run_id,
            execution_id: execution_id.to_owned(),
            timeline_id: timeline_id.to_owned(),
        })
    }
}

/// A missing or contradictory provenance fact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProvenanceError {
    /// One required fact was not supplied in a bounded form.
    InvalidField { field: &'static str },
    /// The native timestep cannot be represented exactly in wire nanoseconds.
    InvalidQuantum,
}

impl fmt::Display for ProvenanceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidField { field } => write!(formatter, "{field} is invalid"),
            Self::InvalidQuantum => formatter.write_str("native quantum is not a positive nanosecond value"),
        }
    }
}

impl std::error::Error for ProvenanceError {}

/// A returned actuation cut and the scalar controls actually applied natively.
#[derive(Clone, Debug)]
pub struct AppliedActuation {
    /// Completed boundary after the native transition.
    pub boundary: u64,
    /// Typed actuation payloads admitted by the remote authority.
    pub requested: Vec<Actuation>,
    /// Complete native control vector applied for this transition.
    pub native_controls: Box<[f64]>,
}

/// A public-authority/native-scene coordination failure.
#[cfg(feature = "native")]
#[derive(Debug)]
pub enum RemoteSceneError<TE, PE> {
    /// Public-session authority lifecycle failure.
    Authority(AuthorityClientError<TE>),
    /// Native scene mutation or snapshot failure.
    Native(SceneError),
    /// Typed provider encoding/decoding failure.
    Provider(PE),
    /// The local and remote boundary identities diverged.
    Fencing(String),
    /// A required provenance fact was not available.
    Provenance(ProvenanceError),
}

#[cfg(feature = "native")]
impl<TE: fmt::Display, PE: fmt::Display> fmt::Display for RemoteSceneError<TE, PE> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Authority(error) => error.fmt(formatter),
            Self::Native(error) => error.fmt(formatter),
            Self::Provider(error) => write!(formatter, "native provider failed: {error}"),
            Self::Fencing(detail) => write!(formatter, "simulation boundary fencing failed: {detail}"),
            Self::Provenance(error) => error.fmt(formatter),
        }
    }
}

#[cfg(feature = "native")]
impl<TE: fmt::Debug + fmt::Display, PE: fmt::Debug + fmt::Display> std::error::Error
    for RemoteSceneError<TE, PE>
{
}

/// The one native MuJoCo scene coordinated through a public simulation session.
#[cfg(feature = "native")]
pub struct RemoteSceneRun<T, P>
where
    T: SimulationTransport,
    P: NativeProvider,
{
    scene: Scene,
    authority: AuthorityClient<T>,
    provider: P,
    provenance: SimulationProvenance,
    state: StateSnapshot,
    applied_actuation: Option<AppliedActuation>,
    generation: u64,
}

#[cfg(feature = "native")]
impl<T, P> fmt::Debug for RemoteSceneRun<T, P>
where
    T: SimulationTransport,
    P: NativeProvider,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteSceneRun")
            .field("authority", &self.authority)
            .field("provenance", &self.provenance)
            .field("boundary", &self.state.boundary())
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "native")]
impl<T, P> RemoteSceneRun<T, P>
where
    T: SimulationTransport,
    P: NativeProvider,
{
    /// Acquire public authority and bind one native scene to it.
    pub async fn acquire(
        scene: Scene,
        transport: T,
        provider: P,
        execution_id: impl Into<String>,
        provenance: ProvenanceInput,
    ) -> Result<Self, RemoteSceneError<T::Error, P::Error>> {
        let model_identity = scene.model().identity().to_hex();
        let quantum_ns = quantum_nanoseconds(scene.quantum())
            .map_err(|error| RemoteSceneError::Fencing(error.to_string()))?;
        if provider.providers().requirements().is_empty() {
            return Err(RemoteSceneError::Fencing(
                "native provider has no immutable provider requirements".to_owned(),
            ));
        }
        if provider.actuation_bindings().is_empty() {
            return Err(RemoteSceneError::Fencing(
                "native provider has no immutable actuation bindings".to_owned(),
            ));
        }
        validate_bindings_for_model(scene.model(), provider.actuation_bindings())
            .map_err(|error| RemoteSceneError::Fencing(error.to_string()))?;
        let mut authority = AuthorityClient::new(
            transport,
            execution_id,
            model_identity,
            quantum_ns,
            provider.providers().clone(),
        )
        .map_err(RemoteSceneError::Authority)?;
        authority
            .acquire()
            .await
            .map_err(RemoteSceneError::Authority)?;
        if authority.boundary() != 0 {
            authority.mark_application_lost();
            return Err(RemoteSceneError::Fencing(
                "authority was acquired above native boundary zero".to_owned(),
            ));
        }
        let state = match scene.snapshot() {
            Ok(state) => state,
            Err(error) => {
                authority.mark_application_lost();
                return Err(RemoteSceneError::Native(error));
            }
        };
        let timeline_id = match authority.timeline_id() {
            Some(timeline_id) => timeline_id.to_owned(),
            None => {
                authority.mark_application_lost();
                return Err(RemoteSceneError::Fencing(
                    "authority was acquired without a timeline identity".to_owned(),
                ));
            }
        };
        let provenance = match SimulationProvenance::from_input(
            provenance,
            scene.model(),
            quantum_ns,
            authority.execution_id(),
            &timeline_id,
        ) {
            Ok(provenance) => provenance,
            Err(error) => {
                authority.mark_application_lost();
                return Err(RemoteSceneError::Provenance(error));
            }
        };
        let generation = authority.generation();
        Ok(Self {
            scene,
            authority,
            provider,
            provenance,
            state,
            applied_actuation: None,
            generation,
        })
    }

    /// Immutable run provenance.
    #[must_use]
    pub fn provenance(&self) -> &SimulationProvenance {
        &self.provenance
    }

    /// Current public authority state.
    #[must_use]
    pub fn authority_state(&self) -> AuthorityState {
        self.authority.state()
    }

    /// Current reset-scoped generation fence.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Current native/remote completed boundary.
    #[must_use]
    pub fn boundary(&self) -> u64 {
        self.state.boundary()
    }

    /// Current reset-scoped timeline identity.
    #[must_use]
    pub fn timeline_id(&self) -> Option<&str> {
        self.authority.timeline_id()
    }

    /// Latest copied post-step native state.
    #[must_use]
    pub fn state(&self) -> &StateSnapshot {
        &self.state
    }

    /// The controls and typed actuation admitted at the latest step.
    #[must_use]
    pub fn applied_actuation(&self) -> Option<&AppliedActuation> {
        self.applied_actuation.as_ref()
    }

    /// Advance one complete robot/native boundary.
    pub async fn step(&mut self) -> Result<&StateSnapshot, RemoteSceneError<T::Error, P::Error>> {
        self.ensure_generation()?;
        let observations = self
            .provider
            .observations(self.scene.model(), &self.state, self.authority.quantum_ns())
            .map_err(RemoteSceneError::Provider)?;
        let response = self
            .authority
            .advance(observations)
            .await
            .map_err(RemoteSceneError::Authority)?;
        self.apply_response(response)
    }

    /// Recover a timed-out/uncertain boundary without re-encoding or replaying
    /// the observation cut under a new correlation identity.
    pub async fn retry_uncertain(
        &mut self,
    ) -> Result<&StateSnapshot, RemoteSceneError<T::Error, P::Error>> {
        self.ensure_generation()?;
        let response = self
            .authority
            .retry_uncertain()
            .await
            .map_err(RemoteSceneError::Authority)?;
        self.apply_response(response)
    }

    /// Renew the authority lease and inspect remote progress.
    pub async fn watchdog_tick(
        &mut self,
    ) -> Result<ProgressResponse, RemoteSceneError<T::Error, P::Error>> {
        self.ensure_generation()?;
        self.authority
            .watchdog_tick()
            .await
            .map_err(RemoteSceneError::Authority)
    }

    /// Reset both authority timeline and native state to boundary zero.
    pub async fn reset(&mut self) -> Result<&StateSnapshot, RemoteSceneError<T::Error, P::Error>> {
        self.ensure_generation()?;
        self.authority
            .reset()
            .await
            .map_err(RemoteSceneError::Authority)?;
        let state = match self.scene.reset() {
            Ok(state) => state,
            Err(error) => {
                self.authority.mark_application_lost();
                self.generation = self.authority.generation();
                return Err(RemoteSceneError::Native(error));
            }
        };
        if let Err(error) = self.provider.reset(self.scene.model(), &state) {
            self.authority.mark_application_lost();
            self.generation = self.authority.generation();
            return Err(RemoteSceneError::Provider(error));
        }
        self.state = state;
        self.applied_actuation = None;
        self.generation = self.authority.generation();
        Ok(&self.state)
    }

    /// Release authority after all native work is complete.
    pub async fn release(
        &mut self,
    ) -> Result<ReleaseAuthorityResponse, RemoteSceneError<T::Error, P::Error>> {
        self.ensure_generation()?;
        let response = self
            .authority
            .release()
            .await
            .map_err(RemoteSceneError::Authority)
            ?;
        self.generation = self.authority.generation();
        Ok(response)
    }

    /// Fail closed after the application loses ownership of native state.
    pub fn mark_application_lost(&mut self) {
        self.authority.mark_application_lost();
        self.generation = self.authority.generation();
    }

    fn apply_response(
        &mut self,
        response: AdvanceResponse,
    ) -> Result<&StateSnapshot, RemoteSceneError<T::Error, P::Error>> {
        if let Err(error) = validate_actuation_bindings(
            self.provider.actuation_bindings(),
            &response.actuation,
            self.state.boundary(),
            self.authority.quantum_ns(),
        ) {
            self.authority.mark_application_lost();
            self.generation = self.authority.generation();
            return Err(RemoteSceneError::Fencing(error.to_string()));
        }
        let controls = match self
            .provider
            .controls(self.scene.model(), &response.actuation)
        {
            Ok(controls) => controls,
            Err(error) => {
                self.authority.mark_application_lost();
                self.generation = self.authority.generation();
                return Err(RemoteSceneError::Provider(error));
            }
        };
        if let Err(error) = self.scene.set_controls(&controls) {
            self.authority.mark_application_lost();
            self.generation = self.authority.generation();
            return Err(RemoteSceneError::Native(error));
        }
        let native_step = match self.scene.step() {
            Ok(step) => step,
            Err(error) => {
                self.authority.mark_application_lost();
                self.generation = self.authority.generation();
                return Err(RemoteSceneError::Native(error));
            }
        };
        if native_step.end_boundary != self.authority.boundary()
            || native_step.state.model_identity() != self.scene.model().identity()
        {
            self.authority.mark_application_lost();
            self.generation = self.authority.generation();
            return Err(RemoteSceneError::Fencing(
                "native and remote completed boundaries or model identities diverged".to_owned(),
            ));
        }
        self.state = native_step.state;
        self.applied_actuation = Some(AppliedActuation {
            boundary: self.state.boundary(),
            requested: response.actuation,
            native_controls: controls.into_boxed_slice(),
        });
        Ok(&self.state)
    }

    fn ensure_generation(&self) -> Result<(), RemoteSceneError<T::Error, P::Error>> {
        if self.generation != self.authority.generation() {
            return Err(RemoteSceneError::Fencing(
                "native scene handle belongs to a stale authority generation".to_owned(),
            ));
        }
        Ok(())
    }
}

#[cfg(feature = "native")]
fn validate_actuation_bindings(
    bindings: &[ActuationBinding],
    actuation: &[Actuation],
    boundary: u64,
    quantum_ns: u64,
) -> Result<(), ActuationBindingError> {
    if bindings.is_empty() || actuation.len() != bindings.len() {
        return Err(ActuationBindingError::Incomplete {
            expected: bindings.len(),
            actual: actuation.len(),
        });
    }
    let expected_valid_until = boundary
        .checked_add(1)
        .and_then(|boundary| boundary.checked_mul(quantum_ns))
        .ok_or(ActuationBindingError::ValidityOverflow)?;
    let expected = bindings
        .iter()
        .map(|binding| (binding.service_instance.as_str(), binding.port.as_str()))
        .collect::<BTreeSet<_>>();
    let mut actual = BTreeSet::new();
    for item in actuation {
        if item.payload.is_empty() {
            return Err(ActuationBindingError::EmptyPayload {
                service_instance: item.service_instance.clone(),
                port: item.port.clone(),
            });
        }
        if item.valid_until_ns < expected_valid_until {
            return Err(ActuationBindingError::Expired {
                expected: expected_valid_until,
                actual: item.valid_until_ns,
            });
        }
        let key = (item.service_instance.as_str(), item.port.as_str());
        if !actual.insert(key) {
            return Err(ActuationBindingError::Duplicate {
                service_instance: item.service_instance.clone(),
                port: item.port.clone(),
            });
        }
        let Some(binding) = bindings.iter().find(|binding| {
            binding.service_instance == item.service_instance && binding.port == item.port
        }) else {
            return Err(ActuationBindingError::Unknown {
                service_instance: item.service_instance.clone(),
                port: item.port.clone(),
            });
        };
        if binding.payload_fqn.is_empty() {
            return Err(ActuationBindingError::InvalidPayloadFqn {
                service_instance: item.service_instance.clone(),
                port: item.port.clone(),
            });
        }
    }
    if actual != expected {
        return Err(ActuationBindingError::Incomplete {
            expected: bindings.len(),
            actual: actuation.len(),
        });
    }
    Ok(())
}

#[cfg(feature = "native")]
fn validate_binding_set(bindings: &[ActuationBinding]) -> Result<(), ActuationBindingError> {
    let mut ports = BTreeSet::new();
    let mut actuators = BTreeSet::new();
    for binding in bindings {
        if !ports.insert((binding.service_instance.as_str(), binding.port.as_str())) {
            return Err(ActuationBindingError::Duplicate {
                service_instance: binding.service_instance.clone(),
                port: binding.port.clone(),
            });
        }
        for actuator_id in &binding.actuator_ids {
            if !actuators.insert(actuator_id.as_str()) {
                return Err(ActuationBindingError::ActuatorMappedTwice {
                    actuator_id: actuator_id.clone(),
                });
            }
        }
    }
    Ok(())
}

#[cfg(feature = "native")]
fn validate_bindings_for_model(
    model: &Model,
    bindings: &[ActuationBinding],
) -> Result<(), ActuationBindingError> {
    validate_binding_set(bindings)?;
    let mut controls = BTreeSet::new();
    for binding in bindings {
        for actuator_id in &binding.actuator_ids {
            let handle = model
                .actuator(actuator_id)
                .map_err(|error| ActuationBindingError::NativeModel {
                    detail: error.to_string(),
                })?
                .ok_or_else(|| ActuationBindingError::UnknownNativeActuator {
                    actuator_id: actuator_id.clone(),
                })?;
            let info = model
                .actuator_info(handle)
                .map_err(|error| ActuationBindingError::NativeModel {
                    detail: error.to_string(),
                })?;
            if !controls.insert(info.control_index) {
                return Err(ActuationBindingError::ActuatorMappedTwice {
                    actuator_id: actuator_id.clone(),
                });
            }
        }
    }
    let expected = model.counts().controls;
    if controls.len() != expected {
        return Err(ActuationBindingError::MissingNativeControl {
            expected,
            actual: controls.len(),
        });
    }
    Ok(())
}

/// Failure while fencing returned actuation to the native scene contract.
#[cfg(feature = "native")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActuationBindingError {
    /// The actuation cut does not contain exactly all configured outputs.
    Incomplete { expected: usize, actual: usize },
    /// A port appears twice in one actuation cut.
    Duplicate {
        service_instance: String,
        port: String,
    },
    /// A port was not configured for this scene.
    Unknown {
        service_instance: String,
        port: String,
    },
    /// The payload has no bytes and therefore cannot carry typed actuation.
    EmptyPayload {
        service_instance: String,
        port: String,
    },
    /// A returned actuation expires before its target boundary.
    Expired { expected: u64, actual: u64 },
    /// Boundary-to-validity conversion overflowed.
    ValidityOverflow,
    /// A configured binding does not carry a payload identity.
    InvalidPayloadFqn {
        service_instance: String,
        port: String,
    },
    /// One native actuator is mapped by more than one output binding.
    ActuatorMappedTwice { actuator_id: String },
    /// A configured actuator name does not exist in the immutable model.
    UnknownNativeActuator { actuator_id: String },
    /// A model lookup failed while validating a configured native actuator.
    NativeModel { detail: String },
    /// The configured bindings do not cover every scalar native control.
    MissingNativeControl { expected: usize, actual: usize },
}

#[cfg(feature = "native")]
impl fmt::Display for ActuationBindingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Incomplete { expected, actual } => {
                write!(formatter, "actuation cut has {actual} items, expected {expected}")
            }
            Self::Duplicate {
                service_instance,
                port,
            } => write!(formatter, "actuation {service_instance}/{port} is duplicated"),
            Self::Unknown {
                service_instance,
                port,
            } => write!(formatter, "actuation {service_instance}/{port} is not configured"),
            Self::EmptyPayload {
                service_instance,
                port,
            } => write!(formatter, "actuation {service_instance}/{port} has an empty payload"),
            Self::Expired { expected, actual } => write!(
                formatter,
                "actuation valid_until_ns {actual} is before required {expected}"
            ),
            Self::ValidityOverflow => formatter.write_str("actuation validity time overflows nanoseconds"),
            Self::InvalidPayloadFqn {
                service_instance,
                port,
            } => write!(formatter, "actuation {service_instance}/{port} has no payload FQN"),
            Self::ActuatorMappedTwice { actuator_id } => {
                write!(formatter, "native actuator {actuator_id} is mapped more than once")
            }
            Self::UnknownNativeActuator { actuator_id } => {
                write!(formatter, "native model has no configured actuator {actuator_id}")
            }
            Self::NativeModel { detail } => {
                write!(formatter, "native model binding lookup failed: {detail}")
            }
            Self::MissingNativeControl { expected, actual } => write!(
                formatter,
                "actuation bindings cover {actual} native controls, expected {expected}"
            ),
        }
    }
}

#[cfg(feature = "native")]
impl std::error::Error for ActuationBindingError {}

/// Convert an exact native timestep into the wire's nanosecond quantum.
#[cfg(feature = "native")]
pub fn quantum_nanoseconds(quantum: PhysicsQuantum) -> Result<u64, QuantumError> {
    let nanos = quantum.as_seconds() * 1_000_000_000.0;
    if !nanos.is_finite() || nanos <= 0.0 || nanos > u64::MAX as f64 {
        return Err(QuantumError::OutOfRange {
            seconds: quantum.as_seconds(),
        });
    }
    let rounded = nanos.round();
    if (nanos - rounded).abs() > 1.0e-6 {
        return Err(QuantumError::NonIntegral {
            seconds: quantum.as_seconds(),
        });
    }
    Ok(rounded as u64)
}

/// Failure converting a native timestep into an exact wire quantum.
#[cfg(feature = "native")]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum QuantumError {
    /// The value is not finite, positive, or representable as `u64` nanoseconds.
    OutOfRange { seconds: f64 },
    /// The value is finite but not an integral nanosecond count.
    NonIntegral { seconds: f64 },
}

#[cfg(feature = "native")]
impl fmt::Display for QuantumError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfRange { seconds } => write!(formatter, "native quantum {seconds} is out of range"),
            Self::NonIntegral { seconds } => write!(formatter, "native quantum {seconds} is not integral nanoseconds"),
        }
    }
}

#[cfg(feature = "native")]
impl std::error::Error for QuantumError {}

/// Encode one model-backed kinematics joint into the exact public sample
/// payload selected by the provider set.
#[cfg(feature = "native")]
pub fn encode_joint_observation(
    providers: &ProviderSet,
    service_instance: &str,
    port: &str,
    joint_id: &str,
    model: &Model,
    state: &StateSnapshot,
    quantum_ns: u64,
) -> Result<Observation, NativeProviderError> {
    let requirement = providers.get(service_instance, port).ok_or_else(|| {
        NativeProviderError::InvalidPayload(format!(
            "no immutable provider requirement for {service_instance}/{port}"
        ))
    })?;
    let kind = PortKind::try_from(requirement.kind).map_err(|_| {
        NativeProviderError::InvalidPayload("provider kind is unknown".to_owned())
    })?;
    if kind != PortKind::Sample
        || requirement.payload_fqn != phoxal_kinematics::JointState::full_name()
    {
        return Err(NativeProviderError::InvalidPayload(
            "joint observation does not match the generated kinematics sample contract".to_owned(),
        ));
    }
    let handle = model.joint(joint_id)?.ok_or_else(|| {
        NativeProviderError::InvalidPayload(format!("model has no joint named {joint_id}"))
    })?;
    let info = model.joint_info(handle)?;
    let position_rad = state.qpos().get(info.qpos_offset).copied().ok_or_else(|| {
        NativeProviderError::InvalidPayload(format!("joint {joint_id} qpos offset is outside the state"))
    })?;
    let velocity_radps = state.qvel().get(info.dof_offset).copied().ok_or_else(|| {
        NativeProviderError::InvalidPayload(format!("joint {joint_id} qvel offset is outside the state"))
    })?;
    let message = phoxal_kinematics::JointState {
        joint_id: joint_id.to_owned(),
        position_rad,
        velocity_radps,
        effort_nm: None,
    };
    message
        .validate()
        .map_err(|error| NativeProviderError::InvalidPayload(error.to_string()))?;
    let capture_time_ns = state
        .boundary()
        .checked_mul(quantum_ns)
        .ok_or_else(|| NativeProviderError::InvalidPayload("capture time overflow".to_owned()))?;
    Ok(Observation {
        service_instance: service_instance.to_owned(),
        port: port.to_owned(),
        capture_time_ns,
        payload: message.encode_to_vec(),
    })
}

/// Encode one model-backed joint as the shared robotics encoder sample.
#[cfg(feature = "native")]
pub fn encode_encoder_observation(
    providers: &ProviderSet,
    service_instance: &str,
    port: &str,
    joint_id: &str,
    model: &Model,
    state: &StateSnapshot,
    quantum_ns: u64,
) -> Result<Observation, NativeProviderError> {
    let requirement = providers.get(service_instance, port).ok_or_else(|| {
        NativeProviderError::InvalidPayload(format!(
            "no immutable provider requirement for {service_instance}/{port}"
        ))
    })?;
    let kind = PortKind::try_from(requirement.kind).map_err(|_| {
        NativeProviderError::InvalidPayload("provider kind is unknown".to_owned())
    })?;
    if kind != PortKind::Sample
        || requirement.payload_fqn != phoxal_robotics::EncoderSample::full_name()
    {
        return Err(NativeProviderError::InvalidPayload(
            "encoder observation does not match the generated robotics sample contract".to_owned(),
        ));
    }
    let handle = model.joint(joint_id)?.ok_or_else(|| {
        NativeProviderError::InvalidPayload(format!("model has no joint named {joint_id}"))
    })?;
    let info = model.joint_info(handle)?;
    let sample = phoxal_robotics::EncoderSample {
        position_rad: Some(state.qpos().get(info.qpos_offset).copied().ok_or_else(|| {
            NativeProviderError::InvalidPayload(format!("joint {joint_id} qpos offset is outside the state"))
        })?),
        velocity_radps: Some(state.qvel().get(info.dof_offset).copied().ok_or_else(|| {
            NativeProviderError::InvalidPayload(format!("joint {joint_id} qvel offset is outside the state"))
        })?),
    };
    sample
        .validate()
        .map_err(|error| NativeProviderError::InvalidPayload(error.to_string()))?;
    let capture_time_ns = state
        .boundary()
        .checked_mul(quantum_ns)
        .ok_or_else(|| NativeProviderError::InvalidPayload("capture time overflow".to_owned()))?;
    Ok(Observation {
        service_instance: service_instance.to_owned(),
        port: port.to_owned(),
        capture_time_ns,
        payload: sample.encode_to_vec(),
    })
}

/// Decode exact motion actuator setpoints into the complete native control
/// vector.  Velocity targets are rejected because a generic MuJoCo motor has
/// no implicit velocity-servo semantics.
#[cfg(feature = "native")]
pub fn decode_motion_controls(
    model: &Model,
    actuation: &[Actuation],
    bindings: &[ActuationBinding],
) -> Result<Vec<f64>, NativeProviderError> {
    validate_bindings_for_model(model, bindings)
        .map_err(|error| NativeProviderError::InvalidActuation(error.to_string()))?;
    let expected_fqn = phoxal_motion::ActuatorSetpoint::full_name();
    let mut controls = vec![0.0; model.counts().controls];
    let mut covered = BTreeSet::new();
    for binding in bindings {
        if binding.payload_fqn != expected_fqn {
            return Err(NativeProviderError::InvalidActuation(format!(
                "{} / {} uses payload {}, expected {expected_fqn}",
                binding.service_instance, binding.port, binding.payload_fqn
            )));
        }
        let item = actuation.iter().find(|item| {
            item.service_instance == binding.service_instance && item.port == binding.port
        }).ok_or_else(|| {
            NativeProviderError::InvalidActuation(format!(
                "missing actuation {}/{}",
                binding.service_instance, binding.port
            ))
        })?;
        let setpoint = phoxal_motion::ActuatorSetpoint::decode(item.payload.as_slice())?;
        setpoint
            .validate_for(binding.actuator_ids.iter().map(String::as_str))
            .map_err(|error| NativeProviderError::InvalidActuation(error.to_string()))?;
        for target in setpoint.targets {
            let handle = model.actuator(&target.actuator_id)?.ok_or_else(|| {
                NativeProviderError::InvalidActuation(format!(
                    "model has no actuator named {}",
                    target.actuator_id
                ))
            })?;
            let info = model.actuator_info(handle)?;
            if !covered.insert(info.control_index) {
                return Err(NativeProviderError::InvalidActuation(format!(
                    "native control index {} is mapped more than once",
                    info.control_index
                )));
            }
            let Some(control) = target.control else {
                return Err(NativeProviderError::InvalidActuation(format!(
                    "actuator {} has no control selection",
                    target.actuator_id
                )));
            };
            match control {
                phoxal_motion::actuator_target::Control::TorqueNm(value) => {
                    if !value.is_finite() {
                        return Err(NativeProviderError::InvalidActuation(format!(
                            "torque for actuator {} is not finite",
                            target.actuator_id
                        )));
                    }
                    if let Some([lower, upper]) = model.control_range(info.control_index)?
                        && (value < lower || value > upper)
                    {
                        return Err(NativeProviderError::InvalidActuation(format!(
                            "torque for actuator {} is outside [{lower}, {upper}]",
                            target.actuator_id
                        )));
                    }
                    controls[info.control_index] = value;
                }
                phoxal_motion::actuator_target::Control::VelocityRadps(_) => {
                    return Err(NativeProviderError::InvalidActuation(
                        "velocity setpoints require an explicit native servo binding".to_owned(),
                    ));
                }
            }
        }
    }
    if covered.len() != controls.len() {
        return Err(NativeProviderError::InvalidActuation(format!(
            "actuation bindings cover {} native controls, expected {}",
            covered.len(),
            controls.len()
        )));
    }
    Ok(controls)
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoxal::communication::simulation::ProductReceipt;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Debug)]
    struct FakeError(String);

    impl fmt::Display for FakeError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(&self.0)
        }
    }

    #[derive(Clone)]
    struct FakeTransport {
        state: Arc<Mutex<FakeState>>,
    }

    struct FakeState {
        acquired: bool,
        hardware: bool,
        uncertain_once: bool,
        session_id: Vec<u8>,
        grant: Vec<u8>,
        model_identity: String,
        quantum_ns: u64,
        timeline_id: String,
        boundary: u64,
        reset_count: u64,
        retained: BTreeMap<Vec<u8>, AdvanceResponse>,
        advance_correlations: Vec<Vec<u8>>,
        progress_calls: usize,
    }

    impl FakeTransport {
        fn new() -> Self {
            Self {
                state: Arc::new(Mutex::new(FakeState {
                    acquired: false,
                    hardware: false,
                    uncertain_once: false,
                    session_id: vec![1, 2, 3],
                    grant: vec![7; AUTHORITY_GRANT_BYTES],
                    model_identity: String::new(),
                    quantum_ns: 0,
                    timeline_id: "timeline-1".to_owned(),
                    boundary: 0,
                    reset_count: 0,
                    retained: BTreeMap::new(),
                    advance_correlations: Vec::new(),
                    progress_calls: 0,
                })),
            }
        }

        fn set_hardware(&self) {
            self.state.lock().expect("fake lock").hardware = true;
        }

        fn set_uncertain_once(&self) {
            self.state.lock().expect("fake lock").uncertain_once = true;
        }

        fn state(&self) -> std::sync::MutexGuard<'_, FakeState> {
            self.state.lock().expect("fake lock")
        }
    }

    impl SimulationTransport for FakeTransport {
        type Error = FakeError;

        fn acquire_authority<'a>(
            &'a self,
            request: AcquireAuthorityRequest,
        ) -> SimulationFuture<'a, AcquireAuthorityResponse, Self::Error> {
            let state = self.state.clone();
            Box::pin(async move {
                let mut state = state.lock().expect("fake lock");
                if state.hardware {
                    return Err(FakeError("hardware execution refuses simulation authority".to_owned()));
                }
                if state.acquired {
                    return Err(FakeError("simulation authority is already held".to_owned()));
                }
                state.acquired = true;
                state.model_identity = request.model_identity.clone();
                state.quantum_ns = request.quantum_ns;
                Ok(AcquireAuthorityResponse {
                    authority_grant: state.grant.clone(),
                    timeline_id: state.timeline_id.clone(),
                    boundary: state.boundary,
                    lease_ms: 1_000,
                    session_id: state.session_id.clone(),
                    execution_id: request.execution_id,
                    model_identity: request.model_identity,
                    quantum_ns: request.quantum_ns,
                    correlation_id: request.correlation_id,
                })
            })
        }

        fn advance<'a>(
            &'a self,
            request: AdvanceRequest,
        ) -> SimulationFuture<'a, AdvanceResponse, Self::Error> {
            let state = self.state.clone();
            Box::pin(async move {
                let mut state = state.lock().expect("fake lock");
                if !state.acquired {
                    return Err(FakeError("authority is not held".to_owned()));
                }
                state.advance_correlations.push(request.correlation_id.clone());
                if let Some(response) = state.retained.get(&request.correlation_id) {
                    return Ok(response.clone());
                }
                if request.boundary != state.boundary {
                    return Err(FakeError("boundary mismatch".to_owned()));
                }
                let completed_boundary = request
                    .boundary
                    .checked_add(1)
                    .ok_or_else(|| FakeError("boundary overflow".to_owned()))?;
                let response = AdvanceResponse {
                    completed_boundary,
                    actuation: Vec::new(),
                    observation_receipts: request
                        .observations
                        .iter()
                        .map(|observation| ProductReceipt {
                            service_instance: observation.service_instance.clone(),
                            port: observation.port.clone(),
                            sequence: 1,
                            boundary: request.boundary,
                            correlation_id: request.correlation_id.clone(),
                        })
                        .collect(),
                    session_id: state.session_id.clone(),
                    execution_id: request.execution_id.clone(),
                    timeline_id: request.timeline_id.clone(),
                    requested_boundary: request.boundary,
                    correlation_id: request.correlation_id.clone(),
                    authority_grant: request.authority_grant.clone(),
                };
                state.boundary = completed_boundary;
                state
                    .retained
                    .insert(request.correlation_id.clone(), response.clone());
                if state.uncertain_once {
                    state.uncertain_once = false;
                    return Err(FakeError("delayed response".to_owned()));
                }
                Ok(response)
            })
        }

        fn reset<'a>(
            &'a self,
            request: ResetRequest,
        ) -> SimulationFuture<'a, ResetResponse, Self::Error> {
            let state = self.state.clone();
            Box::pin(async move {
                let mut state = state.lock().expect("fake lock");
                if request.completed_boundary != state.boundary {
                    return Err(FakeError("reset boundary mismatch".to_owned()));
                }
                state.reset_count += 1;
                state.timeline_id = format!("timeline-{}", state.reset_count + 1);
                state.boundary = 0;
                Ok(ResetResponse {
                    next_timeline_id: state.timeline_id.clone(),
                    boundary: 0,
                    session_id: state.session_id.clone(),
                    execution_id: "execution-1".to_owned(),
                    authority_grant: request.authority_grant,
                    correlation_id: request.correlation_id,
                    previous_timeline_id: request.timeline_id,
                    requested_boundary: request.completed_boundary,
                })
            })
        }

        fn release_authority<'a>(
            &'a self,
            request: ReleaseAuthorityRequest,
        ) -> SimulationFuture<'a, ReleaseAuthorityResponse, Self::Error> {
            let state = self.state.clone();
            Box::pin(async move {
                let mut state = state.lock().expect("fake lock");
                state.acquired = false;
                Ok(ReleaseAuthorityResponse {
                    session_id: state.session_id.clone(),
                    authority_grant: request.authority_grant,
                    correlation_id: request.correlation_id,
                    execution_id: "execution-1".to_owned(),
                    timeline_id: state.timeline_id.clone(),
                    completed_boundary: state.boundary,
                })
            })
        }

        fn progress<'a>(
            &'a self,
            request: ProgressRequest,
        ) -> SimulationFuture<'a, ProgressResponse, Self::Error> {
            let state = self.state.clone();
            Box::pin(async move {
                let mut state = state.lock().expect("fake lock");
                state.progress_calls += 1;
                Ok(ProgressResponse {
                    execution_id: "execution-1".to_owned(),
                    timeline_id: state.timeline_id.clone(),
                    completed_boundary: state.boundary,
                    failed: false,
                    detail: None,
                    session_id: state.session_id.clone(),
                    authority_grant: request.authority_grant,
                    correlation_id: request.correlation_id,
                })
            })
        }
    }

    fn provider_set() -> ProviderSet {
        ProviderSet::new(vec![phoxal::communication::simulation::ProviderRequirement {
            service_instance: "kinematics".to_owned(),
            port: "joints".to_owned(),
            payload_fqn: "phoxal.kinematics.v1.JointState".to_owned(),
            kind: PortKind::Sample as i32,
            input_fqn: "google.protobuf.Empty".to_owned(),
        }])
        .expect("provider set")
    }

    fn observations(boundary: u64) -> Vec<Observation> {
        vec![Observation {
            service_instance: "kinematics".to_owned(),
            port: "joints".to_owned(),
            capture_time_ns: boundary * 1_000,
            payload: vec![1, 2, 3],
        }]
    }

    fn client(transport: FakeTransport) -> AuthorityClient<FakeTransport> {
        AuthorityClient::new(
            transport,
            "execution-1",
            "model-1",
            1_000,
            provider_set(),
        )
        .expect("authority client")
    }

    #[tokio::test]
    async fn authority_is_exclusive_until_release() {
        let transport = FakeTransport::new();
        let mut first = client(transport.clone());
        first.acquire().await.expect("first authority");
        let mut second = client(transport.clone());
        let error = second.acquire().await.expect_err("exclusive authority");
        assert!(error.to_string().contains("already held"));
        first.release().await.expect("release");
        second.acquire().await.expect("authority after release");
    }

    #[tokio::test]
    async fn hardware_refusal_is_not_reinterpreted_as_local_simulation() {
        let transport = FakeTransport::new();
        transport.set_hardware();
        let mut client = client(transport);
        let error = client.acquire().await.expect_err("hardware refusal");
        assert!(error.to_string().contains("hardware execution"));
        assert_eq!(client.state(), AuthorityState::Disconnected);
    }

    #[tokio::test]
    async fn provider_set_rejects_duplicate_and_mismatched_observations() {
        let duplicate = ProviderSet::new(vec![
            phoxal::communication::simulation::ProviderRequirement {
                service_instance: "kinematics".to_owned(),
                port: "joints".to_owned(),
                payload_fqn: "phoxal.kinematics.v1.JointState".to_owned(),
                kind: PortKind::Sample as i32,
                input_fqn: "google.protobuf.Empty".to_owned(),
            },
            phoxal::communication::simulation::ProviderRequirement {
                service_instance: "kinematics".to_owned(),
                port: "joints".to_owned(),
                payload_fqn: "phoxal.kinematics.v1.JointState".to_owned(),
                kind: PortKind::Sample as i32,
                input_fqn: "google.protobuf.Empty".to_owned(),
            },
        ])
        .expect_err("duplicate provider");
        assert!(matches!(duplicate, ProviderSetError::Duplicate { .. }));

        let transport = FakeTransport::new();
        let mut client = client(transport);
        client.acquire().await.expect("authority");
        let mut wrong = observations(0);
        wrong[0].port = "unknown".to_owned();
        let error = client.advance(wrong).await.expect_err("unknown provider");
        assert!(matches!(error, AuthorityClientError::ProviderSet(ProviderSetError::Unknown { .. })));
    }

    #[tokio::test]
    async fn delayed_result_recovers_from_authoritative_progress_without_replay() {
        let transport = FakeTransport::new();
        transport.set_uncertain_once();
        let mut client = client(transport.clone());
        client.acquire().await.expect("authority");
        let error = client.advance(observations(0)).await.expect_err("delayed result");
        assert!(matches!(error, AuthorityClientError::UncertainAdvance { .. }));
        assert_eq!(client.boundary(), 0);
        let response = client.retry_uncertain().await.expect("recover delayed result");
        assert_eq!(response.completed_boundary, 1);
        assert_eq!(client.boundary(), 1);
        let state = transport.state();
        assert_eq!(state.advance_correlations.len(), 2);
        assert_eq!(
            state.advance_correlations[0],
            state.advance_correlations[1],
            "recovery must use the retained correlation, not blind replay"
        );
        assert_eq!(state.retained.len(), 1);
        assert_eq!(state.progress_calls, 1);
    }

    #[tokio::test]
    async fn reset_rotates_timeline_and_application_loss_fences_followups() {
        let transport = FakeTransport::new();
        let mut client = client(transport);
        client.acquire().await.expect("authority");
        let generation = client.generation();
        let response = client.reset().await.expect("reset");
        assert_eq!(response.boundary, 0);
        assert_ne!(client.timeline_id(), Some("timeline-1"));
        assert!(client.generation() > generation);
        client.mark_application_lost();
        assert_eq!(client.state(), AuthorityState::Lost);
        assert!(client.authority_grant().is_none());
        let error = client.watchdog_tick().await.expect_err("lost application");
        assert!(matches!(
            error,
            AuthorityClientError::InvalidState {
                state: AuthorityState::Lost,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn every_intermediate_finite_step_has_one_monotonic_boundary() {
        let transport = FakeTransport::new();
        let mut client = client(transport.clone());
        client.acquire().await.expect("authority");
        for boundary in 0..4 {
            let response = client
                .advance(observations(boundary))
                .await
                .expect("finite step");
            assert_eq!(response.completed_boundary, boundary + 1);
            assert_eq!(client.boundary(), boundary + 1);
        }
        assert_eq!(transport.state().advance_correlations.len(), 4);
    }
}
