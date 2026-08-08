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
    AssetIndex, BinaryCompatibility, BinaryReference, BuildFacts, BundlePath, BundleWriter,
    ComponentBinding, Runtime, RuntimeBundle, RuntimeDocument, RuntimeParticipant,
    StartupRequirement,
};
use phoxal_manifest::{SourceSet, source};
use phoxal_model::identity::ComponentInstanceId;
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
    let (robot, services, drivers, assets) = compiled.into_parts();
    let assets = assets.into_map();
    let asset_index = AssetIndex::from_bytes(&assets).expect("fixture asset index");

    let clock = match robot.clock() {
        Clock::Real => ClockMode::Real,
        Clock::Simulated => ClockMode::Simulation,
    };
    let mut participants = Vec::new();
    let mut binaries = BTreeMap::new();
    let mut stage_participant =
        |rendered_id: String,
         kind: ParticipantKind,
         config: Option<serde_json::Value>,
         component_instance: Option<ComponentInstanceId>| {
            let participant_id = ParticipantId::new(rendered_id)
                .expect("the compiler emitted a normalized participant id");
            let bytes = format!("fixture binary {}", participant_id.as_str()).into_bytes();
            let binary_path =
                BundlePath::new(format!("bin/{}", participant_id.as_str())).expect("binary path");
            let config_schema = if config.is_some() {
                serde_json::json!({})
            } else {
                serde_json::json!({"type": "null"})
            };
            let binary = BinaryReference::from_bytes(
                binary_path.clone(),
                BuildFacts {
                    package: format!("fixture-{}", participant_id.as_str()),
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
                    requirement: None,
                    config_schema,
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
                config,
                binding: component_instance
                    .map(|component_instance| ComponentBinding { component_instance }),
                clock,
            });
        };
    for service in services {
        stage_participant(service.id, ParticipantKind::Service, service.config, None);
    }
    for driver in drivers {
        let instance = robot
            .component_instance(driver.component_instance.as_str())
            .expect("a compiled driver must bind a canonical component instance");
        let rendered_id = format!("{}-{}", instance.component_type(), instance.id());
        stage_participant(
            rendered_id,
            ParticipantKind::Driver,
            Some(serde_json::to_value(driver.config).expect("driver config is serializable")),
            Some(driver.component_instance),
        );
    }
    // Simulator membership is a canonical Robot fact. The tooling assembler
    // selects simulator processes from it here; the source compiler does not
    // invent a process declaration for every component with simulation.yaml.
    for instance in robot.components() {
        if robot
            .simulation_for_instance(instance.id().as_str())
            .is_some()
        {
            let rendered_id = format!("{}-{}", instance.component_type(), instance.id());
            stage_participant(
                rendered_id,
                ParticipantKind::Simulator,
                None,
                Some(instance.id().clone()),
            );
        }
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
