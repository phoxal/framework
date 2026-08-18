//! Staging for the workspace fixture robot.
//!
//! The fixture itself is authored YAML/URDF and nothing else. It lives at
//! `fixture/` in the workspace root, holds no code, and is read straight off
//! disk by the tests that only need documents - `phoxal-manifest`'s compile
//! tests are its largest consumer and never link this crate at all.
//!
//! This crate is the other half: it takes those documents through the whole
//! authoring pipeline, compiling them with `phoxal-manifest` and assembling
//! the result into a finalized bundle with `phoxal-bundle`. That makes it the
//! only place in the workspace where those two crates meet. They are siblings:
//! each depends on `phoxal-model` and `phoxal-runtime-contract`, and neither
//! depends on the other, so nothing else here proves that what the compiler
//! emits is something the assembler can actually accept. The real `phoxal`
//! CLI joins them for a living; this is the test that says it can be done.
//!
//! Its second job is smaller and purely mechanical: `staged_bundle` is needed
//! by both a unit test inside `phoxal/src` and an integration test in
//! `phoxal/tests`, and a dev-dependency crate is the only thing Rust offers
//! that both can reach.
//!
//! Never published: the paths it resolves are relative to this repository's
//! layout and mean nothing outside it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use phoxal::authoring::{SourceSet, source};
use phoxal::bundle::{BundlePath, BundleWriter, RuntimeBundle};
use phoxal::model::identity::ServiceId;
use phoxal::model::{ManifestDocument, Robot};
use tempfile::TempDir;

/// The official services the CLI would resolve for this fixture.
///
/// The framework owns no such list - `SourceSet::compile` takes it from its
/// caller - so this crate states one, exactly as the CLI does.
const OFFICIAL_SERVICES: [&str; 2] = ["drive", "motion"];

/// The authored documents this crate stages, at `fixture/` in the workspace
/// root. Resolved relative to this crate rather than from `cargo metadata`,
/// so staging needs nothing parsed before it can start.
fn authored_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("fixture")
}

/// Stage the fixture into a disposable bundle.
pub struct StagedBundle {
    _parent: TempDir,
    root: PathBuf,
}

impl StagedBundle {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.root
    }
}

#[must_use]
pub fn staged_bundle() -> StagedBundle {
    staged_bundle_from_manifest("robot.yaml")
}

#[cfg(test)]
fn staged_bundle_without_component_models() -> StagedBundle {
    staged_bundle_from_manifest("robot.no-component-models.yaml")
}

#[expect(
    clippy::expect_used,
    reason = "every input is a document committed beside this crate, so a failure here is a broken checkout and the panic is the report"
)]
fn staged_bundle_from_manifest(manifest_name: &str) -> StagedBundle {
    let fixture = authored_root();
    let project = fixture.join("robot/rgbd-imu-diff-drive");
    let bundle = tempfile::tempdir().expect("a staging directory");
    let robot_manifest = project.join(manifest_name);
    let manifest =
        source::robot::Manifest::load(&robot_manifest).expect("the fixture robot manifest");
    let source::robot::Manifest::V0(manifest) = manifest;
    let sources = SourceSet {
        project_root: project.clone(),
        robot_manifest,
        component_roots: manifest
            .used_component_types()
            .into_iter()
            .map(|component_type| {
                (
                    component_type.to_string(),
                    fixture.join("components").join(component_type),
                )
            })
            .collect(),
    };
    let official = OFFICIAL_SERVICES
        .map(|id| ServiceId::new(id).expect("an official service id is a token"))
        .to_vec();
    let compiled = sources
        .compile(official)
        .expect("the fixture sources compile");
    let (document, assets) = compiled.into_document();

    // The expected process set is derived from the manifest and nowhere else:
    // the root brain, one binary per service, one per driven component type.
    let sources_root = bundle.path().join("sources");
    let mut binaries = BTreeMap::new();
    let mut stage = |name: &str| {
        let source = sources_root.join(name);
        std::fs::create_dir_all(&sources_root).expect("fixture binary source directory");
        std::fs::write(&source, b"#!/bin/sh\nexit 0\n").expect("fixture executable source");
        #[cfg(unix)]
        std::fs::set_permissions(&source, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("fixture executable source mode");
        binaries.insert(
            BundlePath::new(format!("bin/{name}")).expect("a canonical bundle path"),
            source,
        );
    };
    stage("brain");
    let robot = document.robot();
    for (service, _) in robot.services() {
        stage(service.as_str());
    }
    for component_type in robot
        .components()
        .filter(|component| component.instance().driver().is_some())
        .map(|component| component.instance().component_type().clone())
        .collect::<std::collections::BTreeSet<_>>()
    {
        stage(component_type.as_str());
    }

    let root = bundle.path().join("bundle");
    BundleWriter::write(&root, &document, &assets.into_map(), &binaries)
        .expect("the fixture bundle writes");
    StagedBundle {
        _parent: bundle,
        root,
    }
}

/// The canonical fixture robot, loaded only from the staged manifest.
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "the fixture documents and compiler are committed together, so a load failure is a broken checkout and the panic is the report"
)]
pub fn robot() -> Robot {
    let bundle = staged_bundle();
    RuntimeBundle::open(bundle.path())
        .expect("the staged bundle must load")
        .robot()
        .clone()
}

