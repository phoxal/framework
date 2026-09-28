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
        "use phoxal::runtime::{InitContext, Runtime, StepContext};\n/// A contract with no endpoints.\n#[phoxal::endpoints]\npub struct BrainApi {}\nstruct Brain;\n#[phoxal::runtime(contract = BrainApi, period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]\nimpl Runtime for Brain {\n    type Config = ();\n    type State = u64;\n    fn init(&self, _ctx: &InitContext, _config: ()) -> phoxal::Result<Self::State> { Ok(0) }\n    fn step(&self, _ctx: &StepContext, state: Self::State, _inputs: &Self::Inputs) -> phoxal::Result<(Self::State, Self::Outputs)> { Ok((state, Self::Outputs::default())) }\n}\nfn main() -> phoxal::Result<()> { phoxal::runtime::run(Brain) }\n",
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
