use phoxal_model::Robot;

const GOLDEN: &[u8] = include_bytes!("golden/rgbd-imu-diff-drive.robot.json");

#[test]
fn golden_robot_round_trips_deterministically() {
    let robot = Robot::decode(GOLDEN).expect("golden robot must decode");
    assert_eq!(robot.encode().expect("robot must encode"), GOLDEN);
    assert_eq!(robot.robot_id(), "rgbd-imu-diff-drive");
    assert_eq!(robot.components().len(), 7);
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
}
