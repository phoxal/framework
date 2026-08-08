use phoxal_bundle::RuntimeBundle;
use phoxal_fixture::staged_bundle;

#[test]
fn canonical_robot_serves_runtime_and_simulator_consumers() -> anyhow::Result<()> {
    let staged = staged_bundle();
    let bundle = RuntimeBundle::open_verified(staged.path())?;
    let robot = bundle.robot();
    assert_eq!(robot.id().as_str(), "rgbd-imu-diff-drive");
    assert_eq!(robot.clock(), phoxal::model::Clock::Real);
    assert!(
        robot
            .simulation_for_component_type("drive_motor")
            .and_then(|simulation| simulation.capability("encoder"))
            .is_some()
    );
    Ok(())
}
