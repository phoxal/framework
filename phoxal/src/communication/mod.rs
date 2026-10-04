//! Framework-owned public protocol messages and admission invariants.
//!
//! These contracts are independent of runner implementation and package
//! versions.
//! They use standard Protobuf messages and remain available to session clients
//! without importing project tooling, official services, or simulation engines.

/// Fixed offer-only bootstrap protocol.
pub mod bootstrap;

/// Public logical-session protocol.
pub mod session;

/// Supervisor-to-runtime execution coordination protocol.
pub mod execution;

/// Public simulation authority and boundary protocol.
pub mod simulation;

pub mod route;
pub mod validation;
pub use route::{PublicOperation, PublicRoute, PublicRouteKind, RouteError};
pub use validation::{
    BootstrapError, DeploymentTarget, MAX_BOOTSTRAP_BYTES, MAX_KEY_PREFIX_BYTES,
    MAX_PROTOCOL_BYTES, MAX_SESSION_OFFERS, SESSION_PROTOCOL, validate_session_offers,
};

/// Standard descriptors assembled from the same records as artifact extraction.
/// No compiler, generated source, transport, or target execution is required.
pub fn file_descriptor_set() -> Result<prost_types::FileDescriptorSet, crate::schema::AssemblyError>
{
    use crate::schema::MessageSchema;
    fn record<T: MessageSchema>() -> crate::schema::DecodedRecord {
        T::retain_schema();
        T::RECORD.to_decoded()
    }
    let records = [
        record::<bootstrap::SessionOffers>(),
        record::<bootstrap::SessionOffer>(),
        record::<session::OpenSessionRequest>(),
        record::<session::OpenSessionResponse>(),
        record::<session::RenewSessionRequest>(),
        record::<session::RenewSessionResponse>(),
        record::<session::CloseSessionRequest>(),
        record::<session::CloseSessionResponse>(),
        record::<session::SupervisorInfoRequest>(),
        record::<session::SupervisorInfoResponse>(),
        record::<session::SupervisorStatusRequest>(),
        record::<session::SupervisorStatusResponse>(),
        record::<session::SupervisorState>(),
        record::<session::ListExecutionsRequest>(),
        record::<session::ListExecutionsResponse>(),
        record::<session::ExecutionSummary>(),
        record::<session::ExecutionState>(),
        record::<session::ListMethodsRequest>(),
        record::<session::ListMethodsResponse>(),
        record::<session::MethodMetadata>(),
        record::<session::MethodShape>(),
        record::<session::BindMethodRequest>(),
        record::<session::BindMethodResponse>(),
        record::<session::OperationRequest>(),
        record::<session::OperationResponse>(),
        record::<session::OperationOutcome>(),
        record::<session::SubscriptionRequest>(),
        record::<session::SubscriptionAdmission>(),
        record::<session::SubscriptionRecord>(),
        record::<session::RecordKind>(),
        record::<execution::ContractRequirement>(),
        record::<execution::ExecutionMode>(),
        record::<execution::AdmitExecutionRequest>(),
        record::<execution::AdmitExecutionResponse>(),
        record::<execution::Ready>(),
        record::<execution::InitializeStateRequest>(),
        record::<execution::InitializeStateResponse>(),
        record::<execution::PinReadViewsRequest>(),
        record::<execution::PinReadViewsResponse>(),
        record::<execution::Invocation>(),
        record::<execution::InvocationAccepted>(),
        record::<execution::ProductReceipt>(),
        record::<execution::InputReceipt>(),
        record::<execution::Actuation>(),
        record::<execution::DeliveryReceipt>(),
        record::<execution::DeliveryAck>(),
        record::<execution::ResetExecutionRequest>(),
        record::<execution::ResetExecutionResponse>(),
        record::<execution::RuntimeFailure>(),
        record::<execution::RuntimeWireMetadata>(),
        record::<simulation::AcquireAuthorityRequest>(),
        record::<simulation::ProviderRequirement>(),
        record::<simulation::AcquireAuthorityResponse>(),
        record::<simulation::TransitionKey>(),
        record::<simulation::ProductDisposition>(),
        record::<simulation::ProductMembership>(),
        record::<simulation::Observation>(),
        record::<simulation::Actuation>(),
        record::<simulation::ProductReceipt>(),
        record::<simulation::PhaseStatus>(),
        record::<simulation::CutReceipt>(),
        record::<simulation::AdmitInitialObservationsRequest>(),
        record::<simulation::AdmitInitialObservationsResponse>(),
        record::<simulation::PrepareBoundaryRequest>(),
        record::<simulation::PrepareBoundaryResponse>(),
        record::<simulation::AdmitObservationsRequest>(),
        record::<simulation::AdmitObservationsResponse>(),
        record::<simulation::ResetRequest>(),
        record::<simulation::ResetResponse>(),
        record::<simulation::ReleaseAuthorityRequest>(),
        record::<simulation::ReleaseAuthorityResponse>(),
        record::<simulation::ProgressRequest>(),
        record::<simulation::ProgressResponse>(),
    ];
    crate::schema::assemble_file_descriptors(&records)
}
