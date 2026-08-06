use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use phoxal_manifest::{CompileError, SourceSet, compile};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("manifest crate has a workspace parent")
        .to_path_buf()
}

fn sources(project_root: &Path) -> SourceSet {
    let manifest = phoxal_manifest::source::robot::read_from_path(project_root.join("robot.yaml"))
        .expect("repository robot manifest must parse");
    let phoxal_manifest::source::robot::Manifest::V0(manifest) = manifest;
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
fn canonical_mesh_references_are_backed_by_compiled_assets() {
    let root = workspace_root().join("fixture/robot/rgbd-imu-diff-drive");
    let compiled = compile(sources(&root)).expect("fixture must compile");
    let asset_ids = compiled
        .assets()
        .iter()
        .map(|(id, _)| id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let model_ids = compiled
        .robot()
        .structure()
        .asset_ids()
        .chain(
            compiled
                .robot()
                .components()
                .map(|instance| {
                    compiled
                        .robot()
                        .component_for_instance(instance.id())
                        .unwrap()
                })
                .flat_map(|component| component.structure().asset_ids()),
        )
        .map(phoxal_model::AssetId::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    assert!(model_ids.contains("components/drive_motor/meshes/drive_motor.obj"));
    assert!(model_ids.is_subset(&asset_ids));
}

#[test]
fn missing_canonical_mesh_is_rejected_at_compile_time() {
    let workspace = workspace_root();
    let source_root = workspace.join("fixture/robot/rgbd-imu-diff-drive");
    let component_source = workspace.join("fixture/component/drive_motor");
    let component_root = tempfile::tempdir().unwrap();
    for file in ["component.yaml", "simulation.yaml", "structure.urdf"] {
        std::fs::copy(
            component_source.join(file),
            component_root.path().join(file),
        )
        .unwrap();
    }
    let mut sources = sources(&source_root);
    sources.component_roots.insert(
        "drive_motor".to_string(),
        component_root.path().to_path_buf(),
    );
    let error = compile(sources).unwrap_err();
    assert!(matches!(error, CompileError::Assets { .. }));
    assert!(
        error
            .to_string()
            .contains("components/drive_motor/meshes/drive_motor.obj")
    );
}

#[test]
fn relative_material_texture_is_normalized_into_the_local_component_namespace() {
    let workspace = workspace_root();
    let source_root = workspace.join("fixture/robot/rgbd-imu-diff-drive");
    let component_source = workspace.join("fixture/component/drive_motor");
    let component_root = tempfile::tempdir().unwrap();
    for file in ["component.yaml", "simulation.yaml"] {
        std::fs::copy(
            component_source.join(file),
            component_root.path().join(file),
        )
        .unwrap();
    }
    let structure = std::fs::read_to_string(component_source.join("structure.urdf"))
        .unwrap()
        .replace(
            "      </geometry>\n    </visual>",
            "      </geometry>\n      <material name=\"wood\"><texture filename=\"wood.png\" /></material>\n    </visual>",
        );
    std::fs::write(component_root.path().join("structure.urdf"), structure).unwrap();
    std::fs::create_dir(component_root.path().join("meshes")).unwrap();
    std::fs::copy(
        component_source.join("meshes/drive_motor.obj"),
        component_root.path().join("meshes/drive_motor.obj"),
    )
    .unwrap();
    std::fs::write(component_root.path().join("meshes/wood.png"), b"texture").unwrap();

    let mut sources = sources(&source_root);
    sources.component_roots.insert(
        "drive_motor".to_string(),
        component_root.path().to_path_buf(),
    );
    let compiled = compile(sources).unwrap();
    let component = compiled
        .robot()
        .component_for_instance("front_left_drive")
        .unwrap();
    assert!(
        component
            .structure()
            .asset_ids()
            .any(|id| id.as_str() == "components/drive_motor/meshes/wood.png")
    );
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

/// A copy of an otherwise-valid fixture project at `temp`, with `robot.yaml`
/// extended by `extra_top_level`.
fn staged_project(temp: &Path, extra_top_level: &str) {
    let source_root = workspace_root().join("fixture/robot/rgbd-imu-diff-drive");
    let mut yaml = std::fs::read_to_string(source_root.join("robot.yaml")).unwrap();
    yaml.push_str(extra_top_level);
    std::fs::write(temp.join("robot.yaml"), yaml).unwrap();
    std::fs::copy(
        source_root.join("structure.urdf"),
        temp.join("structure.urdf"),
    )
    .unwrap();
}

#[test]
fn an_authored_behaviors_directory_is_never_read() {
    // Nothing below `behaviors/` is a compiler input anymore. A tree that is
    // not even valid YAML proves the compiler never opens it, rather than
    // merely proving it produced no asset.
    let temp = tempfile::tempdir().unwrap();
    staged_project(temp.path(), "");
    std::fs::create_dir(temp.path().join("behaviors")).unwrap();
    std::fs::write(
        temp.path().join("behaviors/root.yaml"),
        b"\xff\xfe this is not a behavior document and must never be parsed: [",
    )
    .unwrap();

    let compiled = compile(sources(temp.path())).expect("a behaviors/ directory must be ignored");
    assert!(
        compiled
            .assets()
            .iter()
            .all(|(id, _)| !id.as_str().starts_with("behavior")),
        "no behavior asset may be produced"
    );
    assert!(
        compiled
            .participants()
            .iter()
            .all(|participant| participant.id != "behavior"),
        "no behavior participant may be declared"
    );
}

#[test]
fn a_service_claiming_the_reserved_brain_identity_fails_compilation() {
    let temp = tempfile::tempdir().unwrap();
    // The reference SourceSet is built before the reserved service is added,
    // because reading the authored document is exactly what rejects it.
    staged_project(temp.path(), "");
    let mut sources = sources(temp.path());
    staged_project(temp.path(), "\nservices:\n  brain: {}\n");
    sources.project_root = temp.path().to_path_buf();
    sources.robot_manifest = temp.path().join("robot.yaml");

    let error = compile(sources).unwrap_err();
    assert!(matches!(error, CompileError::Robot { .. }), "{error}");
    let message = error.to_string();
    assert!(message.contains("services.brain is reserved"), "{message}");
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
