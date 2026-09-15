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
                                              phoxal-supervisor = { path = \"../framework/supervisor\", version = \"=0.68.0\", registry = \"phoxal\" }\n")
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
    // Now repeat the test with the supervisor dep present so the scenario
    // refusal path is reached instead of the supervisor-init diagnostic.
    fs::write(robot_root.join("Cargo.toml"), "[package]\n\
                                              name = \"p1-robot\"\n\
                                              version = \"0.1.0\"\n\
                                              edition = \"2024\"\n\n\
                                              [dependencies]\n\
                                              phoxal-supervisor = { path = \"../framework/supervisor\", version = \"=0.68.0\", registry = \"phoxal\" }\n")
        .expect("manifest with supervisor");
    let layout2 = phoxal_project::ProjectLayout::discover(&robot_root).expect("layout2");
    let project2 = phoxal_project::Project::from_layout(layout2).expect("project2");
    let error2 = project2
        .prepare_scenarios(&options)
        .expect_err("locked preparation without scenario setup must refuse");
    let message2 = format!("{error2:?}");
    assert!(
        message2.contains("refusing") && message2.contains("locked"),
        "locked-mode refusal must mention lock state; got: {message2}"
    );
    // Manifest must be unchanged on disk.
    let persisted = fs::read_to_string(robot_root.join("Cargo.toml")).expect("read manifest");
    assert!(
        !persisted.contains("phoxal-scenarios"),
        "manifest must not be mutated when locked mode refuses: {persisted}"
    );
}

