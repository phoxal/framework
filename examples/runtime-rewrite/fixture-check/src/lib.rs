#[cfg(test)]
mod tests {
    //! Pure DTO import + field-shape assertions for the maintained MuJoCo
    //! fixture.
    //!
    //! The inert record family (`RobotDocument`, `ComponentDocument`,
    //! `NativeTargetKind`, etc.) lives in
    //! `phoxal_artifact_format::document`; this test only decodes
    //! shared shapes and inspects fields. Authored-file *parsing*
    //! happens here too because [`ComponentDocument::parse`] and
    //! [`RobotDocument::parse`] are inert decode methods on the
    //! shared types. Semantic authored-document *validation* lives
    //! in `cargo-phoxal`'s `project::document` module, which is a
    //! private module of the project tool and therefore cannot be
    //! reached from this example crate. The same maintained YAML
    //! fixtures are exercised by `parse_and_validate_accepts_the_maintained_fixtures`
    //! in `cargo-phoxal`.

    use std::fs;
    use std::path::{Path, PathBuf};

    use phoxal_artifact_format::document::{ComponentDocument, NativeTargetKind, RobotDocument};

    fn fixture_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
    }

    fn read(relative: impl AsRef<Path>) -> String {
        let path = fixture_root().join(relative);
        fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
    }

    #[test]
    fn repeated_component_fixture_uses_the_current_authored_binding_shape() {
        assert!(
            !fixture_root()
                .join("../components/bench-motor/src/lib.rs")
                .exists()
        );
        assert!(
            !fixture_root()
                .join("../components/bench-imu/src/lib.rs")
                .exists()
        );

        let motor = ComponentDocument::parse(&read("../components/bench-motor/component.yaml"))
            .expect("motor component definition parses");
        assert_eq!(motor.model.file, Path::new("model.xml"));
        assert_eq!(motor.model.root_body, "mount");
        assert_eq!(
            motor.capabilities["motor"].target.kind,
            NativeTargetKind::Actuator
        );
        assert_eq!(motor.capabilities["motor"].target.id, "motor");
        assert_eq!(
            motor.capabilities["motor"].joint.as_deref(),
            Some("motor_joint")
        );
        assert_eq!(
            motor.capabilities["encoder"].target.kind,
            NativeTargetKind::Joint
        );

        let imu = ComponentDocument::parse(&read("../components/bench-imu/component.yaml"))
            .expect("IMU component definition parses");
        assert_eq!(imu.model.file, Path::new("model.xml"));
        assert_eq!(imu.model.root_body, "mount");
        assert_eq!(
            imu.capabilities["accelerometer"].target.kind,
            NativeTargetKind::Site
        );
        assert_eq!(
            imu.capabilities["accelerometer"].signals["acceleration"],
            "accelerometer"
        );

        let robot = RobotDocument::parse(&read("../robots/workspace-robot/robot.yaml"))
            .expect("robot definition parses");
        assert_eq!(robot.robot.model.as_deref(), Some(Path::new("model.xml")));
        assert_eq!(robot.robot.components["left"].mount_site, "left_mount");
        assert_eq!(robot.robot.components["right"].mount_site, "right_mount");
        assert_eq!(robot.robot.components["imu"].mount_site, "imu_mount");
        assert_eq!(
            read("../robots/workspace-robot/simulation/scene.xml")
                .lines()
                .find(|line| line.contains("<option"))
                .expect("scene has native global options")
                .trim(),
            "<option timestep=\"0.001\" gravity=\"0 0 -9.81\" integrator=\"implicitfast\" solver=\"Newton\"/>"
        );
    }
}
