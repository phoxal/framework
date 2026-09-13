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
    project_fixture_with_service_schema(r#"{"type":"object"}"#)
}

fn project_fixture_with_service_schema(
    service_schema: &str,
) -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    write(
        &directory.path().join("Cargo.toml"),
        r#"[package]
name = "fixture-robot"
version = "0.1.0"
edition = "2024"
build = "build.rs"

[dependencies]
counter-service = { path = "counter-service" }
passive-sensor = { path = "passive-sensor" }

[patch.phoxal]
phoxal-supervisor = { path = "supervisor" }
"#,
    )?;
    write(
        &directory.path().join("build.rs"),
        &artifact_build_script(r#"{"type":"null"}"#),
    )?;
    write(
        &directory.path().join("src/main.rs"),
        "include!(concat!(env!(\"OUT_DIR\"), \"/artifact.rs\"));\nfn main() {}\n",
    )?;
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
build = "build.rs"

[lib]
path = "src/lib.rs"

[[bin]]
name = "counter-service"
path = "src/main.rs"
"#,
    )?;
    write(
        &directory.path().join("counter-service/build.rs"),
        &artifact_build_script(service_schema),
    )?;
    write(
        &directory.path().join("counter-service/src/lib.rs"),
        "pub struct Counter;\n",
    )?;
    write(
        &directory.path().join("counter-service/src/main.rs"),
        "include!(concat!(env!(\"OUT_DIR\"), \"/artifact.rs\"));\nfn main() {}\n",
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
        &directory.path().join("supervisor/Cargo.toml"),
        r#"[package]
name = "phoxal-supervisor"
version = "0.68.0"
edition = "2024"

[lib]
path = "src/lib.rs"

[[bin]]
name = "phoxal-supervisor"
path = "src/main.rs"
"#,
    )?;
    write(
        &directory.path().join("supervisor/src/main.rs"),
        r#"fn main() {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    assert_eq!(arguments.len(), 5);
    assert!(std::path::Path::new(&arguments[0]).is_dir());
    assert_eq!(arguments[1], "--scope");
    assert_eq!(arguments[2], "local");
    assert_eq!(arguments[3], "--supervisor-id");
    assert_eq!(arguments[4], "local");
}
"#,
    )?;
    write(
        &directory.path().join("supervisor/src/lib.rs"),
        "pub const SUPERVISOR_SCHEMA: &str = \"phoxal/supervisor/v0\";\n",
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

fn artifact_build_script(config_schema: &str) -> String {
    let inputs = if config_schema.contains("\"type\":\"object\"") {
        r#"[{"name":"input","kind":"latest","max_age_ms":null,"max_items":null,"max_bytes":null,"port":null,"signature":null}]"#
    } else {
        "[]"
    };
    let payload = format!(
        r#"{{"schema":"phoxal/artifact/v0","record":"runtime","period_ms":10,"timeout_ms":20,"init_timeout_ms":30,"config_schema":{config_schema},"inputs":{inputs},"transient_outputs":[],"service_outputs":[]}}"#
    );
    let length = payload.len() as u32;
    let mut frame = b"PHXART0\n".to_vec();
    frame.extend(length.to_le_bytes());
    frame.extend(payload.as_bytes());
    let bytes = frame
        .iter()
        .map(|byte| byte.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let artifact = format!(
        r#"#[used]
#[cfg_attr(target_os = "macos", unsafe(link_section = "__DATA,__phoxal_art"))]
#[cfg_attr(not(target_os = "macos"), unsafe(link_section = ".phoxal_art"))]
static PHOXAL_ARTIFACT: [u8; {length_plus}] = [{bytes}];
"#,
        length_plus = frame.len()
    );
    format!(
        r#"fn main() {{
    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"))
        .join("artifact.rs");
    std::fs::write(output, {artifact:?}).expect("write artifact source");
    println!("cargo:rerun-if-changed=build.rs");
}}
"#
    )
}

