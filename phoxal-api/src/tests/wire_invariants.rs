use phoxal_bus::ProducerId;
use serde_json::{Value, json};

fn rejects_on_both_codecs<T>(value: Value)
where
    T: serde::de::DeserializeOwned,
{
    assert!(serde_json::from_value::<T>(value.clone()).is_err());
    let bytes = rmp_serde::to_vec_named(&value).expect("malformed fixture must encode");
    assert!(rmp_serde::from_slice::<T>(&bytes).is_err());
}

fn producer() -> ProducerId {
    ProducerId::try_from((1_u128 << 124) | 1).expect("canonical test producer")
}

#[test]
fn joint_rejects_non_finite_json_and_messagepack() {
    rejects_on_both_codecs::<crate::domains::v0_2::joint::JointState>(json!({
        "position_rad": null,
        "velocity_radps": 0.0,
        "effort_nm": null,
    }));
}

#[test]
fn frame_rejects_invalid_ids_translation_and_quaternion() {
    rejects_on_both_codecs::<crate::domains::v0_2::frame::FrameTransform>(json!({
        "parent_frame_id": "map/base",
        "child_frame_id": "base",
        "translation_m": [0.0, 0.0, 0.0],
        "rotation_quat_xyzw": [0.0, 0.0, 0.0, 1.0],
        "stamp": null,
    }));
    rejects_on_both_codecs::<crate::domains::v0_2::frame::FrameTransform>(json!({
        "parent_frame_id": "map",
        "child_frame_id": "base",
        "translation_m": [null, 0.0, 0.0],
        "rotation_quat_xyzw": [0.0, 0.0, 0.0, 1.0],
        "stamp": null,
    }));
    rejects_on_both_codecs::<crate::domains::v0_2::frame::FrameTransform>(json!({
        "parent_frame_id": "map",
        "child_frame_id": "base",
        "translation_m": [0.0, 0.0, 0.0],
        "rotation_quat_xyzw": [0.0, 0.0, 0.0, 0.0],
        "stamp": null,
    }));
}

#[test]
fn odometry_and_localization_reject_noncanonical_values() {
    rejects_on_both_codecs::<crate::domains::v0_2::odometry::State>(json!({
        "x_m": 0.0,
        "y_m": 0.0,
        "yaw_rad": 4.0,
        "linear_x_mps": 0.0,
        "angular_z_radps": 0.0,
    }));
    rejects_on_both_codecs::<crate::domains::v0_2::localize::LocalizationState>(json!({
        "x_m": 0.0,
        "y_m": 0.0,
        "yaw_rad": 0.0,
        "confidence": 1.5,
    }));
}

#[test]
fn navigation_rejects_invalid_request_operation_pose_path_and_frontier() {
    rejects_on_both_codecs::<crate::domains::v0_2::navigation::RequestId>(json!({
        "value": "bad/id",
    }));
    assert!(crate::domains::v0_2::navigation::NavigationOperationId::new(producer(), 0).is_none());
    let operation = crate::domains::v0_2::navigation::NavigationOperationId::new(producer(), 1)
        .expect("nonzero operation sequence");
    let mut candidate = serde_json::to_value(&operation).unwrap();
    let object = candidate.as_object_mut().unwrap();
    object.insert("linear_x_mps".into(), Value::Null);
    object.insert("angular_z_radps".into(), json!(0.0));
    rejects_on_both_codecs::<crate::domains::v0_2::navigation::Candidate>(candidate);
    rejects_on_both_codecs::<crate::domains::v0_2::navigation::Pose>(json!({
        "x_m": 0.0,
        "y_m": 0.0,
        "yaw_rad": 4.0,
    }));
    rejects_on_both_codecs::<crate::domains::v0_2::navigation::Path>(json!({
        "poses": [],
        "map_revision": null,
    }));
    rejects_on_both_codecs::<crate::domains::v0_2::navigation::Frontier>(json!({
        "x_m": 0.0,
        "y_m": 0.0,
        "score": 2.0,
        "size": 1,
    }));
}
