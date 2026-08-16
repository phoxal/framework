//! What a bundle promises a process that opens one.
//!
//! The promise is deliberately small. The layout is `manifest.json`, `assets/`
//! and `bin/`. The manifest parses into a validated robot or the open fails. An
//! asset read cannot name a file outside `assets/`. Everything the old boundary
//! also checked (digests, sizes, an asset index, a participant list, a
//! compatibility line) is gone, so these tests state which fence still holds.

use std::collections::BTreeMap;
use std::path::PathBuf;

use phoxal_model::builder::RobotBuilder;
use phoxal_model::manifest::ManifestDocument;
use phoxal_model::{AssetId, Robot};

use crate::{
    ASSETS_DIR, BIN_DIR, BundleError, BundlePath, BundleWriter, MANIFEST_FILE, RuntimeBundle,
};

fn robot() -> Robot {
    RobotBuilder::new("rover")
        .service("drive", None)
        .service("mission", Some(serde_json::json!({ "speed": 1 })))
        .component_type("rgbd", |camera| camera.camera("rgb", "lens"))
        .component("front_camera", "rgbd")
        .build()
        .expect("a valid canonical robot")
}

fn asset(id: &str) -> AssetId {
    AssetId::new(id).expect("a normalized asset id")
}

/// An executable source the writer accepts, staged outside the bundle.
fn executable(directory: &std::path::Path, name: &str) -> PathBuf {
    let path = directory.join(name);
    std::fs::write(&path, b"#!/bin/sh\nexit 0\n").expect("a writable staging directory");
    #[cfg(unix)]
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("the staged executable is marked runnable");
    path
}

/// Write one bundle under `parent/bundle`, with one asset and one binary.
fn written(parent: &std::path::Path) -> RuntimeBundle {
    let sources = parent.join("sources");
    std::fs::create_dir_all(&sources).expect("a writable staging directory");
    BundleWriter::write(
        parent.join("bundle"),
        &ManifestDocument::new(robot()),
        &BTreeMap::from([(asset("robot/meshes/base.stl"), b"mesh bytes".to_vec())]),
        &BTreeMap::from([(
            BundlePath::new("bin/brain").expect("a canonical bundle path"),
            executable(&sources, "brain"),
        )]),
    )
    .expect("the bundle writes")
}

#[test]
fn a_written_bundle_is_the_three_entry_layout_and_nothing_else() {
    let parent = tempfile::tempdir().expect("a staging directory");
    let bundle = written(parent.path());
    let root = bundle.root().to_path_buf();

    assert!(root.join(MANIFEST_FILE).is_file());
    assert!(root.join(ASSETS_DIR).is_dir());
    assert!(root.join(BIN_DIR).is_dir());
    let mut entries = std::fs::read_dir(&root)
        .expect("the bundle root reads")
        .map(|entry| entry.expect("a readable entry").file_name())
        .collect::<Vec<_>>();
    entries.sort();
    assert_eq!(entries, [ASSETS_DIR, BIN_DIR, MANIFEST_FILE]);
    assert!(root.join(BIN_DIR).join("brain").is_file());
}

/// Reopening is the whole read path: the manifest is parsed and the robot it
/// carries is the one that was compiled, configuration included.
#[test]
fn reopening_a_bundle_yields_the_robot_that_was_written() {
    let parent = tempfile::tempdir().expect("a staging directory");
    let root = written(parent.path()).root().to_path_buf();

    let reopened = RuntimeBundle::open(&root).expect("a written bundle reopens");
    assert_eq!(reopened.robot_id().as_str(), "rover");
    assert_eq!(
        reopened.robot().service_config("mission"),
        Some(&serde_json::json!({ "speed": 1 }))
    );
    assert!(reopened.robot().component("front_camera").is_some());
    assert_eq!(
        reopened
            .asset(&asset("robot/meshes/base.stl"))
            .expect("the written asset reads"),
        b"mesh bytes"
    );
}

