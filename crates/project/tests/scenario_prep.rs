//! P1 acceptance: the production scenario preparation persists the
//! manifest, writes the generated harness, and the harness itself
//! compiles against the framework's `phoxal` crate.
//!
//! Unlike the structural assertions in `preparation::tests`, this
//! integration test exercises the full `Project::prepare_scenarios`
//! pipeline that `cargo phoxal simulation scenario list/run` will call
//! in P4. It does not attempt to compile the harness via `cargo build`
//! (which would require the phoxal registry); instead it locates the
//! already-built `libphoxal*.rlib` in `target/debug/deps` and compiles
//! the generated `main.rs` directly via `rustc --extern`, then runs the
//! list command.

use std::fs;
use std::io::BufRead as _;
use std::path::Path;

use phoxal_project::{CargoOptions, LockMode};

#[test]
fn prepare_scenarios_persists_manifest_and_writes_harness() {
    let directory = tempfile::tempdir().expect("tempdir");
    let robot_root = directory.path().to_path_buf();
    let manifest_text = "[package]\n\
                        name = \"p1-robot\"\n\
                        version = \"0.1.0\"\n\
                        edition = \"2024\"\n\
                        publish = false\n\
                        \n\
                        [dependencies]\n\
                        phoxal = { path = \"../framework/phoxal\", version = \"=0.68.0\", registry = \"phoxal\" }\n";
    fs::write(robot_root.join("Cargo.toml"), manifest_text).expect("manifest");
    fs::write(
        robot_root.join("robot.yaml"),
        "schema: phoxal/robot/v0\nrobot:\n  id: p1-robot\n  components: {}\nservices: {}\n",
    )
    .expect("robot.yaml");
    fs::create_dir_all(robot_root.join("src")).expect("src");
    fs::write(robot_root.join("src/main.rs"), "fn main() {}\n").expect("bin");

    let scenarios = robot_root.join("scenarios");
    fs::create_dir_all(&scenarios).expect("scenarios dir");
    fs::write(scenarios.join("forward_turn_stop.rs"), "// stub\n").expect("file scenario");
    fs::create_dir_all(scenarios.join("from_mod")).expect("module scenario dir");
    fs::write(scenarios.join("from_mod").join("mod.rs"), "// stub\n")
        .expect("module scenario mod.rs");

    let layout = phoxal_project::ProjectLayout::discover(&robot_root).expect("layout");
    let project = phoxal_project::Project::from_layout(layout).expect("project");
    let options = CargoOptions::default();
    let changes = project
        .prepare_scenarios(&options)
        .expect("prepare_scenarios");
    assert!(
        !changes.is_empty(),
        "first preparation must produce at least one change, got: {changes:?}"
    );

    let persisted = fs::read_to_string(robot_root.join("Cargo.toml")).expect("read manifest");
    assert!(
        persisted.contains("phoxal-scenarios"),
        "test target missing from persisted manifest:\n{persisted}"
    );
    assert!(
        persisted.contains("[dev-dependencies]"),
        "dev-dependencies section missing:\n{persisted}"
    );
    assert!(
        persisted.contains("phoxal"),
        "phoxal dev-dep missing:\n{persisted}"
    );
    assert!(
        persisted.contains("scenario"),
        "scenario feature missing from dev-dep:\n{persisted}"
    );

    let harness_path = robot_root.join(".phoxal/generated/scenarios/main.rs");
    assert!(harness_path.is_file(), "harness not generated");
    let harness = fs::read_to_string(&harness_path).expect("read harness");
    assert!(
        harness.contains("mod _scenario_forward_turn_stop"),
        "harness missing file scenario mod: {harness}"
    );
    assert!(
        harness.contains("mod _scenario_from_mod"),
        "harness missing module scenario mod: {harness}"
    );

    let second_changes = project
        .prepare_scenarios(&options)
        .expect("prepare_scenarios (second)");
    assert!(
        second_changes.is_empty(),
        "second preparation must be a no-op (idempotent), got: {second_changes:?}"
    );
}