#[test]
fn prepare_scenarios_retains_persistent_setup_when_last_scenario_removed() {
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

    // Delete all scenarios and re-run. Per the plan, the persistent setup
    // (managed `[[test]]` target + `scenario` dev-dep feature) must remain
    // because the user might add scenarios back later. Only the disposable
    // harness is regenerated to an empty body so the binary compiles an
    // empty registry.
    fs::remove_file(scenarios.join("a.rs")).expect("rm a");
    fs::remove_file(scenarios.join("b.rs")).expect("rm b");
    let second = project.prepare_scenarios(&options).expect("second run");
    assert!(
        second.iter().any(|change| matches!(
            change,
            phoxal_project::PreparationChange::HarnessWritten { .. }
        )),
        "harness must be regenerated to the empty form when no scenarios remain: {second:?}"
    );
    let after = fs::read_to_string(robot_root.join("Cargo.toml")).expect("read after");
    assert!(
        after.contains("phoxal-scenarios"),
        "managed `[[test]]` target must be retained when no scenarios remain; manifest:\n{after}"
    );
    assert!(
        after.contains("scenario"),
        "managed `scenario` dev-dep feature must be retained when no scenarios remain; manifest:\n{after}"
    );
    let harness = fs::read_to_string(robot_root.join(".phoxal/generated/scenarios/main.rs"))
        .expect("harness");
    assert!(
        !harness.contains("mod _scenario"),
        "harness must not reference any scenario mod when none remain:\n{harness}"
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
    // Real Cargo workflow: a temp robot package with absolute path-based
    // dependency on the framework's `phoxal` crate, two genuinely
    // annotated scenarios in `scenarios/first.rs` and
    // `scenarios/second/mod.rs`, then the actual `cargo test
    // --test phoxal-scenarios --no-run --message-format=json` invocation.
    // The executable is recovered from the `compiler-artifact` event and
    // executed with `list`; the canonical names reported by the harness
    // must match exactly.
    let directory = tempfile::tempdir().expect("tempdir");
    let robot_root = directory.path().to_path_buf();
    let phoxal_dir = framework_phoxal_dir();
    let phoxal_path = phoxal_dir
        .canonicalize()
        .expect("canonicalize phoxal dir")
        .to_string_lossy()
        .replace('\\', "/");
    let supervisor_path = framework_supervisor_dir()
        .canonicalize()
        .expect("canonicalize supervisor dir")
        .to_string_lossy()
        .replace('\\', "/");
    fs::write(
        robot_root.join("Cargo.toml"),
        format!(
            "[package]\n\
             name = \"p1-robot\"\n\
             version = \"0.1.0\"\n\
             edition = \"2024\"\n\
             rust-version = \"1.88\"\n\
             publish = false\n\
             \n\
             [dependencies]\n\
             phoxal-supervisor = {{ path = {supervisor_path:?} }}\n\
             phoxal = {{ path = {phoxal_path:?}, features = [\"scenario\"] }}\n",
        ),
    )
    .expect("manifest");
    fs::write(
        robot_root.join("robot.yaml"),
        "schema: phoxal/robot/v0\nrobot:\n  id: p1-robot\n  components: {}\nservices: {}\n",
    )
    .expect("robot.yaml");
    write_phoxal_local_registry_config(&robot_root);
    fs::create_dir_all(robot_root.join("src")).expect("src");
    fs::write(robot_root.join("src/main.rs"), "fn main() {}\n").expect("bin");
    let scenarios = robot_root.join("scenarios");
    fs::create_dir_all(&scenarios).expect("scenarios dir");
    fs::write(
        scenarios.join("first.rs"),
        "use phoxal::scenario::{Scenario, ScenarioPlan};\n\
         use std::path::PathBuf;\n\
         use std::time::Duration;\n\
         \n\
         #[derive(Default)]\n\
         pub struct First;\n\
         \n\
         #[phoxal::scenario]\n\
         impl Scenario for First {\n\
             fn plan(&self) -> phoxal::Result<ScenarioPlan> {\n\
                 Ok(ScenarioPlan::new(PathBuf::from(\"first.scene\"), Duration::from_secs(1)))\n\
             }\n\
             fn verify(&self, _run: &phoxal::scenario::ScenarioRun) -> phoxal::Result<()> {\n\
                 Ok(())\n\
             }\n\
         }\n",
    )
    .expect("first scenario");
    fs::create_dir_all(scenarios.join("second")).expect("second scenario dir");
    fs::write(
        scenarios.join("second").join("mod.rs"),
        "use phoxal::scenario::{Scenario, ScenarioPlan};\n\
         use std::path::PathBuf;\n\
         use std::time::Duration;\n\
         \n\
         #[derive(Default)]\n\
         pub struct Second;\n\
         \n\
         #[phoxal::scenario]\n\
         impl Scenario for Second {\n\
             fn plan(&self) -> phoxal::Result<ScenarioPlan> {\n\
                 Ok(ScenarioPlan::new(PathBuf::from(\"second.scene\"), Duration::from_secs(1)))\n\
             }\n\
             fn verify(&self, _run: &phoxal::scenario::ScenarioRun) -> phoxal::Result<()> {\n\
                 Ok(())\n\
             }\n\
         }\n",
    )
    .expect("second scenario mod.rs");

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

    // Build the `phoxal-scenarios` test target through ordinary cargo.
    // The `--message-format=json` stream contains one `compiler-artifact`
    // event per produced binary; we filter to the one whose `target.name`
    // matches `phoxal-scenarios` and `target.kind` contains `test`.
    let output = std::process::Command::new("cargo")
        .args([
            "test",
            "--test",
            "phoxal-scenarios",
            "--no-run",
            "--offline",
            "--message-format=json",
        ])
        .current_dir(&robot_root)
        .env_remove("RUSTC_WRAPPER")
        .output()
        .expect("cargo test invocation");
    assert!(
        output.status.success(),
        "cargo test --test phoxal-scenarios --no-run failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let executable = match parse_test_executable(&output.stdout, "phoxal-scenarios") {
        Some(path) => path,
        None => {
            // Surface cargo's JSON output when the parser fails so a
            // future cargo format change is diagnosed quickly.
            let stdout = String::from_utf8_lossy(&output.stdout);
            panic!("test executable path in cargo JSON output; cargo stdout was:\n{stdout}");
        }
    };
    assert!(
        executable.is_file(),
        "cargo reported executable {executable:?} but the file does not exist"
    );

    // Execute the freshly-built harness with `list` and assert the
    // exact canonical names (`scenarios/<StructIdent>`) appear in
    // alphabetical order. The struct identity — not the filename — is
    // what the registry reports, by plan.
    let list_output = std::process::Command::new(&executable)
        .arg("list")
        .output()
        .expect("list command");
    assert!(
        list_output.status.success(),
        "list command exited non-zero: {:?}\nstderr: {}",
        list_output.status,
        String::from_utf8_lossy(&list_output.stderr),
    );
    let stdout = String::from_utf8(list_output.stdout).expect("utf8 stdout");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        vec![
            "scenarios/First\tphoxal_scenarios::_scenario_first",
            "scenarios/Second\tphoxal_scenarios::_scenario_second",
        ],
        "list output must report the two canonical struct identities in alphabetical order:\n{stdout}"
    );
}

