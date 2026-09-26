//! Typed external access to one or more deployed Phoxal supervisors.
//!
//! This module is the public client facade over the generated
//! `phoxal.session.v1` Protobuf exchange.  It owns one physical Zenoh
//! transport, opens one logical session per selected supervisor, and carries
//! execution, timeline, binding, and correlation identity through every
//! operation.  Service payloads remain the generated owner's `prost` types.

use std::fmt;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use prost::Message;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::communication::session::{
    BindMethodRequest, ExecutionSummary, ListExecutionsRequest, ListMethodsRequest, MethodMetadata,
    MethodShape as SessionMethodShape, OperationOutcome, OperationRequest, RecordKind,
    SubscriptionRecord, SubscriptionRequest, SupervisorInfoResponse, SupervisorStatusResponse,
};
use crate::communication::simulation::{
    AcquireAuthorityRequest, AcquireAuthorityResponse, AdmitInitialObservationsRequest,
    AdmitInitialObservationsResponse, AdmitObservationsRequest, AdmitObservationsResponse,
    PrepareBoundaryRequest, PrepareBoundaryResponse, ProgressRequest, ProgressResponse,
    ReleaseAuthorityRequest, ReleaseAuthorityResponse, ResetRequest, ResetResponse, TransitionKey,
};
use crate::communication::{DeploymentTarget, PublicOperation};
use crate::communication_transport::{
    PublicSessionConnection, PublicSessionTransport, PublicSubscription, PublicTlsCredentials,
    PublicTransportError, PublicTransportLimits, PublicTransportSecurity, SupervisorWatch,
};
use crate::contract::{
    CallMethod, MethodDescriptor, MethodShape, MethodSignature, ObservationMethod,
};
use crate::session::error::SessionError;

const MAX_SIMULATION_PRODUCT_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SIMULATION_CUT_BYTES: u64 = 8 * 1024 * 1024;

/// Configuration for one shared client connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectionConfig {
    endpoint: String,
    scope: String,
    principal: String,
    limits: PublicTransportLimits,
    security: PublicTransportSecurity,
}

impl ConnectionConfig {
    /// Construct a client connection from one router endpoint, scope, and
    /// trusted principal route.
    pub fn new(
        endpoint: impl Into<String>,
        scope: impl Into<String>,
        principal: impl Into<String>,
    ) -> Result<Self, PublicTransportError> {
        let endpoint = endpoint.into();
        let scope = scope.into();
        let principal = principal.into();
        let validation_target = DeploymentTarget::new(&scope, "validation")?;
        let _ = crate::communication::PublicRoute::for_operation(
            &validation_target,
            &principal,
            PublicOperation::Open,
        )
        .map_err(|error| PublicTransportError::Malformed {
            operation: "connect".to_owned(),
            detail: error.to_string(),
        })?;
        if endpoint.is_empty() {
            return Err(PublicTransportError::Malformed {
                operation: "connect".to_owned(),
                detail: "router endpoint is empty".to_owned(),
            });
        }
        Ok(Self {
            endpoint,
            scope,
            principal,
            limits: PublicTransportLimits::default(),
            security: PublicTransportSecurity::default(),
        })
    }

    /// Replace the default finite exchange bounds.
    pub fn with_limits(
        mut self,
        limits: PublicTransportLimits,
    ) -> Result<Self, PublicTransportError> {
        // The transport validates the same values again, keeping a config
        // constructed without an async connection safe to retain.
        let _ = PublicTransportLimits::new(
            limits.max_request_bytes(),
            limits.max_response_bytes(),
            limits.max_reply_count(),
            limits.query_capacity(),
            limits.deadline(),
        )?;
        self.limits = limits;
        Ok(self)
    }

    /// Select plaintext for an explicitly local fixture or mutual TLS for a
    /// routed deployment.
    #[must_use]
    pub fn with_security(mut self, security: PublicTransportSecurity) -> Self {
        self.security = security;
        self
    }

    /// Convenience constructor for file-backed mutual-TLS credentials.
    pub fn with_tls(self, credentials: PublicTlsCredentials) -> Self {
        self.with_security(PublicTransportSecurity::Tls(credentials))
    }

    /// Router endpoint.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Authorized deployment scope.
    #[must_use]
    pub fn scope(&self) -> &str {
        &self.scope
    }

    /// Principal segment assigned by the trusted router.
    #[must_use]
    pub fn principal(&self) -> &str {
        &self.principal
    }

    /// Bounded exchange limits.
    #[must_use]
    pub const fn limits(&self) -> &PublicTransportLimits {
        &self.limits
    }

    /// Selected transport security profile.
    #[must_use]
    pub fn security(&self) -> &PublicTransportSecurity {
        &self.security
    }
}

/// Open one shared physical Zenoh connection.
pub async fn connect(config: ConnectionConfig) -> Result<Connection, SessionError> {
    let transport = PublicSessionTransport::connect_with_security(
        config.endpoint.clone(),
        config.principal.clone(),
        config.security.clone(),
        config.limits.clone(),
    )
    .await?;
    Ok(Connection {
        inner: Arc::new(ConnectionInner {
            transport: Arc::new(transport),
            scope: config.scope,
            principal: config.principal,
            supervisors: Mutex::new(Vec::new()),
            closed: AtomicBool::new(false),
        }),
    })
}

/// One shared physical connection carrying independent logical sessions.
pub struct Connection {
    inner: Arc<ConnectionInner>,
}

struct ConnectionInner {
    transport: Arc<PublicSessionTransport>,
    scope: String,
    principal: String,
    supervisors: Mutex<Vec<Weak<SupervisorInner>>>,
    closed: AtomicBool,
}

impl fmt::Debug for Connection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Connection")
            .field("scope", &self.inner.scope)
            .field("principal", &self.inner.principal)
            .finish_non_exhaustive()
    }
}

