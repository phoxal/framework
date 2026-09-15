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

fn build_harness_binary(
    project: &crate::Project,
    options: &CargoOptions,
) -> Result<std::path::PathBuf, String> {
    let robot_root = project.layout.root().to_owned();
    let robot_package_id = robot_package_id(project);
    // Cargo's structured `compiler-artifact` JSON events report the
    // exact executable path for the package + target kind + target name
    // we asked for. Scanning `target/debug/deps` for the newest
    // matching filename can pick a stale artifact or another
    // package's harness, so we use the JSON stream instead.
    //
    // We override `--message-format=json` for this invocation so the
    // structured stream is always available, regardless of what the
    // caller asked for. Selection (lock, offline, profile, target,
    // features, manifest path) is forwarded through the existing
    // helper so the configured Cargo executable and supported
    // wrappers are honored.
    let mut command = std::process::Command::new(options.cargo_program());
    command
        .args(["test", "--test", "phoxal-scenarios", "--no-run"])
        .args(["--message-format", "json"]);
    options.append_common(&mut command, false, true);
    command.current_dir(&robot_root).env_remove("RUSTC_WRAPPER");
    let output = command
        .output()
        .map_err(|error| format!("cargo invocation: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo test --test phoxal-scenarios --no-run failed:\n--- stderr ---\n{}",
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_artifact_executable(
        &stdout,
        "phoxal-scenarios",
        "test",
        robot_package_id.as_str(),
    )
    .ok_or_else(|| {
        format!(
            "cargo did not report an executable for the `{robot_package_id}` package's \
             `phoxal-scenarios` test target (matching artifacts must declare \
             `package_id` == `{robot_package_id}`, `profile.test` == true, \
             `target.kind` containing `test`, and `target.name` == `phoxal-scenarios`)"
        )
    })
}

/// Resolve the root robot package's `name version` identity from the
/// authored manifest. Cargo emits `<name> <version>` in its
/// `package_id` field, so the lookup uses the same format.
fn robot_package_id(project: &crate::Project) -> String {
    let manifest_text =
        std::fs::read_to_string(project.layout.cargo_manifest()).unwrap_or_default();
    let mut name: Option<String> = None;
    let mut version: Option<String> = None;
    for line in manifest_text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("name") {
            let rest = rest.trim_start_matches('=').trim().trim_matches('"');
            if name.is_none() {
                name = Some(rest.to_owned());
            }
        } else if let Some(rest) = trimmed.strip_prefix("version") {
            let rest = rest.trim_start_matches('=').trim().trim_matches('"');
            if version.is_none() {
                version = Some(rest.to_owned());
            }
        }
    }
    match (name, version) {
        (Some(n), Some(v)) => format!("{n} {v}"),
        (Some(n), None) => n,
        _ => "<unknown>".to_owned(),
    }
}

/// Parse Cargo's `--message-format=json` stream and return the
/// executable path of the named test target. Selection matches
/// exactly on:
///
///  * `package_id` (the Cargo package identity, e.g.
///    "phoxal-rover 0.1.0"), to refuse artifacts produced for sibling
///    workspace members.
///  * `target.name` == `target_name`, to refuse artifacts produced
///    for other targets.
///  * `target.kind` containing `target_kind`, to refuse artifacts
///    produced for other target kinds (a "bin" with the same name
///    must not satisfy the lookup).
///  * `profile.test` == true (the boolean must be explicitly true,
///    not just present), to refuse artifacts built under a different
///    profile.
///
/// Ambiguity (more than one matching artifact) is refused. The
/// function returns `Some(path)` only when a single artifact
/// matches every predicate.
fn parse_artifact_executable(
    stdout: &str,
    target_name: &str,
    target_kind: &str,
    package_id: &str,
) -> Option<PathBuf> {
    let mut found: Option<PathBuf> = None;
    let mut ambiguous = false;
    for line in stdout.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        if !trimmed.starts_with('{') {
            continue;
        }
        let event: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(event) => event,
            Err(_) => continue,
        };
        if event.get("reason").and_then(|v| v.as_str()) != Some("compiler-artifact") {
            continue;
        }
        let package_id_match =
            event.pointer("/package_id").and_then(|v| v.as_str()) == Some(package_id);
        let test_profile = event.pointer("/profile/test").and_then(|v| v.as_bool()) == Some(true);
        let name = event.pointer("/target/name").and_then(|v| v.as_str());
        let kinds: Vec<String> = event
            .pointer("/target/kind")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let executable = event
            .pointer("/executable")
            .and_then(|v| v.as_str())
            .map(PathBuf::from);
        if package_id_match
            && test_profile
            && name == Some(target_name)
            && kinds.iter().any(|k| k == target_kind)
            && let Some(path) = executable
        {
            if found.is_some() {
                ambiguous = true;
            }
            found = Some(path);
        }
    }
    if ambiguous {
        return None;
    }
    found
}

