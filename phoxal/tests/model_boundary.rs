use phoxal::model::Robot;

#[test]
fn canonical_robot_serves_runtime_and_simulator_consumers() -> anyhow::Result<()> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../phoxal-model/tests/golden/rgbd-imu-diff-drive.robot.json");
    let robot = Robot::decode(&std::fs::read(path)?)?;
    assert_eq!(
        (robot.robot_id(), robot.namespace()),
        ("rgbd-imu-diff-drive", "dev")
    );
    assert!(
        robot
            .simulation_for_component_type("drive_motor")
            .and_then(|simulation| simulation.capability("encoder"))
            .is_some()
    );
    Ok(())
}
