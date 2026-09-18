//! Public-session client transport.
//!
//! This module owns the wire exchange, bounded query collection, and the
//! lifetime of the public queryables for the `phoxal.session.v1` protocol
//! from the **client** side. The server half — queryables, dispatch, backend
//! invocation, simulation authority, and the simulation state machine —
//! lives under `framework/supervisor`, not in the SDK. The Protobuf
//! messages and route/admission rules remain in [`crate::communication`].
//!
//! Submodule layout (client only, in the SDK):
//!
//! - [`client`] — bounded, principal-bound, target-bound client transport.
//!   Public types: `PublicSessionTransport`, `PublicSessionConfig`,
//!   `PublicSessionConnection`, `PublicSubscription`, `DiscoveryEvent`,
//!   `SupervisorWatch`, `PublicTlsCredentials`, `PublicTransportSecurity`,
//!   `PublicTransportLimits`, `PublicTransportError`.
//!
//! Shared constants and helpers live in this parent module so the client
//! and the supervisor-side helpers that consume the typed protobuf
//! messages reference the same values without cyclic imports.
#![allow(unused_imports, dead_code)]

pub(crate) mod client;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use prost::Message;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::communication::route::{PublicOperation, PublicOperation as _, PublicRoute, PublicRouteKind};
use crate::communication::session::{
    RecordKind, SubscriptionAdmission, SubscriptionRecord, SubscriptionRequest,
};
use crate::communication::validation::DeploymentTarget;

// ===== Shared constants (used by both client and server) =====
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
/// Server-only: bounded subscription identifier length.
pub const MAX_PUBLIC_SUBSCRIPTION_ID_BYTES: usize = 32;

// ===== Shared helpers (used by both client and server) =====
pub fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        text.push(HEX[usize::from(byte >> 4)] as char);
        text.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    text
}

pub fn operation_key_expression(
    target: &DeploymentTarget,
    operation: PublicOperation,
) -> String {
    if operation.kind() == PublicRouteKind::Simulation {
        format!(
            "{}/clients/*/{}",
            target.simulation_prefix(),
            operation.segment()
        )
    } else {
        let lane = match operation.kind() {
            PublicRouteKind::Control => "control",
            PublicRouteKind::Inspection => "inspection",
            PublicRouteKind::Mutation => "mutation",
            PublicRouteKind::Simulation => {
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

pub fn encode_message<M: Message>(
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

pub fn decode_request<M: Message + Default>(
    payload: &[u8],
    operation: &str,
    limits: &PublicTransportLimits,
) -> Result<M, PublicTransportError> {
    decode_message(payload, operation, limits)
}

pub fn decode_message<M: Message + Default>(
    payload: &[u8],
    operation: &str,
    limits: &PublicTransportLimits,
) -> Result<M, PublicTransportError> {
    let maximum = limits
        .response_limit(operation)
        .max(limits.request_limit(operation));
    if payload.len() > maximum {
        return Err(PublicTransportError::BodyTooLarge {
            operation: operation.to_owned(),
            bytes: payload.len(),
            maximum,
        });
    }
    M::decode(payload).map_err(|error| PublicTransportError::Decode {
        operation: operation.to_owned(),
        detail: error.to_string(),
    })
}

pub fn bounded_error_detail(detail: &str) -> String {
    if detail.len() <= MAX_PUBLIC_ERROR_BYTES {
        return detail.to_owned();
    }
    let mut end = MAX_PUBLIC_ERROR_BYTES;
    while !detail.is_char_boundary(end) {
        end -= 1;
    }
    detail[..end].to_owned()
}

pub fn validate_subscription_admission(
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

pub fn validate_subscription_request(
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

pub fn validate_state_initial_record(
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

pub fn validate_subscription_record(
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

pub fn subscription_key(
    target: &DeploymentTarget,
    principal: &str,
    subscription_id: &[u8],
) -> String {
    format!(
        "{}/clients/{principal}/observations/{}",
        target.session_prefix(),
        hex_bytes(subscription_id)
    )
}

pub async fn cancel_session_subscriptions(
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

pub fn malformed_client(
    operation: &str,
    error: impl std::fmt::Display,
) -> PublicTransportError {
    PublicTransportError::Malformed {
        operation: operation.to_owned(),
        detail: error.to_string(),
    }
}

// ===== Public re-exports =====
//
// Client types only. The server-side types (PublicSessionServer, PrincipalPolicy,
// PublicSessionBackend, PublicSimulationBackend, PublicSimulationContext,
// PublicBindingContext, OperationServerContext) live in
// `phoxal_supervisor::runtime::transport` and are not re-exported here. They
// are no longer part of the SDK's public surface; consumers that need
// server implementation hooks must depend on the supervisor crate directly.
pub use client::{
    DiscoveryEvent, PublicSessionConfig, PublicSessionConnection, PublicSessionTransport,
    PublicSubscription, PublicTlsCredentials, PublicTransportLimits, PublicTransportSecurity,
    SupervisorWatch,
};
// Shared client/server error type (used by both halves; defined in `client.rs`).
pub use client::PublicTransportError;
