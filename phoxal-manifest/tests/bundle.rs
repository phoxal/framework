//! The finalized-bundle contract: what a bundle looks like on disk, and what
//! loading one guarantees.
//!
//! `fixture/bundle/rgbd-imu-diff-drive` is a real finalized bundle checked into
//! the repository. It is the executable specification of the layout the CLI
//! writes and this loader reads, and it is what the framework's own runtime
//! consumers load in their tests. `staging_is_pinned_to_the_checked_in_fixture`
//! keeps it honest; regenerate it with
//! `PHOXAL_UPDATE_BUNDLE_FIXTURE=1 cargo test -p phoxal-manifest`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use phoxal_manifest::bundle::{
    BundleError, BundleFile, BundleResolver, FinalizedBundle, InvalidBundlePath,
};
use phoxal_manifest::source::robot::v0::Clock;
use phoxal_manifest::{SourceSet, compile, source};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("manifest crate has a workspace parent")
        .to_path_buf()
}

fn fixture_bundle() -> PathBuf {
    workspace_root().join("fixture/bundle/rgbd-imu-diff-drive")
}

fn sources(project_root: &Path) -> SourceSet {
    let manifest = source::robot::read_from_path(project_root.join("robot.yaml"))
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
    let compiled = compile(sources.clone()).expect("fixture project must compile");
    let source::robot::Manifest::V0(mut manifest) =
        source::robot::read_from_path(&sources.robot_manifest).expect("manifest must parse");

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

fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.insert(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    std::fs::read(&path).unwrap(),
                );
            }
        }
    }
    files
}

#[test]
fn staging_is_pinned_to_the_checked_in_fixture() {
    let staged = tempfile::tempdir().unwrap();
    stage(
        &workspace_root().join("fixture/robot/rgbd-imu-diff-drive"),
        Clock::Real,
        staged.path(),
    );
    if std::env::var_os("PHOXAL_UPDATE_BUNDLE_FIXTURE").is_some() {
        let fixture = fixture_bundle();
        let _ = std::fs::remove_dir_all(&fixture);
        for (relative, bytes) in tree(staged.path()) {
            let path = fixture.join(&relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        return;
    }
    assert_eq!(
        tree(staged.path()),
        tree(&fixture_bundle()),
        "the checked-in finalized bundle drifted from what staging produces"
    );
}

#[test]
fn a_finalized_bundle_loads_without_any_compiled_model_document() {
    let bundle = FinalizedBundle::load(fixture_bundle()).expect("the fixture bundle must load");
    let robot = bundle.robot();

    assert_eq!(
        (robot.robot_id(), robot.namespace()),
        ("rgbd-imu-diff-drive", "dev")
    );
    assert_eq!(robot.clock(), phoxal_model::Clock::Real);
    assert_eq!(robot.components().len(), 7);
    assert!(
        robot
            .simulation_for_component_type("drive_motor")
            .and_then(|simulation| simulation.capability("encoder"))
            .is_some()
    );
    assert!(
        !fixture_bundle().join("robot.json").exists(),
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
    let bundle = FinalizedBundle::load(fixture_bundle()).expect("the fixture bundle must load");
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
    let resolver = BundleResolver::index(fixture_bundle(), 4 * 1024 * 1024).expect("index");
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

    let bounded = BundleResolver::index(fixture_bundle(), 8).expect("index");
    assert!(matches!(
        bounded.get("bin/brain").unwrap(),
        BundleFile::TooLarge { .. }
    ));
}

/// A bundle copy with `robot.yaml` replaced.
fn bundle_with_manifest(text: &str) -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    let fixture = fixture_bundle();
    for (relative, bytes) in tree(&fixture) {
        let path = temp.path().join(&relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    std::fs::write(temp.path().join("robot.yaml"), text).unwrap();
    temp
}

#[test]
fn an_unresolved_extends_is_refused() {
    let mut text = std::fs::read_to_string(fixture_bundle().join("robot.yaml")).unwrap();
    text = text.replace("clock: real\n", "clock: real\nextends: [base.robot.yaml]\n");
    let bundle = bundle_with_manifest(&text);

    let error = FinalizedBundle::load(bundle.path()).expect_err("extends must be resolved");
    assert!(
        matches!(error, BundleError::UnresolvedExtends { .. }),
        "{error}"
    );
}

#[test]
fn a_foreign_schema_tag_is_refused() {
    let text = std::fs::read_to_string(fixture_bundle().join("robot.yaml"))
        .unwrap()
        .replacen("schema: robot/v0", "schema: robot/v1", 1);
    let bundle = bundle_with_manifest(&text);

    let error = FinalizedBundle::load(bundle.path()).expect_err("an unknown schema must fail");
    assert!(
        format!("{error:#}").contains("robot/v1"),
        "the schema tag selects the variant: {error:#}"
    );
}

#[test]
fn a_structure_path_outside_the_bundle_is_refused() {
    let text = std::fs::read_to_string(fixture_bundle().join("robot.yaml"))
        .unwrap()
        .replacen(
            "structure: robot/structure.urdf",
            "structure: ../robot.yaml",
            1,
        );
    let bundle = bundle_with_manifest(&text);

    let error = FinalizedBundle::load(bundle.path()).expect_err("an escaping path must fail");
    assert!(format!("{error:#}").contains("robot root"), "{error:#}");
}

#[test]
fn a_declared_router_configuration_resolves_inside_the_bundle() {
    let text = format!(
        "{}router:\n  config: router/config.json5\n",
        std::fs::read_to_string(fixture_bundle().join("robot.yaml")).unwrap()
    );
    let bundle = bundle_with_manifest(&text);
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
    let temp = tempfile::tempdir().unwrap();
    for (relative, bytes) in tree(&fixture_bundle()) {
        if relative.starts_with("assets/components/imu/") {
            continue;
        }
        let path = temp.path().join(&relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    let error = FinalizedBundle::load(temp.path()).expect_err("a missing component must fail");
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
    source::robot::read_from_dir(temp.path()).expect("a driver on the real clock is valid");

    std::fs::write(
        temp.path().join("robot.yaml"),
        yaml.replacen("schema: robot/v0", "schema: robot/v0\nclock: simulated", 1),
    )
    .unwrap();
    let error = source::robot::read_from_dir(temp.path())
        .expect_err("a driver under simulated time must fail");
    assert!(
        format!("{error:#}").contains("clock: simulated"),
        "{error:#}"
    );
}

#[test]
fn an_authored_document_defaults_to_the_real_clock() {
    let source::robot::Manifest::V0(manifest) = source::robot::read_from_path(
        workspace_root().join("fixture/robot/rgbd-imu-diff-drive/robot.yaml"),
    )
    .expect("the fixture omits `clock:`");
    assert_eq!(manifest.clock, Clock::Real);

    let compiled = compile(sources(
        &workspace_root().join("fixture/robot/rgbd-imu-diff-drive"),
    ))
    .expect("fixture must compile");
    assert_eq!(compiled.robot().clock(), phoxal_model::Clock::Real);
}
