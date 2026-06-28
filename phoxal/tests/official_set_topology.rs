//! Framework-side official-set gate: every official runtime emits y2026_1
//! metadata, and a representative fixture exercises pub/sub plus query/server
//! topology cardinality.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use phoxal::model::component::v1::capability::Capability;
use phoxal::model::v1::Robot;

#[derive(Debug, serde::Deserialize)]
struct RuntimeMetadata {
    artifact: Artifact,
    api_version: String,
    required_contracts: Vec<Contract>,
}

#[derive(Debug, serde::Deserialize)]
struct Artifact {
    id: String,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct Contract {
    family: String,
    topic: String,
    direction: String,
}

#[derive(Default)]
struct Topology {
    contracts: Vec<Contract>,
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
fn official_runtime_set_matches_y2026_1_fixture_topology() {
    let root = workspace_root();
    let fixture_dir = root.join("fixture/robot/rgbd-imu-diff-drive");
    let fixture = Robot::read_from_dir(&fixture_dir)
        .unwrap_or_else(|e| panic!("failed to load {}: {e:#}", fixture_dir.display()));
    assert_eq!(fixture.manifest.api_version, "y2026_1");

    let names = official_runtime_names(&root);
    assert!(
        !names.is_empty(),
        "runtime/ must contain the official runtime source of truth"
    );

    let mut topology = Topology::default();
    for name in &names {
        let emitted = emit_apis(&root, name);
        assert_eq!(
            emitted.artifact.id, *name,
            "runtime package {name} emitted a different artifact id"
        );
        assert_eq!(
            emitted.api_version, "y2026_1",
            "runtime {name} must report y2026_1"
        );
        topology.add_runtime_contracts(&emitted, &fixture);
    }
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
    topology.add(Contract {
        family: "drive::Target".to_string(),
        topic: "drive/target".to_string(),
        direction: "subscribe".to_string(),
    });

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
    topology.add(Contract {
        family: "drive::State".to_string(),
        topic: "drive/state".to_string(),
        direction: "publish".to_string(),
    });

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
    topology.add(Contract {
        family: "asset::GetRequest".to_string(),
        topic: "asset/get".to_string(),
        direction: "server_request".to_string(),
    });
    topology.add(Contract {
        family: "asset::GetResponse".to_string(),
        topic: "asset/get".to_string(),
        direction: "server_response".to_string(),
    });

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

impl Topology {
    fn add_runtime_contracts(&mut self, metadata: &RuntimeMetadata, robot: &Robot) {
        for contract in &metadata.required_contracts {
            self.contracts.extend(materialize_contract(contract, robot));
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
                self.add(Contract {
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

    fn add(&mut self, contract: Contract) {
        self.contracts.push(contract);
    }

    fn report(&self) -> Report {
        let mut by_topic = BTreeMap::<(String, String), BTreeSet<String>>::new();
        for contract in &self.contracts {
            by_topic
                .entry((contract.family.clone(), contract.topic.clone()))
                .or_default()
                .insert(contract.direction.clone());
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
        report
    }
}

fn materialize_contract(contract: &Contract, robot: &Robot) -> Vec<Contract> {
    let Some(kind) = component_topic_kind(&contract.topic) else {
        return vec![contract.clone()];
    };

    let mut materialized = Vec::new();
    for (instance_id, instance) in &robot.manifest.components.instances {
        let component = robot
            .components
            .get(&instance.component)
            .unwrap_or_else(|| panic!("component type {} should be loaded", instance.component));
        for (capability_id, capability) in &component.capabilities {
            if capability.kind_name() == kind {
                materialized.push(Contract {
                    family: contract.family.clone(),
                    topic: contract
                        .topic
                        .replace("{instance}", instance_id)
                        .replace("{capability}", capability_id),
                    direction: contract.direction.clone(),
                });
            }
        }
    }
    materialized
}

fn component_topic_kind(topic: &str) -> Option<&str> {
    let mut parts = topic.split('/');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("component"), Some("{instance}"), Some(kind), Some("{capability}")) => Some(kind),
        _ => None,
    }
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

fn external_pubsub_inputs() -> Vec<Contract> {
    [
        ("mission::Command", "mission/command"),
        ("motion::ManualCommand", "motion/manual"),
        ("power::Command", "power/command"),
        ("safety::EmergencyStopRequest", "safety/estop"),
    ]
    .into_iter()
    .map(|(family, topic)| Contract {
        family: family.to_string(),
        topic: topic.to_string(),
        direction: "publish".to_string(),
    })
    .collect()
}

fn external_query_clients() -> Vec<Contract> {
    [
        ("asset::GetRequest", "asset/get", "query_request"),
        ("asset::GetResponse", "asset/get", "query_response"),
        ("frame::LookupRequest", "frame/lookup", "query_request"),
        ("frame::LookupResponse", "frame/lookup", "query_response"),
        ("video::OpenRequest", "video/open", "query_request"),
        ("video::OpenResponse", "video/open", "query_response"),
    ]
    .into_iter()
    .map(|(family, topic, direction)| Contract {
        family: family.to_string(),
        topic: topic.to_string(),
        direction: direction.to_string(),
    })
    .collect()
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("phoxal crate has a workspace parent")
        .to_path_buf()
}

fn official_runtime_names(root: &Path) -> Vec<String> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(root.join("runtime")).expect("runtime directory exists") {
        let entry = entry.expect("runtime directory entry is readable");
        if !entry.path().join("Cargo.toml").is_file() {
            continue;
        }
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    names
}

fn emit_apis(root: &Path, name: &str) -> RuntimeMetadata {
    let package = format!("phoxal-runtime-{name}");
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
