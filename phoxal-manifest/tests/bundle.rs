//! The finalized-bundle contract: what a bundle looks like on disk, and what
//! loading one guarantees.
//!
//! Every test here stages its own bundle from the checked-in authored sources
//! (`fixture/robot`, `fixture/component`) into a temporary directory and then
//! loads it back. That round trip - compile, stage, load - is the contract:
//! the layout is proven by producing it, not by comparing against a frozen
//! copy of it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use phoxal_manifest::bundle::{
    BundleError, BundleFile, BundleResolver, FinalizedBundle, InvalidBundlePath,
};
use phoxal_manifest::source::robot::v0::Clock;
use phoxal_manifest::{CompileError, SourceSet, source};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("manifest crate has a workspace parent")
        .to_path_buf()
}

fn sources(project_root: &Path) -> SourceSet {
    let manifest = source::robot::Manifest::load(project_root.join("robot.yaml"))
        .expect("fixture robot manifest must parse");
    let source::robot::Manifest::V0(manifest) = manifest;
    let workspace = workspace_root();
    SourceSet {
        project_root: project_root.to_path_buf(),
        robot_manifest: project_root.join("robot.yaml"),
        component_roots: manifest
            .used_component_types()
            .into_iter()
            .map(|component_type| {
                (
                    component_type.to_string(),
                    workspace.join("fixture/component").join(component_type),
                )
            })
            .collect(),
    }
}

/// Write one finalized bundle from authored sources.
///
/// This mirrors what `phoxal-cli` finalization does; the framework owns only
/// the reader, so this lives in the tests that pin the shape.
fn stage(project_root: &Path, clock: Clock, bundle_root: &Path) {
    let sources = sources(project_root);
    let compiled = sources
        .clone()
        .compile()
        .expect("fixture project must compile");
    let source::robot::Manifest::V0(mut manifest) =
        source::robot::Manifest::load(&sources.robot_manifest).expect("manifest must parse");

    let assets = bundle_root.join("assets");
    std::fs::create_dir_all(assets.join("robot")).unwrap();
    std::fs::copy(
        project_root.join(&manifest.robot.structure),
        assets.join("robot/structure.urdf"),
    )
    .unwrap();
    for (component_type, root) in &sources.component_roots {
        let staged = assets.join("components").join(component_type);
        std::fs::create_dir_all(&staged).unwrap();
        for document in ["component.yaml", "structure.urdf", "simulation.yaml"] {
            if root.join(document).is_file() {
                std::fs::copy(root.join(document), staged.join(document)).unwrap();
            }
        }
    }
    for (id, bytes) in compiled.assets().iter() {
        let path = assets.join(id.as_str());
        std::fs::create_dir_all(path.parent().expect("an asset has a parent")).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    // Finalization resolves every authored path into the bundle's own layout
    // and pins the clock it resolved.
    manifest.extends.clear();
    manifest.clock = clock;
    manifest.robot.structure = PathBuf::from("robot/structure.urdf");
    let document = source::robot::Manifest::V0(manifest);
    std::fs::write(
        bundle_root.join("robot.yaml"),
        serde_yaml::to_string(&document).unwrap(),
    )
    .unwrap();

    std::fs::create_dir_all(bundle_root.join("bin")).unwrap();
    std::fs::write(bundle_root.join("bin/brain"), b"not a real executable").unwrap();
}

/// One finalized bundle, staged for the test that asked for it and removed
/// with the returned directory.
fn staged_bundle() -> tempfile::TempDir {
    let bundle = tempfile::tempdir().unwrap();
    stage(
        &workspace_root().join("fixture/robot/rgbd-imu-diff-drive"),
        Clock::Real,
        bundle.path(),
    );
    bundle
}

/// Rewrite a staged bundle's finalized robot document in place.
fn rewrite_manifest(bundle: &tempfile::TempDir, edit: impl FnOnce(String) -> String) {
    let path = bundle.path().join("robot.yaml");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, edit(text)).unwrap();
}

