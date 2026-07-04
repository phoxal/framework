//! Framework-side official-set gate: every official participant emits y2026_1
//! metadata, and the shared graph checker validates a representative fixture.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use phoxal::check::{
    self, CheckInput, ComponentCapability, Contract as CheckContract, Direction, ParticipantApis,
    ParticipantClass, ParticipantKind, ParticipantScope, PlanMode, RobotGraph,
};
use phoxal::model::component::v1::CapabilityRef;
use phoxal::model::component::v1::capability::Capability;
use phoxal::model::robot::v1::KinematicConfig;
use phoxal::model::v1::Robot;

type SchemaIds = BTreeMap<(String, String), String>;

#[derive(Debug, serde::Deserialize)]
struct ParticipantMetadata {
    artifact: Artifact,
    api_version: String,
    participant_class: String,
    bus_abi: String,
    required_contracts: Vec<Contract>,
}

#[derive(Debug, serde::Deserialize)]
struct Artifact {
    kind: String,
    id: String,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct Contract {
    api_version: String,
    schema_id: String,
    family: String,
    topic: String,
    direction: String,
}

#[test]
fn official_service_set_matches_y2026_1_fixture_topology() {
    let root = workspace_root();
    let fixture_dir = root.join("fixture/robot/rgbd-imu-diff-drive");
    let fixture = Robot::read_from_dir(&fixture_dir)
        .unwrap_or_else(|e| panic!("failed to load {}: {e:#}", fixture_dir.display()));
    assert_eq!(
        fixture.manifest.api_version, None,
        "the fixture intentionally exercises D5's omitted root api_version"
    );
    assert_eq!(
        fixture.manifest.phoxal_artifacts.generation, None,
        "the fixture leaves generation resolution to the catalog solver"
    );

    let names = official_service_names(&root);
    assert!(
        !names.is_empty(),
        "service/ must contain the official service source of truth"
    );

    let mut schema_ids = SchemaIds::new();
    let mut participants = Vec::new();
    for name in &names {
        let emitted = emit_apis(&root, name);
        assert_eq!(
            emitted.artifact.id, *name,
            "service package {name} emitted a different artifact id"
        );
        assert_eq!(
            emitted.artifact.kind, "service",
            "service package {name} must emit the service artifact kind"
        );
        assert_eq!(
            emitted.participant_class, "checked",
            "service package {name} must emit checked participant_class"
        );
        assert_eq!(
            emitted.api_version, "y2026_1",
            "service {name} must report y2026_1"
        );
        assert_eq!(
            emitted.bus_abi, "phoxal-bus/v0",
            "service {name} must report the frozen bus_abi"
        );

        validate_required_contract_metadata(&emitted);
        remember_schema_ids(&mut schema_ids, &emitted);
        participants.push(participant_from_metadata(&emitted, &fixture));
    }

    add_fixture_external_participants(&mut participants, &schema_ids, &fixture);

    let graph = robot_graph(&fixture);
    let report = check::check_plan(CheckInput {
        mode: PlanMode::Deploy,
        participants: &participants,
        robot_graph: &graph,
        substitutions: &[],
    });
    assert!(
        report.problems.is_empty(),
        "fixture topology has shared-check errors: {:#?}",
        report.problems
    );
    let expected_observable_output_warning = check::Warning::MissingConsumer {
        family: "mission::Goal".to_string(),
        topic: "mission/goal".to_string(),
        producers: vec!["mission".to_string()],
    };
    assert!(
        report
            .warnings
            .contains(&expected_observable_output_warning),
        "fixture should warn for mission/goal output without a consumer: {:#?}",
        report.warnings
    );
}

fn participant_from_metadata(metadata: &ParticipantMetadata, robot: &Robot) -> ParticipantApis {
    let participant_class =
        ParticipantClass::parse(&metadata.participant_class).unwrap_or_else(|| {
            panic!(
                "participant {} emitted unknown participant_class '{}'",
                metadata.artifact.id, metadata.participant_class
            )
        });

    ParticipantApis {
        participant_id: metadata.artifact.id.clone(),
        artifact_id: metadata.artifact.id.clone(),
        participant_kind: ParticipantKind::parse(&metadata.artifact.kind),
        participant_class,
        api_version: metadata.api_version.clone(),
        bus_abi: Some(metadata.bus_abi.clone()),
        config_schema: None,
        scope: ParticipantScope::Graph,
        contracts: metadata
            .required_contracts
            .iter()
            .filter(|contract| contract_is_in_fixture_scope(contract, robot))
            .map(|contract| contract_from_metadata(metadata, contract))
            .collect(),
    }
}

fn contract_is_in_fixture_scope(contract: &Contract, robot: &Robot) -> bool {
    component_topic_kind(&contract.topic).is_none_or(|kind| robot_has_capability_kind(robot, kind))
}

fn contract_from_metadata(metadata: &ParticipantMetadata, contract: &Contract) -> CheckContract {
    CheckContract {
        family: contract.family.clone(),
        topic: contract.topic.clone(),
        direction: Direction::parse(&contract.direction).unwrap_or_else(|| {
            panic!(
                "service {} emitted unknown direction '{}'",
                metadata.artifact.id, contract.direction
            )
        }),
        schema_id: contract.schema_id.clone(),
    }
}

fn validate_required_contract_metadata(metadata: &ParticipantMetadata) {
    for contract in &metadata.required_contracts {
        assert_eq!(
            contract.api_version, metadata.api_version,
            "service {} emitted a per-contract api_version that differs from the artifact api_version",
            metadata.artifact.id
        );
        assert!(
            is_schema_id(&contract.schema_id),
            "service {} emitted invalid schema_id '{}' for {}",
            metadata.artifact.id,
            contract.schema_id,
            contract.family
        );
    }
}

fn remember_schema_ids(schema_ids: &mut SchemaIds, metadata: &ParticipantMetadata) {
    for contract in &metadata.required_contracts {
        schema_ids
            .entry((contract.family.clone(), contract.topic.clone()))
            .or_insert_with(|| contract.schema_id.clone());
    }
}

fn add_fixture_external_participants(
    participants: &mut Vec<ParticipantApis>,
    schema_ids: &SchemaIds,
    robot: &Robot,
) {
    let mut external_contracts = Vec::new();
    for (family, topic) in external_pubsub_inputs() {
        external_contracts.push(fixture_contract(
            "fixture/external",
            schema_ids,
            family,
            topic,
            topic,
            Direction::Publish,
        ));
    }
    for (family, topic, direction) in external_query_clients() {
        external_contracts.push(fixture_contract(
            "fixture/external",
            schema_ids,
            family,
            topic,
            topic,
            direction,
        ));
    }
    participants.push(fixture_participant("fixture/external", external_contracts));

    let mut component_contracts = BTreeMap::<String, Vec<CheckContract>>::new();
    for (instance_id, instance) in &robot.manifest.components.instances {
        let component = robot
            .components
            .get(&instance.component)
            .unwrap_or_else(|| panic!("component type {} should be loaded", instance.component));
        for (capability_id, capability) in &component.capabilities {
            if let Some(contract) =
                component_contract(schema_ids, capability, instance_id, capability_id)
            {
                component_contracts
                    .entry(instance_id.clone())
                    .or_default()
                    .push(contract);
            }
        }
    }

    for (instance_id, contracts) in component_contracts {
        participants.push(fixture_participant(
            &format!("fixture/component/{instance_id}"),
            contracts,
        ));
    }
}

fn fixture_participant(id: &str, contracts: Vec<CheckContract>) -> ParticipantApis {
    ParticipantApis {
        participant_id: id.to_string(),
        artifact_id: id.to_string(),
        participant_kind: ParticipantKind::Service,
        participant_class: ParticipantClass::Checked,
        api_version: "y2026_1".to_string(),
        bus_abi: None,
        config_schema: None,
        scope: ParticipantScope::Graph,
        contracts,
    }
}

fn fixture_contract(
    participant_id: &str,
    schema_ids: &SchemaIds,
    family: &str,
    schema_topic: &str,
    topic: &str,
    direction: Direction,
) -> CheckContract {
    let schema_id = schema_ids
        .get(&(family.to_string(), schema_topic.to_string()))
        .unwrap_or_else(|| {
            panic!(
                "fixture participant {participant_id} needs schema for {family} on {schema_topic}"
            )
        })
        .clone();

    CheckContract {
        family: family.to_string(),
        topic: topic.to_string(),
        direction,
        schema_id,
    }
}

fn component_contract(
    schema_ids: &SchemaIds,
    capability: &Capability,
    instance: &str,
    capability_id: &str,
) -> Option<CheckContract> {
    let kind = capability.kind_name();
    let (family_tail, leaf, direction) = match kind {
        "motor" => ("motor::Command", "command", Direction::Subscribe),
        "encoder" => ("encoder::Sample", "sample", Direction::Publish),
        "accelerometer" => ("accelerometer::Sample", "sample", Direction::Publish),
        "gyroscope" => ("gyroscope::Sample", "sample", Direction::Publish),
        "magnetometer" => ("magnetometer::Sample", "sample", Direction::Publish),
        "imu" => ("imu::Sample", "sample", Direction::Publish),
        "gnss" => ("gnss::Sample", "sample", Direction::Publish),
        "camera" => ("camera::Frame", "frame", Direction::Publish),
        "depth" => ("depth::Frame", "frame", Direction::Publish),
        "emergency_stop" => ("emergency_stop::State", "state", Direction::Publish),
        "range" => ("range::Sample", "sample", Direction::Publish),
        "lidar" => ("lidar::Scan", "scan", Direction::Publish),
        "mmwave" => ("mmwave::Scan", "scan", Direction::Publish),
        "microphone" => ("microphone::Frame", "frame", Direction::Publish),
        "led" => ("led::Command", "command", Direction::Subscribe),
        _ => return None,
    };
    let family = format!("component::{family_tail}");
    let schema_topic = format!("component/{{instance}}/{kind}/{{capability}}/{leaf}");
    let topic = format!("component/{instance}/{kind}/{capability_id}/{leaf}");

    schema_ids
        .contains_key(&(family.clone(), schema_topic.clone()))
        .then(|| {
            fixture_contract(
                &format!("fixture/component/{instance}"),
                schema_ids,
                &family,
                &schema_topic,
                &topic,
                direction,
            )
        })
}

fn robot_graph(robot: &Robot) -> RobotGraph {
    let mut component_capabilities = Vec::new();
    for (instance_id, instance) in &robot.manifest.components.instances {
        let component = robot
            .components
            .get(&instance.component)
            .unwrap_or_else(|| panic!("component type {} should be loaded", instance.component));
        for (capability_id, capability) in &component.capabilities {
            component_capabilities.push(ComponentCapability {
                instance: instance_id.clone(),
                capability: capability_id.clone(),
                kind: capability.kind_name().to_string(),
            });
        }
    }
    component_capabilities.sort();

    RobotGraph {
        component_capabilities,
        motion_capabilities: motion_capabilities(&robot.manifest.motion.kinematic),
    }
}

fn motion_capabilities(kinematic: &KinematicConfig) -> BTreeSet<(String, String)> {
    let mut capabilities = BTreeSet::new();
    let mut add = |capability: &CapabilityRef| {
        capabilities.insert((
            capability.component_id.clone(),
            capability.capability_id.clone(),
        ));
    };

    match kinematic {
        KinematicConfig::Differential {
            left_actuators,
            right_actuators,
            left_encoders,
            right_encoders,
            ..
        } => {
            for capability in left_actuators
                .iter()
                .chain(right_actuators)
                .chain(left_encoders)
                .chain(right_encoders)
            {
                add(capability);
            }
        }
        KinematicConfig::Mecanum {
            front_left_actuator,
            front_right_actuator,
            rear_left_actuator,
            rear_right_actuator,
            ..
        } => {
            add(front_left_actuator);
            add(front_right_actuator);
            add(rear_left_actuator);
            add(rear_right_actuator);
        }
        KinematicConfig::Ackermann {
            steering_actuator,
            drive_actuator,
            steering_encoder,
            drive_encoder,
            ..
        } => {
            add(steering_actuator);
            add(drive_actuator);
            if let Some(encoder) = steering_encoder {
                add(encoder);
            }
            if let Some(encoder) = drive_encoder {
                add(encoder);
            }
        }
        KinematicConfig::Omnidirectional {
            actuators,
            encoders,
        } => {
            for capability in actuators.iter().chain(encoders) {
                add(capability);
            }
        }
    }

    capabilities
}

fn is_schema_id(value: &str) -> bool {
    value.len() == 16
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn component_topic_kind(topic: &str) -> Option<&str> {
    let mut parts = topic.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("component"), Some("{instance}" | "*"), Some(kind)) if !kind.starts_with('{') => {
            Some(kind)
        }
        _ => None,
    }
}

