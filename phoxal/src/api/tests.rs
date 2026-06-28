//! Golden tests for the generated API layer: the version-local body shape (plain
//! payload, no `{"v":…}` wrapper — D62), the `ContractBody` consts, the
//! `ApiVersion` id, and the topic keys produced by the api-local builders.

use crate::api::y2026_1 as api;
use crate::api::{ApiVersion, ContractBody};

#[test]
fn api_version_id_is_the_dated_module_name() {
    assert_eq!(<api::Api as ApiVersion>::ID, "y2026_1");
}

#[test]
fn contract_body_consts_are_family_and_topic() {
    assert_eq!(<api::drive::State as ContractBody>::FAMILY, "drive::State");
    assert_eq!(<api::drive::State as ContractBody>::TOPIC, "drive/state");
    assert_eq!(
        <api::drive::Target as ContractBody>::FAMILY,
        "drive::Target"
    );
    assert_eq!(<api::drive::Target as ContractBody>::TOPIC, "drive/target");
    assert_eq!(
        <api::motor::Command as ContractBody>::TOPIC,
        "motor/command"
    );
    assert_eq!(
        <api::localize::LocalizationState as ContractBody>::TOPIC,
        "localize/state"
    );
}

#[test]
fn body_serializes_as_plain_payload_without_version_tag() {
    let target = api::drive::Target {
        linear_x_mps: 1.0,
        angular_z_radps: 0.5,
    };
    // MessagePack-as-JSON projection: the wire body is the plain struct — there is
    // no `v`/`data` envelope around it.
    let json = serde_json::to_value(&target).unwrap();
    assert_eq!(json["linear_x_mps"], 1.0);
    assert!(
        json.get("v").is_none(),
        "wire body must not carry a version tag"
    );
    assert!(json.get("data").is_none());
}

#[test]
fn body_round_trips_through_messagepack() {
    let state = api::drive::State {
        target: api::drive::Target {
            linear_x_mps: 0.3,
            angular_z_radps: -0.2,
        },
        limited_target: api::drive::Target {
            linear_x_mps: 0.3,
            angular_z_radps: -0.2,
        },
        actuator_authority: api::drive::ActuatorAuthority::Active,
        stop_reason: None,
    };
    let bytes = rmp_serde::to_vec_named(&state).unwrap();
    let decoded: api::drive::State = rmp_serde::from_slice(&bytes).unwrap();
    assert_eq!(state, decoded);
}

#[test]
fn topic_builder_keys_match_contract_topics() {
    assert_eq!(api::topic::new().drive().state().key(), "drive/state");
    assert_eq!(api::topic::new().drive().target().key(), "drive/target");
    assert_eq!(api::topic::new().motor().command().key(), "motor/command");
    assert_eq!(
        api::topic::new().presence().heartbeat().key(),
        "presence/heartbeat"
    );
}