#[test]
fn a_finalized_bundle_loads_without_any_compiled_model_document() {
    let staged = staged_bundle();
    let bundle = FinalizedBundle::load(staged.path()).expect("the staged bundle must load");
    let robot = bundle.robot();

    assert_eq!(
        (
            robot.identity().id().as_str(),
            robot.identity().namespace().as_str()
        ),
        ("rgbd-imu-diff-drive", "dev")
    );
    assert_eq!(robot.clock(), phoxal_model::Clock::Real);
    // Naming the instances rather than counting them states what the authored
    // document declares, so adding or renaming one fails here describing the
    // change instead of reporting that a number moved.
    assert_eq!(
        robot
            .components()
            .map(|instance| instance.id().as_str())
            .collect::<Vec<_>>(),
        [
            "front_camera",
            "front_center_tof",
            "front_left_drive",
            "front_right_drive",
            "gnss",
            "imu",
            "rear_left_drive",
            "rear_right_drive",
        ]
    );
    assert!(
        robot
            .simulation_for_component_type("drive_motor")
            .and_then(|simulation| simulation.capability("encoder"))
            .is_some()
    );
    assert!(
        !staged.path().join("robot.json").exists(),
        "the bundle carries no duplicate compiled model"
    );
    assert!(bundle.router_config().is_none());
    assert!(
        bundle
            .participants()
            .iter()
            .any(|participant| participant.id == "drive_motor"),
        "the finalized declarations survive the round trip"
    );
}

#[test]
fn the_participant_resolver_serves_declared_assets_and_nothing_else() {
    let staged = staged_bundle();
    let bundle = FinalizedBundle::load(staged.path()).expect("the staged bundle must load");
    let assets = bundle.assets();
    let ids = assets
        .ids()
        .map(phoxal_model::AssetId::as_str)
        .collect::<BTreeSet<_>>();

    assert!(ids.contains("components/drive_motor/meshes/drive_motor.obj"));
    assert!(ids.contains("components/drive_motor/component.yaml"));
    assert!(ids.contains("robot/structure.urdf"));
    // The participant half of the bundle stops at `assets/`: binaries and the
    // finalized robot document are not reachable through it at all.
    for unreachable in ["bin/brain", "robot.yaml", "../robot.yaml"] {
        assert!(
            phoxal_model::AssetId::new(unreachable)
                .ok()
                .and_then(|id| assets.path(&id).ok())
                .is_none(),
            "{unreachable}"
        );
    }
}

#[test]
fn the_bundle_resolver_serves_the_whole_bundle_safely() {
    let staged = staged_bundle();
    let resolver = BundleResolver::index(staged.path(), 4 * 1024 * 1024).expect("index");
    let paths = resolver
        .paths()
        .map(phoxal_manifest::bundle::BundlePath::as_str)
        .collect::<BTreeSet<_>>();

    assert!(paths.contains("robot.yaml"));
    assert!(paths.contains("bin/brain"));
    assert!(paths.contains("assets/components/drive_motor/structure.urdf"));

    let BundleFile::Found { bytes, .. } = resolver.get("bin/brain").expect("readable") else {
        panic!("the supervisor resolver serves bin/");
    };
    assert_eq!(bytes, b"not a real executable");
    assert!(matches!(
        resolver.get("assets/../bin/brain").unwrap(),
        BundleFile::InvalidPath(InvalidBundlePath::Traversal)
    ));
    assert!(matches!(
        resolver.get("assets").unwrap(),
        BundleFile::Missing
    ));

    let bounded = BundleResolver::index(staged.path(), 8).expect("index");
    assert!(matches!(
        bounded.get("bin/brain").unwrap(),
        BundleFile::TooLarge { .. }
    ));
}

#[test]
fn an_unresolved_extends_is_refused() {
    let bundle = staged_bundle();
    rewrite_manifest(&bundle, |text| {
        text.replace("clock: real\n", "clock: real\nextends: [base.robot.yaml]\n")
    });

    let error = FinalizedBundle::load(bundle.path()).expect_err("extends must be resolved");
    assert!(
        matches!(error, BundleError::UnresolvedExtends { .. }),
        "{error}"
    );
}

