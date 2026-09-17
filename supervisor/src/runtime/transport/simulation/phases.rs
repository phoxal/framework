//! Receipt retention and admission of complete simulation transitions.

use super::authority::{
    authorize_live_simulation_grant, authorize_simulation_grant, release_backend_authority,
    simulation_adapter_error, simulation_backend_error, simulation_rejected,
    validate_simulation_correlation,
};
use super::products::*;
use super::*;

pub(crate) async fn begin_simulation_phase(
    route: &PublicRoute,
    transition_key: &TransitionKey,
    correlation_id: &[u8],
    operation: PublicOperation,
    request_digest: [u8; 32],
    context: PhaseAdmissionContext<'_>,
    now_ms: u64,
) -> Result<SimulationPhaseAdmission, PublicTransportError> {
    let PhaseAdmissionContext {
        adapter,
        authority,
        backend: simulation_backend,
    } = context;
    validate_simulation_correlation(correlation_id)?;
    if transition_key.session_id.is_empty()
        || transition_key.execution_id.is_empty()
        || transition_key.timeline_id.is_empty()
        || transition_key.authority_grant.is_empty()
        || transition_key.operation_sequence == 0
    {
        return Err(simulation_rejected(
            "simulation transition key is incomplete",
        ));
    }
    let mut guard = authority.lock().await;
    let current = guard
        .as_mut()
        .ok_or_else(|| simulation_rejected("simulation authority is not active"))?;
    authorize_simulation_grant(route, &transition_key.authority_grant, current)?;
    if Instant::now() >= current.lease_deadline {
        let expired = guard.take();
        drop(guard);
        if let Some(expired) = expired {
            release_backend_authority(expired, simulation_backend).await;
            return Err(simulation_rejected("simulation authority lease expired"));
        }
        return Err(simulation_rejected("simulation authority lease expired"));
    }
    if let Some(failure) = &current.failure {
        return Err(simulation_rejected(failure));
    }
    if current.active_phase.is_some() {
        return Err(simulation_rejected("simulation phase is still executing"));
    }
    if current.resetting {
        return Err(simulation_rejected("simulation reset is in progress"));
    }
    if transition_key.session_id != current.session_id
        || transition_key.execution_id != current.execution_id
        || transition_key.timeline_id != current.timeline_id
    {
        return Err(simulation_rejected(
            "simulation transition identity does not match authority",
        ));
    }
    if let Some(retained) = current
        .phase_receipts
        .iter()
        .find(|retained| retained.correlation_id == correlation_id)
    {
        if retained.operation != operation
            || retained.transition_key != *transition_key
            || retained.request_digest != request_digest
        {
            return Err(simulation_rejected(
                "simulation correlation_id or transition was reused with different inputs",
            ));
        }
        return Err(simulation_rejected(
            "simulation phase duplicate must be served from its retained receipt",
        ));
    }
    if transition_key.operation_sequence <= current.accepted_sequence_watermark {
        return Err(simulation_rejected(
            "simulation operation is stale because its receipt was evicted",
        ));
    }
    if transition_key.operation_sequence
        != current
            .accepted_sequence_watermark
            .checked_add(1)
            .ok_or_else(|| simulation_rejected("simulation operation sequence is exhausted"))?
    {
        return Err(simulation_rejected(
            "simulation operation sequence is out of order",
        ));
    }
    if current
        .in_flight
        .as_ref()
        .is_some_and(|key| key != transition_key)
    {
        return Err(simulation_rejected(
            "another simulation transition is already in flight",
        ));
    }
    match operation {
        PublicOperation::AdmitInitialObservations => {
            if transition_key.boundary != 0
                || current.boundary != 0
                || current.accepted_sequence_watermark != 0
                || current.in_flight.is_some()
            {
                return Err(simulation_rejected(
                    "initial observations are only admitted once at boundary zero",
                ));
            }
        }
        PublicOperation::PrepareBoundary => {
            if transition_key.boundary != current.boundary
                || current.in_flight.is_some()
                || current.accepted_sequence_watermark == 0
            {
                return Err(simulation_rejected(
                    "prepare boundary is not the current admitted boundary",
                ));
            }
        }
        PublicOperation::AdmitObservations => {
            if transition_key.boundary != current.boundary
                || current.in_flight.as_ref() != Some(transition_key)
                || !current.phase_receipts.iter().any(|phase| {
                    phase.transition_key == *transition_key
                        && phase.operation == PublicOperation::PrepareBoundary
                })
            {
                return Err(simulation_rejected(
                    "observation admission does not match the prepared transition",
                ));
            }
        }
        _ => {
            return Err(simulation_rejected(
                "operation is not a three-phase simulation transition",
            ));
        }
    }
    let mut adapter_guard = adapter.lock().await;
    if !adapter_guard.simulation_session_active(route.principal(), &current.session_id, now_ms) {
        return Err(simulation_rejected(
            "simulation session is no longer active",
        ));
    }
    adapter_guard
        .authorize_simulation_session(route, &current.session_id, now_ms)
        .map_err(|error| simulation_adapter_error(operation.segment(), error))?;
    let summary = adapter_guard
        .simulation_execution(&current.execution_id)
        .map_err(|error| simulation_adapter_error(operation.segment(), error))?;
    if summary.timeline_id != current.timeline_id {
        return Err(simulation_rejected(
            "simulation timeline was invalidated by the supervisor",
        ));
    }
    let definition = adapter_guard
        .simulation_definition(&current.execution_id)
        .map_err(|error| simulation_adapter_error(operation.segment(), error))?;
    let context = PublicSimulationContext {
        principal: current.principal.clone(),
        session_id: current.session_id.clone(),
        authority_grant: current.grant.clone(),
        correlation_id: correlation_id.to_vec(),
        execution_id: current.execution_id.clone(),
        timeline_id: current.timeline_id.clone(),
        completed_boundary: current.boundary,
        model_identity: current.model_identity.clone(),
        quantum_ns: current.quantum_ns,
    };
    current.in_flight = Some(transition_key.clone());
    current.active_phase = Some(operation);
    Ok(SimulationPhaseAdmission {
        context,
        transition_key: transition_key.clone(),
        request_digest,
        definition,
        operation,
        adapter: adapter.clone(),
        route: route.clone(),
        authorized_at_ms: now_ms,
        started: Instant::now(),
    })
}