impl Connection {
    /// Authorized scope carried by this connection.
    #[must_use]
    pub fn scope(&self) -> &str {
        &self.inner.scope
    }

    /// Discover currently advertised supervisors using the default bound.
    pub async fn discover(&self) -> Result<Vec<DeploymentTarget>, SessionError> {
        self.inner
            .transport
            .discover(&self.inner.scope)
            .await
            .map_err(Into::into)
    }

    /// Discover a scope with an explicit finite target bound.
    pub async fn discover_bounded(
        &self,
        maximum: usize,
    ) -> Result<Vec<DeploymentTarget>, SessionError> {
        self.inner
            .transport
            .discover_bounded(&self.inner.scope, maximum)
            .await
            .map_err(Into::into)
    }

    /// Start the race-safe bounded supervisor inventory watch.
    pub async fn watch(&self) -> Result<SupervisorWatch, SessionError> {
        self.inner
            .transport
            .watch(&self.inner.scope)
            .await
            .map_err(Into::into)
    }

    /// Start a watch with explicit target and change bounds.
    pub async fn watch_bounded(
        &self,
        maximum: usize,
        change_capacity: usize,
    ) -> Result<SupervisorWatch, SessionError> {
        self.inner
            .transport
            .watch_bounded(&self.inner.scope, maximum, change_capacity)
            .await
            .map_err(Into::into)
    }

    /// Select and open one supervisor session over the shared physical
    /// connection.
    pub async fn supervisor(
        &self,
        supervisor_id: impl AsRef<str>,
    ) -> Result<Supervisor, SessionError> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(SessionError::StaleHandle {
                resource: "connection",
            });
        }
        let target = DeploymentTarget::new(&self.inner.scope, supervisor_id.as_ref())
            .map_err(PublicTransportError::Bootstrap)?;
        let connection = self.inner.transport.open(target.clone()).await?;
        let session_id = connection.session_id().to_vec();
        let supervisor = Arc::new(SupervisorInner {
            transport: self.inner.transport.clone(),
            target,
            session: Mutex::new(Some(connection)),
            session_id: Mutex::new(session_id),
            generation: AtomicU64::new(1),
            correlation: AtomicU64::new(1),
            closed: AtomicBool::new(false),
            renewal: Mutex::new(None),
            lifecycle: Mutex::new(CancellationToken::new()),
        });
        supervisor
            .renewal
            .lock()
            .await
            .replace(spawn_renewal(&supervisor, 1));
        self.inner
            .supervisors
            .lock()
            .await
            .push(Arc::downgrade(&supervisor));
        Ok(Supervisor { inner: supervisor })
    }

    /// Close every logical session and then the one shared physical transport.
    pub async fn close(self) -> Result<(), SessionError> {
        self.inner.closed.store(true, Ordering::Release);
        let supervisors = std::mem::take(&mut *self.inner.supervisors.lock().await);
        let mut first_error = None;
        for supervisor in supervisors.into_iter().filter_map(|weak| weak.upgrade()) {
            if let Err(error) = supervisor.close_inner().await {
                first_error.get_or_insert(error);
            }
        }
        if let Err(error) = self.inner.transport.shutdown().await {
            first_error.get_or_insert(SessionError::Public(error));
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

/// A selected supervisor and its current logical session.
#[derive(Clone)]
pub struct Supervisor {
    inner: Arc<SupervisorInner>,
}

#[derive(Debug)]
struct SupervisorInner {
    transport: Arc<PublicSessionTransport>,
    target: DeploymentTarget,
    session: Mutex<Option<PublicSessionConnection>>,
    session_id: Mutex<Vec<u8>>,
    generation: AtomicU64,
    correlation: AtomicU64,
    closed: AtomicBool,
    renewal: Mutex<Option<JoinHandle<()>>>,
    lifecycle: Mutex<CancellationToken>,
}

impl fmt::Debug for Supervisor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Supervisor")
            .field("target", &self.inner.target)
            .finish_non_exhaustive()
    }
}

impl Supervisor {
    /// Selected deployment target.
    #[must_use]
    pub fn target(&self) -> &DeploymentTarget {
        &self.inner.target
    }

    /// Information fetched during exact opening of the current logical
    /// session.
    ///
    /// Information is owned by the active transport connection, not by this
    /// reconnectable facade.  A reconnect therefore exposes the new session's
    /// initial response, while a closed or lost session cannot return a stale
    /// cached value.
    pub async fn info(&self) -> Result<SupervisorInfoResponse, SessionError> {
        self.inner.info().await
    }

    /// Management/status handles remain usable independently of execution
    /// readiness.
    #[must_use]
    pub fn management(&self) -> Management {
        Management {
            inner: self.inner.clone(),
        }
    }

    /// Access the dedicated simulation authority and boundary lane.
    ///
    /// The returned handle carries this supervisor's current logical-session
    /// generation. Reconnect or close makes it stale, and every request is
    /// stamped with the active session identifier before transport.
    #[must_use]
    pub fn simulation(&self) -> Simulation {
        Simulation {
            inner: self.inner.clone(),
            generation: self.inner.generation.load(Ordering::Acquire),
        }
    }

    /// Explicitly select one execution by its exact execution identity.
    pub async fn execution(
        &self,
        execution_id: impl AsRef<str>,
    ) -> Result<Execution, SessionError> {
        let execution_id = execution_id.as_ref();
        let session_id = self.inner.session_id().await?;
        let response = self
            .inner
            .list_executions(ListExecutionsRequest {
                page_size: 0,
                page_token: Vec::new(),
                session_id,
            })
            .await?;
        let summary = response
            .executions
            .into_iter()
            .find(|candidate| candidate.execution_id == execution_id)
            .ok_or_else(|| SessionError::InvalidPublicRequest {
                detail: format!("execution `{execution_id}` is not advertised"),
            })?;
        Ok(Execution {
            inner: self.inner.clone(),
            summary,
            generation: self.inner.generation.load(Ordering::Acquire),
        })
    }