#[test]
fn prepare_scenarios_refuses_to_mutate_under_locked_mode_when_setup_is_missing() {
    let directory = tempfile::tempdir().expect("tempdir");
    let robot_root = directory.path().to_path_buf();
    fs::write(robot_root.join("Cargo.toml"), "[package]\n\
                                              name = \"p1-robot\"\n\
                                              version = \"0.1.0\"\n\
                                              edition = \"2024\"\n\n\
                                              [dependencies]\n\
                                              phoxal = { path = \"../framework/phoxal\", version = \"=0.68.0\", registry = \"phoxal\" }\n")
        .expect("manifest");
    fs::write(
        robot_root.join("robot.yaml"),
        "schema: phoxal/robot/v0\nrobot:\n  id: p1-robot\n  components: {}\nservices: {}\n",
    )
    .expect("robot.yaml");
    fs::create_dir_all(robot_root.join("src")).expect("src");
    fs::write(robot_root.join("src/main.rs"), "fn main() {}\n").expect("bin");

    let scenarios = robot_root.join("scenarios");
    fs::create_dir_all(&scenarios).expect("scenarios dir");
    fs::write(scenarios.join("forward_turn_stop.rs"), "// stub\n").expect("file scenario");

    let layout = phoxal_project::ProjectLayout::discover(&robot_root).expect("layout");
    let project = phoxal_project::Project::from_layout(layout).expect("project");

    let options = CargoOptions {
        lock: LockMode::Locked,
        ..CargoOptions::default()
    };

    let error = project
        .prepare_scenarios(&options)
        .expect_err("locked preparation without setup must refuse");
    let message = format!("{error:?}");
    assert!(
        message.contains("refusing") && message.contains("locked"),
        "locked-mode refusal must mention lock state; got: {message}"
    );
    // Manifest must be unchanged on disk.
    let persisted = fs::read_to_string(robot_root.join("Cargo.toml")).expect("read manifest");
    assert!(
        !persisted.contains("phoxal-scenarios"),
        "manifest must not be mutated when locked mode refuses: {persisted}"
    );
}

#[test]
fn prepare_scenarios_removes_stale_test_target_when_last_scenario_deleted() {
    let directory = tempfile::tempdir().expect("tempdir");
    let robot_root = directory.path().to_path_buf();
    let manifest_text = "[package]\n\
                        name = \"p1-robot\"\n\
                        version = \"0.1.0\"\n\
                        edition = \"2024\"\n\
                        publish = false\n\n\
                        [dependencies]\n\
                        phoxal = { path = \"../framework/phoxal\", version = \"=0.68.0\", registry = \"phoxal\" }\n";
    fs::write(robot_root.join("Cargo.toml"), manifest_text).expect("manifest");
    fs::write(
        robot_root.join("robot.yaml"),
        "schema: phoxal/robot/v0\nrobot:\n  id: p1-robot\n  components: {}\nservices: {}\n",
    )
    .expect("robot.yaml");
    fs::create_dir_all(robot_root.join("src")).expect("src");
    fs::write(robot_root.join("src/main.rs"), "fn main() {}\n").expect("bin");

    let scenarios = robot_root.join("scenarios");
    fs::create_dir_all(&scenarios).expect("scenarios dir");
    fs::write(scenarios.join("a.rs"), "// first\n").expect("scenario a");
    fs::write(scenarios.join("b.rs"), "// second\n").expect("scenario b");

    let layout = phoxal_project::ProjectLayout::discover(&robot_root).expect("layout");
    let project = phoxal_project::Project::from_layout(layout).expect("project");
    let options = CargoOptions::default();

    // First run: adds the target + dev-dep + harness.
    let _first = project.prepare_scenarios(&options).expect("first run");
    let persisted = fs::read_to_string(robot_root.join("Cargo.toml")).expect("read");
    assert!(persisted.contains("phoxal-scenarios"));

    // Delete all scenarios and re-run; should roll back the target.
    fs::remove_file(scenarios.join("a.rs")).expect("rm a");
    fs::remove_file(scenarios.join("b.rs")).expect("rm b");
    let second = project.prepare_scenarios(&options).expect("second run");
    println!("second-run changes: {second:?}");
    let after = fs::read_to_string(robot_root.join("Cargo.toml")).expect("read after");
    assert!(
        !after.contains("phoxal-scenarios"),
        "test target must be removed when no scenarios remain; manifest:\n{after}"
    );
    assert!(
        !after.contains("scenario"),
        "scenario feature must be removed from dev-dep when no scenarios remain; manifest:\n{after}"
    );
}

