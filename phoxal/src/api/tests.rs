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

// ---- dynamic / parameterized topics ----------------------------------------

#[test]
fn dynamic_topic_builder_fills_the_key_template() {
    let topic = api::topic::new()
        .component()
        .motor_command("front_left_drive", "motor");
    assert_eq!(
        topic.key(),
        "component/front_left_drive/motor/motor/command"
    );
    let enc = api::topic::new()
        .component()
        .encoder_sample("front_left_drive", "encoder");
    assert_eq!(
        enc.key(),
        "component/front_left_drive/encoder/encoder/sample"
    );
}

#[test]
fn dynamic_topic_contract_body_topic_is_the_template() {
    assert_eq!(
        <api::component::MotorCommand as ContractBody>::TOPIC,
        "component/{instance}/motor/{capability}/command"
    );
    assert_eq!(
        <api::component::MotorCommand as ContractBody>::FAMILY,
        "component::MotorCommand"
    );
}

#[test]
fn dynamic_topic_wildcard_is_subscribe_only() {
    let concrete = api::topic::new().component().motor_command("base", "motor");
    assert!(concrete.publish_key().is_ok());

    let wildcard = api::topic::new().component().motor_command("*", "motor");
    assert_eq!(wildcard.key(), "component/*/motor/motor/command");
    assert!(wildcard.publish_key().is_err());
}

// ---- API inheritance (`extends`, D61) --------------------------------------

mod extends {
    use crate::api::y2026_1 as v1;
    use crate::api::y2026_2 as v2;
    use crate::api::{ApiVersion, ContractBody};

    #[test]
    fn child_version_has_its_own_id() {
        assert_eq!(<v2::Api as ApiVersion>::ID, "y2026_2");
    }

    #[test]
    fn inherited_type_is_a_fresh_type_with_the_same_family_and_topic() {
        // Same FAMILY/TOPIC consts as the parent...
        assert_eq!(
            <v2::localize::LocalizationState as ContractBody>::FAMILY,
            <v1::localize::LocalizationState as ContractBody>::FAMILY,
        );
        assert_eq!(
            <v2::localize::LocalizationState as ContractBody>::TOPIC,
            <v1::localize::LocalizationState as ContractBody>::TOPIC,
        );
        // ...but bound to a different API version.
        assert_eq!(
            <<v2::localize::LocalizationState as ContractBody>::Api as ApiVersion>::ID,
            "y2026_2",
        );
        assert_eq!(
            <<v1::localize::LocalizationState as ContractBody>::Api as ApiVersion>::ID,
            "y2026_1",
        );
    }

    #[test]
    fn inherited_type_is_wire_identical_to_the_parent() {
        // The parent struct is re-emitted verbatim, so the same field values
        // serialize byte-for-byte identically across versions (D61).
        let v1_body = v1::localize::LocalizationState {
            x_m: 1.0,
            y_m: 2.0,
            yaw_rad: 0.5,
            confidence: 0.9,
        };
        let v2_body = v2::localize::LocalizationState {
            x_m: 1.0,
            y_m: 2.0,
            yaw_rad: 0.5,
            confidence: 0.9,
        };
        let v1_bytes = rmp_serde::to_vec_named(&v1_body).unwrap();
        let v2_bytes = rmp_serde::to_vec_named(&v2_body).unwrap();
        assert_eq!(v1_bytes, v2_bytes);

        // And a v1 payload decodes into the v2 type (wire-compatible).
        let decoded: v2::localize::LocalizationState = rmp_serde::from_slice(&v1_bytes).unwrap();
        assert_eq!(decoded, v2_body);
    }

    #[test]
    fn overridden_type_replaces_the_parent_but_keeps_family_and_topic() {
        // y2026_2 overrides drive::Target with an extra field, same family/topic.
        let target = v2::drive::Target {
            linear_x_mps: 0.3,
            angular_z_radps: 0.1,
            curvature_limit_radpm: Some(2.0),
        };
        assert_eq!(target.curvature_limit_radpm, Some(2.0));
        assert_eq!(
            <v2::drive::Target as ContractBody>::FAMILY,
            <v1::drive::Target as ContractBody>::FAMILY,
        );
        assert_eq!(
            <v2::drive::Target as ContractBody>::TOPIC,
            <v1::drive::Target as ContractBody>::TOPIC,
        );
    }