    /// Reopen a fresh logical session after supervisor restart or transport
    /// loss.  Existing execution and port handles retain their old generation
    /// and fail as stale handles.
    pub async fn reconnect(&self) -> Result<(), SessionError> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(SessionError::StaleHandle {
                resource: "supervisor",
            });
        }
        self.inner.stop_renewal().await;
        self.inner.cancel_lifecycle().await;

        // A reconnect is a replacement boundary, not a best-effort refresh of
        // the old session.  Retire the old session before opening its
        // replacement so a failed open cannot leave a handle that appears
        // current while the supervisor is unavailable.
        let old = self.inner.session.lock().await.take();
        self.inner.session_id.lock().await.clear();
        if old.is_some() {
            self.inner.generation.fetch_add(1, Ordering::AcqRel);
        }
        let connection = match self.inner.transport.open(self.inner.target.clone()).await {
            Ok(connection) => connection,
            Err(error) => {
                if let Some(old) = old {
                    let _ = old.close().await;
                }
                return Err(error.into());
            }
        };
        let session_id = connection.session_id().to_vec();
        self.inner.session.lock().await.replace(connection);
        *self.inner.session_id.lock().await = session_id;
        let generation = self.inner.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.inner.correlation.store(1, Ordering::Release);
        self.inner.replace_lifecycle().await;
        self.inner
            .renewal
            .lock()
            .await
            .replace(spawn_renewal(&self.inner, generation));
        if let Some(old) = old {
            let _ = old.close().await;
        }
        Ok(())
    }

    /// Close only this supervisor's logical session.
    pub async fn close(&self) -> Result<(), SessionError> {
        self.inner.close_inner().await
    }
}

/// Supervisor lifecycle and execution inspection operations.
#[derive(Clone)]
pub struct Management {
    inner: Arc<SupervisorInner>,
}

/// Simulation authority and completed-boundary operations for one supervisor.
#[derive(Clone, Debug)]
pub struct Simulation {
    inner: Arc<SupervisorInner>,
    generation: u64,
}

impl Simulation {
    /// Acquire exclusive authority for one simulation-configured execution.
    pub async fn acquire_authority(
        &self,
        mut request: AcquireAuthorityRequest,
    ) -> Result<AcquireAuthorityResponse, SessionError> {
        self.ensure_current()?;
        request.session_id = self.inner.session_id().await?;
        ensure_simulation_correlation(&mut request.correlation_id, &self.inner)?;
        let response = self
            .inner
            .simulation::<AcquireAuthorityRequest, AcquireAuthorityResponse>(
                PublicOperation::AcquireAuthority,
                request.clone(),
            )
            .await?;
        if response.session_id != request.session_id
            || response.authority_grant.len() != 32
            || response.timeline_id.is_empty()
            || response.lease_ms == 0
            || response.execution_id != request.execution_id
            || response.model_identity != request.model_identity
            || response.quantum_ns != request.quantum_ns
            || response.correlation_id != request.correlation_id
            || response.boundary != 0
            || response.max_product_bytes == 0
            || response.max_product_bytes > MAX_SIMULATION_PRODUCT_BYTES
            || response.max_cut_bytes < response.max_product_bytes
            || response.max_cut_bytes > MAX_SIMULATION_CUT_BYTES
            || response.max_product_bytes
                != if request.max_product_bytes == 0 {
                    MAX_SIMULATION_PRODUCT_BYTES
                } else {
                    request.max_product_bytes
                }
            || response.max_cut_bytes
                != if request.max_cut_bytes == 0 {
                    MAX_SIMULATION_CUT_BYTES
                } else {
                    request.max_cut_bytes
                }
        {
            return Err(SessionError::InvalidPublicRequest {
                detail: "simulation authority response does not match its session".to_owned(),
            });
        }
        Ok(response)
    }

    /// Admit the complete boundary-zero observation cut without invoking any
    /// Runtime service.
    pub async fn admit_initial_observations(
        &self,
        mut request: AdmitInitialObservationsRequest,
    ) -> Result<AdmitInitialObservationsResponse, SessionError> {
        self.ensure_current()?;
        let session_id = self.inner.session_id().await?;
        ensure_simulation_transition(&mut request.transition_key, &session_id)?;
        ensure_simulation_correlation(&mut request.correlation_id, &self.inner)?;
        let response = self
            .inner
            .simulation::<AdmitInitialObservationsRequest, AdmitInitialObservationsResponse>(
                PublicOperation::AdmitInitialObservations,
                request.clone(),
            )
            .await?;
        validate_simulation_receipt(
            response.receipt.as_ref(),
            request.transition_key.as_ref(),
            &request.correlation_id,
            crate::communication::simulation::PhaseStatus::InitialAdmitted,
        )?;
        Ok(response)
    }

    /// Prepare one boundary from its already admitted observation cut and
    /// receive the immutable actuator cut for the following native step.
    pub async fn prepare_boundary(
        &self,
        mut request: PrepareBoundaryRequest,
    ) -> Result<PrepareBoundaryResponse, SessionError> {
        self.ensure_current()?;
        let session_id = self.inner.session_id().await?;
        ensure_simulation_transition(&mut request.transition_key, &session_id)?;
        ensure_simulation_correlation(&mut request.correlation_id, &self.inner)?;
        let response = self
            .inner
            .simulation::<PrepareBoundaryRequest, PrepareBoundaryResponse>(
                PublicOperation::PrepareBoundary,
                request.clone(),
            )
            .await?;
        validate_simulation_receipt(
            response.receipt.as_ref(),
            request.transition_key.as_ref(),
            &request.correlation_id,
            crate::communication::simulation::PhaseStatus::Prepared,
        )?;
        Ok(response)
    }

