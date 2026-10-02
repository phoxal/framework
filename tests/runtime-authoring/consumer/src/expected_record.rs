//! The brain consumer fixture's expected compiled runtime record.
//!
//! Shared by this binary's unit test (which compares it against the
//! macro-retained artifact record) and the process acceptance test's
//! bundle construction.

use phoxal::artifact::{
    InputRecord, InputRole, MethodShape, MethodSignature, OutputRecord, OutputRole, RuntimeRecord,
};

fn state_signature(endpoint: &str, response: &str) -> MethodSignature {
    MethodSignature {
        endpoint: endpoint.to_owned(),
        service: response.to_owned(),
        method: endpoint.to_owned(),
        shape: MethodShape::Observation,
        request: "google.protobuf.Empty".to_owned(),
        response: response.to_owned(),
        retained_latest: true,
        lease_valid_for_ms: None,
    }
}

fn leased_signature(endpoint: &str, response: &str, lease_ms: u64) -> MethodSignature {
    MethodSignature {
        endpoint: endpoint.to_owned(),
        service: response.to_owned(),
        method: endpoint.to_owned(),
        shape: MethodShape::Observation,
        request: "google.protobuf.Empty".to_owned(),
        response: response.to_owned(),
        retained_latest: true,
        lease_valid_for_ms: Some(lease_ms),
    }
}

/// The complete runtime record the brain consumer fixture's binary retains.
pub fn expected_runtime_record() -> RuntimeRecord {
    RuntimeRecord::V0 {
        record: "runtime".to_owned(),
        role: None,
        period_ms: 20,
        timeout_ms: 100,
        init_timeout_ms: 1000,
        config_schema: serde_json::json!({"type": "null"}),
        inputs: vec![
            InputRecord {
                name: "start_countdown".to_owned(),
                role: InputRole::CallCompletions,
                max_age_ms: None,
                max_items: Some(16),
                max_bytes: Some(16384),
                port: Some("start".to_owned()),
                signature: Some(MethodSignature {
                    endpoint: "start".to_owned(),
                    service: "phoxal.tests.authoring.countdown.v1.Start".to_owned(),
                    // The generated provider marker spells the method by
                    // its served endpoint, matching the provider's own
                    // call-ingress record.
                    method: "start".to_owned(),
                    shape: MethodShape::Call,
                    request: "phoxal.tests.authoring.countdown.v1.StartRequest".to_owned(),
                    response: "phoxal.tests.authoring.countdown.v1.StartResponse".to_owned(),
                    retained_latest: false,
                    lease_valid_for_ms: None,
                }),
                request_fqn: None,
                response_fqn: None,
            },
            InputRecord {
                name: "countdown_finished".to_owned(),
                role: InputRole::ObservationHistory,
                max_age_ms: None,
                max_items: Some(16),
                max_bytes: Some(4096),
                port: None,
                signature: None,
                request_fqn: None,
                response_fqn: Some("phoxal.tests.authoring.countdown.v1.FinishedEvent".to_owned()),
            },
            InputRecord {
                name: "countdown_status".to_owned(),
                role: InputRole::ObservationLatest,
                max_age_ms: Some(100),
                max_items: None,
                max_bytes: Some(4096),
                port: None,
                signature: None,
                request_fqn: None,
                response_fqn: Some("phoxal.tests.authoring.countdown.v1.CountdownState".to_owned()),
            },
        ],
        transient_outputs: Vec::new(),
        service_outputs: vec![
            OutputRecord {
                name: "handled".to_owned(),
                role: OutputRole::Method,
                port: Some("handled".to_owned()),
                signature: Some(state_signature(
                    "handled",
                    "phoxal.tests.authoring.consumer.v1.ConsumedEventState",
                )),
                input: None,
                project: None,
                max_items: None,
                max_bytes: Some(16384),
                max_request_bytes: None,
                every_steps: None,
                on_change: false,
                bootstrap: true,
                valid_for_ms: None,
                timeout_ms: None,
                cancel_grace_ms: None,
            },
            OutputRecord {
                name: "mission".to_owned(),
                role: OutputRole::Method,
                port: Some("mission".to_owned()),
                signature: Some(state_signature(
                    "mission",
                    "phoxal.tests.authoring.consumer.v1.MissionState",
                )),
                input: None,
                project: None,
                max_items: None,
                max_bytes: Some(16384),
                max_request_bytes: None,
                every_steps: None,
                on_change: false,
                bootstrap: true,
                valid_for_ms: None,
                timeout_ms: None,
                cancel_grace_ms: None,
            },
            OutputRecord {
                name: "command".to_owned(),
                role: OutputRole::Method,
                port: Some("command".to_owned()),
                signature: Some(leased_signature(
                    "command",
                    "phoxal.component.actuator.v1.ActuatorSetpoint",
                    100,
                )),
                input: None,
                project: None,
                max_items: None,
                max_bytes: Some(1024),
                max_request_bytes: None,
                every_steps: None,
                on_change: false,
                bootstrap: false,
                valid_for_ms: Some(100),
                timeout_ms: None,
                cancel_grace_ms: None,
            },
        ],
    }
}
