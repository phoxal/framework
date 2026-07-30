use phoxal_model::{Robot, component::CapabilityRef};

const GOLDEN: &[u8] = include_bytes!("golden/rgbd-imu-diff-drive.robot.json");

#[test]
fn golden_robot_round_trips_deterministically() {
    let robot = Robot::decode(GOLDEN).expect("golden robot must decode");
    assert_eq!(robot.encode().expect("robot must encode"), GOLDEN);
    assert_eq!(robot.robot_id(), "rgbd-imu-diff-drive");
    assert_eq!(robot.components().len(), 7);
}

#[test]
fn component_link_targets_resolve_against_the_canonical_component_structure() {
    let robot = Robot::decode(GOLDEN).expect("golden robot must decode");
    let reference = CapabilityRef::new("front_camera", "imu");
    assert_eq!(
        robot.link_target_frame(&reference).unwrap(),
        "front_camera__imu_link"
    );
}

#[test]
fn decode_rejects_unknown_fields_at_every_level() {
    let mut value: serde_json::Value = serde_json::from_slice(GOLDEN).unwrap();
    value["robot"]["motion"]["limits"]["unknown"] = serde_json::json!(true);
    let error = Robot::decode(&serde_json::to_vec(&value).unwrap()).unwrap_err();
    assert!(error.to_string().contains("unknown field"));

    let mut value: serde_json::Value = serde_json::from_slice(GOLDEN).unwrap();
    value["robot"]["structure"]["joints"][0]["unknown"] = serde_json::json!(true);
    let error = Robot::decode(&serde_json::to_vec(&value).unwrap()).unwrap_err();
    assert!(error.to_string().contains("unknown field"));

    let mut value: serde_json::Value = serde_json::from_slice(GOLDEN).unwrap();
    value["robot"]["component_types"]["drive_motor"]["structure"]["links"][1]["visuals"][0]["geometry"]
        ["unknown"] = serde_json::json!(true);
    let error = Robot::decode(&serde_json::to_vec(&value).unwrap()).unwrap_err();
    assert!(error.to_string().contains("unknown field"));
}

#[test]
fn decode_rejects_duplicate_object_fields() {
    let duplicate = br#"{"schema":"phoxal/robot/v0","schema":"phoxal/robot/v0","robot":{}}"#;
    assert!(
        Robot::decode(duplicate)
            .unwrap_err()
            .to_string()
            .contains("duplicate JSON object field")
    );
}