    /// Admit the complete observation cut captured after the native step.
    /// This phase never invokes the next Runtime service wave.
    pub async fn admit_observations(
        &self,
        mut request: AdmitObservationsRequest,
    ) -> Result<AdmitObservationsResponse, SessionError> {
        self.ensure_current()?;
        let session_id = self.inner.session_id().await?;
        ensure_simulation_transition(&mut request.transition_key, &session_id)?;
        ensure_simulation_correlation(&mut request.correlation_id, &self.inner)?;
        let response = self
            .inner
            .simulation::<AdmitObservationsRequest, AdmitObservationsResponse>(
                PublicOperation::AdmitObservations,
                request.clone(),
            )
            .await?;
        validate_simulation_receipt(
            response.receipt.as_ref(),
            request.transition_key.as_ref(),
            &request.correlation_id,
            crate::communication::simulation::PhaseStatus::ObservationsAdmitted,
        )?;
        Ok(response)
    }

    /// Reset from the current completed boundary and receive the new timeline.
    pub async fn reset(&self, mut request: ResetRequest) -> Result<ResetResponse, SessionError> {
        self.ensure_current()?;
        request.session_id = self.inner.session_id().await?;
        ensure_simulation_correlation(&mut request.correlation_id, &self.inner)?;
        let response = self
            .inner
            .simulation::<ResetRequest, ResetResponse>(PublicOperation::Reset, request.clone())
            .await?;
        if response.session_id != request.session_id
            || response.next_timeline_id.is_empty()
            || response.execution_id != request.execution_id
            || response.previous_timeline_id != request.timeline_id
            || response.requested_boundary != request.completed_boundary
            || response.authority_grant.is_empty()
            || response.authority_grant == request.authority_grant
            || response.correlation_id != request.correlation_id
        {
            return Err(SessionError::InvalidPublicRequest {
                detail: "simulation reset response does not match its session".to_owned(),
            });
        }
        Ok(response)
    }

    /// Release the current exclusive authority.
    pub async fn release_authority(
        &self,
        mut request: ReleaseAuthorityRequest,
    ) -> Result<ReleaseAuthorityResponse, SessionError> {
        self.ensure_current()?;
        request.session_id = self.inner.session_id().await?;
        ensure_simulation_correlation(&mut request.correlation_id, &self.inner)?;
        let response = self
            .inner
            .simulation::<ReleaseAuthorityRequest, ReleaseAuthorityResponse>(
                PublicOperation::ReleaseAuthority,
                request.clone(),
            )
            .await?;
        if response.session_id != request.session_id
            || response.authority_grant != request.authority_grant
            || response.correlation_id != request.correlation_id
            || response.execution_id.is_empty()
            || response.timeline_id.is_empty()
        {
            return Err(SessionError::InvalidPublicRequest {
                detail: "simulation release response does not match its session".to_owned(),
            });
        }
        Ok(response)
    }

    /// Query authoritative progress after a timeout or uncertain advance.
    pub async fn progress(
        &self,
        mut request: ProgressRequest,
    ) -> Result<ProgressResponse, SessionError> {
        self.ensure_current()?;
        request.session_id = self.inner.session_id().await?;
        ensure_simulation_correlation(&mut request.correlation_id, &self.inner)?;
        let response = self
            .inner
            .simulation::<ProgressRequest, ProgressResponse>(
                PublicOperation::Progress,
                request.clone(),
            )
            .await?;
        if response.session_id != request.session_id
            || response.execution_id.is_empty()
            || response.timeline_id.is_empty()
            || response.authority_grant != request.authority_grant
            || response.correlation_id != request.correlation_id
        {
            return Err(SessionError::InvalidPublicRequest {
                detail: "simulation progress response does not match its session".to_owned(),
            });
        }
        Ok(response)
    }

    fn ensure_current(&self) -> Result<(), SessionError> {
        self.inner.ensure_generation(self.generation, "simulation")
    }
}

impl Management {
    /// Read current lifecycle status.
    pub async fn status(&self) -> Result<SupervisorStatusResponse, SessionError> {
        self.inner.status().await
    }

    /// Read one bounded page of execution summaries.
    pub async fn executions(&self) -> Result<Vec<ExecutionSummary>, SessionError> {
        let session_id = self.inner.session_id().await?;
        Ok(self
            .inner
            .list_executions(ListExecutionsRequest {
                page_size: 0,
                page_token: Vec::new(),
                session_id,
            })
            .await?
            .executions)
    }

    /// Close this logical supervisor session.
    pub async fn close(&self) -> Result<(), SessionError> {
        self.inner.close_inner().await
    }
}

/// One explicitly selected execution incarnation.
#[derive(Clone, Debug)]
pub struct Execution {
    inner: Arc<SupervisorInner>,
    summary: ExecutionSummary,
    generation: u64,
}

impl Execution {
    /// Exact execution identity.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.summary.execution_id
    }

    /// Exact timeline identity captured at selection.
    #[must_use]
    pub fn timeline_id(&self) -> &str {
        &self.summary.timeline_id
    }

    /// Current advertised execution state at selection.
    #[must_use]
    pub fn state(&self) -> i32 {
        self.summary.state
    }

    /// Select one deployed service instance in this execution.
    pub async fn service(&self, instance: impl AsRef<str>) -> Result<Service, SessionError> {
        self.ensure_current()?;
        let instance = instance.as_ref().to_owned();
        let session_id = self.inner.session_id().await?;
        self.inner
            .list_methods(ListMethodsRequest {
                execution_id: self.summary.execution_id.clone(),
                service_instance: instance.clone(),
                page_size: 0,
                page_token: Vec::new(),
                session_id,
            })
            .await?;
        Ok(Service {
            inner: self.inner.clone(),
            execution_id: self.summary.execution_id.clone(),
            timeline_id: self.summary.timeline_id.clone(),
            instance,
            generation: self.generation,
        })
    }

    fn ensure_current(&self) -> Result<(), SessionError> {
        self.inner.ensure_generation(self.generation, "execution")
    }
}

