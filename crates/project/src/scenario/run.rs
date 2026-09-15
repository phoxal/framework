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

use cargo_metadata::Message;

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
/// coarse-grained on purpose; the caller decides the rendered string.
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
    // An absent scenarios directory is a valid empty registry, not
    // an error. Without this short-circuit, listing would invoke a
    // non-existent Cargo test target and report a confusing compile
    // failure instead of an empty registry.
    let scenarios_root = project.layout.root().join("scenarios");
    if !scenarios_root.is_dir() {
        return Ok(Vec::new());
    }
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

/// Locate (or rebuild) the `phoxal-scenarios` test executable by
/// invoking `cargo test --no-run --message-format=json-render-diagnostics`
/// against the prepared robot's source closure, parsing the JSON
/// event stream with `cargo_metadata::Message`, and matching the
/// reported `compiler-artifact` exactly on:
///
///  * the resolved root-package identity (from the prepared manifest),
///  * `target.name` == `phoxal-scenarios`,
///  * `target.kind` containing `test`,
///  * `profile.test` == true.
///
/// Broad package/target selectors are not passed; the selection is
/// the single robot package the case host owns.
///
/// Compiler diagnostics emitted on stdout are forwarded to the
/// caller's stderr so a JSON-format build failure that puts its
/// diagnostic in stdout (rather than stderr) still surfaces the
/// useful error message.
fn build_harness_binary(
    project: &crate::Project,
    options: &CargoOptions,
) -> Result<std::path::PathBuf, String> {
    let robot_root = project.layout.root().to_owned();
    let staged_manifest = project.layout.cargo_manifest().to_owned();
    let package_id = resolve_root_package_id(&staged_manifest)
        .map_err(|error| format!("cannot resolve root package identity: {error}"))?;
    let mut command = std::process::Command::new(options.cargo_program());
    command
        .args(["test", "--test", "phoxal-scenarios", "--no-run"])
        .args([
            "--message-format",
            "json-render-diagnostics",
            "--manifest-path",
        ])
        .arg(&staged_manifest);
    options.append_common(&mut command, false, true);
    command.current_dir(&robot_root);
    let output = command
        .output()
        .map_err(|error| format!("cargo invocation: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(format!(
            "cargo test --test phoxal-scenarios --no-run failed:\n--- stderr ---\n{}\n--- stdout ---\n{}",
            stderr, stdout,
        ));
    }
    parse_artifact_executable(&output.stdout, "phoxal-scenarios", "test", &package_id).ok_or_else(
        || {
            format!(
                "cargo did not report an executable for the `{package_id}` package's \
             `phoxal-scenarios` test target (matching artifacts must declare \
             `package_id` == `{package_id}`, `profile.test` == true, \
             `target.kind` containing `test`, and `target.name` == `phoxal-scenarios`)"
            )
        },
    )
}

/// Resolve the root robot package's exact `PackageId` via Cargo's
/// own resolver. The resolver walks the prepared manifest path and
/// returns the opaque `PackageId` (which Cargo emits verbatim in its
/// `compiler-artifact` JSON events). No hand-written string scanner.
fn resolve_root_package_id(
    staged_manifest: &std::path::Path,
) -> Result<cargo_metadata::PackageId, String> {
    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(staged_manifest)
        .no_deps()
        .exec()
        .map_err(|error| format!("cargo metadata: {error}"))?;
    // The root robot package is the workspace root package (or the
    // only package for single-package workspaces).
    metadata
        .resolve
        .as_ref()
        .and_then(|resolve| {
            resolve
                .nodes
                .iter()
                .find(|node| {
                    metadata
                        .workspace_members
                        .iter()
                        .any(|member| member == &node.id)
                })
                .map(|node| node.id.clone())
        })
        .or_else(|| metadata.packages.first().map(|pkg| pkg.id.clone()))
        .ok_or_else(|| "cargo metadata returned no packages".to_owned())
}

/// Parse Cargo's JSON event stream with `cargo_metadata::Message`
/// and return the executable path of the named test target, or
/// `None` when no artifact matches. Selection matches exactly on
/// `package_id`, `target.name`, `target.kind` containing the
/// requested kind, and `profile.test` == true.
///
/// Ambiguity (more than one matching artifact) is refused. Compiler
/// diagnostics are surfaced so the caller can render the actual
/// cause of a build failure.
fn parse_artifact_executable(
    stdout_bytes: &[u8],
    target_name: &str,
    target_kind: &str,
    package_id: &cargo_metadata::PackageId,
) -> Option<PathBuf> {
    let mut found: Option<PathBuf> = None;
    let mut ambiguous = false;
    let mut diagnostics: Vec<String> = Vec::new();
    for message in Message::parse_stream(stdout_bytes) {
        match message {
            Ok(Message::CompilerArtifact(artifact)) => {
                if artifact.target.name != target_name {
                    continue;
                }
                if !artifact.target.kind.iter().any(|k| {
                    let kind_string = format!("{k}");
                    kind_string.as_str() == target_kind
                }) {
                    continue;
                }
                if !artifact.profile.test {
                    continue;
                }
                if &artifact.package_id != package_id {
                    continue;
                }
                let Some(executable) = artifact.executable else {
                    continue;
                };
                if found.is_some() {
                    ambiguous = true;
                }
                found = Some(executable.into_std_path_buf());
            }
            Ok(Message::CompilerMessage(msg)) => {
                diagnostics.push(msg.message.rendered.unwrap_or_default());
            }
            Ok(Message::TextLine(text)) => {
                diagnostics.push(text);
            }
            Ok(_) => {}
            Err(error) => {
                eprintln!(
                    "phoxal-project: cargo message parse failed: {error}; \
                     accumulated diagnostics:\n{}",
                    diagnostics.join("\n")
                );
                break;
            }
        }
    }
    if ambiguous {
        return None;
    }
    if found.is_none() && !diagnostics.is_empty() {
        eprintln!(
            "phoxal-project: cargo build emitted no artifact; diagnostics:\n{}",
            diagnostics.join("\n")
        );
    }
    found
}

