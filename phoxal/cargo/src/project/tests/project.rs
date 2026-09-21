use std::fs;
use std::path::{Component, Path};
use std::process::Command;

use crate::project::bundle::{BundleManifest, BundleProvenance};
use crate::project::document::ComponentDocument;
use crate::project::{
    CargoOperation, CargoOptions, CargoSelection, Error, LockMode, PackageSource, Project,
    RobotDocument, SourceError,
};
use sha2::{Digest, Sha256};

fn write(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    Ok(format!("{:x}", Sha256::digest(fs::read(path)?)))
}

fn copy_tree(source: &Path, destination: &Path) -> std::io::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.is_file() {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, destination)?;
        return Ok(());
    }
    fs::create_dir_all(destination)?;
    let mut entries = fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if matches!(
            entry.file_name().to_str(),
            Some("target" | ".git" | ".codex")
        ) {
            continue;
        }
        copy_tree(&entry.path(), &destination.join(entry.file_name()))?;
    }
    Ok(())
}

fn relative_path(from: &Path, to: &Path) -> std::path::PathBuf {
    let from = from.components().collect::<Vec<_>>();
    let to = to.components().collect::<Vec<_>>();
    let common = from
        .iter()
        .zip(&to)
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative = std::path::PathBuf::new();
    for component in &from[common..] {
        if matches!(component, Component::Normal(_)) {
            relative.push("..");
        }
    }
    for component in &to[common..] {
        relative.push(component.as_os_str());
    }
    relative
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
        &artifact_build_script(r#"{"type":"null"}"#, false),
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
        &artifact_build_script(service_schema, true),
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
        &directory.path().join("passive-sensor/component.yaml"),
        r#"schema: phoxal/component/v0
model: { file: model.xml, root_body: mount }
capabilities:
  sample:
    kind: range
    publish_rate_hz: 20.0
    min_range_m: 0.04
    max_range_m: 4.0
    field_of_view_rad: 0.47
    target: { kind: site, id: sensor_site }
"#,
    )?;
    write(
        &directory.path().join("passive-sensor/model.xml"),
        r#"<mujoco model="passive-sensor">
  <compiler angle="radian"/>
  <worldbody><body name="mount"><site name="sensor_site" size="0.001"/></body></worldbody>
</mujoco>
"#,
    )?;
    write(
        &directory.path().join("supervisor/Cargo.toml"),
        r#"[package]
name = "phoxal-supervisor"
version = "0.68.0"
edition = "2024"

[dependencies]
clap = { version = "4.6.1", features = ["derive"] }

[lib]
path = "src/lib.rs"

[[bin]]
name = "phoxal-supervisor"
path = "src/main.rs"
"#,
    )?;
    write(
        &directory.path().join("supervisor/src/main.rs"),
        r#"use clap::Parser;

#[derive(Parser)]
struct Arguments {
    bundle: std::path::PathBuf,
    #[arg(long)]
    scope: String,
    #[arg(long)]
    supervisor_id: String,
}

fn main() {
    let arguments = Arguments::parse();
    assert!(arguments.bundle.is_dir());
    assert_eq!(arguments.scope, "local");
    assert_eq!(arguments.supervisor_id, "local");
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
      mount_site: sensor_mount
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

fn targetless_component_fixture() -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
    let fixture = project_fixture()?;
    let manifest = fixture.path().join("Cargo.toml");
    let contents = fs::read_to_string(&manifest)?.replace(
        "[dependencies]\n",
        "[dependencies]\nphoxal-supervisor = { path = \"supervisor\" }\n",
    );
    write(&manifest, &contents)?;

    let component_manifest = fixture.path().join("passive-sensor/Cargo.toml");
    write(
        &component_manifest,
        r#"[package]
name = "passive-sensor"
version = "0.1.0"
edition = "2024"
autolib = false
autobins = false
"#,
    )?;
    fs::remove_file(fixture.path().join("passive-sensor/src/lib.rs"))?;
    fs::remove_dir(fixture.path().join("passive-sensor/src"))?;
    Ok(fixture)
}

fn nested_workspace_fixture()
-> Result<(tempfile::TempDir, std::path::PathBuf), Box<dyn std::error::Error>> {
    let workspace = tempfile::tempdir()?;
    write(
        &workspace.path().join("Cargo.toml"),
        r#"[workspace]
members = ["robot"]
resolver = "3"

[workspace.package]
edition = "2024"

[patch.phoxal]
phoxal-supervisor = { path = "robot/supervisor" }
"#,
    )?;
    let source = project_fixture()?;
    let robot = workspace.path().join("robot");
    copy_tree(source.path(), &robot)?;
    let manifest = robot.join("Cargo.toml");
    let contents = fs::read_to_string(&manifest)?
        .replace("edition = \"2024\"", "edition.workspace = true")
        .replace(
            "\n[patch.phoxal]\nphoxal-supervisor = { path = \"supervisor\" }\n",
            "\n",
        );
    write(&manifest, &contents)?;
    Ok((workspace, robot))
}

fn nested_workspace_race_build_script() -> String {
    artifact_build_script(r#"{"type":"null"}"#, false).replace(
        "    println!(\"cargo:rerun-if-changed=build.rs\");",
        r##"    let workspace_manifest = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("Cargo.toml");
    let mut contents = std::fs::read_to_string(&workspace_manifest).expect("read workspace manifest");
    if !contents.contains("# nested workspace race") {
        contents.push_str("# nested workspace race\n");
        std::fs::write(workspace_manifest, contents).expect("write workspace manifest");
    }
    println!("cargo:rerun-if-changed=build.rs");"##,
    )
}

fn artifact_build_script(config_schema: &str, with_input: bool) -> String {
    let inputs = if with_input {
        r#"[{"name":"input","kind":"latest","max_age_ms":null,"max_items":null,"max_bytes":null,"port":null,"signature":null,"request_fqn":null,"response_fqn":"fixture.Sample"}]"#
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
    match &prepared.preparation_changes()[0] {
        crate::project::PreparationChange::SupervisorDependencyAdded { dependency, .. } => {
            assert_eq!(dependency, "phoxal-supervisor");
        }
        other => panic!("expected supervisor dependency addition, got {other:?}"),
    }
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
fn targetless_local_component_uses_an_isolated_inert_carrier()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = targetless_component_fixture()?;
    let root_manifest = fixture.path().join("Cargo.toml");
    let component_root = fixture.path().join("passive-sensor");
    let component_manifest = component_root.join("Cargo.toml");
    let component_definition = component_root.join("component.yaml");
    let component_model = component_root.join("model.xml");
    let before_root = fs::read(&root_manifest)?;
    let before_component_manifest = fs::read(&component_manifest)?;
    let before_component_definition = fs::read(&component_definition)?;
    let before_component_model = fs::read(&component_model)?;

    let prepared = Project::discover(fixture.path())?.prepare(&CargoOptions {
        offline: true,
        ..CargoOptions::default()
    })?;

    assert_eq!(fs::read(&root_manifest)?, before_root);
    assert_eq!(fs::read(&component_manifest)?, before_component_manifest);
    assert_eq!(
        fs::read(&component_definition)?,
        before_component_definition
    );
    assert_eq!(fs::read(&component_model)?, before_component_model);
    assert!(!component_root.join("_cargo/lib.rs").exists());
    assert!(prepared.cargo_lock().is_file());
    let package = prepared
        .metadata()
        .packages
        .iter()
        .find(|package| package.name == "passive-sensor")
        .expect("targetless component is in Cargo metadata");
    assert!(package.targets.iter().any(|target| {
        target.is_lib() && target.src_path.as_std_path().ends_with("_cargo/lib.rs")
    }));
    assert!(prepared.sources().components["sensor"].driver.is_none());
    Ok(())
}

#[test]
fn targetless_local_logical_identity_is_stable_across_shadow_preparations()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = targetless_component_fixture()?;
    let project = Project::discover(fixture.path())?;
    let options = CargoOptions {
        offline: true,
        ..CargoOptions::default()
    };

    let first = project.prepare(&options)?;
    let first_metadata = serde_json::to_string(first.metadata())?;
    let first_sources = first.sources().clone();
    let first_bundle = first.build_bundle(
        &options,
        fixture
            .path()
            .join("target/phoxal/fixture-robot/identity-one"),
    )?;

    let second = project.prepare(&options)?;
    let second_metadata = serde_json::to_string(second.metadata())?;
    let second_bundle = second.build_bundle(
        &options,
        fixture
            .path()
            .join("target/phoxal/fixture-robot/identity-two"),
    )?;

    assert_eq!(first_metadata, second_metadata);
    assert_eq!(first_sources, *second.sources());
    let BundleProvenance::V0 {
        sources: first_provenance_sources,
        source_closure_sha256: first_source_closure_sha256,
        source_tree: first_source_tree,
        toolchain: first_toolchain,
        ..
    } = first_bundle.provenance();
    let BundleProvenance::V0 {
        sources: second_provenance_sources,
        source_closure_sha256: second_source_closure_sha256,
        source_tree: second_source_tree,
        toolchain: second_toolchain,
        ..
    } = second_bundle.provenance();
    assert_eq!(first_provenance_sources, second_provenance_sources);
    assert_eq!(first_source_closure_sha256, second_source_closure_sha256);
    assert_eq!(first_source_tree, second_source_tree);
    assert_eq!(first_toolchain.invocations, second_toolchain.invocations);
    assert!(!first_metadata.contains("_phoxal_path_dependencies"));
    let provenance = serde_json::to_string(first_bundle.provenance())?;
    assert!(!provenance.contains(&fixture.path().display().to_string()));
    let passive = first_provenance_sources
        .iter()
        .find(|source| source.package == "passive-sensor")
        .expect("targetless component provenance");
    assert_eq!(passive.derived_files, vec!["_cargo/lib.rs".to_owned()]);
    assert!(
        passive
            .authored_path
            .as_deref()
            .is_some_and(|path| path.ends_with("passive-sensor"))
    );
    Ok(())
}

#[test]
fn targetless_local_preparation_rolls_back_the_logical_lock_on_selection_failure()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = targetless_component_fixture()?;
    let root_manifest = fixture.path().join("Cargo.toml");
    let component_root = fixture.path().join("passive-sensor");
    let component_manifest = component_root.join("Cargo.toml");
    let component_definition = component_root.join("component.yaml");
    let before_root = fs::read(&root_manifest)?;
    let before_component_manifest = fs::read(&component_manifest)?;
    let before_component_definition = fs::read(&component_definition)?;
    let robot = fixture.path().join("robot.yaml");
    let robot_contents = fs::read_to_string(&robot)?.replace(
        "implementation: counter-service",
        "implementation: counter-servic",
    );
    write(&robot, &robot_contents)?;

    let error = Project::discover(fixture.path())?
        .prepare(&CargoOptions {
            offline: true,
            ..CargoOptions::default()
        })
        .expect_err("source selection failure must not publish staged lock state");
    assert!(matches!(
        error,
        Error::Source(SourceError::DependencyNotDeclared { key, .. }) if key == "counter-servic"
    ));
    assert_eq!(fs::read(&root_manifest)?, before_root);
    assert_eq!(fs::read(&component_manifest)?, before_component_manifest);
    assert_eq!(
        fs::read(&component_definition)?,
        before_component_definition
    );
    assert!(!fixture.path().join("Cargo.lock").exists());
    assert!(!component_root.join("_cargo/lib.rs").exists());
    Ok(())
}

#[test]
fn targetless_local_locked_mode_requires_a_real_existing_lock()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = targetless_component_fixture()?;
    let root_manifest = fixture.path().join("Cargo.toml");
    let before_root = fs::read(&root_manifest)?;
    let project = Project::discover(fixture.path())?;
    let locked = CargoOptions {
        lock: LockMode::Locked,
        offline: true,
        ..CargoOptions::default()
    };
    let error = project
        .prepare(&locked)
        .expect_err("locked targetless preparation must not create a lock");
    assert!(matches!(error, Error::CargoMetadata { .. }));
    assert_eq!(fs::read(&root_manifest)?, before_root);
    assert!(!fixture.path().join("Cargo.lock").exists());
    assert!(!fixture.path().join("passive-sensor/_cargo/lib.rs").exists());

    project.prepare(&CargoOptions {
        offline: true,
        ..CargoOptions::default()
    })?;
    assert!(fixture.path().join("Cargo.lock").is_file());
    project.prepare(&locked)?;
    Ok(())
}