pub(crate) fn retained_phase_response<Response: Message + Default>(
    current: &SimulationAuthority,
    operation: PublicOperation,
    transition_key: &TransitionKey,
    correlation_id: &[u8],
    request_digest: [u8; 32],
) -> Result<Option<Response>, PublicTransportError> {
    let Some(retained) = current
        .phase_receipts
        .iter()
        .find(|retained| retained.correlation_id == correlation_id)
    else {
        return Ok(None);
    };
    if retained.operation != operation
        || retained.transition_key != *transition_key
        || retained.request_digest != request_digest
    {
        return Err(simulation_rejected(
            "simulation correlation_id or transition was reused with different inputs",
        ));
    }
    Response::decode(retained.response.as_slice())
        .map(Some)
        .map_err(|error| {
            simulation_rejected(&format!("retained simulation receipt is invalid: {error}"))
        })
}

pub(crate) async fn admit_initial_observations(
    route: &PublicRoute,
    request: AdmitInitialObservationsRequest,
    adapter: &Arc<Mutex<SupervisorAdapter>>,
    authority: &Arc<Mutex<Option<SimulationAuthority>>>,
    simulation_backend: &Arc<dyn PublicSimulationBackend>,
    now_ms: u64,
) -> Result<AdmitInitialObservationsResponse, PublicTransportError> {
    let key = request
        .transition_key
        .clone()
        .ok_or_else(|| simulation_rejected("initial observation request has no transition key"))?;
    let digest: [u8; 32] = Sha256::digest(request.encode_to_vec()).into();
    {
        let guard = authority.lock().await;
        if let Some(current) = guard.as_ref() {
            authorize_live_simulation_grant(route, &key.authority_grant, current, adapter, now_ms)
                .await?;
        }
        if let Some(current) = guard.as_ref()
            && let Some(response) = retained_phase_response::<AdmitInitialObservationsResponse>(
                current,
                PublicOperation::AdmitInitialObservations,
                &key,
                &request.correlation_id,
                digest,
            )?
        {
            return Ok(response);
        }
    }
    let (max_product_bytes, max_cut_bytes) = {
        let guard = authority.lock().await;
        let current = guard
            .as_ref()
            .ok_or_else(|| simulation_rejected("simulation authority is not active"))?;
        (current.max_product_bytes, current.max_cut_bytes)
    };
    let admission = begin_simulation_phase(
        route,
        &key,
        &request.correlation_id,
        PublicOperation::AdmitInitialObservations,
        digest,
        PhaseAdmissionContext {
            adapter,
            authority,
            backend: simulation_backend,
        },
        now_ms,
    )
    .await?;
    if let Err(error) = validate_observation_cut(
        &request.observations,
        0,
        &admission.definition,
        adapter,
        &admission.context.execution_id,
        (max_product_bytes, max_cut_bytes),
        &request.membership_digest,
    )
    .await
    {
        clear_phase_admission(authority, &admission).await;
        return Err(error);
    }
    let result = tokio::time::timeout(
        DEFAULT_PUBLIC_DEADLINE,
        simulation_backend.admit_initial_observations(admission.context.clone(), request.clone()),
    )
    .await;
    let mut response = phase_backend_result(result, authority, &admission).await?;
    let memberships = observation_memberships(&request.observations)?;
    phase_validation_result(
        normalize_receipt(
            &mut response.receipt,
            &admission,
            &request.correlation_id,
            phoxal::communication::simulation::PhaseStatus::InitialAdmitted,
            memberships,
            0,
        ),
        authority,
        &admission,
    )
    .await?;
    retain_phase_response(
        authority,
        &admission,
        &request.correlation_id,
        response.clone(),
        phoxal::communication::simulation::PhaseStatus::InitialAdmitted,
        false,
    )
    .await
}