/// A participant the manifest never mentions opens the bundle exactly like one
/// it does: there is no selection step left to refuse it.
#[test]
fn a_bundle_refuses_no_participant() {
    let parent = tempfile::tempdir().expect("a staging directory");
    let root = written(parent.path()).root().to_path_buf();

    let bundle = RuntimeBundle::open(&root).expect("any process may open the bundle");
    assert!(bundle.robot().service("not-a-service").is_none());
    assert!(bundle.robot().component("not-a-component").is_none());
}

/// The manifest is the one thing an open still judges: a body that is not a
/// document this train reads fails the open rather than yielding half a robot.
#[test]
fn a_manifest_that_is_not_a_readable_document_fails_the_open() {
    let parent = tempfile::tempdir().expect("a staging directory");
    let root = written(parent.path()).root().to_path_buf();

    for body in [
        "not json at all".to_owned(),
        serde_json::json!({ "schema": "phoxal/manifest/v1" }).to_string(),
        serde_json::json!({ "schema": "phoxal/manifest/v0", "id": "rover" }).to_string(),
    ] {
        std::fs::write(root.join(MANIFEST_FILE), &body).expect("the manifest is writable");
        assert!(
            matches!(
                RuntimeBundle::open(&root),
                Err(BundleError::ManifestJson(_))
            ),
            "{body}"
        );
    }

    std::fs::remove_file(root.join(MANIFEST_FILE)).expect("the manifest is removable");
    assert!(matches!(
        RuntimeBundle::open(&root),
        Err(BundleError::ReadManifest { .. })
    ));
}

/// An asset id is a validated relative path and the bundle path validates the
/// join again, so nothing a caller can construct reads outside `assets/`.
#[test]
fn an_asset_read_cannot_leave_the_assets_directory() {
    let parent = tempfile::tempdir().expect("a staging directory");
    let bundle = written(parent.path());
    std::fs::write(parent.path().join("secret"), b"not yours").expect("a writable directory");

    for escape in ["../secret", "/etc/passwd", "robot/../../secret"] {
        assert!(
            AssetId::new(escape).is_err(),
            "{escape} must not be a logical asset id"
        );
    }
    assert!(matches!(
        bundle.asset(&asset("robot/meshes/absent.stl")),
        Err(BundleError::MissingFile { .. })
    ));
}

/// The bundle appears under its final name only when it is complete, and never
/// replaces one that is already there.
#[test]
fn publication_is_atomic_and_never_overwrites() {
    let parent = tempfile::tempdir().expect("a staging directory");
    let bundle = written(parent.path());
    let root = bundle.root().to_path_buf();

    assert!(matches!(
        BundleWriter::write(
            &root,
            &ManifestDocument::new(robot()),
            &BTreeMap::new(),
            &BTreeMap::new(),
        ),
        Err(BundleError::TargetExists(_))
    ));

    // A refused write leaves nothing behind for the next one to trip over.
    let siblings = std::fs::read_dir(parent.path())
        .expect("the parent reads")
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().contains(".staging-"))
        .count();
    assert_eq!(siblings, 0);
}

/// A staged binary that could not be launched is refused at write time, where
/// the diagnostic still names the source, rather than at launch.
#[test]
fn a_binary_that_is_not_runnable_is_refused_by_the_writer() {
    let parent = tempfile::tempdir().expect("a staging directory");
    let sources = parent.path().join("sources");
    std::fs::create_dir_all(&sources).expect("a writable staging directory");
    let data = sources.join("brain");
    std::fs::write(&data, b"not an executable").expect("a writable file");

    let written = BundleWriter::write(
        parent.path().join("bundle"),
        &ManifestDocument::new(robot()),
        &BTreeMap::new(),
        &BTreeMap::from([(
            BundlePath::new("bin/brain").expect("a canonical bundle path"),
            data,
        )]),
    );
    #[cfg(unix)]
    assert!(matches!(written, Err(BundleError::NotExecutable { .. })));
    #[cfg(not(unix))]
    assert!(written.is_ok());
    assert!(
        !parent.path().join("bundle").exists(),
        "a refused write publishes nothing"
    );
}
