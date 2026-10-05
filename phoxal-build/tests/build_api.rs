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
    // An authored runtime with an empty endpoint contract still compiles.
    fs::write(
        robot.join("robot.yaml"),
        "schema: phoxal/robot/v0\nrobot: { id: proof-empty-brain }\nsupervisor: { source: { path: ../supervisor } }\n",
    )?;
    let framework = Path::new(env!("CARGO_MANIFEST_DIR")).join("../phoxal");
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
        "phoxal::api!();\n\n/// A contract with no endpoints.\n#[phoxal::endpoints]\npub struct BrainApi {}\nstruct Brain;\n#[phoxal::runtime(contract = BrainApi, period_ms = 20)]\nimpl Brain {\n    #[init]\n    fn new(_config: ()) -> phoxal::Result<Self> { Ok(Brain) }\n    #[step]\n    fn advance(&mut self, _ctx: &mut phoxal::runtime::Context<'_, Self>) -> phoxal::Result<()> { Ok(()) }\n}\nfn main() -> phoxal::Result<()> { phoxal::runtime::run::<Brain>() }\n",
    )?;

    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args(["build", "--offline", "--manifest-path"])
        .arg(robot.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../target"))
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
        phoxal_build::prepared_dir(robot_root, &identity, None)?,
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
                "delivery": "call_ingress",
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
            "outputs": [{
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
        format!(
            "schema: phoxal/robot/v0\nrobot: {{ id: marker-proof }}\nsupervisor: {{ source: {{ path: ../supervisor }} }}\nservices:\n{services}"
        ),
    )?;
    let framework = Path::new(env!("CARGO_MANIFEST_DIR")).join("../phoxal");
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
        .current_dir(robot)
        .args(["build", "--offline", "--manifest-path"])
        .arg(robot.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../target"))
        .output()?)
}

#[test]
fn config_watch_and_package_cleanup_refresh_real_prepared_bindings()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let robot = scaffold_robot(
        directory.path(),
        "  provider: { source: { path: ../provider } }\n",
        "phoxal::api!();\nfn main() { println!(\"{}\", <api::operations::proof::watch::v1::Ask as phoxal::contracts::Operation>::METHOD.signature().request); }\n",
    )?;
    let manifest = robot.join("Cargo.toml");
    fs::write(
        &manifest,
        fs::read_to_string(&manifest)?.replace("marker-proof-robot", "watch-reader-proof"),
    )?;
    fs::create_dir(robot.join(".cargo"))?;
    let prepare = |request: &str| {
        write_prepared_service(
            &robot,
            "../provider",
            "proof.watch.v1",
            "proof.watch.v1.Ask",
            "ask",
            request,
            "proof.watch.v1.Response",
        )
    };
    let invoke = |arguments: &[&str]| -> Result<std::process::Output, Box<dyn std::error::Error>> {
        Ok(Command::new(env!("CARGO"))
            .current_dir(&robot)
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("CARGO_BUILD_TARGET_DIR")
            .args(arguments)
            .args(["--offline", "--target-dir"])
            .arg(directory.path().join("compiler-output"))
            .output()?)
    };
    let run = |expected: &str| -> Result<(), Box<dyn std::error::Error>> {
        let output = invoke(&["run", "--quiet"])?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8(output.stdout)?.trim(), expected);
        Ok(())
    };
    prepare("proof.watch.v1.OldRequest")?;
    run("proof.watch.v1.OldRequest")?;
    fs::write(
        robot.join(".cargo/config.toml"),
        "[build]\ntarget-dir='new-inputs'\n",
    )?;
    let missing = invoke(&["build"])?;
    assert!(!missing.status.success());
    let diagnostic = String::from_utf8_lossy(&missing.stderr);
    assert!(
        diagnostic.contains("new-inputs") && diagnostic.contains("cargo phoxal prepare"),
        "{diagnostic}"
    );
    prepare("proof.watch.v1.NewRequest")?;
    run("proof.watch.v1.NewRequest")?;
    fs::remove_file(robot.join(".cargo/config.toml"))?;
    fs::remove_dir(robot.join(".cargo"))?;
    run("proof.watch.v1.OldRequest")?;
    fs::create_dir(robot.join(".cargo"))?;
    fs::write(
        robot.join(".cargo/config.toml"),
        "[build]\ntarget-dir='new-inputs'\n",
    )?;
    prepare("proof.watch.v1.NewRequest")?;
    run("proof.watch.v1.OldRequest")?; // Explicitly accepted cached-directory edge.
    let cleaned = invoke(&["clean", "-p", "watch-reader-proof"])?;
    assert!(cleaned.status.success());
    run("proof.watch.v1.NewRequest")?;
    let warm = invoke(&["build", "-vv"])?;
    assert!(warm.status.success());
    assert!(
        String::from_utf8_lossy(&warm.stderr).contains("Fresh watch-reader-proof"),
        "expected Fresh; Cargo -vv output:\n{}",
        String::from_utf8_lossy(&warm.stderr)
    );
    Ok(())
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
    fs::create_dir_all(robot.join(".cargo"))?;
    fs::write(
        robot.join(".cargo/config.toml"),
        "[build]\ntarget-dir = \"inputs-target\"\n",
    )?;
    let input_root = phoxal_build::prepared_input_root(&robot)?;
    assert!(input_root.starts_with(robot.join("inputs-target/phoxal/prepared")));
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
    // Environment selects the stable input root even when normal configuration
    // changes and a one-off compiler target directory is supplied.
    fs::write(
        robot.join(".cargo/config.toml"),
        "[build]\ntarget-dir = \"other-inputs\"\n",
    )?;
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(&cargo)
        .current_dir(&robot)
        .args(["build", "--offline", "--target-dir"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../target"))
        .env("CARGO_TARGET_DIR", robot.join("inputs-target"))
        .output()?;
    assert!(
        output.status.success(),
        "environment input root with compiler override: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let missing = Command::new(cargo)
        .current_dir(&robot)
        .args(["build", "--offline", "--target-dir"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../target"))
        .env("CARGO_TARGET_DIR", robot.join("not-prepared"))
        .output()?;
    assert!(!missing.status.success());
    let diagnostic = String::from_utf8_lossy(&missing.stderr);
    assert!(diagnostic.contains("not-prepared"), "{diagnostic}");
    assert!(diagnostic.contains("cargo phoxal prepare"), "{diagnostic}");
    assert!(
        diagnostic.contains("overrides relocate outputs only"),
        "{diagnostic}"
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
    let dir = phoxal_build::prepared_dir(&robot, &identity, None)?;
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
        "executable": {"package": "mixed-fixture", "version": "0.1.0"},
        "runtime": {
            "schema": "phoxal/artifact/v0", "record": "runtime",
            "period_ms": 20, "timeout_ms": 100, "init_timeout_ms": 1000,
            "config_schema": {},
            "inputs": [{
                "name": "ask", "delivery": "call_ingress", "port": "ask",
                "max_items": 4, "max_bytes": 1024, "max_age_ms": null,
                "request_fqn": "proof.mixed.v1.AskRequest", "response_fqn": "proof.mixed.v1.Reading",
                "signature": {
                    "endpoint": "ask", "service": "proof.mixed.v1.Ask", "method": "ask",
                    "request": "proof.mixed.v1.AskRequest", "response": "proof.mixed.v1.Reading",
                    "shape": "call", "retained_latest": false, "lease_valid_for_ms": null
                }
            }],
            "outputs": [{
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
/// acquisition by the `tests/host-acquisition` fixture package.
fn write_capability_drive_products(robot_root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use prost::Message as _;
    let identity = PreparedSelection::Path {
        path: "../drive".to_owned(),
    };
    let dir = phoxal_build::prepared_dir(robot_root, &identity, None)?;
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
                "delivery": "leased_value",
                "port": "actuator",
                "max_items": null,
                "max_bytes": 1024,
                "max_age_ms": null,
                "request_fqn": "phoxal.component.actuator.v1.ActuatorCommand",
                "response_fqn": "google.protobuf.Empty",
                "signature": {
                    "endpoint": "actuator",
                    "service": "phoxal.component.actuator.v1.Actuator",
                    "method": "actuator",
                    "request": "phoxal.component.actuator.v1.ActuatorCommand",
                    "response": "google.protobuf.Empty",
                    "shape": "call",
                    "retained_latest": false,
                    "lease_valid_for_ms": 100
                }
            }],
            "outputs": [{
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
        "phoxal::api!();\n\nuse phoxal::contracts::Queue;\n\n/// One brain contract composing both surfaces.\n#[phoxal::endpoints]\npub struct BrainApi {\n    #[phoxal::call]\n    ask: api::operations::proof::capcall::v1::Ask,\n\n    #[phoxal::input(max_items = 16, max_bytes = 512)]\n    encoder: Queue<::phoxal::contracts::component::encoder::EncoderSample>,\n}\n\nfn main() {\n    let method = <api::operations::proof::capcall::v1::Ask as phoxal::contracts::Operation>::METHOD;\n    assert_eq!(method.signature().service, \"proof.capcall.v1.Ask\");\n    // The generated drive module binds its observation and leased call\n    // against the same SDK standard vocabulary the authored field names.\n    let sample: phoxal::contracts::Observation<\n        ::phoxal::contracts::component::encoder::EncoderSample,\n    > = api::drive::encoder();\n    let _ = sample;\n    let setpoint: phoxal::contracts::CallMethod<\n        ::phoxal::contracts::component::actuator::ActuatorCommand,\n        ::phoxal::contracts::Empty,\n    > = api::drive::ACTUATOR;\n    let _ = setpoint;\n    let reexported: &::phoxal::contracts::component::encoder::EncoderSample =\n        &<api::drive::EncoderSample as Default>::default();\n    let _ = reexported;\n}\n",
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
        url: "https://example.invalid/provider.git".to_owned(),
        path: None,
    };
    let dir = phoxal_build::prepared_dir(&robot, &identity, None)?;
    write_prepared_under(
        dir,
        serde_json::json!({"kind": "git", "name": "provider-git", "revision": rev, "url": "https://example.invalid/provider.git", "path": null}),
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
