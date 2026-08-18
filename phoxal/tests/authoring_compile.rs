// This target is test code end to end, which the workspace panic gate exempts
// through `clippy.toml`. That exemption only reaches code lexically inside a
// `#[test]` function, so the source-staging helpers the tests share would trip
// a gate that was never meant to cover them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use phoxal::authoring::{CompileError, SourceSet};
use phoxal::model::identity::ServiceId;

/// The official service set a caller supplies. The framework owns no list of
/// its own, so every compilation in this file states one.
fn official_services(ids: &[&str]) -> Vec<ServiceId> {
    ids.iter()
        .map(|id| ServiceId::new(*id).expect("an official service id is a token"))
        .collect()
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(1)
        .expect("this crate sits at phoxal/ under the workspace root")
        .to_path_buf()
}

fn sources(project_root: &Path) -> SourceSet {
    let manifest =
        phoxal::authoring::source::robot::Manifest::load(project_root.join("robot.yaml"))
            .expect("repository robot manifest must parse");
    let phoxal::authoring::source::robot::Manifest::V0(manifest) = manifest;
    let workspace = workspace_root();
    let component_roots = manifest
        .used_component_types()
        .into_iter()
        .map(|component_type| {
            let root = if component_type == "wheel_drive" {
                workspace.join("examples/hello-rover/components/wheel_drive")
            } else {
                workspace.join("fixture/components").join(component_type)
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

/// The fixture robot declares one instance of every component type the
/// repository authors, so compiling it exercises each authored component
/// grammar once. Variants that differ only in identity, a wheel width or a role
/// hint compile through exactly the same path and prove nothing further, which
/// is why there is one robot here rather than several.
#[test]
fn every_repository_robot_compiles_to_the_canonical_model() {
    let workspace = workspace_root();
    let roots = ["fixture/robot/rgbd-imu-diff-drive", "examples/hello-rover"];

    for relative in roots {
        let root = workspace.join(relative);
        sources(&root)
            .compile(official_services(&[]))
            .unwrap_or_else(|error| panic!("failed to compile {relative}: {error}"));
    }
}

/// The stock wheel keeps its collision on the rotating rotor: Webots needs the
/// bounding object and `rubber_wheel` contact material on that same physical
/// link. The footprint compiler must recognize that this particular cylinder
/// is unchanged by its axis-centered continuous rotation instead of forcing a
/// consumer to fork the component or stripping real collision geometry.
#[test]
fn stock_ddsm115_compiles_with_its_rotating_collision_and_safety_footprint() {
    let project = tempfile::tempdir().expect("temporary robot project");
    std::fs::write(
        project.path().join("robot.yaml"),
        r#"schema: phoxal/robot/v0
robot:
  id: ddsm115-footprint
  structure: structure.urdf
  motion_limits:
    max_linear_speed_mps: 0.6
    max_angular_speed_radps: 2.0
  kinematic:
    kind: omnidirectional
    actuators: [drive.motor]
    encoders: [drive.encoder]
  components:
    drive:
      component: ddsm115
      mount_link: wheel_mount
"#,
    )
    .expect("robot manifest");
    std::fs::write(
        project.path().join("structure.urdf"),
        r#"<?xml version="1.0" ?>
<robot name="ddsm115_footprint">
  <link name="base_footprint" />
  <link name="base_link" />
  <joint name="base_joint" type="fixed">
    <parent link="base_footprint" />
    <child link="base_link" />
  </joint>
  <link name="wheel_mount" />
  <joint name="wheel_mount_joint" type="fixed">
    <parent link="base_link" />
    <child link="wheel_mount" />
    <origin xyz="0 0.2 0" rpy="1.5708 0 0" />
  </joint>
</robot>
"#,
    )
    .expect("robot structure");

    let mut source_set = sources(project.path());
    source_set.component_roots.insert(
        "ddsm115".to_string(),
        workspace_root().join("components/ddsm115"),
    );
    let compiled = source_set
        .compile(official_services(&[]))
        .expect("the stock DDSM115 must compile without consumer overrides");
    let component = compiled
        .robot()
        .component("drive")
        .expect("mounted stock component")
        .component_type();
    assert_eq!(
        component
            .structure()
            .joint("motor_joint")
            .expect("stock motor joint")
            .kind(),
        phoxal::model::structure::JointKind::Continuous
    );
    assert_eq!(
        component
            .structure()
            .link("mount")
            .expect("fixed mount")
            .collisions()
            .len(),
        0,
        "the fixed mount must not steal the wheel's physical contact body"
    );
    assert_eq!(
        component
            .structure()
            .link("rotor_link")
            .expect("rotating wheel")
            .collisions()
            .len(),
        1,
        "the rotor keeps the collision Webots rotates with rubber contact"
    );

    let expected = 0.2 + 0.022 + (0.11_f64.powi(2) + 0.04_f64.powi(2)).sqrt();
    let footprint = compiled
        .robot()
        .footprint_envelope()
        .expect("stock collision produces a safety footprint");
    assert!((footprint.radius_m - expected).abs() < 1.0e-12);
}

#[test]
fn canonical_mesh_references_are_backed_by_compiled_assets() {
    let root = workspace_root().join("fixture/robot/rgbd-imu-diff-drive");
    let compiled = sources(&root)
        .compile(official_services(&[]))
        .expect("fixture must compile");
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
                .flat_map(|component| component.component_type().structure().asset_ids()),
        )
        .map(phoxal::model::AssetId::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    assert!(model_ids.contains("components/drive_motor/meshes/drive_motor.obj"));
    assert!(model_ids.is_subset(&asset_ids));
}

#[test]
fn authored_capability_roles_are_persisted_and_ordered_in_the_robot_model() {
    let root = workspace_root().join("fixture/robot/rgbd-imu-diff-drive");
    let compiled = sources(&root)
        .compile(official_services(&[]))
        .expect("fixture must compile");
    assert_eq!(
        compiled
            .robot()
            .capabilities_with_role(phoxal::model::CapabilityRole::Safety),
        vec![phoxal::model::identity::CapabilityRef::new(
            phoxal::model::identity::ComponentInstanceId::new("front_center_tof").unwrap(),
            phoxal::model::identity::CapabilityId::new("range").unwrap(),
        )]
    );
    assert_eq!(
        serde_json::to_value(compiled.robot()).unwrap()["components"]["front_center_tof"]["roles"]
            ["range"],
        serde_json::json!(["mapping", "traversability", "safety"])
    );
}

#[test]
fn missing_canonical_mesh_is_rejected_at_compile_time() {
    let workspace = workspace_root();
    let source_root = workspace.join("fixture/robot/rgbd-imu-diff-drive");
    let component_source = workspace.join("fixture/components/drive_motor");
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
    let error = sources.compile(official_services(&[])).unwrap_err();
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
    let component_source = workspace.join("fixture/components/drive_motor");
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
    let compiled = sources.compile(official_services(&[])).unwrap();
    let component = compiled
        .robot()
        .component("front_left_drive")
        .unwrap()
        .component_type();
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
    let error = sources.compile(official_services(&[])).unwrap_err();
    assert!(
        matches!(
            &error,
            CompileError::UnresolvedComponentRoot { component_type } if component_type == "imu"
        ),
        "{error}"
    );
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

    let compiled = sources(temp.path())
        .compile(official_services(&[]))
        .expect("a behaviors/ directory must be ignored");
    assert!(
        compiled
            .assets()
            .iter()
            .all(|(id, _)| !id.as_str().starts_with("behavior")),
        "no behavior asset may be produced"
    );
    assert!(
        compiled
            .robot()
            .services()
            .all(|(id, _)| id.as_str() != "behavior"),
        "no behavior service may be declared"
    );
}

#[test]
fn declared_services_are_emitted_as_source_facts() {
    let temp = tempfile::tempdir().unwrap();
    staged_project(
        temp.path(),
        "\nservices:\n  localize:\n    config:\n      rate_hz: 10\n",
    );

    let compiled = sources(temp.path())
        .compile(official_services(&["drive"]))
        .expect("a declared source service must compile");
    // The official set the caller supplied and the authored map are merged, in
    // one ordered `services` map on the robot.
    assert_eq!(
        compiled
            .robot()
            .services()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        ["drive", "localize"]
    );
    assert_eq!(compiled.robot().service_config("drive"), None);
    assert_eq!(
        compiled.robot().service_config("localize"),
        Some(&serde_json::json!({"rate_hz": 10}))
    );
}

/// An official service the document also configures is the same service, so the
/// authored configuration is what it runs with.
#[test]
fn an_authored_config_wins_over_the_official_service_it_names() {
    let temp = tempfile::tempdir().unwrap();
    staged_project(
        temp.path(),
        "\nservices:\n  drive:\n    config:\n      max_linear_speed_mps: 0.4\n",
    );

    let compiled = sources(temp.path())
        .compile(official_services(&["drive"]))
        .expect("an authored official service must compile");
    assert_eq!(
        compiled
            .robot()
            .services()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        ["drive"]
    );
    assert_eq!(
        compiled.robot().service_config("drive"),
        Some(&serde_json::json!({"max_linear_speed_mps": 0.4}))
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

    let error = sources.compile(official_services(&[])).unwrap_err();
    assert!(matches!(error, CompileError::Document { .. }), "{error}");
    let message = error.to_string();
    assert!(message.contains("services.brain is reserved"), "{message}");
}

#[test]
fn a_service_identity_must_be_a_runtime_artifact_token() {
    let temp = tempfile::tempdir().unwrap();
    staged_project(temp.path(), "");
    let mut sources = sources(temp.path());
    staged_project(temp.path(), "\nservices:\n  Bad Service: {}\n");
    sources.project_root = temp.path().to_path_buf();
    sources.robot_manifest = temp.path().join("robot.yaml");

    let error = sources.compile(official_services(&[])).unwrap_err();
    assert!(matches!(error, CompileError::Document { .. }), "{error}");
    let message = error.to_string();
    assert!(message.contains("services.Bad Service"), "{message}");
    assert!(message.contains("lowercase ASCII"), "{message}");
}

/// Both facts that used to live beside the robot are now inside it: a driver is
/// a component whose `driver` block is present, and a simulation is folded into
/// the component type.
#[test]
fn driver_blocks_and_simulations_are_carried_by_the_robot_itself() {
    let workspace = workspace_root();
    let source_root = workspace.join("fixture/robot/rgbd-imu-diff-drive");
    let compiled = sources(&source_root)
        .compile(official_services(&[]))
        .expect("the fixture must compile");
    let driven = compiled
        .robot()
        .components()
        .filter(|component| component.instance().driver().is_some())
        .collect::<Vec<_>>();
    assert_eq!(driven.len(), 4);
    assert!(
        driven
            .iter()
            .all(|component| component.instance().component_type().as_str() == "drive_motor")
    );
    assert_eq!(driven[0].id().as_str(), "front_left_drive");
    assert!(
        compiled
            .robot()
            .component("front_left_drive")
            .and_then(|component| component.simulation())
            .is_some(),
        "a simulation is folded into the component type"
    );
}

/// The compatibility checker's stable entry answers both ways, in the two
/// shapes it promises: an acceptance carries the persisted document and the
/// asset identities, and a refusal carries the reader's own complaint.
///
/// The checker compiles one program naming exactly this function against two
/// trains, so a rename or a reshaped document here silently disables its
/// authored-source leg. This is what makes that a test failure instead.
#[test]
fn the_compatibility_probe_states_both_readings() {
    let workspace = workspace_root();
    let accepted = phoxal::authoring::probe(
        sources(&workspace.join("fixture/robot/rgbd-imu-diff-drive")),
        &official_services(&["drive"]),
    );
    assert_eq!(accepted["accepted"], serde_json::json!(true));
    assert_eq!(
        accepted["canonical"]["robot"]["schema"],
        serde_json::json!("phoxal/manifest/v0")
    );
    assert!(
        accepted["canonical"]["robot"]["services"]
            .get("drive")
            .is_some(),
        "{accepted}"
    );
    let assets = accepted["canonical"]["assets"]
        .as_array()
        .expect("the accepted reading lists its assets");
    assert!(!assets.is_empty());
    for asset in assets {
        assert!(asset["id"].is_string(), "{asset}");
        assert!(asset["bytes"].is_u64(), "{asset}");
    }

    let mut broken = sources(&workspace.join("fixture/robot/rgbd-imu-diff-drive"));
    broken.robot_manifest = workspace.join("fixture/robot/rgbd-imu-diff-drive/nowhere.yaml");
    let rejected = phoxal::authoring::probe(broken, &official_services(&[]));
    assert_eq!(rejected["accepted"], serde_json::json!(false));
    assert!(
        rejected["error"]
            .as_str()
            .expect("a refusal states its error")
            .contains("nowhere.yaml"),
        "{rejected}"
    );
}
