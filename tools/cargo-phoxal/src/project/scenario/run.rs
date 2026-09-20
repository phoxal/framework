//! Case-host CLI entry points.
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

use std::path::{Path, PathBuf};

use cargo_metadata::{Message, MetadataCommand};

use crate::project::Project;
use crate::project::cargo::CargoOptions;

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
    Preparation(String),
    Compilation(String),
    Execution(String),
}

impl std::fmt::Display for ScenarioRunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Preparation(detail) => {
                write!(f, "scenario preparation failed: {detail}")
            }
            Self::Compilation(detail) => {
                write!(f, "harness compilation failed: {detail}")
            }
            Self::Execution(detail) => write!(f, "harness execution failed: {detail}"),
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
        .map_err(|error| ScenarioRunError::Preparation(error.to_string()))?;
    // An absent scenarios directory is a valid empty registry, not
    // an error. Without this short-circuit, listing would invoke a
    // non-existent Cargo test target and report a confusing compile
    // failure instead of an empty registry.
    let scenarios_root = project.layout.root().join("scenarios");
    if !scenarios_root.is_dir() {
        return Ok(Vec::new());
    }
    let harness_binary =
        build_harness_binary(project, options).map_err(ScenarioRunError::Compilation)?;
    let list_output = std::process::Command::new(&harness_binary)
        .arg("list")
        .output()
        .map_err(|error| ScenarioRunError::Execution(error.to_string()))?;
    if !list_output.status.success() {
        return Err(ScenarioRunError::Execution(format!(
            "list command exited non-zero: {:?}\nstderr: {}",
            list_output.status,
            String::from_utf8_lossy(&list_output.stderr),
        )));
    }
    let stdout = String::from_utf8_lossy(&list_output.stdout);
    Ok(parse_list_output(&stdout))
}

/// Run one scenario by struct identity (e.g. `scenarios/First`). The
/// project is prepared, the harness compiled, and the case host
/// drives the harness binary over the private control channel
/// described in plan §9. The case host owns the simulator/supervisor
/// lifecycle; the harness only retains the planned scenario for
/// sealing and verification.
pub fn run_scenario(
    project: &Project,
    options: &CargoOptions,
    scenario_name: &str,
    simulator_executable: Option<&std::path::Path>,
    headless: bool,
) -> Result<ScenarioRunReport, ScenarioRunError> {
    project
        .prepare_scenarios(options)
        .map_err(|error| ScenarioRunError::Preparation(error.to_string()))?;
    let harness_binary =
        build_harness_binary(project, options).map_err(ScenarioRunError::Compilation)?;
    let tool_report = crate::project::scenario::case_host::run_case_host(
        project,
        options,
        &harness_binary,
        scenario_name,
        simulator_executable,
        headless,
    )
    .map_err(|error| ScenarioRunError::Execution(error.to_string()))?;
    let detail = tool_report.detail.clone().unwrap_or_default();
    let stdout = if tool_report.passed {
        format!("scenario {}: PASSED\n{detail}", tool_report.scenario_name)
    } else {
        format!("scenario {}: FAILED\n{detail}", tool_report.scenario_name)
    };
    Ok(ScenarioRunReport {
        scenario_name: tool_report.scenario_name,
        passed: tool_report.passed,
        stdout,
        stderr: String::new(),
        report_artifact_path: None,
    })
}