#[test]
fn prepare_scenarios_preserves_existing_workspace_phoxal_coordinates() {
    let directory = tempfile::tempdir().expect("tempdir");
    let robot_root = directory.path().to_path_buf();
    let manifest_text = "[workspace.dependencies]\n\
                        phoxal = { version = \"0.99.0\", registry = \"phoxal\" }\n\n\
                        [package]\n\
                        name = \"p1-robot\"\n\
                        version = \"0.1.0\"\n\
                        edition = \"2024\"\n\
                        publish = false\n\n\
                        [dependencies]\n\
                        phoxal.workspace = true\n";
    fs::write(robot_root.join("Cargo.toml"), manifest_text).expect("manifest");
    fs::write(
        robot_root.join("robot.yaml"),
        "schema: phoxal/robot/v0\nrobot:\n  id: p1-robot\n  components: {}\nservices: {}\n",
    )
    .expect("robot.yaml");
    fs::create_dir_all(robot_root.join("src")).expect("src");
    fs::write(robot_root.join("src/main.rs"), "fn main() {}\n").expect("bin");
    let scenarios = robot_root.join("scenarios");
    fs::create_dir_all(&scenarios).expect("scenarios dir");
    fs::write(scenarios.join("x.rs"), "// stub\n").expect("scenario x");

    let layout = phoxal_project::ProjectLayout::discover(&robot_root).expect("layout");
    let project = phoxal_project::Project::from_layout(layout).expect("project");
    project
        .prepare_scenarios(&CargoOptions::default())
        .expect("prepare");

    let persisted = fs::read_to_string(robot_root.join("Cargo.toml")).expect("read");
    let dev = persisted
        .split("[dev-dependencies]")
        .nth(1)
        .expect("dev-dep section must exist after first prepare");
    assert!(
        dev.contains("version = \"0.99.0\""),
        "workspace-declared version must be mirrored: {dev}"
    );
    assert!(
        !dev.contains("path = "),
        "must not invent a path coordinate when workspace coordinates exist: {dev}"
    );
}