#[test]
fn preparation_resolves_the_root_brain_services_and_passive_component()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = project_fixture()?;
    let project = Project::discover(fixture.path())?;
    let prepared = project.prepare(&CargoOptions::default())?;

    assert_eq!(prepared.root_package().name.as_str(), "fixture-robot");
    assert_eq!(prepared.preparation_changes().len(), 1);
    assert_eq!(
        prepared.preparation_changes()[0].dependency,
        "phoxal-supervisor"
    );
    assert!(
        fs::read_to_string(fixture.path().join("Cargo.toml"))?
            .contains("phoxal-supervisor = { version = \"*\", registry = \"phoxal\" }")
    );
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
    assert!(prepared.preparation_changes().is_empty());
    Ok(())
}

#[test]
fn locked_or_frozen_missing_initialization_fails_before_manifest_mutation()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = project_fixture()?;
    let project = Project::discover(fixture.path())?;
    let manifest = fixture.path().join("Cargo.toml");
    let before = fs::read(&manifest)?;
    let error = project
        .prepare(&CargoOptions {
            lock: LockMode::Locked,
            offline: true,
            ..CargoOptions::default()
        })
        .expect_err("locked preparation must request explicit initialization");
    assert!(matches!(
        error,
        Error::MissingInitialization {
            dependency,
            lock_mode: "--locked",
            ..
        } if dependency == "phoxal-supervisor"
    ));
    assert_eq!(fs::read(&manifest)?, before);
    assert!(!fixture.path().join("Cargo.lock").exists());

    let error = project
        .prepare(&CargoOptions {
            lock: LockMode::Frozen,
            offline: true,
            ..CargoOptions::default()
        })
        .expect_err("frozen preparation must request explicit initialization");
    assert!(matches!(
        error,
        Error::MissingInitialization {
            dependency,
            lock_mode: "--frozen",
            ..
        } if dependency == "phoxal-supervisor"
    ));
    assert_eq!(fs::read(&manifest)?, before);
    assert!(!fixture.path().join("Cargo.lock").exists());
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
    let manifest = fixture.path().join("Cargo.toml");
    let before_manifest = fs::read(&manifest)?;
    let error = project
        .prepare(&CargoOptions::default())
        .expect_err("fuzzy dependency names must fail");
    assert!(matches!(
        error,
        Error::Source(SourceError::DependencyNotDeclared { key, .. }) if key == "counter-servic"
    ));
    assert_eq!(fs::read(&manifest)?, before_manifest);
    assert!(!fixture.path().join("Cargo.lock").exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn automatic_manifest_replacement_preserves_cargo_toml_permissions()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let fixture = project_fixture()?;
    let manifest = fixture.path().join("Cargo.toml");
    fs::set_permissions(&manifest, fs::Permissions::from_mode(0o640))?;
    let project = Project::discover(fixture.path())?;
    project.prepare(&CargoOptions {
        offline: true,
        ..CargoOptions::default()
    })?;
    assert_eq!(fs::metadata(&manifest)?.mode() & 0o777, 0o640);
    Ok(())
}

