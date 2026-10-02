#![cfg(feature = "e2e")]

use std::fs;
use std::path::Path;
use std::process::Command;

#[test]
fn empty_brain_robot_runtime_compiles_against_generated_empty_contract()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let robot = directory.path().join("robot");
    fs::create_dir_all(robot.join("api/proof/vocabulary/v1"))?;
    fs::create_dir_all(robot.join("src"))?;
    fs::write(
        robot.join("api/proof/vocabulary/v1/messages.proto"),
        "syntax = \"proto3\"; package proof.vocabulary.v1; message Shared { string value = 1; }\n",
    )?;
    // No brain section and no Rust contract: a runtime with manual empty
    // endpoint types still compiles.
    fs::write(
        robot.join("robot.yaml"),
        "schema: phoxal/robot/v0\nrobot: { id: proof-empty-brain }\n",
    )?;
    let framework = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    fs::write(
        robot.join("Cargo.toml"),
        format!(
            "[package]\nname = \"proof-empty-brain-robot\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\nphoxal = {{ path = {:?}, default-features = false, features = [\"runtime\"] }}\n[build-dependencies]\nphoxal = {{ path = {:?}, default-features = false, features = [\"build\"] }}\n",
            framework.display().to_string(),
            framework.display().to_string()
        ),
    )?;
    fs::write(
        robot.join("build.rs"),
        "fn main() -> Result<(), phoxal::build::Error> { phoxal::build::api(phoxal::build::BuildApiConfig::default()) }\n",
    )?;
    fs::write(
        robot.join("src/main.rs"),
        "/// A contract with no endpoints.\n#[phoxal::endpoints]\npub struct BrainApi {}\nstruct Brain;\n#[phoxal::runtime(contract = BrainApi, period_ms = 20)]\nimpl Brain {\n    #[init]\n    fn new(_config: ()) -> phoxal::Result<Self> { Ok(Brain) }\n    #[step]\n    fn advance(&mut self, _ctx: &mut phoxal::runtime::Context<'_, Self>) -> phoxal::Result<()> { Ok(()) }\n}\nfn main() -> phoxal::Result<()> { phoxal::runtime::run::<Brain>() }\n",
    )?;

    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args(["build", "--offline", "--manifest-path"])
        .arg(robot.join("Cargo.toml"))
        .env(
            "CARGO_TARGET_DIR",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"),
        )
        .output()?;
    assert!(
        output.status.success(),
        "empty-brain runtime build failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

use std::path::PathBuf;

use phoxal_build::PreparedSelection;

/// Writes one prepared path-service product: a `contract.json` envelope
/// plus a minimal descriptor closure holding the service's call messages.
/// The service serves one call operation named `endpoint`.
fn write_prepared_service(
    robot_root: &Path,
    rel_path: &str,
    package: &str,
    service: &str,
    endpoint: &str,
    request: &str,
    response: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let identity = PreparedSelection::Path {
        path: rel_path.to_owned(),
    };
    write_prepared_under(
        phoxal_build::prepared_dir(robot_root, &identity, None),
        serde_json::json!({"kind": "path", "path": rel_path}),
        package,
        service,
        endpoint,
        request,
        response,
    )
}

fn write_prepared_under(
    dir: PathBuf,
    selection_json: serde_json::Value,
    package: &str,
    service: &str,
    endpoint: &str,
    request: &str,
    response: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use prost::Message as _;
    fs::create_dir_all(&dir)?;
    let message = |name: &str| prost_types::DescriptorProto {
        name: Some(name.to_owned()),
        field: vec![prost_types::FieldDescriptorProto {
            name: Some("value".to_owned()),
            number: Some(1),
            label: Some(prost_types::field_descriptor_proto::Label::Optional as i32),
            r#type: Some(prost_types::field_descriptor_proto::Type::String as i32),
            json_name: Some("value".to_owned()),
            ..Default::default()
        }],
        ..Default::default()
    };
    let set = prost_types::FileDescriptorSet {
        file: vec![prost_types::FileDescriptorProto {
            name: Some(format!("{package}.proto")),
            package: Some(package.to_owned()),
            message_type: vec![
                message(request.rsplit('.').next().unwrap_or(request)),
                message(response.rsplit('.').next().unwrap_or(response)),
            ],
            ..Default::default()
        }],
    };
    fs::write(dir.join("descriptors.pb"), set.encode_to_vec())?;
    let contract = serde_json::json!({
        "generation": 1,
        "selection": selection_json,
        "binary": null,
        "executable": {
            "sha256": "00".repeat(32),
            "package": "prepared-marker-fixture",
            "version": "0.1.0"
        },
        "runtime": {
            "schema": "phoxal/artifact/v0",
            "record": "runtime",
            "period_ms": 20,
            "timeout_ms": 100,
            "init_timeout_ms": 1_000,
            "config_schema": {},
            "inputs": [{
                "name": endpoint,
                "role": "call_ingress",
                "port": endpoint,
                "max_items": 4,
                "max_bytes": 1024,
                "max_age_ms": null,
                "request_fqn": request,
                "response_fqn": response,
                "signature": {
                    "endpoint": endpoint,
                    "service": service,
                    "method": endpoint,
                    "request": request,
                    "response": response,
                    "shape": "call",
                    "retained_latest": false,
                    "lease_valid_for_ms": null
                }
            }],
            "service_outputs": [],
            "transient_outputs": [{
                "name": format!("{endpoint}_replies"),
                "role": "reply",
                "port": endpoint,
                "input": endpoint,
                "max_items": 4,
                "max_bytes": 1024,
                "max_request_bytes": null,
                "bootstrap": false,
                "every_steps": null,
                "on_change": false,
                "project": null,
                "timeout_ms": null,
                "cancel_grace_ms": null,
                "valid_for_ms": null,
                "signature": null
            }]
        }
    });
    fs::write(dir.join("contract.json"), contract.to_string())?;
    Ok(())
}

/// Scaffolds one cold robot project whose generated API is compiled by a
/// real `cargo build`, mirroring `empty_brain_robot_runtime_...`.
fn scaffold_robot(
    directory: &Path,
    services: &str,
    main_body: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let robot = directory.join("robot");
    fs::create_dir_all(robot.join("src"))?;
    fs::write(
        robot.join("robot.yaml"),
        format!("schema: phoxal/robot/v0\nrobot: {{ id: marker-proof }}\nservices:\n{services}"),
    )?;
    let framework = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    fs::write(
        robot.join("Cargo.toml"),
        format!(
            "[package]\nname = \"marker-proof-robot\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\nphoxal = {{ path = {:?}, default-features = false, features = [\"runtime\"] }}\n[build-dependencies]\nphoxal = {{ path = {:?}, default-features = false, features = [\"build\"] }}\n",
            framework.display().to_string(),
            framework.display().to_string()
        ),
    )?;
    fs::write(
        robot.join("build.rs"),
        "fn main() -> Result<(), phoxal::build::Error> { phoxal::build::api(phoxal::build::BuildApiConfig::default()) }\n",
    )?;
    fs::write(robot.join("src/main.rs"), main_body)?;
    Ok(robot)
}

fn build_robot(robot: &Path) -> Result<std::process::Output, Box<dyn std::error::Error>> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    Ok(Command::new(cargo)
        .args(["build", "--offline", "--manifest-path"])
        .arg(robot.join("Cargo.toml"))
        .env(
            "CARGO_TARGET_DIR",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"),
        )
        .output()?)
}

