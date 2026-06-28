//! Framework-side official-set gate: every official runtime emits y2026_1
//! metadata, and the representative fixture has producers for every consumed
//! pub/sub family.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

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

#[derive(Debug, serde::Deserialize)]
struct Contract {
    family: String,
    topic: String,
    direction: String,
}

#[test]
fn official_runtime_set_matches_y2026_1_fixture_topology() {
    let root = workspace_root();
    let fixture = root.join("fixture/robot/rgbd-imu-diff-drive/robot.yaml");
    let fixture_text = std::fs::read_to_string(&fixture)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", fixture.display()));
    assert!(
        fixture_text.contains("api_version: y2026_1"),
        "representative fixture must select y2026_1"
    );

    let names = official_runtime_names(&root);
    assert_eq!(names.len(), 18, "expected the full official runtime set");

    let mut metadata = Vec::new();
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
        metadata.push(emitted);
    }

    let mut publishers = fixture_pubsub_producers();
    let mut subscribers: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();

    for runtime in &metadata {
        for contract in &runtime.required_contracts {
            let key = (contract.family.clone(), contract.topic.clone());
            match contract.direction.as_str() {
                "publish" => {
                    publishers.insert(key);
                }
                "subscribe" => {
                    subscribers
                        .entry(key)
                        .or_default()
                        .insert(runtime.artifact.id.clone());
                }
                _ => {}
            }
        }
    }

    let missing: Vec<_> = subscribers
        .into_iter()
        .filter(|(key, _)| !publishers.contains(key))
        .collect();
    assert!(
        missing.is_empty(),
        "fixture topology has consumed pub/sub families without producers: {missing:#?}"
    );
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

fn fixture_pubsub_producers() -> BTreeSet<(String, String)> {
    [
        // Operator/tool inputs for the representative robot.
        ("mission::Command", "mission/command"),
        ("motion::ManualCommand", "motion/manual"),
        ("power::Command", "power/command"),
        ("safety::EmergencyStopRequest", "safety/estop"),
        // Component-driver/simulator outputs materialized by the fixture.
        (
            "component::accelerometer::Sample",
            "component/{instance}/accelerometer/{capability}/sample",
        ),
        (
            "component::camera::Frame",
            "component/{instance}/camera/{capability}/frame",
        ),
        (
            "component::depth::Frame",
            "component/{instance}/depth/{capability}/frame",
        ),
        (
            "component::encoder::Sample",
            "component/{instance}/encoder/{capability}/sample",
        ),
        (
            "component::emergency_stop::State",
            "component/{instance}/emergency_stop/{capability}/state",
        ),
        (
            "component::gnss::Sample",
            "component/{instance}/gnss/{capability}/sample",
        ),
        (
            "component::gyroscope::Sample",
            "component/{instance}/gyroscope/{capability}/sample",
        ),
        (
            "component::imu::Sample",
            "component/{instance}/imu/{capability}/sample",
        ),
    ]
    .into_iter()
    .map(|(family, topic)| (family.to_string(), topic.to_string()))
    .collect()
}