pub(crate) async fn prepare_boundary(
    route: &PublicRoute,
    request: PrepareBoundaryRequest,
    adapter: &Arc<Mutex<SupervisorAdapter>>,
    authority: &Arc<Mutex<Option<SimulationAuthority>>>,
    simulation_backend: &Arc<dyn PublicSimulationBackend>,
    now_ms: u64,
) -> Result<PrepareBoundaryResponse, PublicTransportError> {
    let key = request
        .transition_key
        .clone()
        .ok_or_else(|| simulation_rejected("prepare request has no transition key"))?;
    let digest: [u8; 32] = Sha256::digest(request.encode_to_vec()).into();
    {
        let guard = authority.lock().await;
        if let Some(current) = guard.as_ref() {
            authorize_live_simulation_grant(route, &key.authority_grant, current, adapter, now_ms)
                .await?;
        }
        if let Some(current) = guard.as_ref()
            && let Some(response) = retained_phase_response::<PrepareBoundaryResponse>(
                current,
                PublicOperation::PrepareBoundary,
                &key,
                &request.correlation_id,
                digest,
            )?
        {
            return Ok(response);
        }
    }
    let admission = begin_simulation_phase(
        route,
        &key,
        &request.correlation_id,
        PublicOperation::PrepareBoundary,
        digest,
        PhaseAdmissionContext {
            adapter,
            authority,
            backend: simulation_backend,
        },
        now_ms,
    )
    .await?;
    let result = tokio::time::timeout(
        DEFAULT_PUBLIC_DEADLINE,
        simulation_backend.prepare_boundary(admission.context.clone(), request.clone()),
    )
    .await;
    let mut response = phase_backend_result(result, authority, &admission).await?;
    let memberships = phase_validation_result(
        validate_actuation_cut(&response.actuation, key.boundary),
        authority,
        &admission,
    )
    .await?;
    phase_validation_result(
        normalize_receipt(
            &mut response.receipt,
            &admission,
            &request.correlation_id,
            phoxal::communication::simulation::PhaseStatus::Prepared,
            memberships,
            key.boundary,
        ),
        authority,
        &admission,
    )
    .await?;
    retain_phase_response(
        authority,
        &admission,
        &request.correlation_id,
        response.clone(),
        phoxal::communication::simulation::PhaseStatus::Prepared,
        false,
    )
    .await
}