#[test]
fn selection_rejects_a_service_with_a_component_definition()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = project_fixture()?;
    write(
        &fixture.path().join("counter-service/component.yaml"),
        "schema: phoxal/component/v0\n",
    )?;

    let error = Project::discover(fixture.path())?
        .prepare(&CargoOptions::default())
        .expect_err("a component cannot provide a selected service");
    assert!(matches!(
        error,
        Error::Source(SourceError::InvalidPackageRole {
            role,
            package,
            message,
            ..
        }) if role.to_string() == "service"
            && package == "counter-service"
            && message.contains("component")
    ));
    Ok(())
}

#[test]
fn selection_rejects_a_component_without_its_definition_root()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = project_fixture()?;
    fs::remove_file(fixture.path().join("passive-sensor/component.yaml"))?;

    let error = Project::discover(fixture.path())?
        .prepare(&CargoOptions::default())
        .expect_err("a selected component needs a definition root");
    assert!(matches!(
        error,
        Error::Source(SourceError::InvalidPackageRole {
            role,
            package,
            message,
            ..
        }) if role.to_string() == "component"
            && package == "passive-sensor"
            && message.contains("no component definition")
    ));
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
        r#"schema: phoxal/robot/v0
robot:
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
        r#"schema: phoxal/robot/v0