#[test]
fn failed_selection_restores_a_preexisting_workspace_lock_exactly()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = project_fixture()?;
    let options = CargoOptions {
        offline: true,
        ..CargoOptions::default()
    };
    let project = Project::discover(fixture.path())?;
    project.prepare(&options)?;

    let manifest = fixture.path().join("Cargo.toml");
    let without_supervisor = fs::read_to_string(&manifest)?.replace(
        "phoxal-supervisor = { version = \"*\", registry = \"phoxal\" }\n",
        "",
    );
    write(&manifest, &without_supervisor)?;
    let lock = fixture.path().join("Cargo.lock");
    let before_lock = fs::read(&lock)?;
    write(
        &fixture.path().join("supervisor/Cargo.toml"),
        &fs::read_to_string(fixture.path().join("supervisor/Cargo.toml"))?
            .replace("version = \"0.68.0\"", "version = \"0.69.0\""),
    )?;
    write(
        &fixture.path().join("robot.yaml"),
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
        .prepare(&options)
        .expect_err("selection failure must roll back automatic preparation");
    assert!(matches!(
        error,
        Error::Source(SourceError::DependencyNotDeclared { key, .. }) if key == "counter-servic"
    ));
    assert_eq!(fs::read(&manifest)?, without_supervisor.as_bytes());
    assert_eq!(fs::read(&lock)?, before_lock);
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
fn check_validates_the_exact_compiled_configuration_schema()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = project_fixture_with_service_schema(
        r#"{"type":"object","properties":{"count":{"type":"integer"}},"required":["count"],"additionalProperties":false}"#,
    )?;
    write(
        &fixture.path().join("robot.yaml"),
        r#"schema: phoxal/robot/v0
robot:
  id: fixture-robot
  components: {}
brain: {}
services:
  counter:
    implementation: counter-service
    config: {}
connections: {}
"#,
    )?;
    let project = Project::discover(fixture.path())?;
    let prepared = project.prepare(&CargoOptions {
        offline: true,
        ..CargoOptions::default()
    })?;
    let error = prepared
        .check(&CargoOptions {
            offline: true,
            ..CargoOptions::default()
        })
        .expect_err("missing required config field must fail exact schema validation");
    assert!(matches!(
        error,
        Error::ConfigurationInvalid {
            role,
            instance,
            field,
            ..
        } if role == "service"
            && instance == "counter"
            && field == "services.counter.config"
    ));
    Ok(())
}

#[test]
fn build_validates_the_exact_compiled_configuration_schema()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = project_fixture_with_service_schema(
        r#"{"type":"object","properties":{"count":{"type":"integer"}},"required":["count"],"additionalProperties":false}"#,
    )?;
    write(
        &fixture.path().join("robot.yaml"),
        r#"schema: phoxal/robot/v0
robot:
  id: fixture-robot
  components: {}
brain: {}
services:
  counter:
    implementation: counter-service
    config: {}
connections: {}
"#,
    )?;
    let project = Project::discover(fixture.path())?;
    let options = CargoOptions {
        offline: true,
        ..CargoOptions::default()
    };
    let prepared = project.prepare(&options)?;
    let output = fixture.path().join("target/phoxal/fixture-robot/bundle");
    let error = prepared
        .build_bundle(&options, &output)
        .expect_err("bundle assembly must validate authored configuration");
    assert!(matches!(
        error,
        Error::ConfigurationInvalid {
            role,
            instance,
            field,
            ..
        } if role == "service"
            && instance == "counter"
            && field == "services.counter.config"
    ));
    assert!(!output.exists());
    Ok(())
}

#[test]
fn missing_runtime_contract_is_an_error_and_does_not_publish_a_bundle()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = project_fixture()?;
    write(&fixture.path().join("build.rs"), "fn main() {}\n")?;
    write(&fixture.path().join("src/main.rs"), "fn main() {}\n")?;
    let project = Project::discover(fixture.path())?;
    let options = CargoOptions {
        offline: true,
        ..CargoOptions::default()
    };
    let prepared = project.prepare(&options)?;
    let output = fixture.path().join("target/phoxal/fixture-robot/bundle");
    let error = prepared
        .build_bundle(&options, &output)
        .expect_err("a selected executable without a Runtime contract must fail");
    assert!(matches!(
        error,
        Error::MissingArtifactContract {
            role,
            instance,
            package,
            target,
        } if role == "brain"
            && instance == "brain"
            && package == "fixture-robot"
            && target == "fixture-robot"
    ));
    assert!(
        !output.exists(),
        "failed assembly must not publish a partial bundle"
    );
    Ok(())
}

#[test]
fn run_local_builds_the_bundle_and_launches_the_local_supervisor()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = project_fixture()?;
    let project = Project::discover(fixture.path())?;
    let options = CargoOptions {
        offline: true,
        ..CargoOptions::default()
    };
    let prepared = project.prepare(&options)?;
    let output = fixture
        .path()
        .join("target/phoxal/fixture-robot/run-bundle");
    let bundle = prepared.run_local(&options, &output)?;
    assert_eq!(bundle.root(), output.as_path());
    assert!(bundle.executable("brain").is_file());
    assert!(bundle.executable("counter").is_file());
    Ok(())
}

