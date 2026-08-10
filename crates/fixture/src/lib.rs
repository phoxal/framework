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

use phoxal_bundle::{
    AssetIndex, BinaryReference, BinarySource, BundlePath, BundleWriter, ParticipantClock, Runtime,
    RuntimeBundle, RuntimeDocument, RuntimeParticipant, RuntimeRouterConfig,
};
use phoxal_manifest::{SourceSet, source};
use phoxal_model::identity::ComponentInstanceId;
use phoxal_model::{Clock, Robot};
use phoxal_runtime_contract::identity::{ParticipantArtifactId, ParticipantId};
use phoxal_runtime_contract::metadata::{ParticipantContract, ParticipantKind};
use phoxal_runtime_contract::version::FrameworkVersion;
use tempfile::TempDir;

/// The authored documents this crate stages, at `fixture/` in the workspace
/// root. Resolved relative to this crate rather than from `cargo metadata`,
/// so staging needs nothing parsed before it can start.
fn authored_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("fixture")
}

/// Stage the fixture into a disposable `runtime.json` bundle.
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
fn staged_simulation_bundle() -> StagedBundle {
    staged_bundle_from_manifest("robot.simulated.yaml")
}

#[cfg(test)]
fn staged_simulation_bundle_without_component_models() -> StagedBundle {
    staged_bundle_from_manifest("robot.simulated-no-models.yaml")
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
    let compiled = sources.compile().expect("the fixture sources compile");
    let (robot, services, drivers, router, assets) = compiled.into_parts();
    let assets = assets.into_map();
    let asset_index = AssetIndex::from_bytes(&assets).expect("fixture asset index");

    let clock = match robot.clock() {
        Clock::Real => ParticipantClock::Real,
        Clock::Simulated => ParticipantClock::Simulation,
    };
    let mut participants = Vec::new();
    let mut artifacts = BTreeMap::new();
    let mut binaries = BTreeMap::new();
    let mut stage_participant =
        |artifact_name: String,
         participant_name: String,
         kind: ParticipantKind,
         config: Option<serde_json::Value>,
         component_instance: Option<ComponentInstanceId>| {
            let artifact_id = ParticipantArtifactId::new(artifact_name)
                .expect("the compiler emitted a normalized artifact id");
            let participant_id = ParticipantId::new(participant_name)
                .expect("the assembler emitted a normalized participant id");
            if !artifacts.contains_key(&artifact_id) {
                let source = bundle.path().join("sources").join(artifact_id.as_str());
                std::fs::create_dir_all(
                    source.parent().expect("fixture binary source has a parent"),
                )
                .expect("fixture binary source directory");
                std::fs::write(&source, b"#!/bin/sh\nexit 0\n").expect("fixture executable source");
                #[cfg(unix)]
                std::fs::set_permissions(
                    &source,
                    std::os::unix::fs::PermissionsExt::from_mode(0o755),
                )
                .expect("fixture executable source mode");
                let binary_path =
                    BundlePath::new(format!("bin/{}", artifact_id.as_str())).expect("binary path");
                let config_schema = if config.is_some() {
                    serde_json::json!({})
                } else {
                    serde_json::json!({"type": "null"})
                };
                let source = BinarySource::open(&source).expect("fixture executable source opens");
                let binary = BinaryReference::from_source(
                    binary_path.clone(),
                    ParticipantContract {
                        framework: FrameworkVersion::CURRENT,
                        id: artifact_id.clone(),
                        kind,
                        requirement: None,
                        config_schema,
                    },
                    &source,
                )
                .expect("fixture binary source hashes");
                binaries.insert(binary_path, source);
                artifacts.insert(artifact_id.clone(), binary);
            }
            participants.push(RuntimeParticipant::new(
                participant_id,
                artifact_id,
                config,
                component_instance,
                clock,
            ));
        };
    stage_participant(
        "brain".to_string(),
        "brain".to_string(),
        ParticipantKind::Brain,
        None,
        None,
    );
    for service in services {
        stage_participant(
            service.id.as_str().to_string(),
            service.id.as_str().to_string(),
            ParticipantKind::Service,
            service.config,
            None,
        );
    }
    // Final topology owns the execution-mode choice. A simulated runtime has
    // simulator participants, while a real runtime has hardware drivers; the
    // persisted graph never contains both roles for one robot.
    if robot.clock() == Clock::Real {
        for driver in drivers {
            let instance = robot
                .component_instance(driver.component_instance.as_str())
                .expect("a compiled driver must bind a canonical component instance");
            let artifact = driver.implementation.clone();
            stage_participant(
                artifact.as_str().to_string(),
                format!("{artifact}-{}", instance.id()),
                ParticipantKind::Driver,
                Some(serde_json::to_value(driver.config).expect("driver config is serializable")),
                Some(driver.component_instance),
            );
        }
    }
    if robot.clock() == Clock::Simulated {
        stage_participant(
            "webots-controller".to_string(),
            "webots-controller".to_string(),
            ParticipantKind::Simulator,
            None,
            None,
        );
    }
    let router = router.map(|router| {
        RuntimeRouterConfig::new(
            BundlePath::new(format!("assets/{}", router.asset.as_str()))
                .expect("compiled router asset path"),
        )
    });
    let runtime = Runtime::new(robot, artifacts, participants, asset_index, router)
        .expect("fixture runtime is valid");
    let document = RuntimeDocument::new(runtime);
    let root = bundle.path().join("bundle");
    BundleWriter::write(&root, &document, &assets, &binaries)
        .expect("the fixture runtime bundle writes");
    StagedBundle {
        _parent: bundle,
        root,
    }
}