/// One selected service instance within an execution.
#[derive(Clone, Debug)]
pub struct Service {
    inner: Arc<SupervisorInner>,
    execution_id: String,
    timeline_id: String,
    instance: String,
    generation: u64,
}

impl Service {
    /// Deployed instance identity.
    #[must_use]
    pub fn instance(&self) -> &str {
        &self.instance
    }

    /// Bind one owner-generated call or observation descriptor.
    pub async fn method<M>(&self, descriptor: M) -> Result<M::Handle, SessionError>
    where
        M: PublicMethodDescriptor,
    {
        self.inner.ensure_generation(self.generation, "service")?;
        let session_id = self.inner.session_id().await?;
        let methods = list_all_methods(
            &self.inner,
            &self.execution_id,
            &self.instance,
            session_id.clone(),
        )
        .await?;
        let signature = descriptor.signature();
        let metadata = methods
            .into_iter()
            .find(|metadata| metadata.endpoint == signature.endpoint)
            .ok_or_else(|| SessionError::MethodNotAdmitted {
                detail: format!("method `{}` is not advertised", signature.endpoint),
            })?;
        validate_method(signature, &metadata)?;
        let response = self
            .inner
            .bind_method(BindMethodRequest {
                session_id,
                execution_id: self.execution_id.clone(),
                service_instance: self.instance.clone(),
                expected: Some(metadata.clone()),
            })
            .await?;
        let binding_id = response.binding_id;
        if binding_id.len() != 32 {
            return Err(SessionError::MethodNotAdmitted {
                detail: "server returned an invalid binding identifier".to_owned(),
            });
        }
        let core = MethodHandleCore {
            supervisor: self.inner.clone(),
            session_id: self.inner.session_id().await?,
            binding_id,
            execution_id: self.execution_id.clone(),
            timeline_id: self.timeline_id.clone(),
            metadata,
            generation: self.generation,
        };
        Ok(M::from_core(core))
    }
}

/// Sealed generated descriptor family accepted by the external session API.
pub trait PublicMethodDescriptor: MethodDescriptor + private::Sealed {
    /// Handle type returned after remote binding.
    type Handle;

    fn from_core(core: MethodHandleCore) -> Self::Handle;
}

mod private {
    pub trait Sealed {}
}

impl<Request: 'static, Response: 'static> private::Sealed for CallMethod<Request, Response> {}
impl<Value: 'static> private::Sealed for ObservationMethod<Value> {}

/// Opaque validated handle construction state.
pub struct MethodHandleCore {
    supervisor: Arc<SupervisorInner>,
    session_id: Vec<u8>,
    binding_id: Vec<u8>,
    execution_id: String,
    timeline_id: String,
    metadata: MethodMetadata,
    generation: u64,
}

impl<Request: 'static, Response: 'static> PublicMethodDescriptor for CallMethod<Request, Response> {
    type Handle = CallHandle<Request, Response>;

    fn from_core(core: MethodHandleCore) -> Self::Handle {
        CallHandle {
            core,
            request: PhantomData,
            response: PhantomData,
        }
    }
}

impl<Value: 'static> PublicMethodDescriptor for ObservationMethod<Value> {
    type Handle = ObservationHandle<Value>;

    fn from_core(core: MethodHandleCore) -> Self::Handle {
        ObservationHandle {
            core,
            payload: PhantomData,
        }
    }
}

/// A statically typed generated observation handle.
pub struct ObservationHandle<T> {
    core: MethodHandleCore,
    payload: PhantomData<fn() -> T>,
}

/// A statically typed generated unary call handle.
pub struct CallHandle<Request, Response> {
    core: MethodHandleCore,
    request: PhantomData<fn(Request)>,
    response: PhantomData<fn() -> Response>,
}

/// Definite or uncertain result of a public call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CallOutcome<Response> {
    /// The provider returned the typed response body.
    Received(Response),
    /// Local admission proved that no request was sent.
    NotSent(OutcomeReason),
    /// The target refused before operation admission.
    RejectedBeforeAdmission(OutcomeReason),
    /// Transmission or response delivery did not establish the result.
    OutcomeUnknown(OutcomeReason),
}

/// Bounded explanation attached to a non-received public operation outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutcomeReason {
    detail: String,
}

impl OutcomeReason {
    /// Construct a bounded reason from local or remote diagnostic text.
    #[must_use]
    pub fn new(detail: impl Into<String>) -> Self {
        let mut detail = detail.into();
        if detail.len() > 4096 {
            detail.truncate(4096);
        }
        Self { detail }
    }

    /// Diagnostic detail.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for OutcomeReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

/// One bounded record from a generated observation method.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ObservationItem<T> {
    /// A retained observation had no value at the admitted initial cursor.
    InitialAbsent { revision: u64 },
    /// One decoded owner publication.
    Value { revision: u64, value: T },
    /// Records were dropped at a bounded queue boundary.
    Gap { revision: u64, dropped: u64 },
    /// The owner ended the stream.
    End { revision: u64 },
    /// The owner failed the stream.
    Failed { revision: u64, detail: String },
}

/// Bounded typed observation subscription.
pub struct ObservationSubscription<T> {
    initial: Option<SubscriptionRecord>,
    source: PublicSubscription,
    expected: SubscriptionRequest,
    payload: PhantomData<fn() -> T>,
}