#[test]
fn component_driver_uses_the_component_owner_dependency_and_selected_binary()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = project_fixture()?;
    let root_manifest = fixture.path().join("Cargo.toml");
    let root_source = fs::read_to_string(&root_manifest)?.replace(
        "[dependencies]\n",
        "[features]\nsensor-driver = [\"passive-sensor/driver\"]\n\n[dependencies]\n",
    );
    write(&root_manifest, &root_source)?;
    write(
        &fixture.path().join("passive-sensor/Cargo.toml"),
        r#"[package]
name = "passive-sensor"
version = "0.1.0"
edition = "2024"

[lib]
path = "src/lib.rs"

[features]
driver = ["dep:sensor-driver"]

[dependencies]
sensor-driver = { path = "../sensor-driver", optional = true }
"#,
    )?;
    write(
        &fixture.path().join("sensor-driver/Cargo.toml"),
        r#"[package]
name = "sensor-driver"
version = "0.1.0"
edition = "2024"
build = "build.rs"

[lib]
path = "src/lib.rs"

[[bin]]
name = "sensor-driver"
path = "src/main.rs"
"#,
    )?;
    write(
        &fixture.path().join("sensor-driver/build.rs"),
        &artifact_build_script(r#"{"type":"object"}"#),
    )?;
    write(
        &fixture.path().join("sensor-driver/src/main.rs"),
        "include!(concat!(env!(\"OUT_DIR\"), \"/artifact.rs\"));\nfn main() {}\n",
    )?;
    write(
        &fixture.path().join("sensor-driver/src/lib.rs"),
        "pub const DRIVER_NAME: &str = \"sensor-driver\";\n",
    )?;
    write(
        &fixture.path().join("robot.yaml"),
        r#"schema: phoxal/robot/v0
robot:
  id: fixture-robot
  components:
    sensor:
      component: passive-sensor
      mount_link: sensor_mount
      driver:
        dependency: sensor-driver
        binary: sensor-driver
        config: {}
brain: {}
services:
  counter:
    implementation: counter-service
connections: {}
"#,
    )?;
    let project = Project::discover(fixture.path())?;
    let options = CargoOptions {
        offline: true,
        features: vec!["sensor-driver".to_owned()],
        ..CargoOptions::default()
    };
    let prepared = project.prepare(&options)?;
    let driver = prepared.sources().components["sensor"]
        .driver
        .as_ref()
        .expect("driver selection");
    assert_eq!(driver.dependency_key, "sensor-driver");
    assert_eq!(driver.package, "sensor-driver");
    assert_eq!(driver.binary.target, "sensor-driver");
    let output = fixture.path().join("target/phoxal/fixture-robot/driver");
    let bundle = prepared.build_bundle(&options, &output)?;
    assert!(bundle.executable("sensor").is_file());
    Ok(())
}

#[test]
fn build_bundle_contains_the_complete_selected_executable_set_and_provenance()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = project_fixture()?;
    write(&fixture.path().join("assets/mesh.stl"), "mesh-bytes\n")?;
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
    let model_closure = bundle
        .provenance()
        .model_closure
        .as_ref()
        .expect("the model closure is recorded");
    assert_eq!(model_closure.entry, "assets/model.xml");
    assert_eq!(
        model_closure
            .resources
            .iter()
            .map(|resource| resource.path.as_str())
            .collect::<Vec<_>>(),
        ["assets/assets/mesh.stl", "assets/model.xml"]
    );
    assert_eq!(
        fs::read(output.join("assets/model.xml"))?,
        fs::read(fixture.path().join("model.xml"))?
    );
    assert_eq!(
        fs::read(output.join("assets/assets/mesh.stl"))?,
        fs::read(fixture.path().join("assets/mesh.stl"))?
    );

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
