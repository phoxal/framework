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
    fs::write(
        provider.join("api/motion.proto"),
        "syntax = \"proto3\"; package proof.motion.v1; import \"google/protobuf/empty.proto\"; import \"shared.proto\"; message ManualRequest { double linear_x_mps = 1; } service Motion { rpc Manual(ManualRequest) returns (google.protobuf.Empty); }\n",
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
        second_provider.join("api/second.proto"),
        "syntax = \"proto3\"; package proof.second.v1; import \"google/protobuf/empty.proto\"; import \"shared.proto\"; service Second { rpc Read(proof.shared.v1.Shared) returns (google.protobuf.Empty); }\n",
    )?;
    fs::write(
        robot.join("robot.yaml"),
        "schema: phoxal/robot/v0\nrobot: { id: proof-robot }\nservices:\n  motion:\n    source: { path: ../provider }\n  second:\n    source: { path: ../second-provider }\n",
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
        "phoxal::api!();\nfn main() { let _call = api::motion::manual(api::motion::ManualRequest { linear_x_mps: 0.5 }); let _other = api::second::read(api::second::Shared { value: String::new() }); }\n",
    )?;
    fs::write(
        robot.join("tests/consumer.rs"),
        "phoxal::api!();\n#[test] fn request_is_inert() { let _call = api::motion::manual(api::motion::ManualRequest { linear_x_mps: 0.5 }); }\n",
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
