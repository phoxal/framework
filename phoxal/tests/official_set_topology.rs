//! Framework-side official-set gate: every official participant emits y2026_1
//! metadata, and a representative fixture exercises pub/sub plus query/server
//! topology cardinality.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use phoxal::model::component::v1::capability::Capability;
use phoxal::model::v1::Robot;

#[derive(Debug, serde::Deserialize)]
struct ParticipantMetadata {
    artifact: Artifact,
    api_version: String,
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

#[derive(Clone, Debug)]
struct TopologyContract {
    artifact_id: String,
    family: String,
    topic: String,
    direction: String,
}

#[derive(Default)]
struct Topology {
    contracts: Vec<TopologyContract>,
}

#[derive(Debug, PartialEq, Eq)]
struct Finding {
    kind: &'static str,
    family: String,
    topic: String,
}

#[derive(Default)]
struct Report {
    errors: Vec<Finding>,
    warnings: Vec<Finding>,
}

#[test]
fn official_service_set_matches_y2026_1_fixture_topology() {
    let root = workspace_root();
    let fixture_dir = root.join("fixture/robot/rgbd-imu-diff-drive");
    let fixture = Robot::read_from_dir(&fixture_dir)
        .unwrap_or_else(|e| panic!("failed to load {}: {e:#}", fixture_dir.display()));
    assert_eq!(fixture.manifest.api_version, "y2026_1");

    let names = official_service_names(&root);
    assert!(
        !names.is_empty(),
        "service/ must contain the official service source of truth"
    );

    let mut topology = Topology::default();
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
            emitted.api_version, "y2026_1",
            "service {name} must report y2026_1"
        );
        assert_eq!(
            emitted.bus_abi, "phoxal-bus/v0",
            "service {name} must report the frozen bus_abi"
        );
        topology.add_service_contracts(&emitted, &fixture);
    }
    // The participants injected below are FIXTURES, not real official producers:
    // their artifact ids are namespaced `fixture/...` (see `topology_contract`
    // calls in `add_fixture_external_participants`). They stand in for the
    // external clients/components a real robot would run, so a topic that only a
    // fixture would publish never masks a missing REAL official producer: the
    // cardinality report attributes responders by artifact id.
    topology.add_fixture_external_participants(&fixture);

    let report = topology.report();
    assert!(
        report.errors.is_empty(),
        "fixture topology has cardinality errors: {:#?}",
        report.errors
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.kind == "publisher_without_subscriber"),
        "fixture should exercise output-without-consumer warnings"
    );
}

#[test]
fn subscriber_without_publisher_is_topology_error() {
    let mut topology = Topology::default();
    topology.add(topology_contract(
        "fixture/external",
        "drive::Target",
        "drive/target",
        "subscribe",
    ));

    let report = topology.report();
    assert!(report.warnings.is_empty());
    assert_eq!(
        report.errors,
        vec![Finding {
            kind: "subscriber_without_publisher",
            family: "drive::Target".to_string(),
            topic: "drive/target".to_string(),
        }]
    );
}

#[test]
fn publisher_without_subscriber_is_topology_warning() {
    let mut topology = Topology::default();
    topology.add(topology_contract(
        "fixture/external",
        "drive::State",
        "drive/state",
        "publish",
    ));

    let report = topology.report();
    assert!(report.errors.is_empty());
    assert_eq!(
        report.warnings,
        vec![Finding {
            kind: "publisher_without_subscriber",
            family: "drive::State".to_string(),
            topic: "drive/state".to_string(),
        }]
    );
}

#[test]
fn server_only_query_topic_is_topology_error() {
    let mut topology = Topology::default();
    topology.add(topology_contract(
        "asset",
        "asset::GetRequest",
        "asset/get",
        "server_request",
    ));
    topology.add(topology_contract(
        "asset",
        "asset::GetResponse",
        "asset/get",
        "server_response",
    ));

    let report = topology.report();
    assert!(report.warnings.is_empty());
    assert_eq!(
        report.errors,
        vec![
            Finding {
                kind: "query_server_missing_peer",
                family: "asset::GetRequest".to_string(),
                topic: "asset/get".to_string(),
            },
            Finding {
                kind: "query_server_missing_peer",
                family: "asset::GetResponse".to_string(),
                topic: "asset/get".to_string(),
            },
        ]
    );
}