robot:
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
fn explicit_cargo_selection_runs_once_without_target_duplication()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = project_fixture()?;
    let project = Project::discover(fixture.path())?;
    let options = CargoOptions {
        offline: true,
        release: true,
        cargo_args: vec!["--color".into(), "never".into()],
        selection: CargoSelection {
            packages: vec!["fixture-robot".to_owned()],
            all_targets: true,
            ..CargoSelection::default()
        },
        ..CargoOptions::default()
    };
    let prepared = project.prepare(&options)?;
    let outputs = prepared.run(CargoOperation::Check, &options)?;
    assert_eq!(outputs.len(), 1);
    let package_count = outputs[0]
        .arguments
        .windows(2)
        .filter(|window| window[0] == "--package")
        .count();
    assert_eq!(package_count, 1);
    assert!(
        outputs[0]
            .arguments
            .windows(2)
            .any(|window| window[0] == "--package" && window[1] == "fixture-robot")
    );
    assert!(
        outputs[0]
            .arguments
            .iter()
            .any(|argument| argument == "--all-targets")
    );
    let release_position = outputs[0]
        .arguments
        .iter()
        .position(|argument| argument == "--release")
        .expect("typed release flag is forwarded");
    let color_position = outputs[0]
        .arguments
        .iter()
        .position(|argument| argument == "--color")
        .expect("raw Cargo arguments are forwarded");
    assert!(release_position < color_position);
    assert!(
        outputs[0]
            .arguments
            .windows(2)
            .any(|window| window[0] == "--color" && window[1] == "never")
    );
    let build_outputs = prepared.run(CargoOperation::Build, &options)?;
    assert_eq!(build_outputs.len(), 1);
    let build_package_count = build_outputs[0]
        .arguments
        .windows(2)
        .filter(|window| window[0] == "--package")
        .count();
    assert_eq!(build_package_count, 1);
    assert!(
        build_outputs[0]
            .arguments
            .iter()
            .any(|argument| argument == "--all-targets")
    );
    let test_options = CargoOptions {
        test_args: vec!["--nocapture".into()],
        ..options.clone()
    };
    let test_outputs = prepared.run(CargoOperation::Test, &test_options)?;
    assert_eq!(test_outputs.len(), 1);
    let delimiter = test_outputs[0]
        .arguments
        .iter()
        .position(|argument| argument == "--")
        .expect("test command has a Cargo test delimiter");
    assert!(matches!(
        test_outputs[0].arguments.get(delimiter + 1),
        Some(argument) if argument == "--nocapture"
    ));
    Ok(())
}

