use phoxal::bundle::FinalizedBundle;

#[test]
fn canonical_robot_serves_runtime_and_simulator_consumers() -> anyhow::Result<()> {
    let bundle = FinalizedBundle::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../fixture/bundle/rgbd-imu-diff-drive"),
    )?;
    let robot = bundle.robot();
    assert_eq!(
        (robot.robot_id(), robot.namespace()),
        ("rgbd-imu-diff-drive", "dev")
    );
    assert_eq!(robot.clock(), phoxal::model::Clock::Real);
    assert!(
        robot
            .simulation_for_component_type("drive_motor")
            .and_then(|simulation| simulation.capability("encoder"))
            .is_some()
    );
    Ok(())
}