#[cfg(test)]
mod parse_artifact_tests {
    use super::parse_artifact_executable;
    use cargo_metadata::PackageId;
    use std::path::PathBuf;

    fn pkg() -> PackageId {
        // PackageId is a `str_newtype!` macro wrapper; the macro
        // derives Deserialize for T: Deserialize. Construct from the
        // exact JSON Cargo would emit.
        serde_json::from_str::<PackageId>("\"phoxal-rover 0.1.0 (path+file:///tmp/rover)\"")
            .expect("package id")
    }

    fn msg_line(line: serde_json::Value) -> String {
        serde_json::to_string(&line).expect("serialize")
    }

    #[test]
    fn matches_test_profile_target() {
        let stdout = [
            msg_line(test_artifact("/build/phoxal_scenarios-abc")),
            msg_line(other_artifact("/build/other")),
        ]
        .join("\n");
        assert_eq!(
            parse_artifact_executable(stdout.as_bytes(), "phoxal-scenarios", "test", &pkg()),
            Some(PathBuf::from("/build/phoxal_scenarios-abc"))
        );
    }

    #[test]
    fn ignores_non_test_profile() {
        let stdout = [
            msg_line(test_artifact_with_profile("/build/wrong", false)),
            msg_line(test_artifact_with_profile("/build/right", true)),
        ]
        .join("\n");
        assert_eq!(
            parse_artifact_executable(stdout.as_bytes(), "phoxal-scenarios", "test", &pkg()),
            Some(PathBuf::from("/build/right"))
        );
    }

    #[test]
    fn ignores_other_packages() {
        let stdout = msg_line(other_artifact("/build/other"));
        assert_eq!(
            parse_artifact_executable(stdout.as_bytes(), "phoxal-scenarios", "test", &pkg()),
            None
        );
    }

    #[test]
    fn rejects_ambiguous_artifacts() {
        let stdout = [
            msg_line(test_artifact("/build/one")),
            msg_line(test_artifact("/build/two")),
        ]
        .join("\n");
        assert_eq!(
            parse_artifact_executable(stdout.as_bytes(), "phoxal-scenarios", "test", &pkg()),
            None
        );
    }

    fn test_artifact(executable: &str) -> serde_json::Value {
        test_artifact_with_profile(executable, true)
    }

    fn test_artifact_with_profile(executable: &str, test: bool) -> serde_json::Value {
        serde_json::json!({
            "reason": "compiler-artifact",
            "package_id": "phoxal-rover 0.1.0 (path+file:///tmp/rover)",
            "manifest_path": "/tmp/rover/Cargo.toml",
            "target": {
                "name": "phoxal-scenarios",
                "kind": ["test"],
                "crate_types": ["bin"],
                "required-features": [],
                "src_path": "/tmp/rover/.phoxal/generated/scenarios/main.rs",
                "edition": "2024",
                "doc": true,
                "doctest": true,
                "test": true,
            },
            "profile": {
                "opt_level": "0",
                "debuginfo": 0,
                "debug_assertions": true,
                "overflow_checks": true,
                "test": test,
            },
            "features": [],
            "filenames": [executable],
            "executable": executable,
            "fresh": true,
        })
    }

    fn other_artifact(executable: &str) -> serde_json::Value {
        serde_json::json!({
            "reason": "compiler-artifact",
            "package_id": "other-pkg 0.1.0 (path+file:///tmp/other)",
            "manifest_path": "/tmp/other/Cargo.toml",
            "target": {
                "name": "other-binary",
                "kind": ["bin"],
                "crate_types": ["bin"],
                "required-features": [],
                "src_path": "/tmp/other/src/main.rs",
                "edition": "2024",
                "doc": true,
                "doctest": true,
                "test": true,
            },
            "profile": {
                "opt_level": "0",
                "debuginfo": 0,
                "debug_assertions": true,
                "overflow_checks": true,
                "test": true,
            },
            "features": [],
            "filenames": [executable],
            "executable": executable,
            "fresh": true,
        })
    }
}