#[test]
fn decode_rejects_invalid_references_and_values() {
    let mut value: serde_json::Value = serde_json::from_slice(GOLDEN).unwrap();
    value["robot"]["component_instances"]["imu"]["component_type"] = serde_json::json!("missing");
    assert!(
        Robot::decode(&serde_json::to_vec(&value).unwrap())
            .unwrap_err()
            .to_string()
            .contains("unknown component type")
    );

    let mut value: serde_json::Value = serde_json::from_slice(GOLDEN).unwrap();
    value["robot"]["motion"]["limits"]["max_linear_speed_mps"] = serde_json::json!(-1.0);
    assert!(
        Robot::decode(&serde_json::to_vec(&value).unwrap())
            .unwrap_err()
            .to_string()
            .contains("max_linear_speed_mps")
    );

    let mut value: serde_json::Value = serde_json::from_slice(GOLDEN).unwrap();
    value["robot"]["structure"]["joints"][1]["parent"] =
        serde_json::json!("front_right_wheel_mount");
    value["robot"]["structure"]["joints"][2]["parent"] =
        serde_json::json!("front_left_wheel_mount");
    assert!(
        Robot::decode(&serde_json::to_vec(&value).unwrap())
            .unwrap_err()
            .to_string()
            .contains("joint cycle")
    );

    let mut value: serde_json::Value = serde_json::from_slice(GOLDEN).unwrap();
    value["robot"]["component_instances"]["front_left_drive"]["direction_signs"]["motor"] =
        serde_json::json!(2);
    assert!(
        Robot::decode(&serde_json::to_vec(&value).unwrap())
            .unwrap_err()
            .to_string()
            .contains("direction sign")
    );

    let mut value: serde_json::Value = serde_json::from_slice(GOLDEN).unwrap();
    let instance = value["robot"]["component_instances"]
        .as_object_mut()
        .unwrap()
        .remove("front_left_drive")
        .unwrap();
    value["robot"]["component_instances"]["front__left_drive"] = instance;
    value["robot"]["component_instances"]["front__left_drive"]["id"] =
        serde_json::json!("front__left_drive");
    assert!(
        Robot::decode(&serde_json::to_vec(&value).unwrap())
            .unwrap_err()
            .to_string()
            .contains("reserved separator")
    );

    let mut value: serde_json::Value = serde_json::from_slice(GOLDEN).unwrap();
    value["robot"]["structure"]["links"][0]["inertial"]["mass_kg"] = serde_json::json!(-1.0);
    assert!(
        Robot::decode(&serde_json::to_vec(&value).unwrap())
            .unwrap_err()
            .to_string()
            .contains("mass")
    );

    let mut value: serde_json::Value = serde_json::from_slice(GOLDEN).unwrap();
    value["robot"]["component_types"]["drive_motor"]["capabilities"]["motor"]["target"]["id"] =
        serde_json::json!("missing_joint");
    assert!(
        Robot::decode(&serde_json::to_vec(&value).unwrap())
            .unwrap_err()
            .to_string()
            .contains("references unknown joint 'missing_joint'")
    );

    let mut value: serde_json::Value = serde_json::from_slice(GOLDEN).unwrap();
    let inertia = &mut value["robot"]["structure"]["links"][0]["inertial"]["inertia"];
    inertia["ixx"] = serde_json::json!(1.0);
    inertia["iyy"] = serde_json::json!(1.0);
    inertia["izz"] = serde_json::json!(1.0);
    inertia["ixy"] = serde_json::json!(1.0);
    inertia["ixz"] = serde_json::json!(2.0);
    inertia["iyz"] = serde_json::json!(2.0);
    assert!(
        Robot::decode(&serde_json::to_vec(&value).unwrap())
            .unwrap_err()
            .to_string()
            .contains("positive semidefinite")
    );

    let mut value: serde_json::Value = serde_json::from_slice(GOLDEN).unwrap();
    value["robot"]["structure"]["joints"][0]["name"] = serde_json::json!("reserved__joint");
    assert!(
        Robot::decode(&serde_json::to_vec(&value).unwrap())
            .unwrap_err()
            .to_string()
            .contains("reserved separator")
    );

    let mut value: serde_json::Value = serde_json::from_slice(GOLDEN).unwrap();
    value["robot"]["component_types"]["drive_motor"]["structure"]["joints"][0]["kind"] =
        serde_json::json!("floating");
    assert!(
        Robot::decode(&serde_json::to_vec(&value).unwrap())
            .unwrap_err()
            .to_string()
            .contains("unsupported runtime kind")
    );
}

#[test]
fn unsupported_schema_is_reported_before_payload_parsing() {
    let mut value: serde_json::Value = serde_json::from_slice(GOLDEN).unwrap();
    value["schema"] = serde_json::json!("phoxal/robot/v1");
    value["robot"] = serde_json::json!({"future": true});
    assert_eq!(
        Robot::decode(&serde_json::to_vec(&value).unwrap())
            .unwrap_err()
            .to_string(),
        "unsupported robot schema 'phoxal/robot/v1'"
    );
}

#[test]
fn missing_schema_has_a_dedicated_error() {
    let mut value: serde_json::Value = serde_json::from_slice(GOLDEN).unwrap();
    value.as_object_mut().unwrap().remove("schema");
    assert_eq!(
        Robot::decode(&serde_json::to_vec(&value).unwrap())
            .unwrap_err()
            .to_string(),
        "canonical robot is missing string field 'schema'"
    );
}
