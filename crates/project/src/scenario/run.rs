//! P4 case-host CLI entry points.
//!
//! `cargo phoxal simulation scenario list` and
//! `cargo phoxal simulation scenario run <name>` land here. Both
//! functions reuse the [`Project::prepare_scenarios`] flow already
//! gated by the lock-aware preparation; they do not reimplement the
//! Cargo integration.
//!
//! The plan specifies that the CLI must:
//!  * prepare artifacts (handled by [`crate::Project::prepare`]),
//!  * own top-level orchestration (these helpers),
//!  * own cleanup (the caller discards the tempdir the project
//!    lives in once the report is on disk).

use std::path::PathBuf;

use crate::Project;
use crate::cargo::CargoOptions;

/// One line in the `scenario list` output. The struct identity is
/// `scenarios/<StructIdent>` — never the file or module path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioListEntry {
    pub name: String,
    pub module_path: String,
    pub source_line: u32,
}

/// Errors returned by the case-host CLI helpers. The variants are
/// coarse-grained on purpose: the caller decides the rendered string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScenarioRunError {
    NoSuchScenario(String),
    PreparationFailed(String),
    HarnessCompilationFailed(String),
    HarnessExecutionFailed(String),
}

impl std::fmt::Display for ScenarioRunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSuchScenario(name) => write!(f, "scenario `{name}` is not registered"),
            Self::PreparationFailed(detail) => {
                write!(f, "scenario preparation failed: {detail}")
            }
            Self::HarnessCompilationFailed(detail) => {
                write!(f, "harness compilation failed: {detail}")
            }
            Self::HarnessExecutionFailed(detail) => write!(f, "harness execution failed: {detail}"),
        }
    }
}

impl std::error::Error for ScenarioRunError {}

/// List every scenario the project registers after a fresh
/// preparation. Runs [`Project::prepare_scenarios`] so the harness
/// exists on disk, then shells out to `cargo test --no-run` and
/// executes the test binary with `list`. The caller is responsible
/// for the surrounding CLI plumbing.
pub fn list_scenarios(
    project: &Project,
    options: &CargoOptions,
) -> Result<Vec<ScenarioListEntry>, ScenarioRunError> {
    project
        .prepare_scenarios(options)
        .map_err(|error| ScenarioRunError::PreparationFailed(error.to_string()))?;
    let harness_binary = build_harness_binary(project, options)
        .map_err(ScenarioRunError::HarnessCompilationFailed)?;
    let list_output = std::process::Command::new(&harness_binary)
        .arg("list")
        .output()
        .map_err(|error| ScenarioRunError::HarnessExecutionFailed(error.to_string()))?;
    if !list_output.status.success() {
        return Err(ScenarioRunError::HarnessExecutionFailed(format!(
            "list command exited non-zero: {:?}\nstderr: {}",
            list_output.status,
            String::from_utf8_lossy(&list_output.stderr),
        )));
    }
    let stdout = String::from_utf8_lossy(&list_output.stdout);
    Ok(parse_list_output(&stdout))
}

/// Run one scenario by struct identity (e.g. `scenarios/First`). The
/// project is prepared, the harness compiled, and the binary
/// executed with `run <name>`.
pub fn run_scenario(
    project: &Project,
    options: &CargoOptions,
    scenario_name: &str,
) -> Result<ScenarioRunReport, ScenarioRunError> {
    project
        .prepare_scenarios(options)
        .map_err(|error| ScenarioRunError::PreparationFailed(error.to_string()))?;
    let harness_binary = build_harness_binary(project, options)
        .map_err(ScenarioRunError::HarnessCompilationFailed)?;
    let run_output = std::process::Command::new(&harness_binary)
        .arg("run")
        .arg(scenario_name)
        .output()
        .map_err(|error| ScenarioRunError::HarnessExecutionFailed(error.to_string()))?;
    let stdout = String::from_utf8_lossy(&run_output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&run_output.stderr).into_owned();
    if !run_output.status.success() {
        return Err(ScenarioRunError::HarnessExecutionFailed(format!(
            "run command exited non-zero: {:?}\nstderr: {}",
            run_output.status, stderr,
        )));
    }
    Ok(ScenarioRunReport {
        scenario_name: scenario_name.to_owned(),
        passed: true,
        stdout,
        stderr,
        report_artifact_path: None,
    })
}

/// One aggregated scenario run report. Mirrors
/// [`phoxal::scenario::HarnessRun`] in shape; P5 fills in
/// `report_artifact_path` once the case host seals a structured
/// report under `<robot>/.phoxal/reports/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioRunReport {
    pub scenario_name: String,
    pub passed: bool,
    pub stdout: String,
    pub stderr: String,
    pub report_artifact_path: Option<PathBuf>,
}

fn parse_list_output(stdout: &str) -> Vec<ScenarioListEntry> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(2, '\t');
            let name = parts.next()?.to_owned();
            let module_path = parts.next()?.to_owned();
            Some(ScenarioListEntry {
                name,
                module_path,
                source_line: 0,
            })
        })
        .collect()
}

/// Locate (or rebuild) the `phoxal-scenarios` test executable. We
/// prefer the existing artifact in `target/debug/deps/`; if cargo
/// insists on rebuilding, the caller observes a fresh compile via
/// the `compiler-artifact` JSON event stream and we hand back the
/// reported path. A conservative rebuild flag is used because the
/// plan restricts ad-hoc `cargo` invocations outside the supervisor
/// workflow.
fn build_harness_binary(
    project: &crate::Project,
    _options: &CargoOptions,
) -> Result<std::path::PathBuf, String> {
    let robot_root = project.layout.root().to_owned();
    let cargo_output = std::process::Command::new("cargo")
        .args([
            "test",
            "--test",
            "phoxal-scenarios",
            "--no-run",
            "--offline",
        ])
        .current_dir(&robot_root)
        .env_remove("RUSTC_WRAPPER")
        .output()
        .map_err(|error| format!("cargo invocation: {error}"))?;
    if !cargo_output.status.success() {
        return Err(format!(
            "cargo test --test phoxal-scenarios --no-run failed:\n--- stderr ---\n{}",
            String::from_utf8_lossy(&cargo_output.stderr),
        ));
    }
    // Find the freshly built executable. The harness binary lives
    // in `target/debug/deps/phoxal_scenarios-<hash>` and the test
    // name never appears on disk with the test suffix. We scan
    // `target/debug/deps` for the newest matching binary.
    let workspace_root = robot_root
        .ancestors()
        .find_map(|ancestor| {
            if ancestor.join("Cargo.toml").is_file() && ancestor.join("target").is_dir() {
                Some(ancestor.to_path_buf())
            } else {
                None
            }
        })
        .unwrap_or_else(|| robot_root.clone());
    let deps_dir = workspace_root.join("target/debug/deps");
    let read = std::fs::read_dir(&deps_dir)
        .map_err(|error| format!("read {}: {error}", deps_dir.display()))?;
    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = read
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str())?;
            if name.starts_with("phoxal_scenarios-")
                && path.extension().is_none()
                && !name.ends_with(".rmeta")
                && !name.ends_with(".d")
            {
                let modified = entry.metadata().and_then(|m| m.modified()).ok()?;
                Some((modified, path))
            } else {
                None
            }
        })
        .collect();
    candidates.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    candidates
        .into_iter()
        .map(|(_, path)| path)
        .next()
        .ok_or_else(|| "harness binary not produced by cargo".to_owned())
}