impl<T: Message + Default + Send + Sync + 'static> ObservationSubscription<T> {
    /// Receive the initial retained cursor or next bounded observation record.
    pub async fn recv(&mut self) -> Option<Result<ObservationItem<T>, SessionError>> {
        let record = if let Some(initial) = self.initial.take() {
            Some(Ok(initial))
        } else {
            self.source
                .recv()
                .await
                .map(|result| result.map_err(SessionError::Public))
        }?;
        let record = match record {
            Ok(record) => record,
            Err(error) => return Some(Err(error)),
        };
        if record.session_id != self.expected.session_id
            || record.binding_id != self.expected.binding_id
            || record.subscription_id != self.expected.subscription_id
            || record.execution_id != self.expected.execution_id
            || record.timeline_id != self.expected.timeline_id
        {
            return Some(Err(SessionError::InvalidPublicRequest {
                detail: "subscription record context does not match its binding".to_owned(),
            }));
        }
        let kind = match RecordKind::try_from(record.kind) {
            Ok(kind) => kind,
            Err(_) => {
                return Some(Err(SessionError::InvalidPublicRequest {
                    detail: "subscription record kind is unknown".to_owned(),
                }));
            }
        };
        let item = match kind {
            RecordKind::InitialAbsent => ObservationItem::InitialAbsent {
                revision: record.revision,
            },
            RecordKind::Value => match T::decode(record.payload.as_slice()) {
                Ok(value) => ObservationItem::Value {
                    revision: record.revision,
                    value,
                },
                Err(error) => {
                    return Some(Err(SessionError::InvalidPublicRequest {
                        detail: format!("subscription payload could not be decoded: {error}"),
                    }));
                }
            },
            RecordKind::Gap => ObservationItem::Gap {
                revision: record.revision,
                dropped: record.dropped,
            },
            RecordKind::End => ObservationItem::End {
                revision: record.revision,
            },
            RecordKind::Failed => ObservationItem::Failed {
                revision: record.revision,
                detail: record.detail.unwrap_or_default(),
            },
            RecordKind::Unspecified => {
                return Some(Err(SessionError::InvalidPublicRequest {
                    detail: "subscription record kind is unspecified".to_owned(),
                }));
            }
        };
        Some(Ok(item))
    }
}

impl<T: Message + Default + Send + Sync + 'static> ObservationHandle<T> {
    /// Start one bounded typed observation.
    pub async fn observe(&self) -> Result<ObservationSubscription<T>, SessionError> {
        let request = self.core.subscription_request()?;
        let mut source = self.core.supervisor.observe(request.clone()).await?;
        let initial = take_source_initial(&mut source);
        Ok(ObservationSubscription {
            initial,
            source,
            expected: request,
            payload: PhantomData,
        })
    }
}

impl<Request: Message + Send + Sync + 'static, Response: Message + Default + Send + Sync + 'static>
    CallHandle<Request, Response>
{
    /// Issue one typed call with a finite caller deadline.
    pub async fn call(
        &self,
        request: Request,
        timeout: Duration,
    ) -> Result<CallOutcome<Response>, SessionError> {
        let result = self.core.invoke(request, timeout).await?;
        Ok(match result {
            RawOutcome::Received(payload) => match Response::decode(payload.as_slice()) {
                Ok(response) => CallOutcome::Received(response),
                Err(error) => CallOutcome::OutcomeUnknown(OutcomeReason::new(error.to_string())),
            },
            RawOutcome::NotSent(reason) => CallOutcome::NotSent(reason),
            RawOutcome::Rejected(reason) => CallOutcome::RejectedBeforeAdmission(reason),
            RawOutcome::Unknown(reason) => CallOutcome::OutcomeUnknown(reason),
        })
    }
}

enum RawOutcome {
    Received(Vec<u8>),
    NotSent(OutcomeReason),
    Rejected(OutcomeReason),
    Unknown(OutcomeReason),
}

impl MethodHandleCore {
    fn subscription_request(&self) -> Result<SubscriptionRequest, SessionError> {
        self.ensure_current("subscription")?;
        let subscription_id = self.next_id();
        let max_items = self.metadata.max_buffered_items.max(1);
        let max_bytes = self
            .metadata
            .max_message_bytes
            .saturating_mul(max_items)
            .max(self.metadata.max_message_bytes);
        Ok(SubscriptionRequest {
            session_id: self.session_id.clone(),
            binding_id: self.binding_id.clone(),
            subscription_id,
            execution_id: self.execution_id.clone(),
            timeline_id: self.timeline_id.clone(),
            max_buffered_items: max_items,
            max_buffered_bytes: max_bytes,
        })
    }

    async fn invoke<Request: Message>(
        &self,
        request: Request,
        timeout: Duration,
    ) -> Result<RawOutcome, SessionError> {
        self.ensure_current("operation")?;
        if timeout.is_zero() {
            return Ok(RawOutcome::NotSent(OutcomeReason::new(
                "operation timeout must be nonzero",
            )));
        }
        let encoded_len = request.encoded_len();
        if encoded_len > usize::try_from(self.metadata.max_message_bytes).unwrap_or(usize::MAX) {
            return Ok(RawOutcome::NotSent(OutcomeReason::new(
                "request exceeds the admitted method byte bound",
            )));
        }
        let mut payload = Vec::with_capacity(encoded_len);
        request
            .encode(&mut payload)
            .map_err(|error| SessionError::InvalidPublicRequest {
                detail: format!("request could not be encoded: {error}"),
            })?;
        let correlation_id = self.next_id();
        let timeout_ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        let request = OperationRequest {
            session_id: self.session_id.clone(),
            binding_id: self.binding_id.clone(),
            correlation_id,
            execution_id: self.execution_id.clone(),
            timeline_id: self.timeline_id.clone(),
            payload,
            timeout_ms: timeout_ms.max(1),
        };
        let response = match self.supervisor.call(request).await {
            Ok(response) => response,
            Err(error) => return Ok(map_operation_error(error)),
        };
        let outcome = OperationOutcome::try_from(response.outcome).map_err(|_| {
            SessionError::InvalidPublicRequest {
                detail: "operation response outcome is unknown".to_owned(),
            }
        })?;
        let reason = || OutcomeReason::new(response.detail.clone().unwrap_or_default());
        Ok(match outcome {
            OperationOutcome::Received => {
                if response.payload.len()
                    > usize::try_from(self.metadata.max_message_bytes).unwrap_or(usize::MAX)
                {
                    RawOutcome::Unknown(OutcomeReason::new(
                        "response exceeds the admitted method byte bound",
                    ))
                } else {
                    RawOutcome::Received(response.payload)
                }
            }
            OperationOutcome::NotSent => RawOutcome::NotSent(reason()),
            OperationOutcome::RejectedBeforeAdmission => RawOutcome::Rejected(reason()),
            OperationOutcome::Unknown => RawOutcome::Unknown(reason()),
            OperationOutcome::Unspecified => RawOutcome::Unknown(OutcomeReason::new(
                "operation response outcome is unspecified",
            )),
        })
    }