#[test]
fn duplicate_server_responder_is_topology_error() {
    let mut topology = Topology::default();
    topology.add(topology_contract(
        "fixture/external",
        "asset::GetRequest",
        "asset/get",
        "query_request",
    ));
    topology.add(topology_contract(
        "fixture/external",
        "asset::GetResponse",
        "asset/get",
        "query_response",
    ));
    for artifact_id in ["asset", "asset-copy"] {
        topology.add(topology_contract(
            artifact_id,
            "asset::GetRequest",
            "asset/get",
            "server_request",
        ));
        topology.add(topology_contract(
            artifact_id,
            "asset::GetResponse",
            "asset/get",
            "server_response",
        ));
    }

    let report = topology.report();
    assert!(report.warnings.is_empty());
    assert_eq!(
        report.errors,
        vec![Finding {
            kind: "multiple_server_responders",
            family: "asset::GetResponse".to_string(),
            topic: "asset/get".to_string(),
        }]
    );
}

#[test]
fn dynamic_component_contracts_are_checked_at_family_level() {
    let mut topology = Topology::default();
    topology.add(topology_contract(
        "drive",
        "component::motor::Command",
        "component/{instance}/motor/{capability}/command",
        "publish",
    ));
    topology.add(topology_contract(
        "fixture/component/front_left_drive",
        "component::motor::Command",
        "component/front_left_drive/motor/wheel/command",
        "subscribe",
    ));

    let report = topology.report();
    assert!(report.errors.is_empty());
    assert!(report.warnings.is_empty());
    assert_eq!(
        topology.contracts[0].topic,
        "component/{instance}/motor/{capability}/command"
    );
    assert_eq!(
        topology.contracts[1].topic,
        "component/{instance}/motor/{capability}/command"
    );
}

impl Topology {
    fn add_service_contracts(&mut self, metadata: &ParticipantMetadata, robot: &Robot) {
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
            if let Some(kind) = component_topic_kind(&contract.topic)
                && !robot_has_capability_kind(robot, kind)
            {
                continue;
            }
            self.add(TopologyContract {
                artifact_id: metadata.artifact.id.clone(),
                family: contract.family.clone(),
                topic: contract.topic.clone(),
                direction: contract.direction.clone(),
            });
        }
    }

    fn add_fixture_external_participants(&mut self, robot: &Robot) {
        for contract in external_pubsub_inputs() {
            self.add(contract);
        }
        for contract in external_query_clients() {
            self.add(contract);
        }

        for (instance_id, instance) in &robot.manifest.components.instances {
            let component = robot
                .components
                .get(&instance.component)
                .unwrap_or_else(|| {
                    panic!("component type {} should be loaded", instance.component)
                });
            for (capability_id, capability) in &component.capabilities {
                let Some((family, topic, command_like)) =
                    component_contract(capability, instance_id, capability_id)
                else {
                    continue;
                };
                self.add(TopologyContract {
                    artifact_id: format!("fixture/component/{instance_id}"),
                    family,
                    topic,
                    direction: if command_like {
                        "subscribe".to_string()
                    } else {
                        "publish".to_string()
                    },
                });
            }
        }
    }

    fn add(&mut self, contract: TopologyContract) {
        let topic =
            normalize_component_topic(&contract.topic).unwrap_or_else(|| contract.topic.clone());
        self.contracts.push(TopologyContract { topic, ..contract });
    }

    fn report(&self) -> Report {
        let mut by_topic = BTreeMap::<(String, String), BTreeSet<String>>::new();
        let mut by_contract = BTreeMap::<(String, String, String), BTreeSet<String>>::new();
        for contract in &self.contracts {
            by_topic
                .entry((contract.family.clone(), contract.topic.clone()))
                .or_default()
                .insert(contract.direction.clone());
            by_contract
                .entry((
                    contract.family.clone(),
                    contract.topic.clone(),
                    contract.direction.clone(),
                ))
                .or_default()
                .insert(contract.artifact_id.clone());
        }

        let mut report = Report::default();
        for ((family, topic), directions) in by_topic {
            let has_publish = directions.contains("publish");
            let has_subscribe = directions.contains("subscribe");
            if has_subscribe && !has_publish {
                report.errors.push(Finding {
                    kind: "subscriber_without_publisher",
                    family: family.clone(),
                    topic: topic.clone(),
                });
            }
            if has_publish && !has_subscribe {
                report.warnings.push(Finding {
                    kind: "publisher_without_subscriber",
                    family: family.clone(),
                    topic: topic.clone(),
                });
            }

            let query_side =
                directions.contains("query_request") || directions.contains("query_response");
            let server_side =
                directions.contains("server_request") || directions.contains("server_response");
            if query_side != server_side {
                report.errors.push(Finding {
                    kind: "query_server_missing_peer",
                    family,
                    topic,
                });
            }
        }
        for ((family, topic, direction), artifact_ids) in by_contract {
            if direction == "server_response" && artifact_ids.len() > 1 {
                report.errors.push(Finding {
                    kind: "multiple_server_responders",
                    family,
                    topic,
                });
            }
        }
        report
    }
}

