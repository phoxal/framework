use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use phoxal_manifest::{CompileError, SourceSet, compile};

const GOLDEN: &[u8] =
    include_bytes!("../../phoxal-model/tests/golden/rgbd-imu-diff-drive.robot.json");

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("manifest crate has a workspace parent")
        .to_path_buf()
}

fn sources(project_root: &Path) -> SourceSet {
    let manifest = phoxal_manifest::source::robot::read_from_path(project_root.join("robot.yaml"))
        .expect("repository robot manifest must parse");
    let workspace = workspace_root();
    let component_roots = manifest
        .used_component_types()
        .into_iter()
        .map(|component_type| {
            let root = if component_type == "wheel_drive" {
                workspace.join("examples/hello-rover/components/wheel_drive")
            } else {
                workspace.join("fixture/component").join(component_type)
            };
            (component_type.to_string(), root)
        })
        .collect::<BTreeMap<_, _>>();
    SourceSet {
        project_root: project_root.to_path_buf(),
        robot_manifest: project_root.join("robot.yaml"),
        component_roots,
    }
}

#[test]
fn every_repository_robot_compiles_to_the_canonical_model() {
    let workspace = workspace_root();
    let roots = [
        "fixture/robot/rgbd-diff-drive",
        "fixture/robot/rgbd-imu-diff-drive",
        "fixture/robot/rgbd-imu-gnss-outdoor",
        "fixture/robot/rgbd-imu-orb-lowres",
        "examples/hello-rover",
    ];

    for relative in roots {
        let root = workspace.join(relative);
        compile(sources(&root))
            .unwrap_or_else(|error| panic!("failed to compile {relative}: {error}"));
    }
}

#[test]
fn canonical_golden_is_pinned_to_its_authored_producer() {
    let root = workspace_root().join("fixture/robot/rgbd-imu-diff-drive");
    let compiled = compile(sources(&root)).expect("fixture must compile");
    assert_eq!(compiled.robot().encode().unwrap(), GOLDEN);
}

#[test]
fn source_set_errors_preserve_the_failed_input() {
    let root = workspace_root().join("fixture/robot/rgbd-imu-diff-drive");
    let mut sources = sources(&root);
    sources.component_roots.remove("imu");
    let error = compile(sources).unwrap_err();
    assert!(matches!(
        &error,
        CompileError::Component { component_type, .. } if component_type == "imu"
    ));
    let message = error.to_string();
    assert!(message.contains("imu"));
    assert!(message.contains("resolved component root"));
}

#[test]
fn duplicate_participant_identities_are_rejected() {
    let workspace = workspace_root();
    let source_root = workspace.join("fixture/robot/rgbd-imu-diff-drive");
    let temp = tempfile::tempdir().unwrap();
    let mut yaml = std::fs::read_to_string(source_root.join("robot.yaml")).unwrap();
    yaml.push_str("\nbehavior:\n  root: system.root\nservices:\n  behavior: {}\n");
    std::fs::write(temp.path().join("robot.yaml"), yaml).unwrap();
    std::fs::copy(
        source_root.join("structure.urdf"),
        temp.path().join("structure.urdf"),
    )
    .unwrap();
    std::fs::create_dir(temp.path().join("behaviors")).unwrap();
    std::fs::write(
        temp.path().join("behaviors/root.yaml"),
        "schema: behavior/v0\nid: system.root\nversion: \"1\"\nroot:\n  type: wait\n  id: settle\n  duration_ms: 10\n",
    )
    .unwrap();

    let error = compile(sources(temp.path())).unwrap_err();
    assert!(matches!(error, CompileError::Participants { .. }));
    assert!(
        error
            .to_string()
            .contains("duplicate participant declaration")
    );
}

#[test]
fn one_component_can_have_distinct_driver_and_simulator_participants() {
    let workspace = workspace_root();
    let source_root = workspace.join("fixture/robot/rgbd-imu-diff-drive");
    let temp = tempfile::tempdir().unwrap();
    let yaml = std::fs::read_to_string(source_root.join("robot.yaml"))
        .unwrap()
        .replacen(
            "      mount_link: front_left_wheel_mount\n",
            "      mount_link: front_left_wheel_mount\n\
             \x20\x20\x20\x20\x20\x20driver:\n\
             \x20\x20\x20\x20\x20\x20\x20\x20connection:\n\
             \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20type: can\n\
             \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20bus: 0\n\
             \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20node_id: 1\n",
            1,
        );
    std::fs::write(temp.path().join("robot.yaml"), yaml).unwrap();
    std::fs::copy(
        source_root.join("structure.urdf"),
        temp.path().join("structure.urdf"),
    )
    .unwrap();

    let compiled = compile(sources(temp.path())).expect("driver plus simulator must compile");
    let matching = compiled
        .participants()
        .iter()
        .filter(|participant| {
            participant.id == "drive_motor"
                && participant.component_instance.as_deref() == Some("front_left_drive")
        })
        .collect::<Vec<_>>();
    assert_eq!(matching.len(), 2);
    assert!(
        matching
            .iter()
            .any(|participant| { participant.kind == phoxal_manifest::ParticipantKind::Driver })
    );
    assert!(
        matching
            .iter()
            .any(|participant| { participant.kind == phoxal_manifest::ParticipantKind::Simulator })
    );
}