/// Two provider instances of one contract beside a third provider sharing
/// the top package prefix: the canonical operations tree emits each shared
/// module once, equivalent descriptors deduplicate to one marker, and the
/// cold consumer compiles against both markers.
#[test]
fn shared_package_prefixes_and_two_instances_compile_one_operations_tree()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    let robot = scaffold_robot(
        root,
        "  alpha: { source: { path: ../provider-a } }\n  beta: { source: { path: ../provider-a } }\n  gamma: { source: { path: ../provider-b } }\n",
        "phoxal::api!();\n\n/// One contract naming both generated provider markers.\n#[phoxal::endpoints]\npub struct BrainApi {\n    #[phoxal::call]\n    ask: api::operations::proof::shared::v1::Ask,\n}\n\nfn main() {\n    let method = <api::operations::proof::shared::v1::Ask as phoxal::contracts::Operation>::METHOD;\n    assert_eq!(method.signature().service, \"proof.shared.v1.Ask\");\n    let _ = <api::operations::proof::other::v1::Ask as phoxal::contracts::Operation>::METHOD;\n}\n",
    )?;
    write_prepared_service(
        &robot,
        "../provider-a",
        "proof.shared.v1",
        "proof.shared.v1.Ask",
        "ask",
        "proof.shared.v1.AskRequest",
        "proof.shared.v1.AskResponse",
    )?;
    write_prepared_service(
        &robot,
        "../provider-b",
        "proof.other.v1",
        "proof.other.v1.Ask",
        "ask",
        "proof.other.v1.AskRequest",
        "proof.other.v1.AskResponse",
    )?;
    let output = build_robot(&robot)?;
    assert!(
        output.status.success(),
        "shared-prefix operations build failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

/// The same Rust marker spelling naming two unequal operations in one
/// package is diagnosed at generation, never silently resolved.
#[test]
fn unequal_marker_collision_is_diagnosed_at_generation() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    let robot = scaffold_robot(
        root,
        "  alpha: { source: { path: ../provider-a } }\n  gamma: { source: { path: ../provider-c } }\n",
        "phoxal::api!();\n\nfn main() {}\n",
    )?;
    write_prepared_service(
        &robot,
        "../provider-a",
        "proof.shared.v1",
        "proof.shared.v1.Ask",
        "ask",
        "proof.shared.v1.AskRequest",
        "proof.shared.v1.AskResponse",
    )?;
    write_prepared_service(
        &robot,
        "../provider-c",
        "proof.shared.v1",
        "proof.shared.v1.Ask",
        "ask",
        "proof.shared.v1.OtherRequest",
        "proof.shared.v1.AskResponse",
    )?;
    let output = build_robot(&robot)?;
    assert!(
        !output.status.success(),
        "unequal marker collision must fail the build"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("operation marker collision"),
        "the failure must diagnose the collision, got:\n{stderr}"
    );
    Ok(())
}