#[test]
fn explicit_update_validates_the_fresh_graph_before_success()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = project_fixture()?;
    let project = Project::discover(fixture.path())?;
    let options = CargoOptions {
        offline: true,
        cargo_args: vec!["--dry-run".into()],
        selection: CargoSelection {
            packages: vec!["fixture-robot".to_owned()],
            ..CargoSelection::default()
        },
        ..CargoOptions::default()
    };
    let outputs = project.update(&options)?;
    assert_eq!(outputs.len(), 1);
    assert!(
        outputs[0]
            .arguments
            .iter()
            .any(|argument| argument == "update")
    );
    assert!(
        outputs[0]
            .arguments
            .iter()
            .any(|argument| argument == "fixture-robot")
    );
    assert!(
        !outputs[0]
            .arguments
            .iter()
            .any(|argument| argument == "--package")
    );
    assert!(fixture.path().join("Cargo.lock").is_file());
    assert!(
        fixture
            .path()
            .join("Cargo.toml")
            .display()
            .to_string()
            .ends_with("Cargo.toml")
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn update_repairs_resolution_before_metadata_including_targetless_sources()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;

    for targetless in [false, true] {
        let fixture = if targetless {
            targetless_component_fixture()?
        } else {
            project_fixture()?
        };
        let wrapper_root = tempfile::tempdir()?;
        let wrapper = wrapper_root.path().join("cargo");
        let real_cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        // Model a graph whose existing lock cannot resolve until Cargo updates
        // it. Forward every actual operation to Cargo, including the staged
        // targetless graph and the subsequent contract validation builds.
        let quote = |value: &str| format!("'{}'", value.replace('\'', "'\"'\"'"));
        write(
            &wrapper,
            &format!(
                r#"#!/bin/sh
marker={marker}
case "$1" in
  update) touch "$marker" ;;
  metadata) test -f "$marker" || exit 86 ;;
esac
exec {cargo} "$@"
"#,
                marker = quote(&wrapper_root.path().join("updated").display().to_string()),
                cargo = quote(&real_cargo)
            ),
        )?;
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700))?;
        let outputs = Project::discover(fixture.path())?.update(&CargoOptions {
            cargo_path: Some(wrapper),
            offline: true,
            ..CargoOptions::default()
        })?;
        assert_eq!(outputs.len(), 1);
        assert!(fixture.path().join("Cargo.lock").is_file());
        if targetless {
            assert!(!fixture.path().join("passive-sensor/_cargo").exists());
        }
    }
    Ok(())
}

