use std::fs;
use std::path::Path;
use std::process::Command;

#[test]
fn separate_robot_binary_and_integration_test_use_prepared_local_api()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let provider = directory.path().join("provider");
    let second_provider = directory.path().join("second-provider");
    let robot = directory.path().join("robot");
    fs::create_dir_all(provider.join("api"))?;
    fs::create_dir_all(second_provider.join("api"))?;
    fs::create_dir_all(robot.join("src"))?;
    fs::create_dir_all(robot.join("tests"))?;
    // Both providers author the same relative file name with distinct
    // packages, and both embed an equivalent shared vocabulary: composition
    // must merge the definitions instead of colliding on the file names.
    fs::write(
        provider.join("api/messages.proto"),
        "syntax = \"proto3\"; package proof.motion.v1; import \"shared.proto\"; message ManualRequest { double linear_x_mps = 1; }\n",
    )?;
    fs::write(
        provider.join("service.yaml"),
        "schema: phoxal/service/v0\noperations:\n  manual:\n    contract: proof.motion.v1.Manual\n    request: proof.motion.v1.ManualRequest\n    response: google.protobuf.Empty\n    max_items: 4\n    max_bytes: 1024\n",
    )?;
    fs::write(
        provider.join("api/shared.proto"),
        "syntax = \"proto3\"; package proof.shared.v1; message Shared { string value = 1; }\n",
    )?;
    fs::write(
        second_provider.join("api/shared.proto"),
        "syntax = \"proto3\"; package proof.shared.v1; message Shared { string value = 1; } message Additional { string value = 1; }\n",
    )?;
    fs::write(
        second_provider.join("api/messages.proto"),
        "syntax = \"proto3\"; package proof.second.v1; import \"shared.proto\";\n",
    )?;
    fs::write(
        second_provider.join("service.yaml"),
        "schema: phoxal/service/v0\noperations:\n  read:\n    contract: proof.second.v1.Read\n    request: proof.shared.v1.Shared\n    response: google.protobuf.Empty\n    max_items: 4\n    max_bytes: 1024\n",
    )?;
    // One local API is bound under two instance names beside a third service.
    fs::write(
        robot.join("robot.yaml"),
        "schema: phoxal/robot/v0\nrobot: { id: proof-robot }\nservices:\n  left_motion:\n    source: { path: ../provider }\n  right_motion:\n    source: { path: ../provider }\n  second:\n    source: { path: ../second-provider }\n",
    )?;
    let framework = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    fs::write(
        robot.join("Cargo.toml"),
        format!(
            "[package]\nname = \"proof-robot\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\nphoxal = {{ path = {:?}, default-features = false, features = [\"contract\"] }}\n[build-dependencies]\nphoxal = {{ path = {:?}, default-features = false, features = [\"build\"] }}\n",
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
        "phoxal::api!();\nfn main() { let _left = api::left_motion::manual(api::left_motion::ManualRequest { linear_x_mps: 0.5 }); let _right = api::right_motion::manual(api::right_motion::ManualRequest { linear_x_mps: 0.5 }); let _other = api::second::read(api::second::Shared { value: String::new() }); }\n",
    )?;
    fs::write(
        robot.join("tests/consumer.rs"),
        "phoxal::api!();\n#[test] fn request_is_inert() { let _call = api::left_motion::manual(api::left_motion::ManualRequest { linear_x_mps: 0.5 }); }\n",
    )?;

    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args(["test", "--offline", "--manifest-path"])
        .arg(robot.join("Cargo.toml"))
        .env(
            "CARGO_TARGET_DIR",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"),
        )
        .output()?;
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!robot.join(".phoxal").exists());
    assert!(!robot.join("generated").exists());
    assert!(!fs::read_to_string(robot.join("Cargo.toml"))?.contains("proof-motion"));
    Ok(())
}

