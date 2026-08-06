use std::path::Path;

use phoxal::bundle::FinalizedBundle;

/// Stage a finalized bundle from the checked-in authored fixtures.
///
/// Finalization belongs to `phoxal-cli`; the two edits it makes to the authored
/// document (pin the resolved clock, resolve the structure path into `assets/`)
/// are small enough to reproduce here, so no bundle is checked in anywhere.
fn staged_bundle() -> tempfile::TempDir {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixture");
    let project = fixture.join("robot/rgbd-imu-diff-drive");
    let bundle = tempfile::tempdir().expect("a staging directory");
    let assets = bundle.path().join("assets");

    std::fs::create_dir_all(assets.join("robot")).unwrap();
    std::fs::copy(
        project.join("structure.urdf"),
        assets.join("robot/structure.urdf"),
    )
    .unwrap();
    for component_type in ["camera_rgbd_640x480", "drive_motor", "imu", "range_tof"] {
        let source = fixture.join("component").join(component_type);
        let staged = assets.join("components").join(component_type);
        std::fs::create_dir_all(&staged).unwrap();
        for document in ["component.yaml", "simulation.yaml", "structure.urdf"] {
            std::fs::copy(source.join(document), staged.join(document)).unwrap();
        }
        for mesh in std::fs::read_dir(source.join("meshes"))
            .into_iter()
            .flatten()
        {
            let mesh = mesh.unwrap();
            std::fs::create_dir_all(staged.join("meshes")).unwrap();
            std::fs::copy(mesh.path(), staged.join("meshes").join(mesh.file_name())).unwrap();
        }
    }
    std::fs::write(
        bundle.path().join("robot.yaml"),
        std::fs::read_to_string(project.join("robot.yaml"))
            .unwrap()
            .replacen("schema: robot/v0", "schema: robot/v0\nclock: real", 1)
            .replacen(
                "structure: structure.urdf",
                "structure: robot/structure.urdf",
                1,
            ),
    )
    .unwrap();
    bundle
}

#[test]
fn canonical_robot_serves_runtime_and_simulator_consumers() -> anyhow::Result<()> {
    let staged = staged_bundle();
    let bundle = FinalizedBundle::load(staged.path())?;
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