#[test]
fn failed_cargo_test_preserves_the_assertion_diagnostic() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = project_fixture()?;
    let main = fixture.path().join("src/main.rs");
    let mut source = fs::read_to_string(&main)?;
    source.push_str("\n#[test] fn failed_sensor_acceptance() { panic!(\"capture exceeded the freshness budget\"); }\n");
    write(&main, &source)?;
    let prepared = Project::discover(fixture.path())?.prepare(&CargoOptions {
        offline: true,
        ..CargoOptions::default()
    })?;
    let error = prepared
        .run(
            CargoOperation::Test,
            &CargoOptions {
                offline: true,
                ..CargoOptions::default()
            },
        )
        .expect_err("the intentionally failing user test must be reported");
    let diagnostic = error.to_string();
    assert!(
        diagnostic.contains("capture exceeded the freshness budget"),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains("failed_sensor_acceptance"),
        "{diagnostic}"
    );
    Ok(())
}

#[test]
fn update_rejects_target_selectors_before_preparation_mutation()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = project_fixture()?;
    let project = Project::discover(fixture.path())?;
    let manifest = fixture.path().join("Cargo.toml");
    let before_manifest = fs::read(&manifest)?;
    let error = project
        .update(&CargoOptions {
            offline: true,
            selection: CargoSelection {
                lib: true,
                ..CargoSelection::default()
            },
            ..CargoOptions::default()
        })
        .expect_err("cargo update must reject target selectors");
    assert!(matches!(
        error,
        Error::InvalidOptions { message } if message.contains("target selectors")
    ));
    assert_eq!(fs::read(&manifest)?, before_manifest);
    assert!(!fixture.path().join("Cargo.lock").exists());

    let error = project
        .update(&CargoOptions {
            offline: true,
            cargo_args: vec!["--bin".into(), "fixture-robot".into()],
            ..CargoOptions::default()
        })
        .expect_err("raw cargo update target selectors must be rejected");
    assert!(matches!(
        error,
        Error::InvalidOptions { message } if message.contains("target selectors")
    ));
    assert_eq!(fs::read(&manifest)?, before_manifest);
    assert!(!fixture.path().join("Cargo.lock").exists());

    let error = project
        .update(&CargoOptions {
            offline: true,
            selection: CargoSelection {
                excludes: vec!["fixture-robot".to_owned()],
                ..CargoSelection::default()
            },
            ..CargoOptions::default()
        })
        .expect_err("cargo update must reject unsupported exclude selectors");
    assert!(matches!(
        error,
        Error::InvalidOptions { message } if message.contains("exclude")
    ));
    assert_eq!(fs::read(&manifest)?, before_manifest);
    assert!(!fixture.path().join("Cargo.lock").exists());
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
    assert!(bundle.executable("supervisor").is_file());
    let BundleProvenance::V0 {
        supervisor: bundle_supervisor,
        ..
    } = bundle.provenance();
    assert_eq!(bundle_supervisor.instance, "supervisor");
    assert_eq!(bundle_supervisor.role, "supervisor");
    assert_eq!(bundle_supervisor.package, "phoxal-supervisor");
    assert_eq!(bundle_supervisor.version, "0.68.0");
    assert_eq!(
        bundle_supervisor.bytes,
        fs::metadata(bundle.executable("supervisor"))?.len()
    );
    assert_eq!(
        bundle_supervisor.sha256,
        sha256_file(&bundle.executable("supervisor"))?
    );
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
        &artifact_build_script(r#"{"type":"object"}"#, false),
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
      mount_site: sensor_mount
      driver:
        dependency: sensor-driver
        binary: sensor-driver
        config: {}
brain: {}
services: {}
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
fn missing_configured_component_driver_is_rejected_before_startup()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = project_fixture()?;
    write(
        &fixture.path().join("robot.yaml"),
        r#"schema: phoxal/robot/v0
robot:
  id: fixture-robot
  components:
    sensor:
      component: passive-sensor
      mount_site: sensor_mount
      driver:
        dependency: missing-driver
        binary: missing-driver
        config: {}
brain: {}
services:
  counter:
    implementation: counter-service
connections: {}
"#,
    )?;
    let project = Project::discover(fixture.path())?;
    let manifest = fixture.path().join("Cargo.toml");
    let before_manifest = fs::read(&manifest)?;
    let error = project
        .prepare(&CargoOptions::default())
        .expect_err("a configured driver must resolve from the component owner");
    assert!(matches!(
        error,
        Error::Source(SourceError::DependencyNotDeclared {
            role,
            instance,
            key,
        }) if role == crate::project::TargetRole::Driver
            && instance == "sensor"
            && key == "missing-driver"
    ));
    assert_eq!(fs::read(&manifest)?, before_manifest);
    assert!(
        !fixture.path().join("Cargo.lock").exists(),
        "failed driver selection must not publish a lockfile"
    );
    Ok(())
}