/// A mixed provider serves a call beside a retained observation output,
/// and the cold consumer imports the observation while naming the call
/// marker: both surfaces of one prepared contract compile and connect.
#[test]
fn mixed_import_and_export_provider_compiles_cold() -> Result<(), Box<dyn std::error::Error>> {
    use prost::Message as _;
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    let robot = scaffold_robot(
        root,
        "  mixed: { source: { path: ../provider-mixed } }\n",
        "phoxal::api!();\n\nuse phoxal::contracts::Latest;\n\n/// A mixed brain: one call requirement beside one observation import.\n#[phoxal::endpoints]\npub struct BrainApi {\n    #[phoxal::call]\n    ask: api::operations::proof::mixed::v1::Ask,\n\n    #[phoxal::input(max_age_ms = 500)]\n    reading: Latest<api::types::proof::mixed::v1::Reading>,\n}\n\nfn main() {}\n",
    )?;
    let identity = PreparedSelection::Path {
        path: "../provider-mixed".to_owned(),
    };
    let dir = phoxal_build::prepared_dir(&robot, &identity, None);
    fs::create_dir_all(&dir)?;
    let set = prost_types::FileDescriptorSet {
        file: vec![prost_types::FileDescriptorProto {
            name: Some("proof.mixed.v1.proto".to_owned()),
            package: Some("proof.mixed.v1".to_owned()),
            message_type: vec![
                prost_types::DescriptorProto {
                    name: Some("AskRequest".to_owned()),
                    ..Default::default()
                },
                prost_types::DescriptorProto {
                    name: Some("Reading".to_owned()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }],
    };
    fs::write(dir.join("descriptors.pb"), set.encode_to_vec())?;
    let contract = serde_json::json!({
        "generation": 1,
        "selection": {"kind": "path", "path": "../provider-mixed"},
        "binary": null,
        "executable": {"sha256": "00".repeat(32), "package": "mixed-fixture", "version": "0.1.0"},
        "runtime": {
            "schema": "phoxal/artifact/v0", "record": "runtime",
            "period_ms": 20, "timeout_ms": 100, "init_timeout_ms": 1000,
            "config_schema": {},
            "inputs": [{
                "name": "ask", "role": "call_ingress", "port": "ask",
                "max_items": 4, "max_bytes": 1024, "max_age_ms": null,
                "request_fqn": "proof.mixed.v1.AskRequest", "response_fqn": "proof.mixed.v1.Reading",
                "signature": {
                    "endpoint": "ask", "service": "proof.mixed.v1.Ask", "method": "ask",
                    "request": "proof.mixed.v1.AskRequest", "response": "proof.mixed.v1.Reading",
                    "shape": "call", "retained_latest": false, "lease_valid_for_ms": null
                }
            }],
            "service_outputs": [],
            "transient_outputs": [{
                "name": "reading", "role": "method", "port": "reading", "input": null,
                "max_items": null, "max_bytes": 4096, "max_request_bytes": null,
                "bootstrap": false, "every_steps": null, "on_change": false, "project": null,
                "timeout_ms": null, "cancel_grace_ms": null, "valid_for_ms": null,
                "signature": {
                    "endpoint": "reading", "service": "proof.mixed.v1.Reading", "method": "reading",
                    "request": "google.protobuf.Empty", "response": "proof.mixed.v1.Reading",
                    "shape": "observation", "retained_latest": true, "lease_valid_for_ms": null
                }
            }]
        }
    });
    fs::write(dir.join("contract.json"), contract.to_string())?;
    let output = build_robot(&robot)?;
    assert!(
        output.status.success(),
        "mixed import/export build failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

/// Writes prepared products mirroring what `cargo phoxal prepare`
/// extracts from a component whose `component.yaml` declares motor and
/// encoder capabilities: one leased actuator input and one queued encoder
/// output, both typed in the SDK standard vocabulary. This is a
/// generator-input fixture — the real derivation and splice are proven by
/// the `standard-plus-custom` component fixture, and the real end-to-end
/// acquisition by the host acceptance path.
fn write_capability_drive_products(robot_root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use prost::Message as _;
    let identity = PreparedSelection::Path {
        path: "../drive".to_owned(),
    };
    let dir = phoxal_build::prepared_dir(robot_root, &identity, None);
    fs::create_dir_all(&dir)?;
    // Both endpoints resolve through the SDK standard vocabulary, so the
    // descriptor closure carries nothing of its own.
    fs::write(
        dir.join("descriptors.pb"),
        prost_types::FileDescriptorSet::default().encode_to_vec(),
    )?;
    let contract = serde_json::json!({
        "generation": 1,
        "selection": {"kind": "path", "path": "../drive"},
        "binary": null,
        "executable": {
            "sha256": "00".repeat(32),
            "package": "capability-drive-fixture",
            "version": "0.1.0"
        },
        "runtime": {
            "schema": "phoxal/artifact/v0",
            "record": "runtime",
            "period_ms": 20,
            "timeout_ms": 100,
            "init_timeout_ms": 1_000,
            "config_schema": {},
            "inputs": [{
                "name": "actuator",
                "role": "leased_value",
                "port": "actuator",
                "max_items": null,
                "max_bytes": 1024,
                "max_age_ms": null,
                "request_fqn": "phoxal.component.actuator.v1.ActuatorSetpoint",
                "response_fqn": "google.protobuf.Empty",
                "signature": {
                    "endpoint": "actuator",
                    "service": "phoxal.component.actuator.v1.Actuator",
                    "method": "actuator",
                    "request": "phoxal.component.actuator.v1.ActuatorSetpoint",
                    "response": "google.protobuf.Empty",
                    "shape": "call",
                    "retained_latest": false,
                    "lease_valid_for_ms": 100
                }
            }],
            "service_outputs": [],
            "transient_outputs": [{
                "name": "encoder",
                "role": "method",
                "port": "encoder",
                "input": null,
                "max_items": 16,
                "max_bytes": 512,
                "max_request_bytes": null,
                "bootstrap": false,
                "every_steps": null,
                "on_change": false,
                "project": null,
                "timeout_ms": null,
                "cancel_grace_ms": null,
                "valid_for_ms": null,
                "signature": {
                    "endpoint": "encoder",
                    "service": "phoxal.robotics.v1.Encoder",
                    "method": "encoder",
                    "request": "google.protobuf.Empty",
                    "response": "phoxal.robotics.v1.EncoderSample",
                    "shape": "observation",
                    "retained_latest": false,
                    "lease_valid_for_ms": null
                }
            }]
        }
    });
    fs::write(dir.join("contract.json"), contract.to_string())?;
    Ok(())
}

/// A capability component's derived standard endpoints and a second
/// provider's generated call marker compose in one brain API: the authored
/// contract names the SDK standard sample type and the generated marker in
/// one contract, and the generated instance module types its helpers and
/// re-exports against exactly that SDK vocabulary.
#[test]
fn capability_standard_endpoints_compose_with_generated_call_markers_cold()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    let robot = scaffold_robot(
        root,
        "  drive: { source: { path: ../drive } }\n  advisor: { source: { path: ../advisor } }\n",
        "phoxal::api!();\n\nuse phoxal::contracts::Queue;\n\n/// One brain contract composing both surfaces.\n#[phoxal::endpoints]\npub struct BrainApi {\n    #[phoxal::call]\n    ask: api::operations::proof::capcall::v1::Ask,\n\n    #[phoxal::input(max_items = 16, max_bytes = 512)]\n    encoder: Queue<::phoxal::contracts::component::encoder::EncoderSample>,\n}\n\nfn main() {\n    let method = <api::operations::proof::capcall::v1::Ask as phoxal::contracts::Operation>::METHOD;\n    assert_eq!(method.signature().service, \"proof.capcall.v1.Ask\");\n    // The generated drive module binds its observation and leased call\n    // against the same SDK standard vocabulary the authored field names.\n    let sample: phoxal::contracts::Observation<\n        ::phoxal::contracts::component::encoder::EncoderSample,\n    > = api::drive::encoder();\n    let _ = sample;\n    let setpoint: phoxal::contracts::CallMethod<\n        ::phoxal::contracts::component::actuator::ActuatorSetpoint,\n        ::phoxal::contracts::Empty,\n    > = api::drive::ACTUATOR;\n    let _ = setpoint;\n    let reexported: &::phoxal::contracts::component::encoder::EncoderSample =\n        &<api::drive::EncoderSample as Default>::default();\n    let _ = reexported;\n}\n",
    )?;
    write_prepared_service(
        &robot,
        "../advisor",
        "proof.capcall.v1",
        "proof.capcall.v1.Ask",
        "ask",
        "proof.capcall.v1.AskRequest",
        "proof.capcall.v1.AskResponse",
    )?;
    write_capability_drive_products(&robot)?;
    let output = build_robot(&robot)?;
    assert!(
        output.status.success(),
        "capability standard endpoints x generated call marker build failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

/// A generator-input fixture: a robot selecting a Git source with
/// hand-written prepared products compiles. This proves the generator's
/// Git-identity plumbing only — real acquisition is proven by
/// `local_git_repository_acquisition_prepares_real_products` above.
#[test]
fn synthetic_pinned_git_generator_fixture_compiles_cold() -> Result<(), Box<dyn std::error::Error>>
{
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    let rev = "0123456789abcdef0123456789abcdef01234567";
    let robot = scaffold_robot(
        root,
        &format!(
            "  pinned: {{ source: {{ git: {{ name: provider-git, url: \"https://example.invalid/provider.git\", rev: {rev} }} }} }}\n"
        ),
        "phoxal::api!();\n\nfn main() {\n    let _ = <api::operations::proof::git::v1::Ask as phoxal::contracts::Operation>::METHOD;\n}\n",
    )?;
    let identity = PreparedSelection::Git {
        name: "provider-git".to_owned(),
        revision: rev.to_owned(),
    };
    let dir = phoxal_build::prepared_dir(&robot, &identity, None);
    write_prepared_under(
        dir,
        serde_json::json!({"kind": "git", "name": "provider-git", "revision": rev}),
        "proof.git.v1",
        "proof.git.v1.Ask",
        "ask",
        "proof.git.v1.AskRequest",
        "proof.git.v1.AskResponse",
    )?;
    let output = build_robot(&robot)?;
    assert!(
        output.status.success(),
        "pinned-git acquisition failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

/// A generator-input fixture: a robot selecting a registry source with
/// hand-written prepared products compiles. This proves the generator's
/// registry-identity plumbing only — real registry acquisition is not
/// exercised by this test.
#[test]
fn synthetic_registry_generator_fixture_compiles_cold() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    let robot = scaffold_robot(
        root,
        "  from_registry: { source: { package: { name: provider-registry, version: 1.2.3, registry: temp } } }\n",
        "phoxal::api!();\n\nfn main() {\n    let _ = <api::operations::proof::registry::v1::Ask as phoxal::contracts::Operation>::METHOD;\n}\n",
    )?;
    let identity = PreparedSelection::Registry {
        registry: "temp".to_owned(),
        name: "provider-registry".to_owned(),
        version: "1.2.3".to_owned(),
    };
    let dir = phoxal_build::prepared_dir(&robot, &identity, None);
    write_prepared_under(
        dir,
        serde_json::json!({
            "kind": "registry", "registry": "temp",
            "name": "provider-registry", "version": "1.2.3"
        }),
        "proof.registry.v1",
        "proof.registry.v1.Ask",
        "ask",
        "proof.registry.v1.AskRequest",
        "proof.registry.v1.AskResponse",
    )?;
    let output = build_robot(&robot)?;
    assert!(
        output.status.success(),
        "registry acquisition failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

/// The standalone provider package checked into the temporary Git fixture
/// repository below: one authored call endpoint, self-contained manifest.
const GIT_PROVIDER_MAIN: &str = r#"use phoxal::runtime::Context;

#[phoxal::messages(package = "proof.gitreal.v1")]
mod contract {
    pub struct AskRequest {
        #[phoxal(tag = 1)]
        pub value: u64,
    }

    pub struct AskResponse {
        #[phoxal(tag = 1)]
        pub value: u64,
    }

    #[phoxal::endpoints]
    pub struct ProviderApi {
        #[phoxal::operation]
        ask: phoxal::contracts::RequestReply<AskRequest, AskResponse>,
    }
}

struct Provider;

#[phoxal::runtime(contract = contract::ProviderApi, period_ms = 20)]
impl Provider {
    #[init]
    fn new(_config: ()) -> phoxal::Result<Self> {
        Ok(Self)
    }

    #[handle(ask)]
    fn ask(
        &mut self,
        _ctx: &mut Context<'_, Self>,
        request: contract::AskRequest,
    ) -> phoxal::Result<contract::AskResponse> {
        Ok(contract::AskResponse {
            value: request.value.saturating_add(1),
        })
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run::<Provider>()
}
"#;

/// The host-acceptance gate shared by the acquisition paths: they launch
/// nested Cargo and require explicit opt-in through the environment.
fn require_host_acquisition() {
    assert!(
        std::env::var_os("PHOXAL_HOST_ACQUISITION").is_some(),
        "PHOXAL_HOST_ACQUISITION=1 acknowledges the nested Cargo builds"
    );
}

/// Builds the `cargo-phoxal` prerequisite, so a stale prebuilt tool can
/// never stand in for the current sources.
fn build_cargo_phoxal() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
    let tool_build = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args(["build", "-p", "cargo-phoxal", "--manifest-path"])
        .arg(target.join("../Cargo.toml"))
        .env("CARGO_TARGET_DIR", &target)
        .env("CARGO_BUILD_JOBS", "2")
        .output()?;
    assert!(
        tool_build.status.success(),
        "cargo build -p cargo-phoxal failed:\n{}\n{}",
        String::from_utf8_lossy(&tool_build.stdout),
        String::from_utf8_lossy(&tool_build.stderr)
    );
    let tool = target.join("debug/cargo-phoxal");
    assert!(
        tool.is_file(),
        "the built tool is missing at {}",
        tool.display()
    );
    Ok(tool)
}

/// Acquires a provider from a real local Git repository pinned to a real
/// commit through the normal `cargo phoxal prepare` command path, then
/// compiles a cold consumer naming the generated marker. Temporary
/// fixture repository commits are part of this local acquisition test
/// only; nothing is published and no project checkout is committed.
///
/// Host acceptance path: the test builds its own `cargo-phoxal`
/// prerequisite, launches nested Cargo installs (network on a cold
/// cache), and runs only in the integration lane (`e2e` feature) under
/// `PHOXAL_HOST_ACQUISITION=1`, so the deterministic suite neither
/// depends on a stale prebuilt tool nor runs nested compilers.
#[test]
fn local_git_repository_acquisition_prepares_real_products()
-> Result<(), Box<dyn std::error::Error>> {
    require_host_acquisition();
    let directory = tempfile::tempdir()?;
    let root = directory.path();

    // A real repository with one real commit carrying the provider.
    let repo = root.join("provider-repo");
    fs::create_dir_all(repo.join("src"))?;
    fs::write(
        repo.join("Cargo.toml"),
        format!(
            "[package]\nname = \"proof-git-provider\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[[bin]]\nname = \"proof-git-provider\"\npath = \"src/main.rs\"\n\n[dependencies]\nphoxal = {{ path = {:?}, default-features = false, features = [\"runtime\"] }}\n",
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .display()
                .to_string()
        ),
    )?;
    fs::write(repo.join("src/main.rs"), GIT_PROVIDER_MAIN)?;
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .args([
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .current_dir(&repo)
            .args(args)
            .output()?;
        assert!(
            output.status.success(),
            "git {:?} failed: {}{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    };
    git(&["init", "--quiet"])?;
    // `cargo install --locked` requires a committed lock file.
    let lock = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args(["generate-lockfile", "--manifest-path"])
        .arg(repo.join("Cargo.toml"))
        .output()?;
    assert!(
        lock.status.success(),
        "cargo generate-lockfile failed: {}{}",
        String::from_utf8_lossy(&lock.stdout),
        String::from_utf8_lossy(&lock.stderr)
    );
    git(&["add", "."])?;
    git(&["commit", "--quiet", "-m", "provider fixture"])?;
    let rev_output = Command::new("git")
        .current_dir(&repo)
        .args(["rev-parse", "HEAD"])
        .output()?;
    assert!(rev_output.status.success());
    let rev = String::from_utf8_lossy(&rev_output.stdout)
        .trim()
        .to_owned();
    assert_eq!(rev.len(), 40, "a real pinned commit");

    // A robot project acquiring the repository through the normal command.
    let robot = scaffold_robot(
        root,
        &format!(
            "  pinned: {{ source: {{ git: {{ name: proof-git-provider, url: {:?}, rev: {rev} }} }} }}\n",
            format!("file://{}", repo.canonicalize()?.display())
        ),
        "phoxal::api!();\n\nfn main() {\n    let _ = <api::operations::proof::gitreal::v1::Ask as phoxal::contracts::Operation>::METHOD;\n}\n",
    )?;
    // The prerequisite tool is built by this acceptance path itself, so no
    // stale prebuilt binary can stand in for the current sources.
    let cargo_phoxal = build_cargo_phoxal()?;
    let home = root.join("phoxal-home");
    fs::create_dir_all(&home)?;
    let prepared = std::process::Command::new(&cargo_phoxal)
        .arg("prepare")
        .current_dir(&robot)
        .env("PHOXAL_HOME", &home)
        .env(
            "CARGO_TARGET_DIR",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"),
        )
        .env("CARGO_BUILD_JOBS", "2")
        .output()?;
    assert!(
        prepared.status.success(),
        "cargo phoxal prepare failed:\n{}\n{}",
        String::from_utf8_lossy(&prepared.stdout),
        String::from_utf8_lossy(&prepared.stderr)
    );
    let prepared_root = robot.join(".phoxal/prepared");
    let git_products: Vec<_> = fs::read_dir(&prepared_root)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("git-"))
        })
        .collect();
    assert_eq!(
        git_products.len(),
        1,
        "the pinned Git acquisition produced {git_products:?}"
    );
    assert!(git_products[0].join("contract.json").is_file());
    assert!(git_products[0].join("descriptors.pb").is_file());

    let output = build_robot(&robot)?;
    assert!(
        output.status.success(),
        "git-acquired consumer build failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

/// The standalone provider package published to the temporary local
/// registry below: one authored call endpoint with a true SDK dependency.
const REGISTRY_PROVIDER_MAIN: &str = r#"use phoxal::runtime::Context;

#[phoxal::messages(package = "proof.regreal.v1")]
mod contract {
    pub struct AskRequest {
        #[phoxal(tag = 1)]
        pub value: u64,
    }

    pub struct AskResponse {
        #[phoxal(tag = 1)]
        pub value: u64,
    }

    #[phoxal::endpoints]
    pub struct ProviderApi {
        #[phoxal::operation]
        ask: phoxal::contracts::RequestReply<AskRequest, AskResponse>,
    }
}

struct Provider;

#[phoxal::runtime(contract = contract::ProviderApi, period_ms = 20)]
impl Provider {
    #[init]
    fn new(_config: ()) -> phoxal::Result<Self> {
        Ok(Self)
    }

    #[handle(ask)]
    fn ask(
        &mut self,
        _ctx: &mut Context<'_, Self>,
        request: contract::AskRequest,
    ) -> phoxal::Result<contract::AskResponse> {
        Ok(contract::AskResponse {
            value: request.value.saturating_add(1),
        })
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run::<Provider>()
}
"#;

/// Real registry acquisition with an isolated local SDK dependency patch.
///
/// The provider package is acquired through a temporary git-protocol
/// registry — a real clone of a fixture index, a real checksummed
/// `.crate` download — while its true `phoxal` dependency is satisfied by
/// a config-level `[patch.crates-io]` pointing at this framework, so the
/// development SDK is neither published nor mirrored. The packaged
/// `Cargo.lock` records the patched path entry, keeping
/// `cargo install --locked` consistent.
///
/// Routes investigated on this host's cargo, recorded as evidence:
/// - a `sparse+file://` index cannot be fetched: the sparse client parses
///   request URLs with the `http` crate, whose URI grammar rejects
///   `file:///` with `InvalidFormat` (reproduced by a minimal
///   `http::Uri` parse probe);
/// - a `[patch]` in the provider manifest is stripped by `cargo package`
///   from the normalized manifest inside the `.crate`;
/// - `cargo install` from a package-root working directory does not
///   discover the project's `.cargo/config.toml` (the same limitation
///   `registry_config` documents), so a project-level patch never reaches
///   the install.
///
/// The working route delivers the registry and the patch through an
/// isolated temporary `CARGO_HOME` the install inherits. Alongside the
/// registry participant, one real capability component — `component.yaml`
/// motor and encoder capabilities beside an empty authored contract — is
/// prepared by the same command, and the consumer composes the registry
/// provider's generated call marker with the capability-derived standard
/// endpoints in one authored contract.
///
/// Host acceptance path: like the Git acquisition test above it builds
/// its own `cargo-phoxal`, commits fixture repositories, launches nested
/// Cargo (network on a cold temporary `CARGO_HOME`), and runs only in
/// the integration lane (`e2e` feature) under `PHOXAL_HOST_ACQUISITION=1`.
#[test]
fn local_registry_acquisition_with_sdk_patch_prepares_real_products()
-> Result<(), Box<dyn std::error::Error>> {
    require_host_acquisition();
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    // `CARGO_MANIFEST_DIR` here is `…/framework/phoxal/build`, so the SDK
    // crate root is one directory up and the shared target two.
    let sdk = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize()?;
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let target = sdk.join("../../target");

    // The registry index exists before packaging: the patch target's
    // workspace manifests name the `phoxal` registry, so any
    // packaging-time manifest parse needs it configured.
    let git_index = root.join("registry-index");
    let dl = root.join("registry-dl");
    fs::create_dir_all(git_index.join("pr/oo"))?;
    fs::write(
        git_index.join("config.json"),
        format!("{{\"dl\":\"file://{}\",\"api\":null}}\n", dl.display()),
    )?;

    // One provider package with a true SDK dependency and no registry
    // coordinate, resolved through the same local patch the install uses.
    let provider = root.join("provider");
    fs::create_dir_all(provider.join("src"))?;
    fs::create_dir_all(provider.join(".cargo"))?;
    fs::write(
        provider.join("Cargo.toml"),
        "[package]\nname = \"proof-registry-provider\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n\
         [[bin]]\nname = \"proof-registry-provider\"\npath = \"src/main.rs\"\n\n\
         [dependencies]\n\
         phoxal = { version = \"0.0.0-dev.7\", default-features = false, features = [\"runtime\"] }\n",
    )?;
    fs::write(provider.join("src/main.rs"), REGISTRY_PROVIDER_MAIN)?;
    fs::write(
        provider.join(".cargo/config.toml"),
        format!(
            "[registries.phoxal]\nindex = \"file://{}\"\n\n[patch.crates-io]\nphoxal = {{ path = \"{}\" }}\n",
            git_index.display(),
            sdk.display()
        ),
    )?;
    let package = Command::new(&cargo)
        .args(["package", "--no-verify", "--allow-dirty"])
        .arg("--target-dir")
        .arg(&target)
        .current_dir(&provider)
        .env("CARGO_BUILD_JOBS", "2")
        .output()?;
    assert!(
        package.status.success(),
        "cargo package failed:\n{}\n{}",
        String::from_utf8_lossy(&package.stdout),
        String::from_utf8_lossy(&package.stderr)
    );
    let crate_file = target.join("package/proof-registry-provider-0.1.0.crate");
    assert!(crate_file.is_file(), "the packaged crate is missing");

    // The registry serves that real artifact under its true checksum, with
    // an index entry carrying the package's true SDK dependency.
    let cksum_output = Command::new("shasum")
        .arg("-a")
        .arg("256")
        .arg(&crate_file)
        .output()?;
    assert!(cksum_output.status.success());
    let cksum = String::from_utf8_lossy(&cksum_output.stdout)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned();
    assert_eq!(cksum.len(), 64, "a real sha256 checksum");
    fs::create_dir_all(dl.join("proof-registry-provider/0.1.0"))?;
    fs::copy(
        &crate_file,
        dl.join("proof-registry-provider/0.1.0/download"),
    )?;
    fs::write(
        git_index.join("pr/oo/proof-registry-provider"),
        format!(
            "{{\"name\":\"proof-registry-provider\",\"vers\":\"0.1.0\",\"deps\":\
             [{{\"name\":\"phoxal\",\"req\":\"0.0.0-dev.7\",\"features\":[\"runtime\"],\
             \"optional\":false,\"default_features\":false,\"target\":null,\
             \"kind\":\"normal\",\"registry\":null,\"package\":null}}],\
             \"features\":{{}},\"yanked\":false,\"links\":null,\"v\":1,\"cksum\":\"{cksum}\"}}\n"
        ),
    )?;
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .args([
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .current_dir(&git_index)
            .args(args)
            .output()?;
        assert!(
            output.status.success(),
            "git {:?} failed: {}{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    };
    git(&["init", "--quiet", "-b", "master"])?;
    git(&["add", "."])?;
    git(&["commit", "--quiet", "-m", "fixture registry index"])?;

    // One real capability component: motor and encoder capabilities beside
    // an empty authored contract the derived standard endpoints splice
    // into, prepared as a local path participant by the same command.
    let drive = root.join("drive");
    fs::create_dir_all(drive.join("src"))?;
    fs::write(
        drive.join("Cargo.toml"),
        format!(
            "[package]\nname = \"capability-drive-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n\
             [[bin]]\nname = \"capability-drive-fixture\"\npath = \"src/main.rs\"\n\n\
             [dependencies]\n\
             phoxal = {{ path = {:?}, default-features = false, features = [\"runtime\"] }}\n\n\
             [build-dependencies]\n\
             phoxal = {{ path = {:?}, default-features = false, features = [\"build\"] }}\n",
            sdk.display().to_string(),
            sdk.display().to_string()
        ),
    )?;
    fs::write(
        drive.join("build.rs"),
        "fn main() -> Result<(), phoxal::build::Error> { phoxal::build::api(phoxal::build::BuildApiConfig::default()) }\n",
    )?;
    fs::write(
        drive.join("component.yaml"),
        "schema: phoxal/component/v0\nmodel: { file: model.xml, root_body: mount }\ncapabilities:\n  motor:\n    kind: motor\n    command: velocity\n    max_torque_nm: 2.0\n  encoder:\n    kind: encoder\n    publish_rate_hz: 50.0\n",
    )?;
    fs::write(
        drive.join("model.xml"),
        "<mujoco model=\"fixture\">\n  <worldbody>\n    <body name=\"mount\"><geom type=\"sphere\" size=\"0.01\"/></body>\n  </worldbody>\n</mujoco>\n",
    )?;
    fs::write(
        drive.join("src/main.rs"),
        "/// The component's endpoint surface: the derived standard actuator\n/// input and encoder output splice in beside this empty authored\n/// contract.\n#[phoxal::endpoints]\npub struct DriveApi {}\n\nstruct Drive;\n\n#[phoxal::runtime(contract = DriveApi, period_ms = 20)]\nimpl Drive {\n    #[init]\n    fn new(_config: ()) -> phoxal::Result<Self> {\n        Ok(Drive)\n    }\n\n    #[step]\n    fn advance(&mut self, _ctx: &mut phoxal::runtime::Context<'_, Self>) -> phoxal::Result<()> {\n        Ok(())\n    }\n}\n\nfn main() -> phoxal::Result<()> {\n    phoxal::runtime::run::<Drive>()\n}\n",
    )?;

    // The robot composes both participants in one authored contract.
    let robot = scaffold_robot(
        root,
        "  pinned: { source: { package: { name: proof-registry-provider, version: 0.1.0 } } }\n  drive: { source: { path: ../drive } }\n",
        "phoxal::api!();\n\nuse phoxal::contracts::Queue;\n\n/// One brain contract composing the registry provider's generated call\n/// marker with the capability component's derived standard endpoint.\n#[phoxal::endpoints]\npub struct BrainApi {\n    #[phoxal::call]\n    ask: api::operations::proof::regreal::v1::Ask,\n\n    #[phoxal::input(max_items = 16, max_bytes = 512)]\n    encoder: Queue<::phoxal::contracts::component::encoder::EncoderSample>,\n}\n\nfn main() {\n    let method = <api::operations::proof::regreal::v1::Ask as phoxal::contracts::Operation>::METHOD;\n    assert_eq!(method.signature().service, \"proof.regreal.v1.Ask\");\n    let sample: phoxal::contracts::Observation<\n        ::phoxal::contracts::component::encoder::EncoderSample,\n    > = api::drive::encoder();\n    let _ = sample;\n}\n",
    )?;

    // The isolated Cargo configuration: the temporary registry and the
    // local SDK patch live only in this temporary CARGO_HOME, which the
    // install inherits. No user configuration is touched.
    let cargo_home = root.join("cargo-home");
    fs::create_dir_all(&cargo_home)?;
    fs::write(
        cargo_home.join("config.toml"),
        format!(
            "[registries.phoxal]\nindex = \"file://{}\"\n\n[patch.crates-io]\nphoxal = {{ path = \"{}\" }}\n",
            git_index.display(),
            sdk.display()
        ),
    )?;
    let home = root.join("phoxal-home");
    fs::create_dir_all(&home)?;
    let cargo_phoxal = build_cargo_phoxal()?;
    let prepared = Command::new(&cargo_phoxal)
        .arg("prepare")
        .current_dir(&robot)
        .env("PHOXAL_HOME", &home)
        .env("CARGO_HOME", &cargo_home)
        .env("CARGO_TARGET_DIR", &target)
        .env("CARGO_BUILD_JOBS", "2")
        .output()?;
    assert!(
        prepared.status.success(),
        "cargo phoxal prepare failed:\n{}\n{}",
        String::from_utf8_lossy(&prepared.stdout),
        String::from_utf8_lossy(&prepared.stderr)
    );
    let prepared_root = robot.join(".phoxal/prepared");
    let registry_products: Vec<_> = fs::read_dir(&prepared_root)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name().is_some_and(|name| {
                name.to_string_lossy()
                    .starts_with("registry-phoxal-proof-registry-provider")
            })
        })
        .collect();
    assert_eq!(
        registry_products.len(),
        1,
        "the registry acquisition produced {registry_products:?}"
    );
    assert!(registry_products[0].join("contract.json").is_file());
    assert!(registry_products[0].join("descriptors.pb").is_file());
    let drive_products: Vec<_> = fs::read_dir(&prepared_root)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("path-drive"))
        })
        .collect();
    assert_eq!(
        drive_products.len(),
        1,
        "the capability component preparation produced {drive_products:?}"
    );
    assert!(drive_products[0].join("contract.json").is_file());
    assert!(drive_products[0].join("descriptors.pb").is_file());

    let output = build_robot(&robot)?;
    assert!(
        output.status.success(),
        "registry-acquired consumer build failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