/// Repeated preparation with no changes must produce byte-identical
/// `Cargo.toml` and lockfile outputs. This protects against
/// accumulator-style mutations that would dirty a repo's diff.
#[test]
fn prepare_scenarios_byte_identical_on_unchanged_repeat() {
    let directory = tempfile::tempdir().expect("tempdir");
    let robot_root = directory.path().to_path_buf();
    let manifest_text = "[package]\n\
                        name = \"p1-robot\"\n\
                        version = \"0.1.0\"\n\
                        edition = \"2024\"\n\
                        publish = false\n\n\
                        [dependencies]\n\
                        phoxal = { path = \"../framework/phoxal\", version = \"=0.68.0\", registry = \"phoxal\" }";
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
    fs::write(scenarios.join("only.rs"), "// only scenario\n").expect("only");

    let layout = phoxal_project::ProjectLayout::discover(&robot_root).expect("layout");
    let project = phoxal_project::Project::from_layout(layout).expect("project");
    let first = project
        .prepare_scenarios(&CargoOptions::default())
        .expect("first");
    let first_cargo = fs::read_to_string(robot_root.join("Cargo.toml")).expect("read first");
    let first_lock_path = robot_root.join("Cargo.lock");
    let first_lock = fs::read_to_string(&first_lock_path).ok();

    let second = project
        .prepare_scenarios(&CargoOptions::default())
        .expect("second");
    assert!(
        second.is_empty(),
        "second preparation must report no changes on a no-op input: {second:?}"
    );
    let second_cargo = fs::read_to_string(robot_root.join("Cargo.toml")).expect("read second");
    let second_lock = fs::read_to_string(&first_lock_path).ok();
    assert_eq!(
        first_cargo, second_cargo,
        "Cargo.toml must be byte-identical across repeated preparation"
    );
    assert_eq!(
        first_lock, second_lock,
        "Cargo.lock must be byte-identical across repeated preparation"
    );
    let _ = first;
}