#[test]
fn selected_hardware_fixture_driver_is_resolved_without_simulation_assets()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = project_fixture()?;
    let driver_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/hardware/driver")
        .canonicalize()?;
    let driver_path = relative_path(&fixture.path().canonicalize()?, &driver_path);
    let root_manifest = fixture.path().join("Cargo.toml");
    let root_source = fs::read_to_string(&root_manifest)?.replace(
        "[dependencies]\n",
        &format!(
            "[dependencies]\nhardware-driver-fixture = {{ package = \"phoxal-hardware-driver-fixture\", path = \"{}\" }}\n",
            driver_path.display()
        ),
    );
    write(&root_manifest, &root_source)?;
    write(
        &fixture.path().join("robot.yaml"),
        r#"schema: phoxal/robot/v0
robot:
  id: fixture-robot
  components:
    sensor:
      component: hardware-driver-fixture
      mount_site: fixture_joint
      driver:
        binary: phoxal-hardware-driver-fixture
        config:
          device_id: fixture-0
brain: {}
services: {}
connections: {}
"#,
    )?;
    let project = Project::discover(fixture.path())?;
    let options = CargoOptions {
        offline: true,
        ..CargoOptions::default()
    };
    let prepared = project.prepare(&options)?;
    let driver = prepared.sources().components["sensor"]
        .driver
        .as_ref()
        .expect("selected component driver");
    assert_eq!(driver.package, "phoxal-hardware-driver-fixture");
    assert_eq!(driver.binary.target, "phoxal-hardware-driver-fixture");
    let RobotDocument::V0 { robot, .. } = prepared.document();
    assert!(robot.model.is_none());
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

    let BundleManifest::V0 {
        executables: bundle_executables,
        components: bundle_components,
        ..
    } = bundle.manifest();
    let BundleProvenance::V0 {
        cargo_lock_sha256: bundle_cargo_lock_sha256,
        cargo_workspace_manifest_sha256: bundle_cargo_workspace_manifest_sha256,
        supervisor: bundle_supervisor,
        source_tree: bundle_source_tree,
        toolchain: bundle_toolchain,
        model: bundle_model,
        model_closure: bundle_model_closure,
        ..
    } = bundle.provenance();
    assert_eq!(bundle_executables.len(), 2);
    assert_eq!(bundle_components.len(), 1);
    assert_eq!(bundle_components[0].mount_site, "sensor_mount");
    let ComponentDocument::V0 {
        model: component_model,
        capabilities: component_capabilities,
        ..
    } = &bundle_components[0].definition;
    assert_eq!(component_model.file, Path::new("model.xml"));
    assert_eq!(component_capabilities["sample"].target.id, "sensor_site");
    assert_eq!(
        bundle_executables
            .iter()
            .map(|executable| executable.instance.as_str())
            .collect::<Vec<_>>(),
        ["brain", "counter"]
    );
    assert!(bundle.executable("brain").is_file());
    assert!(bundle.executable("counter").is_file());
    assert!(bundle.executable("supervisor").is_file());
    assert!(output.join("manifest.json").is_file());
    assert!(output.join("provenance.json").is_file());
    assert!(bundle_cargo_lock_sha256.is_some());
    assert!(!bundle_cargo_workspace_manifest_sha256.is_empty());
    assert_eq!(bundle_supervisor.path, "bin/supervisor");
    assert_eq!(bundle_source_tree.path, "source");
    assert!(bundle.source_root().join("Cargo.lock").is_file());
    assert!(
        bundle_source_tree
            .files
            .iter()
            .any(|file| file.path == "Cargo.lock")
    );
    assert!(!bundle_toolchain.cargo.is_empty());
    assert!(!bundle_toolchain.rustc.is_empty());
    assert!(bundle_model.is_some());
    let model_closure = bundle_model_closure
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

#[test]
fn bundle_records_the_owning_workspace_manifest() -> Result<(), Box<dyn std::error::Error>> {
    let (workspace, robot) = nested_workspace_fixture()?;
    let workspace_manifest = workspace.path().join("Cargo.toml");
    let project = Project::discover(&robot)?;
    let options = CargoOptions {
        offline: true,
        ..CargoOptions::default()
    };
    let prepared = project.prepare(&options)?;
    let output = workspace.path().join("target/phoxal/nested-bundle");
    let bundle = prepared.build_bundle(&options, &output)?;

    let BundleProvenance::V0 {
        cargo_workspace_manifest_sha256: workspace_manifest_sha256,
        source_tree: bundle_source_tree,
        ..
    } = bundle.provenance();
    assert_eq!(
        workspace_manifest_sha256.clone(),
        sha256_file(&workspace_manifest)?
    );
    assert!(
        bundle_source_tree
            .files
            .iter()
            .any(|file| file.path == "Cargo.toml")
    );
    Ok(())
}