    fn next_id(&self) -> Vec<u8> {
        self.supervisor.next_correlation()
    }

    fn ensure_current(&self, resource: &'static str) -> Result<(), SessionError> {
        self.supervisor
            .ensure_generation(self.generation, resource)?;
        if self.session_id.is_empty() || self.binding_id.is_empty() {
            return Err(SessionError::StaleHandle { resource });
        }
        Ok(())
    }
}

fn take_source_initial(source: &mut PublicSubscription) -> Option<SubscriptionRecord> {
    source.take_initial()
}

fn map_operation_error(error: PublicTransportError) -> RawOutcome {
    let reason = OutcomeReason::new(error.to_string());
    match error {
        PublicTransportError::Malformed { .. }
        | PublicTransportError::BodyTooLarge { .. }
        | PublicTransportError::LeaseExpired => RawOutcome::NotSent(reason),
        PublicTransportError::Rejected { .. }
        | PublicTransportError::Unauthorized { .. }
        | PublicTransportError::Adapter { .. } => RawOutcome::Rejected(reason),
        _ => RawOutcome::Unknown(reason),
    }
}

fn validate_method(
    signature: MethodSignature,
    metadata: &MethodMetadata,
) -> Result<(), SessionError> {
    let shape = match signature.shape {
        MethodShape::Call => SessionMethodShape::Call,
        MethodShape::Observation => SessionMethodShape::Observation,
    };
    if metadata.shape != shape as i32
        || metadata.input_fqn != signature.request
        || metadata.output_fqn != signature.response
        || metadata.retained_latest != signature.retained_latest
        || metadata.lease_valid_for_ms != signature.lease.map(|lease| lease.valid_for_ms())
    {
        return Err(SessionError::MethodNotAdmitted {
            detail: format!(
                "endpoint `{}` descriptor mismatch: expected shape `{shape:?}`, request `{}`, response `{}`, got shape `{}`, request `{}`, response `{}`",
                signature.endpoint,
                signature.request,
                signature.response,
                metadata.shape,
                metadata.input_fqn,
                metadata.output_fqn,
            ),
        });
    }
    Ok(())
}

async fn list_all_methods(
    supervisor: &Arc<SupervisorInner>,
    execution_id: &str,
    service_instance: &str,
    session_id: Vec<u8>,
) -> Result<Vec<MethodMetadata>, SessionError> {
    let mut token = Vec::new();
    let mut methods = Vec::new();
    for _ in 0..1024 {
        let response = supervisor
            .list_methods(ListMethodsRequest {
                execution_id: execution_id.to_owned(),
                service_instance: service_instance.to_owned(),
                page_size: 0,
                page_token: token,
                session_id: session_id.clone(),
            })
            .await?;
        methods.extend(response.methods);
        if response.next_page_token.is_empty() {
            return Ok(methods);
        }
        token = response.next_page_token;
    }
    Err(SessionError::InvalidPublicRequest {
        detail: "public method inventory exceeded its bounded page count".to_owned(),
    })
}

fn ensure_simulation_correlation(
    correlation_id: &mut Vec<u8>,
    supervisor: &Arc<SupervisorInner>,
) -> Result<(), SessionError> {
    if correlation_id.is_empty() {
        *correlation_id = supervisor.next_correlation();
    }
    if correlation_id.len() > 64 {
        return Err(SessionError::InvalidPublicRequest {
            detail: "simulation correlation_id exceeds 64 bytes".to_owned(),
        });
    }
    Ok(())
}

fn ensure_simulation_transition(
    transition: &mut Option<TransitionKey>,
    session_id: &[u8],
) -> Result<(), SessionError> {
    let transition = transition
        .as_mut()
        .ok_or_else(|| SessionError::InvalidPublicRequest {
            detail: "simulation transition key is missing".to_owned(),
        })?;
    transition.session_id = session_id.to_vec();
    if transition.execution_id.is_empty()
        || transition.timeline_id.is_empty()
        || transition.authority_grant.is_empty()
        || transition.operation_sequence == 0
    {
        return Err(SessionError::InvalidPublicRequest {
            detail: "simulation transition key is incomplete".to_owned(),
        });
    }
    Ok(())
}

fn validate_simulation_receipt(
    receipt: Option<&crate::communication::simulation::CutReceipt>,
    transition: Option<&TransitionKey>,
    correlation_id: &[u8],
    expected_status: crate::communication::simulation::PhaseStatus,
) -> Result<(), SessionError> {
    let Some(receipt) = receipt else {
        return Err(SessionError::InvalidPublicRequest {
            detail: "simulation phase response omitted its receipt".to_owned(),
        });
    };
    if transition.is_none()
        || receipt.status != expected_status as i32
        || receipt.transition_key.as_ref() != transition
        || receipt.correlation_id != correlation_id
        || receipt.membership_digest.len() != 32
    {
        return Err(SessionError::InvalidPublicRequest {
            detail: "simulation phase response does not match its transition".to_owned(),
        });
    }
    Ok(())
}

const SESSION_RENEWAL_PERIOD: Duration = Duration::from_secs(10);