#[test]
fn a_foreign_schema_tag_is_refused() {
    let bundle = staged_bundle();
    rewrite_manifest(&bundle, |text| {
        text.replacen("schema: phoxal/robot/v0", "schema: phoxal/robot/v1", 1)
    });

    let error = FinalizedBundle::load(bundle.path()).expect_err("an unknown schema must fail");
    assert!(
        format!("{error:#}").contains("phoxal/robot/v1"),
        "the schema tag selects the variant: {error:#}"
    );
}

#[test]
fn a_structure_path_outside_the_bundle_is_refused() {
    let bundle = staged_bundle();
    rewrite_manifest(&bundle, |text| {
        text.replacen(
            "structure: robot/structure.urdf",
            "structure: ../robot.yaml",
            1,
        )
    });

    let error = FinalizedBundle::load(bundle.path()).expect_err("an escaping path must fail");
    assert!(
        matches!(
            error,
            BundleError::Compile(CompileError::Escapes { ref root, .. })
                if root.ends_with("assets")
        ),
        "{error}"
    );
}

#[test]
fn a_declared_router_configuration_resolves_inside_the_bundle() {
    let bundle = staged_bundle();
    rewrite_manifest(&bundle, |text| {
        format!("{text}router:\n  config: router/config.json5\n")
    });
    std::fs::create_dir_all(bundle.path().join("assets/router")).unwrap();
    std::fs::write(
        bundle.path().join("assets/router/config.json5"),
        b"{ mode: \"router\" }",
    )
    .unwrap();

    let loaded = FinalizedBundle::load(bundle.path()).expect("a router config must load");
    assert!(
        loaded
            .router_config()
            .is_some_and(|path| path.ends_with("assets/router/config.json5")),
        "{:?}",
        loaded.router_config()
    );
}

#[test]
fn a_missing_component_directory_is_refused() {
    let bundle = staged_bundle();
    std::fs::remove_dir_all(bundle.path().join("assets/components/imu")).unwrap();

    let error = FinalizedBundle::load(bundle.path()).expect_err("a missing component must fail");
    assert!(format!("{error:#}").contains("imu"), "{error:#}");
}

#[test]
fn a_simulated_clock_cannot_coexist_with_a_component_driver() {
    let temp = tempfile::tempdir().unwrap();
    let source_root = workspace_root().join("fixture/robot/rgbd-imu-diff-drive");
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
    std::fs::write(temp.path().join("robot.yaml"), &yaml).unwrap();
    std::fs::copy(
        source_root.join("structure.urdf"),
        temp.path().join("structure.urdf"),
    )
    .unwrap();

    // The driver alone is legal: it is the combination that is meaningless.
    source::robot::Manifest::load(temp.path()).expect("a driver on the real clock is valid");

    std::fs::write(
        temp.path().join("robot.yaml"),
        yaml.replacen(
            "schema: phoxal/robot/v0",
            "schema: phoxal/robot/v0\nclock: simulated",
            1,
        ),
    )
    .unwrap();
    let error = source::robot::Manifest::load(temp.path())
        .expect_err("a driver under simulated time must fail");
    assert!(
        format!("{error:#}").contains("clock: simulated"),
        "{error:#}"
    );
}

#[test]
fn an_authored_document_defaults_to_the_real_clock() {
    let source::robot::Manifest::V0(manifest) = source::robot::Manifest::load(
        workspace_root().join("fixture/robot/rgbd-imu-diff-drive/robot.yaml"),
    )
    .expect("the fixture omits `clock:`");
    assert_eq!(manifest.clock, Clock::Real);

    let compiled = sources(&workspace_root().join("fixture/robot/rgbd-imu-diff-drive"))
        .compile()
        .expect("fixture must compile");
    assert_eq!(compiled.robot().clock(), phoxal_model::Clock::Real);
}
