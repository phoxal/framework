//! Staging for the workspace fixture robot.
//!
//! The fixture remains authored YAML/URDF beside this crate. Tests that need
//! a runtime artifact compile those sources once and stage the resulting
//! canonical model, participant records, assets, and disposable binary bytes
//! through `phoxal-bundle`'s explicit assembly API. No finalized source
//! document is copied into the runtime root.
//!
//! This crate is never published: the paths it resolves are relative to this
//! repository's layout and mean nothing outside it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use phoxal_bundle::{
    AssetIndex, BinaryCompatibility, BinaryReference, BuildFacts, BundlePath, BundleWriter,
    ComponentBinding, Runtime, RuntimeBundle, RuntimeDocument, RuntimeParticipant,
    StartupRequirement,
};
use phoxal_manifest::{ParticipantKind as SourceParticipantKind, SourceSet, source};
use phoxal_model::{Clock, Robot};
use phoxal_runtime_contract::identity::ParticipantId;
use phoxal_runtime_contract::launch::ClockMode;
use phoxal_runtime_contract::metadata::{ParticipantKind, ParticipantSchemas};
use phoxal_runtime_contract::version::{
    BusAbi, ComponentSchema, LaunchAbi, RobotApi, RobotSchema, SimulationSchema,
};
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
    let (robot, declarations, assets) = compiled.into_parts();
    let assets = assets.into_map();
    let asset_index = AssetIndex::from_bytes(&assets).expect("fixture asset index");

    let clock = match robot.clock() {
        Clock::Real => ClockMode::Real,
        Clock::Simulated => ClockMode::Simulation,
    };
    let mut participants = Vec::new();
    let mut binaries = BTreeMap::new();
    for declaration in declarations.into_vec() {
        // A component type can be mounted more than once. The build-side
        // assembler therefore gives each compiled process a unique topology
        // ParticipantId while retaining the typed component binding below.
        let rendered_id = declaration.component_instance.as_ref().map_or_else(
            || declaration.id.clone(),
            |instance| format!("{}-{instance}", declaration.id),
        );
        let participant_id = ParticipantId::new(rendered_id)
            .expect("the compiler emitted a normalized participant id");
        let kind = match declaration.kind {
            SourceParticipantKind::Service => ParticipantKind::Service,
            SourceParticipantKind::Driver => ParticipantKind::Driver,
            SourceParticipantKind::Simulator => ParticipantKind::Simulator,
            SourceParticipantKind::Brain => ParticipantKind::Brain,
        };
        let bytes = format!("fixture binary {}", participant_id.as_str()).into_bytes();
        let binary_path =
            BundlePath::new(format!("bin/{}", participant_id.as_str())).expect("binary path");
        let binary = BinaryReference::from_bytes(
            binary_path.clone(),
            BuildFacts {
                package: format!("fixture-{}", declaration.id),
                target: "host".to_string(),
                profile: "debug".to_string(),
            },
            BinaryCompatibility {
                participant_id: participant_id.clone(),
                kind,
                api: RobotApi::V0_2,
                schemas: ParticipantSchemas {
                    bus: BusAbi::V0,
                    launch: LaunchAbi::V0,
                    robot: RobotSchema::V0,
                    component: ComponentSchema::V0,
                    simulation: SimulationSchema::V0,
                },
                config_schema: serde_json::json!({"type": "null"}),
            },
            &bytes,
        );
        binaries.insert(binary_path, bytes);
        participants.push(RuntimeParticipant {
            id: participant_id,
            kind,
            binary,
            startup: StartupRequirement {
                required: true,
                ready: true,
            },
            config: declaration.config,
            binding: declaration
                .component_instance
                .map(|instance| ComponentBinding {
                    component_instance: phoxal_model::identity::ComponentInstanceId::new(instance)
                        .expect("the compiler emitted a normalized component instance"),
                }),
            clock,
        });
    }
    let document = RuntimeDocument::new(Runtime {
        robot,
        participants,
        assets: asset_index,
        router: None,
    })
    .expect("fixture runtime document");
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
    RuntimeBundle::open(bundle.path())
        .expect("the staged bundle must load")
        .robot()
        .clone()
}

#[cfg(test)]
mod tests {
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
}