fn topology_contract(
    artifact_id: &str,
    family: &str,
    topic: &str,
    direction: &str,
) -> TopologyContract {
    TopologyContract {
        artifact_id: artifact_id.to_string(),
        family: family.to_string(),
        topic: topic.to_string(),
        direction: direction.to_string(),
    }
}

fn normalize_component_topic(topic: &str) -> Option<String> {
    let mut parts = topic.split('/');
    let component = parts.next()?;
    let _instance = parts.next()?;
    let kind = parts.next()?;
    let _capability = parts.next()?;
    let leaf = parts.next()?;
    if component == "component" && parts.next().is_none() {
        Some(format!(
            "component/{{instance}}/{kind}/{{capability}}/{leaf}"
        ))
    } else {
        None
    }
}

fn is_schema_id(value: &str) -> bool {
    value.len() == 16
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn component_topic_kind(topic: &str) -> Option<&str> {
    let mut parts = topic.split('/');
    match (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) {
        (Some("component"), Some(_), Some(kind), Some(_), Some(_)) if parts.next().is_none() => {
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

fn component_contract(
    capability: &Capability,
    instance: &str,
    capability_id: &str,
) -> Option<(String, String, bool)> {
    let kind = capability.kind_name();
    let (family_tail, leaf, command_like) = match kind {
        "motor" => ("motor::Command", "command", true),
        "encoder" => ("encoder::Sample", "sample", false),
        "accelerometer" => ("accelerometer::Sample", "sample", false),
        "gyroscope" => ("gyroscope::Sample", "sample", false),
        "magnetometer" => ("magnetometer::Sample", "sample", false),
        "imu" => ("imu::Sample", "sample", false),
        "gnss" => ("gnss::Sample", "sample", false),
        "camera" => ("camera::Frame", "frame", false),
        "depth" => ("depth::Frame", "frame", false),
        "emergency_stop" => ("emergency_stop::State", "state", false),
        "range" => ("range::Sample", "sample", false),
        "lidar" => ("lidar::Scan", "scan", false),
        "mmwave" => ("mmwave::Scan", "scan", false),
        "microphone" => ("microphone::Frame", "frame", false),
        "led" => ("led::Command", "command", true),
        _ => return None,
    };
    Some((
        format!("component::{family_tail}"),
        format!("component/{instance}/{kind}/{capability_id}/{leaf}"),
        command_like,
    ))
}

fn external_pubsub_inputs() -> Vec<TopologyContract> {
    [
        ("mission::Command", "mission/command"),
        ("motion::ManualCommand", "motion/manual"),
        ("power::Command", "power/command"),
        ("safety::EmergencyStopRequest", "safety/estop"),
        // Each participant publishes its own `command`-role heartbeat; the
        // presence participant subscribes them and republishes one aggregate on the
        // `state`-role `presence/state`. The producers are external participants
        // (see the `runtime_async_entrypoint` example), so they stand in here as a
        // fixture input - otherwise presence's heartbeat subscribe would look like
        // a subscriber without a publisher.
        ("presence::Heartbeat", "presence/heartbeat"),
    ]
    .into_iter()
    .map(|(family, topic)| topology_contract("fixture/external", family, topic, "publish"))
    .collect()
}

fn external_query_clients() -> Vec<TopologyContract> {
    [
        ("asset::GetRequest", "asset/get", "query_request"),
        ("asset::GetResponse", "asset/get", "query_response"),
        ("frame::LookupRequest", "frame/lookup", "query_request"),
        ("frame::LookupResponse", "frame/lookup", "query_response"),
        ("video::OpenRequest", "video/open", "query_request"),
        ("video::OpenResponse", "video/open", "query_response"),
    ]
    .into_iter()
    .map(|(family, topic, direction)| {
        topology_contract("fixture/external", family, topic, direction)
    })
    .collect()
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