fn robot_has_capability_kind(robot: &Robot, kind: &str) -> bool {
    robot
        .manifest
        .components
        .instances
        .values()
        .any(|instance| {
            robot
                .components
                .get(&instance.component)
                .is_some_and(|component| {
                    component
                        .capabilities
                        .values()
                        .any(|capability| capability.kind_name() == kind)
                })
        })
}

fn external_pubsub_inputs() -> Vec<(&'static str, &'static str)> {
    vec![
        ("mission::Command", "mission/command"),
        ("motion::ManualCommand", "motion/manual"),
        ("power::Command", "power/command"),
        ("safety::EmergencyStopRequest", "safety/estop"),
    ]
}

fn external_query_clients() -> Vec<(&'static str, &'static str, Direction)> {
    vec![
        ("asset::GetRequest", "asset/get", Direction::QueryRequest),
        ("asset::GetResponse", "asset/get", Direction::QueryResponse),
        (
            "frame::LookupRequest",
            "frame/lookup",
            Direction::QueryRequest,
        ),
        (
            "frame::LookupResponse",
            "frame/lookup",
            Direction::QueryResponse,
        ),
        ("video::OpenRequest", "video/open", Direction::QueryRequest),
        (
            "video::OpenResponse",
            "video/open",
            Direction::QueryResponse,
        ),
    ]
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("phoxal crate has a workspace parent")
        .to_path_buf()
}

fn official_service_names(root: &Path) -> Vec<String> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(root.join("service")).expect("service directory exists") {
        let entry = entry.expect("service directory entry is readable");
        if !entry.path().join("Cargo.toml").is_file() {
            continue;
        }
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    names
}

fn emit_apis(root: &Path, name: &str) -> ParticipantMetadata {
    let package = format!("phoxal-service-{name}");
    let output = Command::new("cargo")
        .args(["run", "--quiet", "-p", &package, "--", "emit-apis"])
        .current_dir(root)
        .output()
        .unwrap_or_else(|e| panic!("failed to run {package} emit-apis: {e}"));
    if !output.status.success() {
        panic!(
            "{package} emit-apis failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("{package} emitted invalid metadata JSON: {e}"))
}
