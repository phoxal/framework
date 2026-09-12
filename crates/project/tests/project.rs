use std::fs;
use std::path::Path;

use phoxal_project::{
    CargoOperation, CargoOptions, Error, LockMode, PackageSource, Project, SourceError,
};

fn write(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)
}

fn project_fixture() -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    write(
        &directory.path().join("Cargo.toml"),
        r#"[package]
name = "fixture-robot"
version = "0.1.0"
edition = "2024"

[dependencies]
counter-service = { path = "counter-service" }
passive-sensor = { path = "passive-sensor" }
"#,
    )?;
    write(&directory.path().join("src/main.rs"), "fn main() {}\n")?;
    write(
        &directory.path().join("model.xml"),
        "<model name=\"fixture\" />\n",
    )?;
    write(
        &directory.path().join("counter-service/Cargo.toml"),
        r#"[package]
name = "counter-service"
version = "0.1.0"
edition = "2024"

[lib]
path = "src/lib.rs"

[[bin]]
name = "counter-service"
path = "src/main.rs"
"#,
    )?;
    write(
        &directory.path().join("counter-service/src/lib.rs"),
        "pub struct Counter;\n",
    )?;
    write(
        &directory.path().join("counter-service/src/main.rs"),
        "fn main() {}\n",
    )?;
    write(
        &directory.path().join("passive-sensor/Cargo.toml"),
        r#"[package]
name = "passive-sensor"
version = "0.1.0"
edition = "2024"

[lib]
path = "src/lib.rs"
"#,
    )?;
    write(
        &directory.path().join("passive-sensor/src/lib.rs"),
        "pub struct Sensor;\n",
    )?;
    write(
        &directory.path().join("robot.yaml"),
        r#"schema: phoxal/robot/v0
robot:
  id: fixture-robot
  model: model.xml
  components:
    sensor:
      component: passive-sensor
      mount_link: sensor_mount
brain: {}
services:
  counter:
    implementation: counter-service
connections:
  counter.input: sensor.sample
"#,
    )?;
    Ok(directory)
}

#[test]
fn preparation_resolves_the_root_brain_services_and_passive_component()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = project_fixture()?;
    let project = Project::discover(fixture.path())?;
    let prepared = project.prepare(&CargoOptions::default())?;

    assert_eq!(prepared.root_package().name.as_str(), "fixture-robot");
    assert_eq!(prepared.sources().brain.target, "fixture-robot");
    let service = &prepared.sources().services["counter"];
    assert_eq!(service.dependency_key, "counter-service");
    assert_eq!(service.binary.target, "counter-service");
    assert_eq!(service.library.target, "counter_service");
    assert!(matches!(service.source, PackageSource::Local { .. }));
    assert_eq!(
        prepared.sources().components["sensor"].package,
        "passive-sensor"
    );
    assert!(prepared.cargo_lock().ends_with("Cargo.lock"));
    Ok(())
}

#[test]
fn locked_preparation_uses_the_existing_root_lock() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = project_fixture()?;
    let project = Project::discover(fixture.path())?;
    let unlocked = CargoOptions {
        offline: true,
        ..CargoOptions::default()
    };
    project.prepare(&unlocked)?;

    let locked = CargoOptions {
        lock: LockMode::Locked,
        offline: true,
        ..CargoOptions::default()
    };
    let prepared = project.prepare(&locked)?;
    assert!(prepared.cargo_lock().is_file());
    Ok(())
}

#[test]
fn an_unresolved_or_fuzzy_service_key_is_rejected_without_fallback()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = project_fixture()?;
    let robot = fixture.path().join("robot.yaml");
    write(
        &robot,
        r#"robot:
  id: fixture-robot
  components: {}
services:
  counter:
    implementation: counter-servic
connections: {}
"#,
    )?;
    let project = Project::discover(fixture.path())?;
    let error = project
        .prepare(&CargoOptions::default())
        .expect_err("fuzzy dependency names must fail");
    assert!(matches!(
        error,
        Error::Source(SourceError::DependencyNotDeclared { key, .. }) if key == "counter-servic"
    ));
    Ok(())
}

#[test]
fn check_runs_the_root_and_selected_service_targets() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = project_fixture()?;
    let project = Project::discover(fixture.path())?;
    let options = CargoOptions {
        offline: true,
        ..CargoOptions::default()
    };
    let prepared = project.prepare(&options)?;
    let outputs = prepared.run(CargoOperation::Check, &options)?;
    assert_eq!(outputs.len(), 2);
    Ok(())
}

#[test]
fn build_bundle_contains_the_complete_selected_executable_set_and_provenance()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = project_fixture()?;
    let project = Project::discover(fixture.path())?;
    let options = CargoOptions {
        offline: true,
        ..CargoOptions::default()
    };
    let prepared = project.prepare(&options)?;
    let output = fixture.path().join("target/phoxal/fixture-robot/bundle");
    let bundle = prepared.build_bundle(&options, &output)?;

    assert_eq!(bundle.manifest().schema, "phoxal/bundle/v0");
    assert_eq!(bundle.manifest().executables.len(), 2);
    assert_eq!(
        bundle
            .manifest()
            .executables
            .iter()
            .map(|executable| executable.instance.as_str())
            .collect::<Vec<_>>(),
        ["brain", "counter"]
    );
    assert!(bundle.executable("brain").is_file());
    assert!(bundle.executable("counter").is_file());
    assert!(output.join("manifest.json").is_file());
    assert!(output.join("provenance.json").is_file());
    assert!(bundle.provenance().cargo_lock_sha256.is_some());
    assert!(bundle.provenance().model.is_some());

    let output_modified = std::fs::metadata(&output)?.modified()?;
    let second = prepared.build_bundle(&options, &output)?;
    assert_eq!(second.manifest(), bundle.manifest());
    assert_eq!(
        std::fs::metadata(&output)?.modified()?,
        output_modified,
        "unchanged assembly must preserve its output timestamp"
    );
    Ok(())
}