/// The document the fixture compiles to, for a consumer that needs the tag.
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "the fixture documents and compiler are committed together, so a load failure is a broken checkout and the panic is the report"
)]
pub fn manifest() -> ManifestDocument {
    let bundle = staged_bundle();
    RuntimeBundle::open(bundle.path())
        .expect("the staged bundle must load")
        .manifest()
        .clone()
}

#[cfg(test)]
mod tests {
    use phoxal::bundle::RuntimeBundle;

    use super::{robot, staged_bundle, staged_bundle_without_component_models};

    #[test]
    fn the_staged_bundle_has_only_the_three_entry_layout() {
        let bundle = staged_bundle();
        let root = bundle.path();
        assert!(root.join("manifest.json").is_file());
        assert!(root.join("assets").is_dir());
        assert!(root.join("bin").is_dir());
        // Authored source does not survive into a bundle.
        assert!(!root.join("robot.yaml").exists());
    }

    /// The whole process set is derivable from the manifest, and the binaries
    /// staged beside it are named after exactly those ids.
    #[test]
    fn every_expected_runtime_has_a_binary_named_after_its_id() {
        let bundle = staged_bundle();
        let loaded = RuntimeBundle::open(bundle.path()).expect("the staged bundle loads");
        let robot = loaded.robot();

        let mut expected = vec!["brain".to_owned()];
        expected.extend(robot.services().map(|(id, _)| id.as_str().to_owned()));
        expected.extend(
            robot
                .components()
                .filter(|component| component.instance().driver().is_some())
                .map(|component| component.instance().component_type().as_str().to_owned()),
        );
        expected.sort();
        expected.dedup();

        for id in expected {
            assert!(
                bundle.path().join("bin").join(&id).is_file(),
                "bin/{id} is missing"
            );
        }
    }

    /// A driver's participant id is its component instance id, and its
    /// configuration is the driver block on that instance.
    #[test]
    fn a_driven_component_carries_the_config_its_driver_reads() {
        let robot = robot();
        let drive = robot
            .component("front_left_drive")
            .expect("the fixture mounts a driven component");
        assert_eq!(drive.instance().component_type().as_str(), "drive_motor");
        assert_eq!(
            drive
                .instance()
                .driver()
                .and_then(|driver| driver.get("connection"))
                .and_then(|connection| connection.get("type")),
            Some(&serde_json::json!("can"))
        );
    }

    /// A component type with no `simulation.yaml` carries no simulation, which
    /// is a different fact from an empty one.
    #[test]
    fn a_component_type_without_a_model_carries_no_simulation() {
        let bundle = staged_bundle_without_component_models();
        let loaded = RuntimeBundle::open(bundle.path()).expect("the staged bundle loads");
        assert!(
            loaded
                .robot()
                .components()
                .all(|component| component.simulation().is_none())
        );

        // The ordinary fixture is the counterpart: there, the model is present.
        assert!(
            robot()
                .components()
                .any(|component| component.simulation().is_some())
        );
    }
}