    #[test]
    fn inherited_type_reflects_an_overridden_dependency() {
        // `drive::State` embeds `drive::Target`. y2026_2 overrides `Target`, so the
        // inherited `y2026_2::drive::State` embeds the NEW Target (extra field) and
        // is NOT byte-identical to y2026_1's — correct versioning (a contract
        // changes with its dependencies).
        let v1_state = v1::drive::State {
            target: v1::drive::Target {
                linear_x_mps: 0.0,
                angular_z_radps: 0.0,
            },
            limited_target: v1::drive::Target {
                linear_x_mps: 0.0,
                angular_z_radps: 0.0,
            },
            actuator_authority: v1::drive::ActuatorAuthority::Active,
            stop_reason: None,
        };
        let v2_state = v2::drive::State {
            target: v2::drive::Target {
                linear_x_mps: 0.0,
                angular_z_radps: 0.0,
                curvature_limit_radpm: None,
            },
            limited_target: v2::drive::Target {
                linear_x_mps: 0.0,
                angular_z_radps: 0.0,
                curvature_limit_radpm: None,
            },
            actuator_authority: v2::drive::ActuatorAuthority::Active,
            stop_reason: None,
        };
        let v1_bytes = rmp_serde::to_vec_named(&v1_state).unwrap();
        let v2_bytes = rmp_serde::to_vec_named(&v2_state).unwrap();
        assert_ne!(
            v1_bytes, v2_bytes,
            "the inherited State embeds the overridden Target, so it changes with it"
        );
    }

    #[test]
    fn new_family_exists_only_in_the_child() {
        assert_eq!(
            <v2::battery::State as ContractBody>::FAMILY,
            "battery::State"
        );
        assert_eq!(<v2::battery::State as ContractBody>::TOPIC, "battery/state");
    }

    #[test]
    fn child_topic_builders_cover_inherited_overridden_and_new_families() {
        // inherited
        assert_eq!(v2::topic::new().localize().state().key(), "localize/state");
        assert_eq!(v2::topic::new().motor().command().key(), "motor/command");
        // overridden
        assert_eq!(v2::topic::new().drive().target().key(), "drive/target");
        // new
        assert_eq!(v2::topic::new().battery().state().key(), "battery/state");
    }
}

// A self-contained three-version tree exercising multi-level inheritance
// (`vc extends vb extends va`) without touching the production api tree.
mod multi_level {
    use crate::api::{ApiVersion, ContractBody};

    crate::phoxal_api_tree! {
        version va {
            sensor {
                struct Reading { value_a: f32 }
                topic reading: pubsub Reading;
            }
        }
        version vb extends va {
            beacon {
                struct Ping { seq: u64 }
                topic ping: pubsub Ping;
            }
        }
        version vc extends vb {
            sensor {
                struct Reading { value_a: f32, value_b: f32 }
                topic reading: pubsub Reading;
            }
        }
    }

    #[test]
    fn transitive_inheritance_carries_through_two_levels() {
        // `beacon` is declared in vb and inherited by vc (two levels deep).
        assert_eq!(<vc::beacon::Ping as ContractBody>::TOPIC, "beacon/ping");
        assert_eq!(<vc::beacon::Ping as ContractBody>::FAMILY, "beacon::Ping");
        assert_eq!(
            <<vc::beacon::Ping as ContractBody>::Api as ApiVersion>::ID,
            "vc"
        );

        // `sensor::Reading` is inherited unchanged into vb, then overridden in vc.
        assert_eq!(
            <vb::sensor::Reading as ContractBody>::TOPIC,
            "sensor/reading"
        );
        let _vc_reading = vc::sensor::Reading {
            value_a: 1.0,
            value_b: 2.0,
        };
        assert_eq!(vc::topic::new().beacon().ping().key(), "beacon/ping");
        assert_eq!(vc::topic::new().sensor().reading().key(), "sensor/reading");
    }
}