#[cfg(test)]
mod parse_artifact_tests {
    use super::parse_artifact_executable;
    use std::path::PathBuf;

    #[test]
    fn matches_test_profile_target() {
        let stdout = r#"
{"reason":"compiler-artifact","profile":{"test":true},"package_id":"phoxal-rover 0.1.0","target":{"name":"phoxal-scenarios","kind":["test"]},"executable":"/build/phoxal_scenarios-abc"}
{"reason":"compiler-artifact","profile":{"test":true},"package_id":"other-pkg 0.1.0","target":{"name":"other-binary","kind":["bin"]},"executable":"/build/other"}
"#;
        assert_eq!(
            parse_artifact_executable(stdout, "phoxal-scenarios", "test", "phoxal-rover 0.1.0"),
            Some(PathBuf::from("/build/phoxal_scenarios-abc"))
        );
    }

    #[test]
    fn ignores_non_test_profile_even_with_correct_name() {
        let stdout = r#"
{"reason":"compiler-artifact","profile":{"dev":true},"package_id":"phoxal-rover 0.1.0","target":{"name":"phoxal-scenarios","kind":["test"]},"executable":"/build/wrong"}
{"reason":"compiler-artifact","profile":{"test":true},"package_id":"phoxal-rover 0.1.0","target":{"name":"phoxal-scenarios","kind":["test"]},"executable":"/build/right"}
"#;
        assert_eq!(
            parse_artifact_executable(stdout, "phoxal-scenarios", "test", "phoxal-rover 0.1.0"),
            Some(PathBuf::from("/build/right"))
        );
    }

    #[test]
    fn ignores_other_targets() {
        let stdout = r#"
{"reason":"compiler-artifact","profile":{"test":true},"package_id":"phoxal-rover 0.1.0","target":{"name":"other-binary","kind":["test"]},"executable":"/build/other"}
"#;
        assert_eq!(
            parse_artifact_executable(stdout, "phoxal-scenarios", "test", "phoxal-rover 0.1.0"),
            None
        );
    }

    #[test]
    fn rejects_ambiguous_artifacts() {
        let stdout = r#"
{"reason":"compiler-artifact","profile":{"test":true},"package_id":"phoxal-rover 0.1.0","target":{"name":"phoxal-scenarios","kind":["test"]},"executable":"/build/one"}
{"reason":"compiler-artifact","profile":{"test":true},"package_id":"phoxal-rover 0.1.0","target":{"name":"phoxal-scenarios","kind":["test"]},"executable":"/build/two"}
"#;
        assert_eq!(
            parse_artifact_executable(stdout, "phoxal-scenarios", "test", "phoxal-rover 0.1.0"),
            None
        );
    }

    #[test]
    fn rejects_profile_test_false() {
        let stdout = r#"
{"reason":"compiler-artifact","profile":{"test":false},"package_id":"phoxal-rover 0.1.0","target":{"name":"phoxal-scenarios","kind":["test"]},"executable":"/build/wrong"}
"#;
        assert_eq!(
            parse_artifact_executable(stdout, "phoxal-scenarios", "test", "phoxal-rover 0.1.0"),
            None
        );
    }
}
