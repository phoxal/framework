use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use phoxal::api::{asset, drive};

#[test]
fn drive_target_contract_wire_shape_is_tagged_by_version() {
    assert_contract_shape(
        drive::Target::V1(drive::v1::Target {
            linear_x_mps: 1.25,
            angular_z_radps: -0.5,
        }),
        json!({
            "linear_x_mps": 1.25,
            "angular_z_radps": -0.5,
        }),
    );
}

#[test]
fn drive_state_contract_wire_shape_is_tagged_by_version() {
    assert_contract_shape(
        drive::State::V1(drive::v1::State {
            target: drive::v1::Target {
                linear_x_mps: 1.25,
                angular_z_radps: -0.5,
            },
            limited_target: drive::v1::Target {
                linear_x_mps: 0.75,
                angular_z_radps: -0.25,
            },
            actuator_authority: drive::v1::ActuatorAuthority::Degraded,
            stop_reason: Some(drive::v1::StopReason::SafetyStop),
        }),
        json!({
            "target": {
                "linear_x_mps": 1.25,
                "angular_z_radps": -0.5,
            },
            "limited_target": {
                "linear_x_mps": 0.75,
                "angular_z_radps": -0.25,
            },
            "actuator_authority": "degraded",
            "stop_reason": "safety_stop",
        }),
    );
}

#[test]
fn asset_query_contract_wire_shape_is_tagged_by_version() {
    assert_contract_shape(
        asset::GetRequest::V1(asset::v1::GetRequest {
            path: "fixture-map".to_string(),
        }),
        json!({
            "path": "fixture-map",
        }),
    );

    assert_contract_shape(
        asset::GetResponse::V1(asset::v1::GetResponse::Ok {
            bytes: vec![1, 2, 3, 5, 8],
        }),
        json!({
            "Ok": {
                "bytes": [1, 2, 3, 5, 8],
            },
        }),
    );
}

fn assert_contract_shape<T>(value: T, data: Value)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let encoded = rmp_serde::to_vec_named(&value).expect("contract should encode");
    let roundtrip: T = rmp_serde::from_slice(&encoded).expect("contract should decode");
    let structure: Value =
        rmp_serde::from_slice(&encoded).expect("contract should decode to value");

    assert_eq!(roundtrip, value);
    assert_eq!(
        structure,
        json!({
            "v": "v1",
            "data": data,
        })
    );
}