#[test]
fn bundle_rejects_a_workspace_manifest_changed_during_build()
-> Result<(), Box<dyn std::error::Error>> {
    let (workspace, robot) = nested_workspace_fixture()?;
    write(
        &robot.join("build.rs"),
        &nested_workspace_race_build_script(),
    )?;
    let project = Project::discover(&robot)?;
    let options = CargoOptions {
        offline: true,
        ..CargoOptions::default()
    };
    let prepared = project.prepare(&options)?;
    let output = workspace.path().join("target/phoxal/nested-race");
    let error = prepared
        .build_bundle(&options, &output)
        .expect_err("a workspace manifest race must not publish a bundle");
    assert!(
        matches!(error, Error::BundleSourceChanged { message } if message.contains("owning workspace Cargo.toml"))
    );
    assert!(!output.exists());
    Ok(())
}

#[test]
fn bundle_carries_a_relocatable_nested_external_path_closure()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = project_fixture()?;
    write(
        &fixture.path().join(".cargo/config.toml"),
        "[build]\nrustflags = []\n",
    )?;
    let parent = fixture
        .path()
        .parent()
        .ok_or("fixture has no temporary parent")?;
    let leaf = tempfile::Builder::new()
        .prefix("phoxal-external-leaf-")
        .tempdir_in(parent)?;
    let helper = tempfile::Builder::new()
        .prefix("phoxal-external-helper-")
        .tempdir_in(parent)?;
    let leaf_name = leaf
        .path()
        .file_name()
        .ok_or("leaf has no directory name")?
        .to_string_lossy();
    let helper_name = helper
        .path()
        .file_name()
        .ok_or("helper has no directory name")?
        .to_string_lossy();
    write(
        &leaf.path().join("Cargo.toml"),
        "[package]\nname = \"fixture-external-leaf\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[lib]\npath = \"src/lib.rs\"\n",
    )?;
    write(
        &leaf.path().join("src/lib.rs"),
        "pub const VALUE: u32 = 7;\n",
    )?;
    write(
        &helper.path().join("Cargo.toml"),
        &format!(
            "[package]\nname = \"fixture-external-helper\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[lib]\npath = \"src/lib.rs\"\n\n[dependencies]\nfixture-external-leaf = {{ path = \"../{leaf_name}\" }}\n"
        ),
    )?;
    write(
        &helper.path().join("src/lib.rs"),
        "pub fn value() -> u32 { fixture_external_leaf::VALUE }\n",
    )?;
    let root_manifest = fixture.path().join("Cargo.toml");
    let root = fs::read_to_string(&root_manifest)?.replace(
        "[dependencies]\n",
        &format!("[dependencies]\nfixture-external-helper = {{ path = \"../{helper_name}\" }}\n"),
    );
    write(&root_manifest, &root)?;
    write(
        &fixture.path().join("src/main.rs"),
        "include!(concat!(env!(\"OUT_DIR\"), \"/artifact.rs\"));\nfn main() { let _ = fixture_external_helper::value(); }\n",
    )?;

    let project = Project::discover(fixture.path())?;
    let options = CargoOptions {
        offline: true,
        ..CargoOptions::default()
    };
    let prepared = project.prepare(&options)?;
    let output = fixture.path().join("target/phoxal/fixture-robot/closure");
    let bundle = prepared.build_bundle(&options, &output)?;
    let source_root = bundle.source_root();
    assert!(source_root.join("Cargo.lock").is_file());
    assert!(source_root.join(".cargo/config.toml").is_file());
    assert_eq!(
        fs::read(source_root.join("Cargo.lock"))?,
        fs::read(prepared.cargo_lock())?
    );
    let source_manifest = fs::read_to_string(source_root.join("Cargo.toml"))?;
    assert!(!source_manifest.contains(&fixture.path().display().to_string()));
    assert!(!source_manifest.contains(&helper.path().display().to_string()));
    let external_root = source_root.join("_phoxal_path_dependencies");
    let external_count = fs::read_dir(&external_root)?.count();
    assert!(external_count >= 2);
    let BundleProvenance::V0 { sources, .. } = bundle.provenance();
    assert!(
        sources
            .iter()
            .any(|source| source.package == "fixture-external-helper")
    );
    assert!(
        sources
            .iter()
            .any(|source| source.package == "fixture-external-leaf")
    );

    let relocated = tempfile::tempdir()?;
    let relocated_root = relocated.path().join("source");
    copy_tree(&source_root, &relocated_root)?;
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let target = relocated.path().join("target");
    let output = Command::new(cargo)
        .current_dir(&relocated_root)
        .args([
            "--config",
            "registries.phoxal.index=\"sparse+https://phoxal.github.io/registry/\"",
            "check",
            "--offline",
            "--locked",
            "--manifest-path",
            &relocated_root.join("Cargo.toml").display().to_string(),
            "--target-dir",
            &target.display().to_string(),
        ])
        .output()?;
    assert!(
        output.status.success(),
        "relocated source closure failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[test]
fn bundle_records_the_full_pinned_git_revision_and_subdirectory()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = project_fixture()?;
    let parent = fixture
        .path()
        .parent()
        .ok_or("fixture has no temporary parent")?;
    let repository = tempfile::Builder::new()
        .prefix("phoxal-git-service-")
        .tempdir_in(parent)?;
    let package_root = repository.path().join("packages/git-service");
    write(
        &package_root.join("Cargo.toml"),
        r#"[package]
name = "fixture-git-service"
version = "0.1.0"
edition = "2024"
build = "build.rs"

[lib]
path = "src/lib.rs"

[[bin]]
name = "fixture-git-service"
path = "src/main.rs"
"#,
    )?;
    write(
        &package_root.join("build.rs"),
        &artifact_build_script(r#"{"type":"object"}"#, true),
    )?;
    write(&package_root.join("src/lib.rs"), "pub struct GitService;\n")?;
    write(
        &package_root.join("src/main.rs"),
        "include!(concat!(env!(\"OUT_DIR\"), \"/artifact.rs\"));\nfn main() {}\n",
    )?;
    let init = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(repository.path())
        .output()?;
    assert!(init.status.success());
    for arguments in [
        vec!["config", "user.name", "Phoxal Test"],
        vec!["config", "user.email", "phoxal@example.invalid"],
        vec!["add", "."],
        vec!["commit", "--quiet", "-m", "fixture"],
    ] {
        let output = Command::new("git")
            .args(arguments)
            .current_dir(repository.path())
            .output()?;
        assert!(
            output.status.success(),
            "git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let revision = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repository.path())
            .output()?
            .stdout,
    )?
    .trim()
    .to_owned();
    let repository_url = format!("file://{}", repository.path().display());
    let root_manifest = fixture.path().join("Cargo.toml");
    let root = fs::read_to_string(&root_manifest)?.replace(
        "[dependencies]\n",
        &format!(
            "[dependencies]\ngit-service = {{ package = \"fixture-git-service\", git = \"{repository_url}\", rev = \"{revision}\" }}\n"
        ),
    );
    write(&root_manifest, &root)?;
    write(
        &fixture.path().join("robot.yaml"),
        &fs::read_to_string(fixture.path().join("robot.yaml"))?.replace(
            "implementation: counter-service",
            "implementation: git-service",
        ),
    )?;

    let project = Project::discover(fixture.path())?;
    let options = CargoOptions::default();
    let prepared = project.prepare(&options)?;
    let output = fixture
        .path()
        .join("target/phoxal/fixture-robot/git-bundle");
    let bundle = prepared.build_bundle(&options, &output)?;
    let BundleProvenance::V0 { sources, .. } = bundle.provenance();
    let source = sources
        .iter()
        .find(|source| source.package == "fixture-git-service")
        .ok_or("Git source was not retained in bundle provenance")?;
    assert_eq!(source.kind, crate::project::BundleSourceKind::Git);
    assert_eq!(source.registry_checksum, None);
    let git = source.git.as_ref().ok_or("Git provenance is missing")?;
    assert_eq!(git.repository, "local-git");
    assert_eq!(git.revision, revision);
    assert_eq!(git.subdirectory, "packages/git-service");
    assert!(!source.files.iter().any(|file| file.path == ".cargo-ok"));
    assert!(source.identity.starts_with(&source.package_id));
    Ok(())
}

#[test]
fn bundle_records_registry_checksums_from_the_root_lock() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = project_fixture()?;
    let root_manifest = fixture.path().join("Cargo.toml");
    let root = fs::read_to_string(&root_manifest)?.replace(
        "[dependencies]\n",
        "[dependencies]\nserde = { version = \"1.0\", features = [\"derive\"] }\n",
    );
    write(&root_manifest, &root)?;
    let project = Project::discover(fixture.path())?;
    let options = CargoOptions {
        offline: true,
        ..CargoOptions::default()
    };
    let prepared = project.prepare(&options)?;
    let output = fixture
        .path()
        .join("target/phoxal/fixture-robot/registry-bundle");
    let bundle = prepared.build_bundle(&options, &output)?;
    let BundleProvenance::V0 { sources, .. } = bundle.provenance();
    let registry_sources = sources
        .iter()
        .filter(|source| source.kind == crate::project::BundleSourceKind::Registry)
        .collect::<Vec<_>>();
    assert!(!registry_sources.is_empty());
    assert!(
        registry_sources
            .iter()
            .all(|source| source.registry_checksum.is_some())
    );
    Ok(())
}
