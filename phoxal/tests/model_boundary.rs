use phoxal::bundle::RuntimeBundle;
use phoxal_fixture::staged_bundle;

/// One `manifest.json` serves every consumer, and the one lookup all of them
/// need - instance, type, and how a simulated world models that type - is a
/// single read rather than a hand-written join across three maps.
#[test]
fn one_manifest_serves_runtime_and_simulation_consumers() -> anyhow::Result<()> {
    let staged = staged_bundle();
    let bundle = RuntimeBundle::open(staged.path())?;
    let robot = bundle.robot();
    assert_eq!(robot.id().as_str(), "rgbd-imu-diff-drive");

    let drive = robot
        .component("front_left_drive")
        .ok_or_else(|| anyhow::anyhow!("the fixture mounts a driven component"))?;
    assert_eq!(drive.instance().component_type().as_str(), "drive_motor");
    // The driver block is kept in every mode: simulation is a launch decision,
    // never a bundle fact.
    assert!(drive.instance().driver().is_some());
    assert!(
        drive
            .simulation()
            .and_then(|simulation| simulation.capability("encoder"))
            .is_some()
    );
    Ok(())
}