pub(crate) async fn admit_observations(
    route: &PublicRoute,
    request: AdmitObservationsRequest,
    adapter: &Arc<Mutex<SupervisorAdapter>>,
    authority: &Arc<Mutex<Option<SimulationAuthority>>>,
    simulation_backend: &Arc<dyn PublicSimulationBackend>,
    now_ms: u64,
) -> Result<AdmitObservationsResponse, PublicTransportError> {
    let key = request
        .transition_key
        .clone()
        .ok_or_else(|| simulation_rejected("observation admission has no transition key"))?;
    let digest: [u8; 32] = Sha256::digest(request.encode_to_vec()).into();
    {
        let guard = authority.lock().await;
        if let Some(current) = guard.as_ref() {
            authorize_live_simulation_grant(route, &key.authority_grant, current, adapter, now_ms)
                .await?;
        }
        if let Some(current) = guard.as_ref()
            && let Some(response) = retained_phase_response::<AdmitObservationsResponse>(
                current,
                PublicOperation::AdmitObservations,
                &key,
                &request.correlation_id,
                digest,
            )?
        {
            return Ok(response);
        }
    }
    let (max_product_bytes, max_cut_bytes) = {
        let guard = authority.lock().await;
        let current = guard
            .as_ref()
            .ok_or_else(|| simulation_rejected("simulation authority is not active"))?;
        (current.max_product_bytes, current.max_cut_bytes)
    };
    let admission = begin_simulation_phase(
        route,
        &key,
        &request.correlation_id,
        PublicOperation::AdmitObservations,
        digest,
        PhaseAdmissionContext {
            adapter,
            authority,
            backend: simulation_backend,
        },
        now_ms,
    )
    .await?;
    let expected_boundary = key
        .boundary
        .checked_add(1)
        .ok_or_else(|| simulation_rejected("simulation boundary overflow"))?;
    if let Err(error) = validate_observation_cut(
        &request.observations,
        expected_boundary,
        &admission.definition,
        adapter,
        &admission.context.execution_id,
        (max_product_bytes, max_cut_bytes),
        &request.membership_digest,
    )
    .await
    {
        clear_phase_admission(authority, &admission).await;
        return Err(error);
    }
    let result = tokio::time::timeout(
        DEFAULT_PUBLIC_DEADLINE,
        simulation_backend.admit_observations(admission.context.clone(), request.clone()),
    )
    .await;
    let mut response = phase_backend_result(result, authority, &admission).await?;
    let memberships = observation_memberships(&request.observations)?;
    phase_validation_result(
        normalize_receipt(
            &mut response.receipt,
            &admission,
            &request.correlation_id,
            phoxal::communication::simulation::PhaseStatus::ObservationsAdmitted,
            memberships,
            expected_boundary,
        ),
        authority,
        &admission,
    )
    .await?;
    retain_phase_response(
        authority,
        &admission,
        &request.correlation_id,
        response.clone(),
        phoxal::communication::simulation::PhaseStatus::ObservationsAdmitted,
        true,
    )
    .await
}

pub(crate) async fn clear_phase_admission(
    authority: &Arc<Mutex<Option<SimulationAuthority>>>,
    admission: &SimulationPhaseAdmission,
) {
    if let Some(current) = authority.lock().await.as_mut()
        && current.in_flight.as_ref() == Some(&admission.transition_key)
    {
        current.active_phase = None;
        if admission.operation != PublicOperation::AdmitObservations {
            current.in_flight = None;
        }
    }
}

