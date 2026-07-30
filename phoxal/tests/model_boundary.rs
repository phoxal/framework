use std::any::TypeId;

use phoxal::model::{Robot, source};

#[test]
fn canonical_consumers_do_not_match_source_versions() -> anyhow::Result<()> {
    fn runtime_consumer(robot: &Robot) -> (&str, &str) {
        (robot.robot_id(), robot.namespace())
    }

    fn simulator_consumer(
        robot: &Robot,
    ) -> Option<&phoxal::model::simulation::capability::Capability> {
        robot
            .simulation_for_component_type("drive_motor")?
            .capabilities
            .get("encoder")
    }

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../fixture/robot/rgbd-imu-diff-drive");
    let robot = Robot::read_from_dir(root)?;
    assert_eq!(runtime_consumer(&robot), ("rgbd-imu-diff-drive", "dev"));
    assert!(simulator_consumer(&robot).is_some());
    Ok(())
}

#[test]
fn exact_v0_source_manifests_are_directly_nameable() {
    fn robot_source(_: Option<source::robot::v0::Manifest>) {}
    fn component_source(_: Option<source::component::v0::Manifest>) {}
    fn simulation_source(_: Option<source::simulation::v0::Manifest>) {}

    robot_source(None);
    component_source(None);
    simulation_source(None);
}

#[test]
fn mixed_document_versions_are_an_explicit_extension_point() {
    // These are distinct document-kind version axes rather than one shared
    // graph version. A future v1 is a sibling only for the kind whose source
    // grammar changes.
    assert_ne!(
        TypeId::of::<source::robot::v0::Schema>(),
        TypeId::of::<source::component::v0::Schema>()
    );
    assert_ne!(
        TypeId::of::<source::component::v0::Schema>(),
        TypeId::of::<source::simulation::v0::Schema>()
    );
}
