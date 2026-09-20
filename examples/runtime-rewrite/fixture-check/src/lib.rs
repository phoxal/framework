#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use phoxal::artifact::document::{ComponentDocument, NativeTargetKind, RobotDocument};

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

        let motor: ComponentDocument =
            serde_yaml::from_str(&read("../components/bench-motor/component.yaml"))
                .expect("motor component definition parses");
        let ComponentDocument::V0 {
            model,
            capabilities,
            ..
        } = motor;
        assert_eq!(model.file, Path::new("model.xml"));
        assert_eq!(model.root_body, "mount");
        assert_eq!(
            capabilities["motor"].target.kind,
            NativeTargetKind::Actuator
        );
        assert_eq!(capabilities["motor"].target.id, "motor");
        assert_eq!(capabilities["motor"].joint.as_deref(), Some("motor_joint"));
        assert_eq!(
            capabilities["encoder"].target.kind,
            NativeTargetKind::Joint
        );

        let imu: ComponentDocument =
            serde_yaml::from_str(&read("../components/bench-imu/component.yaml"))
                .expect("IMU component definition parses");
        let ComponentDocument::V0 {
            model,
            capabilities,
            ..
        } = imu;
        assert_eq!(model.file, Path::new("model.xml"));
        assert_eq!(model.root_body, "mount");
        assert_eq!(
            capabilities["accelerometer"].target.kind,
            NativeTargetKind::Site
        );
        assert_eq!(
            capabilities["accelerometer"].signals["acceleration"],
            "accelerometer"
        );

        let robot: RobotDocument =
            serde_yaml::from_str(&read("../robots/workspace-robot/robot.yaml"))
                .expect("robot definition parses");
        let RobotDocument::V0 { robot, .. } = robot;
        assert_eq!(robot.model.as_deref(), Some(Path::new("model.xml")));
        assert_eq!(robot.components["left"].mount_site, "left_mount");
        assert_eq!(robot.components["right"].mount_site, "right_mount");
        assert_eq!(robot.components["imu"].mount_site, "imu_mount");
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