/// Two scenarios that share a struct name (one in a file, one in a
/// module under a different filename) must produce duplicate
/// detection when listing through the harness. The struct identity is
/// the public name; both files declare a struct called `SameName` and
/// the registry must reject this before any execution.
#[test]
fn prepare_scenarios_duplicate_struct_identities_rejected() {
    let directory = tempfile::tempdir().expect("tempdir");
    let robot_root = directory.path().to_path_buf();
    let phoxal_path = framework_phoxal_dir()
        .canonicalize()
        .expect("phoxal canonicalize")
        .to_string_lossy()
        .replace('\\', "/");
    let supervisor_path = framework_supervisor_dir()
        .canonicalize()
        .expect("supervisor canonicalize")
        .to_string_lossy()
        .replace('\\', "/");
    fs::write(
        robot_root.join("Cargo.toml"),
        format!(
            "[package]\nname = \"p1-robot\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
             publish = false\n\n[dependencies]\n\
             phoxal-supervisor = {{ path = {supervisor_path:?} }}\n\
             phoxal = {{ path = {phoxal_path:?}, features = [\"scenario\"] }}\n"
        ),
    )
    .expect("manifest");
    fs::write(
        robot_root.join("robot.yaml"),
        "schema: phoxal/robot/v0\nrobot:\n  id: p1-robot\n  components: {}\nservices: {}\n",
    )
    .expect("robot.yaml");
    write_phoxal_local_registry_config(&robot_root);
    fs::create_dir_all(robot_root.join("src")).expect("src");
    fs::write(robot_root.join("src/main.rs"), "fn main() {}\n").expect("bin");
    let scenarios = robot_root.join("scenarios");
    fs::create_dir_all(&scenarios).expect("scenarios dir");
    let scene_decl = "use phoxal::scenario::{Scenario, ScenarioPlan};\n\
                      use std::path::PathBuf;\n\
                      use std::time::Duration;\n\
                      #[derive(Default)]\n\
                      pub struct SameName;\n\
                      #[phoxal::scenario]\n\
                      impl Scenario for SameName {\n\
                          fn plan(&self) -> phoxal::Result<ScenarioPlan> {\n\
                              Ok(ScenarioPlan::new(PathBuf::from(\"same.scene\"), Duration::from_secs(1)))\n\
                          }\n\
                          fn verify(&self, _run: &phoxal::scenario::ScenarioRun) -> phoxal::Result<()> { Ok(()) }\n\
                      }\n";
    fs::write(scenarios.join("first_layout.rs"), scene_decl).expect("first file scenario");
    fs::create_dir_all(scenarios.join("second_layout")).expect("second scenario dir");
    fs::write(scenarios.join("second_layout").join("mod.rs"), scene_decl).expect("module scenario");

    let layout = phoxal_project::ProjectLayout::discover(&robot_root).expect("layout");
    let project = phoxal_project::Project::from_layout(layout).expect("project");
    project
        .prepare_scenarios(&CargoOptions::default())
        .expect("prepare");

    let cargo_output = std::process::Command::new("cargo")
        .args([
            "build",
            "--test",
            "phoxal-scenarios",
            "--offline",
            "--message-format=json",
        ])
        .current_dir(&robot_root)
        .env_remove("RUSTC_WRAPPER")
        .output()
        .expect("cargo build");
    assert!(
        cargo_output.status.success(),
        "cargo build must succeed; the duplicate is a runtime contract, not a compile error.\n--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&cargo_output.stdout),
        String::from_utf8_lossy(&cargo_output.stderr),
    );
    let executable =
        parse_test_executable(&cargo_output.stdout, "phoxal-scenarios").expect("test executable");
    let list_output = std::process::Command::new(&executable)
        .arg("list")
        .output()
        .expect("list");
    let stderr = String::from_utf8_lossy(&list_output.stderr);
    let stdout = String::from_utf8_lossy(&list_output.stdout);
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        combined.contains("duplicate") || combined.contains("Duplicate"),
        "duplicate struct identity must be reported before execution; got:\n{combined}"
    );
}

#[allow(dead_code, clippy::expect_used, clippy::unwrap_used)]
fn write_phoxal_local_registry_config(robot_root: &std::path::Path) {
    // The framework's `phoxal` crate publishes into the local `phoxal`
    // registry; point Cargo at that local registry so the path
    // dependency resolves under `--offline`.
    fs::create_dir_all(robot_root.join(".cargo")).expect("cargo dir");
    let registry_path = framework_registry_dir()
        .canonicalize()
        .expect("canonicalize registry dir")
        .to_string_lossy()
        .replace('\\', "/");
    fs::write(
        robot_root.join(".cargo/config.toml"),
        format!("[registries.phoxal]\nindex = \"sparse+file://{registry_path}/\"\n"),
    )
    .expect("cargo config");
}

#[allow(dead_code, clippy::expect_used, clippy::unwrap_used)]
fn framework_phoxal_dir() -> std::path::PathBuf {
    // The framework's `phoxal` crate is a sibling of the
    // `phoxal-project` test crate, so we can locate it directly from the
    // manifest dir resolved at compile time.
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .ancestors()
        .find_map(|ancestor| {
            let candidate = ancestor.join("phoxal");
            candidate.join("Cargo.toml").is_file().then_some(candidate)
        })
        .expect("phoxal crate must be a sibling of crates/project")
}

#[allow(dead_code, clippy::expect_used, clippy::unwrap_used)]
fn framework_supervisor_dir() -> std::path::PathBuf {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .ancestors()
        .find_map(|ancestor| {
            let candidate = ancestor.join("supervisor");
            candidate.join("Cargo.toml").is_file().then_some(candidate)
        })
        .expect("supervisor crate must be a sibling of crates/project")
}

#[allow(dead_code, clippy::expect_used, clippy::unwrap_used)]
fn framework_registry_dir() -> std::path::PathBuf {
    // The framework's sibling `registry directory is the local registry
    // index that the `phoxal` package publishes into. The test temp
    // robots need it on disk so path-based dependencies resolve under
    // `--offline`.
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .ancestors()
        .find_map(|ancestor| {
            let candidate = ancestor.join("registry");
            candidate.join("config.json").is_file().then_some(candidate)
        })
        .expect("registry directory must be a sibling of crates/project")
}

#[allow(dead_code, clippy::unwrap_used)]
fn parse_test_executable(stdout: &[u8], target_name: &str) -> Option<std::path::PathBuf> {
    for line in stdout.lines() {
        let Ok(line) = line else { continue };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if value.get("reason").and_then(|r| r.as_str()) != Some("compiler-artifact") {
            continue;
        }
        let target = value.get("target")?;
        if target.get("name").and_then(|n| n.as_str()) != Some(target_name) {
            continue;
        }
        let is_test = target
            .get("kind")
            .and_then(|k| k.as_array())
            .is_some_and(|kinds| kinds.iter().any(|k| k.as_str() == Some("test")));
        if !is_test {
            continue;
        }
        // Cargo emits test executables as `kind: ["test"]`, `crate_types: ["bin"]`,
        // with the absolute binary path in the top-level `executable` field.
        if let Some(exec) = value.get("executable").and_then(|e| e.as_str()) {
            return Some(std::path::PathBuf::from(exec));
        }
    }
    None
}