/// The canonical fixture robot, loaded only from runtime.json.
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "the fixture documents and compiler are committed together, so a load failure is a broken checkout and the panic is the report"
)]
pub fn robot() -> Robot {
    let bundle = staged_bundle();
    RuntimeBundle::open_verified(bundle.path())
        .expect("the staged bundle must load")
        .robot()
        .clone()
}

#[cfg(test)]
mod tests {
    use phoxal_bundle::RuntimeBundle;
    use phoxal_runtime_contract::identity::ParticipantArtifactId;
    use phoxal_runtime_contract::metadata::ParticipantKind;

    use super::{
        robot, staged_bundle, staged_simulation_bundle,
        staged_simulation_bundle_without_component_models,
    };

    #[test]
    fn the_staged_bundle_has_only_runtime_layout() {
        let bundle = staged_bundle();
        let root = bundle.path();
        assert!(root.join("runtime.json").is_file());
        assert!(root.join("assets").is_dir());
        assert!(root.join("bin").is_dir());
        assert!(!root.join("robot.yaml").exists());
    }

    #[test]
    fn the_fixture_robot_loads_on_the_real_clock() {
        assert_eq!(robot().clock(), phoxal_model::Clock::Real);
    }

    #[test]
    fn real_fixture_selects_drivers_without_a_simulator() {
        let bundle = staged_bundle();
        let loaded = RuntimeBundle::open_verified(bundle.path()).expect("the staged bundle loads");
        let driver = ParticipantArtifactId::new("drive_motor").expect("driver artifact");
        assert_eq!(
            loaded
                .participants()
                .iter()
                .filter(|participant| participant.artifact() == &driver)
                .count(),
            4
        );
        assert_eq!(loaded.artifacts().len(), 2);
        assert_eq!(
            loaded
                .participants()
                .iter()
                .filter(|participant| {
                    loaded
                        .artifacts()
                        .get(participant.artifact())
                        .expect("participant artifact")
                        .contract()
                        .kind
                        == ParticipantKind::Simulator
                })
                .count(),
            0
        );
        let router = loaded
            .document()
            .runtime()
            .router()
            .expect("compiled router config is retained");
        assert_eq!(router.path().as_str(), "assets/robot/router.json5");
        let router_id = phoxal_model::AssetId::new("robot/router.json5").expect("router asset id");
        assert_eq!(
            loaded.assets().read(&router_id).expect("router bytes"),
            b"{}\n"
        );
    }

    #[test]
    fn simulated_fixture_selects_one_simulator_without_drivers() {
        let bundle = staged_simulation_bundle();
        let loaded = RuntimeBundle::open_verified(bundle.path()).expect("the staged bundle loads");
        let kinds = loaded
            .participants()
            .iter()
            .map(|participant| {
                loaded
                    .artifacts()
                    .get(participant.artifact())
                    .expect("participant artifact")
                    .contract()
                    .kind
            })
            .collect::<Vec<_>>();
        assert_eq!(
            kinds
                .iter()
                .filter(|kind| **kind == ParticipantKind::Simulator)
                .count(),
            1
        );
        assert_eq!(
            kinds
                .iter()
                .filter(|kind| **kind == ParticipantKind::Driver)
                .count(),
            0
        );
    }

    #[test]
    fn simulated_fixture_without_component_models_still_selects_world_authority() {
        let bundle = staged_simulation_bundle_without_component_models();
        let loaded = RuntimeBundle::open_verified(bundle.path()).expect("the staged bundle loads");
        assert!(loaded.robot().components().all(|instance| {
            loaded
                .robot()
                .simulation_for_instance(instance.id().as_str())
                .is_none()
        }));
        let simulator_count = loaded
            .participants()
            .iter()
            .filter(|participant| {
                loaded.artifacts()[participant.artifact()].contract().kind
                    == ParticipantKind::Simulator
            })
            .count();
        assert_eq!(simulator_count, 1);
    }
}