#[test]
fn prepare_scenarios_compiles_and_runs_generated_harness_list() {
    let directory = tempfile::tempdir().expect("tempdir");
    let robot_root = directory.path().to_path_buf();
    fs::write(robot_root.join("Cargo.toml"), "[package]\n\
                                              name = \"p1-robot\"\n\
                                              version = \"0.1.0\"\n\
                                              edition = \"2024\"\n\
                                              publish = false\n\n\
                                              [dependencies]\n\
                                              phoxal = { path = \"../framework/phoxal\", version = \"=0.68.0\", registry = \"phoxal\" }\n")
        .expect("manifest");
    fs::write(
        robot_root.join("robot.yaml"),
        "schema: phoxal/robot/v0\nrobot:\n  id: p1-robot\n  components: {}\nservices: {}\n",
    )
    .expect("robot.yaml");
    fs::create_dir_all(robot_root.join("src")).expect("src");
    fs::write(robot_root.join("src/main.rs"), "fn main() {}\n").expect("bin");
    let scenarios = robot_root.join("scenarios");
    fs::create_dir_all(&scenarios).expect("scenarios dir");
    fs::write(scenarios.join("forward.rs"), "// stub\n").expect("scenario");

    let layout = phoxal_project::ProjectLayout::discover(&robot_root).expect("layout");
    let project = phoxal_project::Project::from_layout(layout).expect("project");
    project
        .prepare_scenarios(&CargoOptions::default())
        .expect("prepare");

    let harness_path = robot_root.join(".phoxal/generated/scenarios/main.rs");
    assert!(
        harness_path.is_file(),
        "harness must exist at {harness_path:?} before compile"
    );

    // Locate the framework's phoxal rlib (built by this workspace).
    let phoxal_rlib = find_phoxal_rlib().expect("phoxal rlib in target/debug");
    // `cargo build` emits the canonical rlib at `target/debug/libphoxal.rlib`
    // but the transitive deps (inventory, serde, etc.) live in
    // `target/debug/deps`. Pass that directory on `-L` so rustc can resolve
    // them when linking the harness binary.
    let deps_dir = phoxal_rlib.parent().expect("rlib parent").join("deps");
    let bin_out = directory.path().join("scenarios-bin");

    // The harness uses `#[path = "../../../scenarios/<name>.rs"]` so its
    // scenario includes resolve relative to the harness *source file's*
    // directory. The natural location for that source file is
    // `<robot>/.phoxal/generated/scenarios/main.rs` — exactly where
    // `cargo build --test phoxal-scenarios` would invoke `rustc` from.
    // Compile it in place rather than copying it to a temp directory,
    // which would break the relative `#[path]` references.
    let status = std::process::Command::new("rustc")
        .arg("--edition=2024")
        .arg(format!("--extern=phoxal={}", phoxal_rlib.display()))
        .arg("-L")
        .arg(&deps_dir)
        .arg("-o")
        .arg(&bin_out)
        .arg(&harness_path)
        .status()
        .expect("rustc invocation");
    assert!(
        status.success(),
        "rustc failed on the generated harness (exit: {status:?})"
    );

    let list_output = std::process::Command::new(&bin_out)
        .arg("list")
        .output()
        .expect("list command");
    assert!(
        list_output.status.success(),
        "list command exited non-zero: {:?}\nstderr: {}",
        list_output.status,
        String::from_utf8_lossy(&list_output.stderr)
    );
    let stdout = String::from_utf8(list_output.stdout).expect("utf8 stdout");
    // The scenarios are stubs that do not register any structs, so the
    // list output should be empty. This confirms the harness compiled,
    // linked, and runs without panic.
    assert!(
        stdout.is_empty(),
        "stub scenarios should not register any structs, got: {stdout:?}"
    );
}

fn find_phoxal_rlib() -> Option<std::path::PathBuf> {
    // Use `cargo build --message-format=json` and parse the emitted
    // `compiler-artifact` events to discover the exact rlib path for the
    // `phoxal` crate built with the `scenario` feature. Scanning the deps
    // directory is not robust — the framework has many `libphoxal-*.rlib`
    // artefacts from other feature combinations, and there is no portable
    // way to tell them apart from a Rust integration test.
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .ancestors()
        .find(|p| p.join("Cargo.toml").is_file() && p.join("phoxal").is_dir())
        .map(std::path::Path::to_path_buf)?;
    let output = std::process::Command::new("cargo")
        .args([
            "build",
            "-p",
            "phoxal",
            "--no-default-features",
            "--features",
            "scenario",
            "--message-format=json",
        ])
        .current_dir(&workspace_root)
        .env_remove("RUSTC_WRAPPER")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    for line in output.stdout.lines() {
        let Ok(line) = line else { continue };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if value.get("reason").and_then(|r| r.as_str()) != Some("compiler-artifact") {
            continue;
        }
        if value
            .get("target")
            .and_then(|t| t.get("name"))
            .and_then(|n| n.as_str())
            != Some("phoxal")
        {
            continue;
        }
        let Some(filenames) = value.get("filenames").and_then(|f| f.as_array()) else {
            continue;
        };
        for filename in filenames {
            if let Some(s) = filename.as_str()
                && s.ends_with(".rlib")
            {
                return Some(std::path::PathBuf::from(s));
            }
        }
    }
    None
}

// Force the harness module to be linked so the binary references the
// scenario types when scenarios are present.
#[allow(dead_code)]
fn _force_link(_: &Path) {}