#[test]
fn manifest_service_and_contract_client_compile_standalone()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let service = directory.path().join("consumer");
    let client = directory.path().join("client");
    fs::create_dir_all(service.join("api/example/contract_evaluation/v1"))?;
    fs::create_dir_all(service.join("src"))?;
    fs::create_dir_all(client.join("src"))?;
    fs::write(
        service.join("api/example/contract_evaluation/v1/messages.proto"),
        "syntax = \"proto3\"; package example.contract_evaluation.v1; message ConsumerStatus { string phase = 1; }\n",
    )?;
    fs::write(
        service.join("service.yaml"),
        "schema: phoxal/service/v0\ninputs:\n  encoder:\n    type: phoxal.robotics.v1.EncoderSample\n    delivery: latest\n    required: true\n    max_age_ms: 100\n    max_bytes: 1024\noutputs:\n  status:\n    type: example.contract_evaluation.v1.ConsumerStatus\n    delivery: latest\n    retained_latest: true\n    max_bytes: 4096\noperations:\n  inspect:\n    contract: example.contract_evaluation.v1.InspectConsumer\n    request: google.protobuf.Empty\n    response: example.contract_evaluation.v1.ConsumerStatus\n    max_items: 8\n    max_bytes: 4096\ncalls:\n  read_encoder:\n    contract: example.contract_evaluation.v1.ReadEncoder\n    request: google.protobuf.Empty\n    response: phoxal.robotics.v1.EncoderSample\n    required: true\n    max_items: 8\n    max_bytes: 1024\n",
    )?;
    let framework = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    fs::write(
        service.join("Cargo.toml"),
        format!(
            "[package]\nname = \"proof-manifest-consumer\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\nphoxal = {{ path = {:?}, default-features = false, features = [\"contract\", \"runtime\", \"robotics\"] }}\n[build-dependencies]\nphoxal = {{ path = {:?}, default-features = false, features = [\"build\"] }}\n",
            framework.display().to_string(),
            framework.display().to_string()
        ),
    )?;
    fs::write(
        service.join("build.rs"),
        "fn main() -> Result<(), phoxal::build::Error> { phoxal::build::api(phoxal::build::BuildApiConfig::default()) }\n",
    )?;
    // No handwritten Inputs/Outputs structs and no associated-type declarations:
    // the whole endpoint surface comes from service.yaml.
    fs::write(
        service.join("src/main.rs"),
        "phoxal::api!();\nuse phoxal::runtime::{InitContext, Runtime, StepContext};\nstruct Consumer;\n#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]\nimpl Runtime for Consumer {\n    type Config = ();\n    type State = u64;\n    fn init(&self, _ctx: &InitContext, _config: ()) -> phoxal::Result<Self::State> { Ok(0) }\n    fn step(&self, ctx: &StepContext, state: Self::State, inputs: &Self::Inputs) -> phoxal::Result<(Self::State, Self::Outputs)> {\n        assert!(inputs.encoder_fresh(ctx.now()) || state > 0 || true);\n        let mut outputs = Self::Outputs::default();\n        let ticket = outputs.send(ctx, crate::api::calls::read_encoder(phoxal::contract::Empty {}))?;\n        Ok((state.wrapping_add(1).wrapping_add(ticket.id()), outputs))\n    }\n}\nfn main() -> phoxal::Result<()> { phoxal::runtime::run(Consumer) }\n",
    )?;
    fs::write(
        client.join("robot.yaml"),
        "schema: phoxal/robot/v0\nrobot: { id: proof-client }\nservices:\n  consumer:\n    source: { path: ../consumer }\n",
    )?;
    fs::write(
        client.join("Cargo.toml"),
        format!(
            "[package]\nname = \"proof-contract-client\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\nphoxal = {{ path = {:?}, default-features = false, features = [\"contract\", \"robotics\"] }}\n[build-dependencies]\nphoxal = {{ path = {:?}, default-features = false, features = [\"build\"] }}\n",
            framework.display().to_string(),
            framework.display().to_string()
        ),
    )?;
    fs::write(
        client.join("build.rs"),
        "fn main() -> Result<(), phoxal::build::Error> { phoxal::build::api(phoxal::build::BuildApiConfig::default()) }\n",
    )?;
    fs::write(
        client.join("src/main.rs"),
        "phoxal::api!();\nfn main() { let _observation = api::consumer::status(); let _call = api::consumer::inspect(phoxal::contract::Empty {}); }\n",
    )?;

    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    for (manifest, why) in [
        (service.join("Cargo.toml"), "service"),
        (client.join("Cargo.toml"), "client"),
    ] {
        let output = Command::new(&cargo)
            .args(["build", "--offline", "--manifest-path"])
            .arg(&manifest)
            .env(
                "CARGO_TARGET_DIR",
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"),
            )
            .output()?;
        assert!(
            output.status.success(),
            "{why} build failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

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
    // No brain section: the brain's contract is empty, and its runtime still
    // compiles against the generated empty Inputs/Outputs and provider.
    fs::write(
        robot.join("robot.yaml"),
        "schema: phoxal/robot/v0\nrobot: { id: proof-empty-brain }\n",
    )?;
    let framework = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    fs::write(
        robot.join("Cargo.toml"),
        format!(
            "[package]\nname = \"proof-empty-brain-robot\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\nphoxal = {{ path = {:?}, default-features = false, features = [\"contract\", \"runtime\"] }}\n[build-dependencies]\nphoxal = {{ path = {:?}, default-features = false, features = [\"build\"] }}\n",
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
        "phoxal::api!();\nuse phoxal::runtime::{InitContext, Runtime, StepContext};\nstruct Brain;\n#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]\nimpl Runtime for Brain {\n    type Config = ();\n    type State = u64;\n    fn init(&self, _ctx: &InitContext, _config: ()) -> phoxal::Result<Self::State> { Ok(0) }\n    fn step(&self, _ctx: &StepContext, state: Self::State, _inputs: &Self::Inputs) -> phoxal::Result<(Self::State, Self::Outputs)> { Ok((state, Self::Outputs::default())) }\n}\nfn main() -> phoxal::Result<()> { phoxal::runtime::run(Brain) }\n",
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

#[test]
fn manifest_attachment_conflicts_fail_compilation_with_diagnostics()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let service = directory.path().join("consumer");
    fs::create_dir_all(service.join("api/example/contract_evaluation/v1"))?;
    fs::create_dir_all(service.join("src"))?;
    fs::write(
        service.join("api/example/contract_evaluation/v1/messages.proto"),
        "syntax = \"proto3\"; package example.contract_evaluation.v1; message ConsumerStatus { string phase = 1; }\n",
    )?;
    fs::write(
        service.join("service.yaml"),
        "schema: phoxal/service/v0\noutputs:\n  status:\n    type: example.contract_evaluation.v1.ConsumerStatus\n    delivery: latest\n    retained_latest: true\n    max_bytes: 4096\n",
    )?;
    let framework = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    fs::write(
        service.join("Cargo.toml"),
        format!(
            "[package]\nname = \"proof-manifest-conflict\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\nphoxal = {{ path = {:?}, default-features = false, features = [\"contract\", \"runtime\", \"robotics\"] }}\n[build-dependencies]\nphoxal = {{ path = {:?}, default-features = false, features = [\"build\"] }}\n",
            framework.display().to_string(),
            framework.display().to_string()
        ),
    )?;
    fs::write(
        service.join("build.rs"),
        "fn main() -> Result<(), phoxal::build::Error> { phoxal::build::api(phoxal::build::BuildApiConfig::default()) }\n",
    )?;
    let cases: &[(&str, &str)] = &[
        (
            "redeclared-inputs",
            "phoxal::api!();\nuse phoxal::runtime::{InitContext, Runtime};\nstruct Consumer;\n#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]\nimpl Runtime for Consumer {\n    type Config = ();\n    type State = u64;\n    type Inputs = phoxal::runtime::Outputs;\n    fn init(&self, _ctx: &InitContext, _config: ()) -> phoxal::Result<Self::State> { Ok(0) }\n    fn step(&self, _ctx: &phoxal::runtime::StepContext, state: Self::State, _inputs: &Self::Inputs) -> phoxal::Result<(Self::State, Self::Outputs)> { Ok((state, Self::Outputs::default())) }\n}\nfn main() -> phoxal::Result<()> { phoxal::runtime::run(Consumer) }\n",
        ),
        (
            "wrong-publication-payload",
            "phoxal::api!();\nuse phoxal::runtime::{InitContext, Runtime, StepContext};\nstruct Consumer;\n#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]\nimpl Runtime for Consumer {\n    type Config = ();\n    type State = u64;\n    fn init(&self, _ctx: &InitContext, _config: ()) -> phoxal::Result<Self::State> { Ok(0) }\n    fn step(&self, _ctx: &StepContext, state: Self::State, _inputs: &Self::Inputs) -> phoxal::Result<(Self::State, Self::Outputs)> {\n        let mut outputs = Self::Outputs::default();\n        outputs.status(())?;\n        Ok((state, outputs))\n    }\n}\nfn main() -> phoxal::Result<()> { phoxal::runtime::run(Consumer) }\n",
        ),
    ];
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    for (name, source) in cases {
        fs::write(service.join("src/main.rs"), source)?;
        let output = Command::new(&cargo)
            .args(["build", "--offline", "--manifest-path"])
            .arg(service.join("Cargo.toml"))
            .env(
                "CARGO_TARGET_DIR",
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"),
            )
            .output()?;
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        assert!(!output.status.success(), "{name} must fail to compile");
        let expected = match *name {
            "redeclared-inputs" => "endpoints are declared in service.yaml".to_owned(),
            _ => "expected `ConsumerStatus`, found `()`".to_owned(),
        };
        assert!(
            stderr.contains(&expected),
            "{name} must explain the conflict, got: {stderr}"
        );
    }
    Ok(())
}