pub(crate) async fn phase_backend_result<T>(
    result: Result<Result<T, PublicBackendError>, tokio::time::error::Elapsed>,
    authority: &Arc<Mutex<Option<SimulationAuthority>>>,
    admission: &SimulationPhaseAdmission,
) -> Result<T, PublicTransportError> {
    let error = match result {
        Ok(Ok(response)) => return Ok(response),
        Ok(Err(
            error @ (PublicBackendError::RejectedBeforeAdmission(_) | PublicBackendError::Capacity),
        )) => {
            clear_phase_admission(authority, admission).await;
            return Err(simulation_backend_error(
                admission.operation.segment(),
                error,
            ));
        }
        Ok(Err(error)) => simulation_backend_error(admission.operation.segment(), error),
        Err(_) => PublicTransportError::Timeout {
            operation: admission.operation.segment().to_owned(),
        },
    };
    if let Some(current) = authority.lock().await.as_mut()
        && current.grant == admission.context.authority_grant
        && current.timeline_id == admission.context.timeline_id
    {
        current.failure = Some(bounded_error_detail(&error.to_string()));
        current.active_phase = None;
    }
    Err(error)
}

/// A malformed reply after backend mutation is terminal, even if no receipt
/// can be retained. Repeating the request must never repeat that mutation.
async fn phase_validation_result<T>(
    result: Result<T, PublicTransportError>,
    authority: &Arc<Mutex<Option<SimulationAuthority>>>,
    admission: &SimulationPhaseAdmission,
) -> Result<T, PublicTransportError> {
    if let Err(error) = &result
        && let Some(current) = authority.lock().await.as_mut()
        && current.grant == admission.context.authority_grant
        && current.timeline_id == admission.context.timeline_id
    {
        current.failure = Some(bounded_error_detail(&error.to_string()));
        current.active_phase = None;
    }
    result
}

pub(crate) async fn retain_phase_response<Response: Message>(
    authority: &Arc<Mutex<Option<SimulationAuthority>>>,
    admission: &SimulationPhaseAdmission,
    correlation_id: &[u8],
    response: Response,
    status: phoxal::communication::simulation::PhaseStatus,
    completes_transition: bool,
) -> Result<Response, PublicTransportError> {
    let encoded = response.encode_to_vec();
    let mut guard = authority.lock().await;
    let current = guard
        .as_mut()
        .ok_or_else(|| simulation_rejected("simulation authority is no longer active"))?;
    if current.grant != admission.context.authority_grant
        || current.timeline_id != admission.context.timeline_id
        || current.session_id != admission.context.session_id
        || current.principal != admission.context.principal
        || current.resetting
        || current.failure.is_some()
        || current.active_phase != Some(admission.operation)
        || current.in_flight.as_ref() != Some(&admission.transition_key)
    {
        return Err(simulation_rejected(
            "simulation authority changed while a phase was executing",
        ));
    }
    authorize_live_simulation_grant(
        &admission.route,
        &admission.context.authority_grant,
        current,
        &admission.adapter,
        admission.authorized_at_ms.saturating_add(
            admission
                .started
                .elapsed()
                .as_millis()
                .min(u128::from(u64::MAX)) as u64,
        ),
    )
    .await?;
    if encoded.len() > current.receipt_byte_cap {
        let detail = "simulation phase receipt exceeds its negotiated retention byte cap";
        current.failure = Some(detail.into());
        current.active_phase = None;
        return Err(simulation_rejected(detail));
    }
    current.active_phase = None;
    current.phase_receipts.push_back(RetainedSimulationPhase {
        operation: admission.operation,
        transition_key: admission.transition_key.clone(),
        correlation_id: correlation_id.to_vec(),
        request_digest: admission.request_digest,
        response: encoded,
        status,
        bytes: response.encoded_len(),
    });
    while current.phase_receipts.len() > MAX_RETAINED_SIMULATION_PHASES
        || current
            .phase_receipts
            .iter()
            .map(|receipt| receipt.bytes)
            .sum::<usize>()
            > current.receipt_byte_cap
    {
        let Some(evicted) = current.phase_receipts.pop_front() else {
            break;
        };
        if evicted.transition_key == admission.transition_key && !completes_transition {
            current.phase_receipts.push_front(evicted);
            break;
        }
    }
    current.lease_deadline = Instant::now()
        .checked_add(SIMULATION_AUTHORITY_LEASE)
        .ok_or_else(|| simulation_rejected("simulation authority lease overflows the clock"))?;
    if completes_transition {
        current.boundary = admission
            .transition_key
            .boundary
            .checked_add(1)
            .ok_or_else(|| simulation_rejected("simulation boundary overflow"))?;
        current.accepted_sequence_watermark = admission.transition_key.operation_sequence;
        current.in_flight = None;
    } else if admission.operation == PublicOperation::AdmitInitialObservations {
        current.accepted_sequence_watermark = admission.transition_key.operation_sequence;
        current.in_flight = None;
    }
    Ok(response)
}