/// One aggregated scenario run report.
/// It mirrors the SDK's private `HarnessRun` in shape.
/// `report_artifact_path` is populated when the case host seals a structured
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
    project: &crate::project::Project,
    options: &CargoOptions,
) -> Result<std::path::PathBuf, String> {
    validate_options_for_case_host(options).map_err(|error| error.to_string())?;
    let robot_root = project.layout.root().to_owned();
    let staged_manifest = project.layout.cargo_manifest().to_owned();
    let package_id = resolve_root_package_id(&staged_manifest, &robot_root, options)
        .map_err(|error| format!("cannot resolve root package identity: {error}"))?;
    let mut command = std::process::Command::new(options.cargo_program());
    command
        .args(["test", "--test", "phoxal-scenarios", "--no-run"])
        // Explicit single-package selection. The selection the user
        // asked for (`options.selection`) is intentionally dropped
        // here so a workspace selector that names a different package
        // cannot silently override the robot the case host owns.
        .args(["--package", &format!("{package_id}")])
        .args([
            "--message-format",
            "json-render-diagnostics",
            "--manifest-path",
        ])
        .arg(&staged_manifest);
    // Forward lock/offline/target/profile/feature/wrapper flags from
    // the prepared Cargo context so the harness build uses the same
    // Cargo executable, registry injection, and lock file as the
    // rest of the project. `include_selection` is forced false here
    // because the explicit `--package` already names the single
    // robot package. `include_message_format` is forced false
    // because the literal `--message-format json-render-diagnostics`
    // above is required for the harness build to parse artifacts;
    // `append_common` would add a second copy if the caller had set
    // a non-default `message_format`.
    options.append_common(&mut command, false, false);
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

/// Reject `CargoOptions` whose selection flags conflict with the
/// case host's ownership of the single robot package. The case host
/// already passes the explicit `--package <root-id>` it resolved from
/// the staged manifest; a user-supplied `--workspace`, `--package`,
/// or `--exclude` would silently drop or override that owned
/// selection, so conflicts are rejected before preparation writes anything.
fn validate_options_for_case_host(options: &CargoOptions) -> Result<(), CaseHostOptionError> {
    if options.selection.workspace {
        return Err(CaseHostOptionError::ConflictingSelection(
            "--workspace".to_owned(),
        ));
    }
    if let Some(package) = options.selection.packages.first() {
        return Err(CaseHostOptionError::ConflictingSelection(format!(
            "--package {package}"
        )));
    }
    if let Some(exclude) = options.selection.excludes.first() {
        return Err(CaseHostOptionError::ConflictingSelection(format!(
            "--exclude {exclude}"
        )));
    }
    if options.selection.has_target_selectors() {
        return Err(CaseHostOptionError::ConflictingSelection(
            "target selectors such as --lib, --bin, --test, --all-targets".to_owned(),
        ));
    }
    for arg in &options.cargo_args {
        let rendered = arg.to_string_lossy();
        if rendered == "--workspace"
            || rendered == "-w"
            || is_package_selector(&rendered)
            || is_exclude_selector(&rendered)
        {
            return Err(CaseHostOptionError::ConflictingSelection(
                rendered.into_owned(),
            ));
        }
    }
    Ok(())
}

/// Returns `true` when `arg` is a cargo `--package` / `-p` selector
/// followed by either `=` (combined form) or end-of-arg (separate
/// form). The case-host validator uses this predicate so the
/// check matches `--package` and `--package=<name>` / `-p` /
/// `-p=<name>` while ignoring unrelated flags that happen to
/// share the prefix (e.g. `--packages-all`).
fn is_package_selector(arg: &str) -> bool {
    arg == "--package" || arg == "-p" || arg.starts_with("--package=") || arg.starts_with("-p=")
}

/// Returns `true` when `arg` is a cargo `--exclude` selector
/// followed by either `=` (combined form) or end-of-arg (separate
/// form). See [`is_package_selector`] for the rationale.
fn is_exclude_selector(arg: &str) -> bool {
    arg == "--exclude" || arg.starts_with("--exclude=")
}

/// Errors raised by [`validate_options_for_case_host`]. The variant
/// carries the conflicting selector so the caller can render it back
/// to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CaseHostOptionError {
    /// A selection flag conflicts with the case host's owned
    /// `--package <root-id>` selection.
    ConflictingSelection(String),
}

impl std::fmt::Display for CaseHostOptionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConflictingSelection(selector) => write!(
                f,
                "scenario list/run selects the staged root package; \
                 `{selector}` would silently override the owned selection and is not allowed"
            ),
        }
    }
}

