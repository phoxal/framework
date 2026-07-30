use phoxal::model::{Robot, source};

#[test]
fn canonical_robot_serves_runtime_and_simulator_consumers() -> anyhow::Result<()> {
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
    let robot: source::robot::v0::Manifest = serde_yaml::from_str(
        "schema: robot/v0\nrobot:\n  id: bot\n  namespace: test\n  kinematic: \
         { kind: omnidirectional, actuators: [], encoders: [] }\n  motion_limits: \
         { max_linear_speed_mps: 1.0, max_angular_speed_radps: 1.0 }\n  components: {}\n",
    )
    .expect("robot/v0 DTO parses independently");
    let component: source::component::v0::Manifest =
        serde_yaml::from_str("schema: component/v0\ncapabilities: {}\n")
            .expect("component/v0 DTO parses independently");
    let simulation: source::simulation::v0::Manifest =
        serde_yaml::from_str("schema: simulation/v0\ncapabilities: {}\n")
            .expect("simulation/v0 DTO parses independently");

    assert_eq!(robot.schema, source::robot::v0::Schema::V0);
    assert_eq!(component.schema, source::component::v0::Schema::V0);
    assert_eq!(simulation.schema, source::simulation::v0::Schema::V0);
}

#[test]
fn document_kinds_expose_independent_schema_axes() {
    fn robot_schema(_: source::robot::v0::Schema) {}
    fn component_schema(_: source::component::v0::Schema) {}
    fn simulation_schema(_: source::simulation::v0::Schema) {}

    robot_schema(source::robot::v0::Schema::V0);
    component_schema(source::component::v0::Schema::V0);
    simulation_schema(source::simulation::v0::Schema::V0);
}