pub(crate) fn retained_phase_progress(
    current: &SimulationAuthority,
    request: &ProgressRequest,
) -> Result<ProgressResponse, PublicTransportError> {
    let key = request
        .transition_key
        .as_ref()
        .ok_or_else(|| simulation_rejected("progress transition query has no transition key"))?;
    let requested_phase = phoxal::communication::simulation::PhaseStatus::try_from(request.phase)
        .unwrap_or(phoxal::communication::simulation::PhaseStatus::Unspecified);
    let retained = current.phase_receipts.iter().find(|phase| {
        phase.transition_key == *key
            && (requested_phase == phoxal::communication::simulation::PhaseStatus::Unspecified
                || phase.status == requested_phase)
    });
    let mut response = ProgressResponse {
        execution_id: current.execution_id.clone(),
        timeline_id: current.timeline_id.clone(),
        completed_boundary: current.boundary,
        failed: false,
        detail: None,
        session_id: current.session_id.clone(),
        authority_grant: current.grant.clone(),
        correlation_id: request.correlation_id.clone(),
        phase_status: phoxal::communication::simulation::PhaseStatus::Unknown as i32,
        prepared_boundary: current.boundary,
        admitted_observation_boundary: current.boundary,
        accepted_sequence_watermark: current.accepted_sequence_watermark,
        request_digest: Vec::new(),
        membership_digest: Vec::new(),
    };
    let Some(retained) = retained else {
        response.phase_status = if key.operation_sequence <= current.accepted_sequence_watermark {
            phoxal::communication::simulation::PhaseStatus::Stale as i32
        } else {
            phoxal::communication::simulation::PhaseStatus::Unknown as i32
        };
        response.detail = Some(
            if response.phase_status == phoxal::communication::simulation::PhaseStatus::Stale as i32
            {
                "simulation phase receipt was evicted; its sequence watermark is retained"
                    .to_owned()
            } else {
                "simulation phase outcome is not retained".to_owned()
            },
        );
        return Ok(response);
    };
    let receipt = retained_receipt(retained)?;
    response.phase_status = retained.status as i32;
    response.prepared_boundary = receipt.prepared_boundary;
    response.admitted_observation_boundary = receipt.admitted_observation_boundary;
    response.request_digest = receipt.request_digest;
    response.membership_digest = receipt.membership_digest;
    Ok(response)
}

pub(crate) fn retained_receipt(
    retained: &RetainedSimulationPhase,
) -> Result<phoxal::communication::simulation::CutReceipt, PublicTransportError> {
    let receipt = match retained.operation {
        PublicOperation::AdmitInitialObservations => {
            AdmitInitialObservationsResponse::decode(retained.response.as_slice())
                .map_err(|error| {
                    simulation_rejected(&format!("retained receipt is invalid: {error}"))
                })?
                .receipt
        }
        PublicOperation::PrepareBoundary => {
            PrepareBoundaryResponse::decode(retained.response.as_slice())
                .map_err(|error| {
                    simulation_rejected(&format!("retained receipt is invalid: {error}"))
                })?
                .receipt
        }
        PublicOperation::AdmitObservations => {
            AdmitObservationsResponse::decode(retained.response.as_slice())
                .map_err(|error| {
                    simulation_rejected(&format!("retained receipt is invalid: {error}"))
                })?
                .receipt
        }
        _ => None,
    };
    receipt.ok_or_else(|| simulation_rejected("retained simulation receipt is incomplete"))
}