/// Resolve the root robot package's exact `PackageId` via Cargo's
/// own resolver. The resolver walks the prepared manifest path and
/// returns the opaque `PackageId` (which Cargo emits verbatim in its
/// `compiler-artifact` JSON events).
///
/// Selection is by exact `manifest_path` against the manifest the
/// project is about to compile. `metadata.packages.first()` is not a
/// stable identifier: a sibling sorted before the robot in a
/// workspace, or a fixture helper package whose manifest is part of
/// the same Cargo graph, would otherwise be chosen. `resolve.nodes`
/// likewise depends on Cargo's traversal order and is not stable
/// across workspace layouts.
///
/// The metadata invocation reuses the prepared Cargo context
/// (`CargoOptions`) so the configured Cargo executable, registered
/// registry, lock/offline policy, and feature selection match the
/// rest of the project. Constructing an independent `MetadataCommand`
/// here would let the user's `cargo_path`/`offline`/`CARGO_TARGET_DIR`
/// settings be ignored at exactly the boundary the harness build requires.
fn resolve_root_package_id(
    staged_manifest: &Path,
    current_dir: &Path,
    options: &CargoOptions,
) -> Result<cargo_metadata::PackageId, String> {
    let mut command = MetadataCommand::new();
    command
        .cargo_path(options.cargo_program())
        .manifest_path(staged_manifest)
        .current_dir(current_dir)
        .features(cargo_metadata::CargoOpt::SomeFeatures(
            options.features.clone(),
        ));
    if options.all_features {
        command.features(cargo_metadata::CargoOpt::AllFeatures);
    }
    if options.no_default_features {
        command.features(cargo_metadata::CargoOpt::NoDefaultFeatures);
    }
    let mut extra: Vec<String> = options
        .lock
        .flags()
        .iter()
        .map(|flag| (*flag).to_owned())
        .collect();
    // Inject the official Phoxal registry configuration so metadata
    // resolves the same coordinates the rest of the project uses.
    // The phoxal registry is not a private Cargo workspace registry;
    // it must be present even when Cargo would otherwise use the
    // caller's default.
    extra.push("--config".to_owned());
    extra.push(format!(
        "registries.phoxal.index=\"{}\"",
        crate::project::cargo::PHOXAL_REGISTRY_INDEX
    ));
    if options.offline {
        extra.push("--offline".to_owned());
    }
    if let Some(target) = &options.target {
        extra.push("--filter-platform".to_owned());
        extra.push(target.clone());
    }
    command.other_options(extra);
    let metadata = command
        .exec()
        .map_err(|error| format!("cargo metadata: {error}"))?;
    let canonical_manifest =
        std::fs::canonicalize(staged_manifest).unwrap_or_else(|_| staged_manifest.to_owned());
    metadata
        .packages
        .iter()
        .find(|pkg| {
            std::fs::canonicalize(&pkg.manifest_path)
                .map(|path| path == canonical_manifest)
                .unwrap_or_else(|_| pkg.manifest_path == staged_manifest)
        })
        .map(|pkg| pkg.id.clone())
        .ok_or_else(|| {
            format!(
                "cargo metadata did not return a package for manifest `{}`",
                staged_manifest.display()
            )
        })
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
                    "cargo-phoxal: cargo message parse failed: {error}; \
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
            "cargo-phoxal: cargo build emitted no artifact; diagnostics:\n{}",
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

    /// End-to-end exercise of [`super::resolve_root_package_id`]
    /// against a real workspace with two packages. The
    /// alphabetically-sorted sibling (`a-helper`) would otherwise be
    /// picked by `packages.first()`; selecting by exact
    /// `manifest_path` must always return the package the staged
    /// source tree owns.
    ///
    /// The temporary directory is wired up as a real workspace at
    /// the top level so the phoxal registry config plus
    /// `--manifest-path` invocation actually exercise the resolver
    /// rather than a sibling-only layout.
    #[test]
    fn resolver_picks_staged_manifest_not_first_package() {
        let directory = tempfile::tempdir().expect("tempdir");
        let helper = directory.path().join("a-helper");
        let robot = directory.path().join("z-review-robot");
        std::fs::create_dir_all(helper.join("src")).expect("mkdir helper");
        std::fs::create_dir_all(robot.join("src")).expect("mkdir robot");
        let workspace_root = directory.path();
        std::fs::write(
            workspace_root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"a-helper\", \"z-review-robot\"]\nresolver = \"2\"\n",
        )
        .expect("write workspace manifest");
        std::fs::write(
            helper.join("Cargo.toml"),
            "[package]\nname = \"a-helper\"\nedition = \"2024\"\nversion = \"0.1.0\"\n",
        )
        .expect("write helper manifest");
        std::fs::write(
            robot.join("Cargo.toml"),
            "[package]\nname = \"z-review-robot\"\nedition = \"2024\"\nversion = \"0.1.0\"\n",
        )
        .expect("write robot manifest");
        std::fs::write(helper.join("src/lib.rs"), "pub fn x() {}").expect("helper src");
        std::fs::write(robot.join("src/lib.rs"), "pub fn x() {}").expect("robot src");
        let resolved = super::resolve_root_package_id(
            robot.join("Cargo.toml").as_path(),
            workspace_root,
            &crate::project::cargo::CargoOptions::default(),
        )
        .expect("resolve by exact manifest path");
        let resolved_str = format!("{resolved}");
        assert!(
            resolved_str.contains("z-review-robot"),
            "resolver returned `{resolved_str}`; expected the manifest-owned package",
        );
    }

    // Prepared Cargo-context propagation and selection-flag validation for
    // `list_scenarios`.

    #[test]
    fn case_host_validates_conflicting_package_selector() {
        // Listing scenarios owns the single root package the staged
        // manifest declared; a caller-supplied `--package` would
        // silently override that ownership. The validator refuses
        // the conflicting selection before preparation writes
        // anything.
        let mut options = crate::project::cargo::CargoOptions::default();
        options
            .selection
            .packages
            .push("some-other-crate".to_owned());
        let err = super::validate_options_for_case_host(&options)
            .expect_err("conflicting selection must be refused");
        match err {
            super::CaseHostOptionError::ConflictingSelection(selector) => {
                assert!(
                    selector.contains("some-other-crate"),
                    "expected the conflicting selector to be named; got `{selector}`"
                );
            }
        }
    }

    #[test]
    fn case_host_validates_workspace_selector() {
        let mut options = crate::project::cargo::CargoOptions::default();
        options.selection.workspace = true;
        let err = super::validate_options_for_case_host(&options)
            .expect_err("workspace selector must be refused");
        assert!(matches!(
            err,
            super::CaseHostOptionError::ConflictingSelection(_)
        ));
    }

    #[test]
    fn case_host_validates_target_selector() {
        let mut options = crate::project::cargo::CargoOptions::default();
        options.selection.tests = true;
        let err = super::validate_options_for_case_host(&options)
            .expect_err("--tests selector must be refused");
        assert!(matches!(
            err,
            super::CaseHostOptionError::ConflictingSelection(_)
        ));
    }

    #[test]
    fn case_host_validates_raw_cargo_package_argument() {
        // Raw arguments that override package selection are also
        // refused; silently dropping them is not validation.
        let mut options = crate::project::cargo::CargoOptions::default();
        options
            .cargo_args
            .push(std::ffi::OsString::from("--package=other"));
        let err = super::validate_options_for_case_host(&options)
            .expect_err("raw --package override must be refused");
        assert!(matches!(
            err,
            super::CaseHostOptionError::ConflictingSelection(_)
        ));
    }

    #[test]
    fn case_host_does_not_match_packages_all_prefix() {
        // Regression: `starts_with("--package")` would incorrectly
        // match `--packages-all`, `--package-foo`, etc. The
        // validator must only refuse `--package` and `--package=`.
        let mut options = crate::project::cargo::CargoOptions::default();
        options
            .cargo_args
            .push(std::ffi::OsString::from("--packages-all"));
        super::validate_options_for_case_host(&options)
            .expect("--packages-all must not be flagged as a conflicting selector");
        let mut options = crate::project::cargo::CargoOptions::default();
        options
            .cargo_args
            .push(std::ffi::OsString::from("--package-foo"));
        super::validate_options_for_case_host(&options)
            .expect("--package-foo must not be flagged as a conflicting selector");
    }

    #[test]
    fn case_host_accepts_short_package_form() {
        // The validator must also accept the short `-p` form for
        // `--package` and `-p=<name>` as the conflicting form.
        let mut options = crate::project::cargo::CargoOptions::default();
        options.cargo_args.push(std::ffi::OsString::from("-p"));
        let err = super::validate_options_for_case_host(&options)
            .expect_err("raw -p override must be refused");
        assert!(matches!(
            err,
            super::CaseHostOptionError::ConflictingSelection(_)
        ));
        let mut options = crate::project::cargo::CargoOptions::default();
        options
            .cargo_args
            .push(std::ffi::OsString::from("-p=other"));
        let err = super::validate_options_for_case_host(&options)
            .expect_err("-p= override must be refused");
        assert!(matches!(
            err,
            super::CaseHostOptionError::ConflictingSelection(_)
        ));
    }

    #[test]
    fn case_host_accepts_default_options() {
        // Default options carry no conflicting selectors; the
        // validator must accept them so listing works out of the
        // box.
        let options = crate::project::cargo::CargoOptions::default();
        super::validate_options_for_case_host(&options).expect("default options must validate");
    }
}
