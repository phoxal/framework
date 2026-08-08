//! Staging for the workspace fixture robot.
//!
//! The fixture remains authored YAML/URDF beside this crate. Tests that need
//! a runtime artifact compile those sources once and let this build-side
//! assembler combine the resulting canonical model, source-owned service and
//! driver facts, simulation membership, assets, and disposable binary bytes
//! through `phoxal-bundle`'s explicit assembly API. No finalized source
//! document is copied into the runtime root.
//!
//! This crate is never published: the paths it resolves are relative to this
//! repository's layout and mean nothing outside it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use phoxal_bundle::{
    AssetIndex, BinaryReference, BundlePath, BundleWriter, ParticipantClock, Runtime,
    RuntimeBundle, RuntimeDocument, RuntimeParticipant,
};
use phoxal_manifest::{SourceSet, source};
use phoxal_model::identity::ComponentInstanceId;
use phoxal_model::{Clock, Robot};
use phoxal_runtime_contract::identity::{ParticipantArtifactId, ParticipantId};
use phoxal_runtime_contract::metadata::{ParticipantContract, ParticipantKind, ParticipantSchemas};
use phoxal_runtime_contract::version::{BusAbi, LaunchAbi, RobotApi, RuntimeSchema};
use tempfile::TempDir;

fn authored_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
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
#[expect(
    clippy::expect_used,
    reason = "every input is a document committed beside this crate, so a failure here is a broken checkout and the panic is the report"
)]
pub fn staged_bundle() -> StagedBundle {
    let fixture = authored_root();
    let project = fixture.join("robot/rgbd-imu-diff-drive");
    let bundle = tempfile::tempdir().expect("a staging directory");
    let manifest = source::robot::Manifest::load(project.join("robot.yaml"))
        .expect("the fixture robot manifest");
    let source::robot::Manifest::V0(manifest) = manifest;
    let sources = SourceSet {
        project_root: project.clone(),
        robot_manifest: project.join("robot.yaml"),
        component_roots: manifest
            .used_component_types()
            .into_iter()
            .map(|component_type| {
                (
                    component_type.to_string(),
                    fixture.join("component").join(component_type),
                )
            })
            .collect(),
    };
    let compiled = sources.compile().expect("the fixture sources compile");
    let (robot, services, drivers, assets) = compiled.into_parts();
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
                let binary = BinaryReference::from_file(
                    binary_path.clone(),
                    ParticipantContract {
                        id: artifact_id.clone(),
                        kind,
                        api: RobotApi::V0_2,
                        schemas: ParticipantSchemas {
                            bus: BusAbi::V0,
                            launch: LaunchAbi::V0,
                            runtime: RuntimeSchema::V0,
                        },
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
    for service in services {
        stage_participant(
            service.id.clone(),
            service.id,
            ParticipantKind::Service,
            service.config,
            None,
        );
    }
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
    if robot.components().any(|instance| {
        robot
            .simulation_for_instance(instance.id().as_str())
            .is_some()
    }) {
        stage_participant(
            "webots-controller".to_string(),
            "webots-controller".to_string(),
            ParticipantKind::Simulator,
            None,
            None,
        );
    }
    let runtime = Runtime::new(robot, artifacts, participants, asset_index, None)
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

    use super::{robot, staged_bundle};

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
    fn driver_instances_reuse_one_artifact_and_simulation_has_one_controller() {
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
            1
        );
    }
}