fn spawn_renewal(supervisor: &Arc<SupervisorInner>, generation: u64) -> JoinHandle<()> {
    let weak = Arc::downgrade(supervisor);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(SESSION_RENEWAL_PERIOD);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // `interval` ticks immediately when constructed. Consume that tick so
        // a newly opened session is not renewed in the same scheduling turn.
        interval.tick().await;
        loop {
            interval.tick().await;
            let Some(inner) = weak.upgrade() else {
                return;
            };
            if inner.closed.load(Ordering::Acquire)
                || inner.generation.load(Ordering::Acquire) != generation
            {
                return;
            }
            let result = {
                let mut guard = inner.session.lock().await;
                if inner.closed.load(Ordering::Acquire)
                    || inner.generation.load(Ordering::Acquire) != generation
                {
                    return;
                }
                match guard.as_mut() {
                    Some(connection) => connection.renew().await,
                    None => return,
                }
            };
            if result.is_err() {
                if inner
                    .generation
                    .compare_exchange(
                        generation,
                        generation.saturating_add(1),
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    inner.cancel_lifecycle().await;
                    let old = inner.session.lock().await.take();
                    inner.session_id.lock().await.clear();
                    if let Some(old) = old {
                        let _ = old.close().await;
                    }
                }
                return;
            }
        }
    })
}

impl SupervisorInner {
    async fn info(&self) -> Result<SupervisorInfoResponse, SessionError> {
        let guard = self.session.lock().await;
        if self.closed.load(Ordering::Acquire) {
            return Err(SessionError::StaleHandle {
                resource: "supervisor",
            });
        }
        guard
            .as_ref()
            .map(|connection| connection.info().clone())
            .ok_or(SessionError::StaleHandle {
                resource: "supervisor",
            })
    }

    async fn session_id(&self) -> Result<Vec<u8>, SessionError> {
        let session = self.session.lock().await;
        if self.closed.load(Ordering::Acquire) || session.is_none() {
            return Err(SessionError::StaleHandle {
                resource: "supervisor",
            });
        }
        let session_id = self.session_id.lock().await.clone();
        if session_id.is_empty() {
            return Err(SessionError::StaleHandle {
                resource: "supervisor",
            });
        }
        Ok(session_id)
    }

    fn ensure_generation(
        &self,
        generation: u64,
        resource: &'static str,
    ) -> Result<(), SessionError> {
        if self.closed.load(Ordering::Acquire)
            || self.generation.load(Ordering::Acquire) != generation
        {
            Err(SessionError::StaleHandle { resource })
        } else {
            Ok(())
        }
    }

    fn next_correlation(&self) -> Vec<u8> {
        let value = self.correlation.fetch_add(1, Ordering::AcqRel);
        value.to_be_bytes().to_vec()
    }

    async fn status(&self) -> Result<SupervisorStatusResponse, SessionError> {
        let guard = self.session.lock().await;
        let connection = guard.as_ref().ok_or(SessionError::StaleHandle {
            resource: "supervisor",
        })?;
        connection.status().await.map_err(Into::into)
    }

    async fn list_executions(
        &self,
        request: ListExecutionsRequest,
    ) -> Result<crate::communication::session::ListExecutionsResponse, SessionError> {
        let guard = self.session.lock().await;
        let connection = guard.as_ref().ok_or(SessionError::StaleHandle {
            resource: "supervisor",
        })?;
        connection
            .list_executions(request)
            .await
            .map_err(Into::into)
    }

    async fn list_methods(
        &self,
        request: ListMethodsRequest,
    ) -> Result<crate::communication::session::ListMethodsResponse, SessionError> {
        let guard = self.session.lock().await;
        let connection = guard.as_ref().ok_or(SessionError::StaleHandle {
            resource: "supervisor",
        })?;
        connection.list_methods(request).await.map_err(Into::into)
    }

    async fn bind_method(
        &self,
        request: BindMethodRequest,
    ) -> Result<crate::communication::session::BindMethodResponse, SessionError> {
        let guard = self.session.lock().await;
        let connection = guard.as_ref().ok_or(SessionError::StaleHandle {
            resource: "supervisor",
        })?;
        connection.bind_method(request).await.map_err(Into::into)
    }

    async fn call(
        &self,
        request: OperationRequest,
    ) -> Result<crate::communication::session::OperationResponse, PublicTransportError> {
        let guard = self.session.lock().await;
        let connection = guard.as_ref().ok_or(PublicTransportError::LeaseExpired)?;
        connection.call(request).await
    }

    async fn simulation<Request, Response>(
        &self,
        operation: PublicOperation,
        request: Request,
    ) -> Result<Response, SessionError>
    where
        Request: Message,
        Response: Message + Default,
    {
        let guard = self.session.lock().await;
        let connection = guard.as_ref().ok_or(SessionError::StaleHandle {
            resource: "simulation",
        })?;
        connection
            .simulation(operation, request)
            .await
            .map_err(Into::into)
    }

    async fn observe(
        &self,
        request: SubscriptionRequest,
    ) -> Result<PublicSubscription, SessionError> {
        let lifecycle = self.lifecycle.lock().await.clone();
        let guard = self.session.lock().await;
        let connection = guard.as_ref().ok_or(SessionError::StaleHandle {
            resource: "subscription",
        })?;
        connection
            .observe_with_cancel(request, lifecycle)
            .await
            .map_err(Into::into)
    }

    async fn stop_renewal(&self) {
        let task = self.renewal.lock().await.take();
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
    }

    async fn cancel_lifecycle(&self) {
        self.lifecycle.lock().await.cancel();
    }

    async fn replace_lifecycle(&self) -> CancellationToken {
        let mut lifecycle = self.lifecycle.lock().await;
        lifecycle.cancel();
        let replacement = CancellationToken::new();
        *lifecycle = replacement.clone();
        replacement
    }

    async fn close_inner(&self) -> Result<(), SessionError> {
        if self.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.cancel_lifecycle().await;
        self.stop_renewal().await;
        let old = self.session.lock().await.take();
        self.session_id.lock().await.clear();
        if let Some(connection) = old {
            connection.close().await.map(|_| ()).map_err(Into::into)
        } else {
            Ok(())
        }
    }
}
