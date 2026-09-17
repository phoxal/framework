use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, ExitStatus};

use cargo_metadata::{CargoOpt, Message, Metadata, MetadataCommand};

use crate::error::Error;

/// The retained public sparse index used by official Phoxal package coordinates.
pub(crate) const PHOXAL_REGISTRY_INDEX: &str = "sparse+https://phoxal.github.io/registry/";

fn registry_config() -> String {
    format!("registries.phoxal.index=\"{PHOXAL_REGISTRY_INDEX}\"")
}

/// Cargo's lockfile policy for project preparation and commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LockMode {
    /// Let Cargo resolve within the authored requirements and update the lock.
    #[default]
    Unlocked,
    /// Require the existing lock to be current.
    Locked,
    /// Require the existing lock and forbid all network access.
    Frozen,
}

impl LockMode {
    /// Returns the flags used for this policy.
    #[must_use]
    pub const fn flags(self) -> &'static [&'static str] {
        match self {
            Self::Unlocked => &[],
            Self::Locked => &["--locked"],
            Self::Frozen => &["--frozen"],
        }
    }

    /// Whether this policy includes Cargo's offline guarantee.
    #[must_use]
    pub const fn is_offline(self) -> bool {
        matches!(self, Self::Frozen)
    }
}

/// Shared preparation and Cargo-command options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CargoOptions {
    /// Cargo executable selected by the caller.
    ///
    /// When absent, the `CARGO` environment variable is used and finally the
    /// `cargo` executable is resolved through `PATH`.  Capturing this once in
    /// the options keeps metadata, checks, builds, tests, and updates on the
    /// same Cargo installation.
    pub cargo_path: Option<std::path::PathBuf>,
    /// Lockfile policy.
    pub lock: LockMode,
    /// Forbid registry and Git network access without requiring `--frozen`.
    pub offline: bool,
    /// Optional target triple.
    pub target: Option<String>,
    /// Optional Cargo profile.
    pub profile: Option<String>,
    /// Features enabled on the root invocation.
    pub features: Vec<String>,
    /// Enable every root feature.
    pub all_features: bool,
    /// Disable default root features.
    pub no_default_features: bool,
    /// Build with Cargo's release profile.
    pub release: bool,
    /// Cargo compiler output format.
    pub message_format: Option<String>,
    /// Additional Cargo arguments before a test delimiter.
    pub cargo_args: Vec<OsString>,
    /// Arguments after Cargo's `--` test delimiter.
    pub test_args: Vec<OsString>,
    /// Cargo package and target-selection options supplied by the caller.
    pub selection: CargoSelection,
}

impl Default for CargoOptions {
    fn default() -> Self {
        Self {
            cargo_path: None,
            lock: LockMode::Unlocked,
            offline: false,
            target: None,
            profile: None,
            features: Vec::new(),
            all_features: false,
            no_default_features: false,
            release: false,
            message_format: None,
            cargo_args: Vec::new(),
            test_args: Vec::new(),
            selection: CargoSelection::default(),
        }
    }
}

/// Cargo package and target selectors that remain meaningful at the
/// `cargo-phoxal` boundary.
///
/// The project compiler still selects the validated brain, service, and
/// driver targets for bundle assembly.  These selectors are forwarded to
/// source-development Cargo invocations so root, workspace, package, and
/// test workflows retain Cargo's documented semantics.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CargoSelection {
    /// Restrict Cargo to the workspace rather than the root package.
    pub workspace: bool,
    /// Explicit package specifications, in caller order.
    pub packages: Vec<String>,
    /// Workspace packages excluded from a workspace selection.
    pub excludes: Vec<String>,
    /// Select every target in the selected package set.
    pub all_targets: bool,
    /// Select the package library target.
    pub lib: bool,
    /// Select all binary targets.
    pub bins: bool,
    /// Select named binary targets.
    pub binaries: Vec<String>,
    /// Select all example targets.
    pub examples: bool,
    /// Select named example targets.
    pub examples_named: Vec<String>,
    /// Select all integration tests.
    pub tests: bool,
    /// Select named integration tests.
    pub tests_named: Vec<String>,
    /// Select all benchmarks.
    pub benches: bool,
    /// Select named benchmarks.
    pub benches_named: Vec<String>,
}

impl CargoSelection {
    fn append_to(&self, command: &mut Command) {
        if self.workspace {
            command.arg("--workspace");
        }
        for package in &self.packages {
            command.args(["--package", package]);
        }
        for exclude in &self.excludes {
            command.args(["--exclude", exclude]);
        }
        if self.all_targets {
            command.arg("--all-targets");
        }
        if self.lib {
            command.arg("--lib");
        }
        if self.bins {
            command.arg("--bins");
        }
        for binary in &self.binaries {
            command.args(["--bin", binary]);
        }
        if self.examples {
            command.arg("--examples");
        }
        for example in &self.examples_named {
            command.args(["--example", example]);
        }
        if self.tests {
            command.arg("--tests");
        }
        for test in &self.tests_named {
            command.args(["--test", test]);
        }
        if self.benches {
            command.arg("--benches");
        }
        for bench in &self.benches_named {
            command.args(["--bench", bench]);
        }
    }

    fn is_empty(&self) -> bool {
        !self.workspace
            && self.packages.is_empty()
            && self.excludes.is_empty()
            && !self.all_targets
            && !self.lib
            && !self.bins
            && self.binaries.is_empty()
            && !self.examples
            && self.examples_named.is_empty()
            && !self.tests
            && self.tests_named.is_empty()
            && !self.benches
            && self.benches_named.is_empty()
    }

    fn has_update_unsupported_targets(&self) -> bool {
        self.has_target_selectors()
    }

    /// Returns `true` when the selection specifies any non-package
    /// selector (target, example, test, bench, lib, bins). The
    /// case-host validator shares this predicate because every
    /// such selector conflicts with the owned `--package <root-id>`
    /// selection the harness build emits. See Gate P2 of
    /// the scenario acceptance review.
    pub(crate) fn has_target_selectors(&self) -> bool {
        self.all_targets
            || self.lib
            || self.bins
            || !self.binaries.is_empty()
            || self.examples
            || !self.examples_named.is_empty()
            || self.tests
            || !self.tests_named.is_empty()
            || self.benches
            || !self.benches_named.is_empty()
    }
}

impl CargoOptions {
    /// Validates flags before any Cargo process or manifest mutation occurs.
    pub fn validate(&self) -> Result<(), Error> {
        if self.all_features && self.no_default_features {
            return Err(Error::InvalidOptions {
                message: "--all-features and --no-default-features cannot be combined".to_owned(),
            });
        }
        if self
            .features
            .iter()
            .any(|feature| feature.trim().is_empty())
        {
            return Err(Error::InvalidOptions {
                message: "--features cannot contain an empty feature name".to_owned(),
            });
        }
        Ok(())
    }

    pub(crate) fn append_common(
        &self,
        command: &mut Command,
        include_message_format: bool,
        include_selection: bool,
    ) {
        for flag in self.lock.flags() {
            command.arg(flag);
        }
        if self.offline {
            command.arg("--offline");
        }
        if let Some(target) = &self.target {
            command.args(["--target", target]);
        }
        if let Some(profile) = &self.profile {
            command.args(["--profile", profile]);
        }
        if self.all_features {
            command.arg("--all-features");
        }
        if self.no_default_features {
            command.arg("--no-default-features");
        }
        if !self.features.is_empty() {
            command.args(["--features", &self.features.join(",")]);
        }
        if self.release {
            command.arg("--release");
        }
        if include_message_format && let Some(message_format) = &self.message_format {
            command.args(["--message-format", message_format]);
        }
        if include_selection {
            self.selection.append_to(command);
        }
        command.args(&self.cargo_args);
    }

    pub(crate) fn cargo_program(&self) -> std::path::PathBuf {
        self.cargo_path
            .clone()
            .or_else(|| std::env::var_os("CARGO").map(std::path::PathBuf::from))
            .unwrap_or_else(|| std::path::PathBuf::from("cargo"))
    }
}

/// The supported source-development Cargo operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CargoOperation {
    /// Validate the root brain and selected executable targets.
    Check,
    /// Build the root brain and selected executable targets.
    Build,
    /// Run tests selected by Cargo for the root robot package.
    Test,
    /// Resolve permitted package updates in the root Cargo graph.
    Update,
}

impl CargoOperation {
    /// The Cargo subcommand spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Build => "build",
            Self::Test => "test",
            Self::Update => "update",
        }
    }
}

/// Captured output from one successful Cargo invocation.
#[derive(Debug)]
pub struct CargoOutput {
    /// Exact argv passed to Cargo, excluding the executable path.
    pub arguments: Vec<OsString>,
    /// Process exit status.
    pub status: ExitStatus,
    /// Captured standard output.
    pub stdout: Vec<u8>,
    /// Captured standard error.
    pub stderr: Vec<u8>,
}

/// Invokes Cargo metadata from an optional isolated source tree.
pub(crate) fn load_metadata_at(
    manifest: &Path,
    current_dir: &Path,
    target_dir: Option<&Path>,
    options: &CargoOptions,
) -> Result<Metadata, Error> {
    options.validate()?;
    let mut command = MetadataCommand::new();
    command.cargo_path(options.cargo_program());
    command
        .manifest_path(manifest)
        .current_dir(current_dir)
        .features(CargoOpt::SomeFeatures(options.features.clone()));
    if let Some(target_dir) = target_dir {
        command.env("CARGO_TARGET_DIR", target_dir);
    }
    if options.all_features {
        command.features(CargoOpt::AllFeatures);
    }
    if options.no_default_features {
        command.features(CargoOpt::NoDefaultFeatures);
    }
    let mut extra = options
        .lock
        .flags()
        .iter()
        .map(|flag| (*flag).to_owned())
        .collect::<Vec<_>>();
    extra.extend(["--config".to_owned(), registry_config()]);
    if options.offline {
        extra.push("--offline".to_owned());
    }
    if let Some(target) = &options.target {
        extra.extend(["--filter-platform".to_owned(), target.clone()]);
    }
    command.other_options(extra);
    command.exec().map_err(|source| Error::CargoMetadata {
        manifest: manifest.to_owned(),
        source,
    })
}

/// Runs the selected target set from the prepared project's root graph.
pub(crate) fn run(
    prepared: &crate::PreparedProject,
    operation: CargoOperation,
    options: &CargoOptions,
) -> Result<Vec<CargoOutput>, Error> {
    options.validate()?;
    let mut outputs = Vec::new();
    match operation {
        CargoOperation::Test => {
            let root = prepared.cargo_root_package();
            let mut command = command_for(prepared, operation, options, true, true);
            if options.selection.is_empty() {
                command.args(["--package", root.id.to_string().as_str()]);
            }
            command.args([
                "--manifest-path",
                &prepared.cargo_manifest_path().display().to_string(),
            ]);
            command.args(["--"]);
            command.args(&options.test_args);
            outputs.push(run_command(command, operation)?);
        }
        CargoOperation::Check | CargoOperation::Build => {
            if !options.selection.is_empty() {
                let mut command = command_for(prepared, operation, options, true, true);
                command.args([
                    "--manifest-path",
                    &prepared.cargo_manifest_path().display().to_string(),
                ]);
                outputs.push(run_command(command, operation)?);
                return Ok(outputs);
            }
            for target in prepared.execution_targets() {
                let mut command = command_for(prepared, operation, options, true, false);
                command.args([
                    "--manifest-path",
                    &prepared.cargo_manifest_path().display().to_string(),
                ]);
                append_target_selection(&mut command, prepared, target);
                outputs.push(run_command(command, operation)?);
            }
        }
        CargoOperation::Update => {
            return Err(Error::InvalidOptions {
                message: "cargo update must be invoked through Project::update so the resulting graph is validated".to_owned(),
            });
        }
    }
    Ok(outputs)
}

fn command_for(
    prepared: &crate::PreparedProject,
    operation: CargoOperation,
    options: &CargoOptions,
    include_message_format: bool,
    include_selection: bool,
) -> Command {
    let mut command = Command::new(options.cargo_program());
    command.current_dir(prepared.cargo_workdir());
    if let Some(target_dir) = prepared.cargo_target_dir() {
        command.env("CARGO_TARGET_DIR", target_dir);
    }
    command.arg(operation.as_str());
    command.args(["--config", &registry_config()]);
    options.append_common(&mut command, include_message_format, include_selection);
    command
}

/// Runs one explicit Cargo update from the owning project root.
pub(crate) fn update(
    manifest: &Path,
    current_dir: &Path,
    target_dir: Option<&Path>,
    options: &CargoOptions,
) -> Result<CargoOutput, Error> {
    validate_update_options(options)?;
    let mut command = Command::new(options.cargo_program());
    command.current_dir(current_dir);
    if let Some(target_dir) = target_dir {
        command.env("CARGO_TARGET_DIR", target_dir);
    }
    command.args(["update", "--config", &registry_config()]);
    for flag in options.lock.flags() {
        command.arg(flag);
    }
    if options.offline {
        command.arg("--offline");
    }
    command.args(["--manifest-path", &manifest.display().to_string()]);
    if options.selection.workspace {
        command.arg("--workspace");
    }
    command.args(&options.selection.packages);
    command.args(&options.cargo_args);
    run_command(command, CargoOperation::Update)
}

/// Validates the subset of Cargo options that `cargo update` accepts before
/// project preparation can mutate the authored manifest or lockfile.
pub(crate) fn validate_update_options(options: &CargoOptions) -> Result<(), Error> {
    options.validate()?;
    if !options.selection.excludes.is_empty()
        || options.selection.has_update_unsupported_targets()
        || options.cargo_args.iter().any(is_update_target_argument)
    {
        return Err(Error::InvalidOptions {
            message: "cargo phoxal update accepts only Cargo package specifications and --workspace; exclude and target selectors such as --lib, --bin, --test, and --all-targets are not cargo update options".to_owned(),
        });
    }
    if options.target.is_some()
        || options.profile.is_some()
        || !options.features.is_empty()
        || options.all_features
        || options.no_default_features
        || options.release
        || options.message_format.is_some()
        || options
            .cargo_args
            .iter()
            .any(is_update_unsupported_argument)
    {
        return Err(Error::InvalidOptions {
            message: "cargo phoxal update accepts Cargo update options only; target, profile, feature, release, and compiler-message options belong to check/build/test".to_owned(),
        });
    }
    Ok(())
}

fn is_update_unsupported_argument(argument: &OsString) -> bool {
    is_update_target_argument(argument) || is_update_compiler_argument(argument)
}

fn is_update_compiler_argument(argument: &OsString) -> bool {
    let argument = argument.to_string_lossy();
    matches!(
        argument.as_ref(),
        "--target"
            | "--profile"
            | "--features"
            | "--all-features"
            | "--no-default-features"
            | "--message-format"
            | "--release"
            | "-r"
    ) || argument.starts_with("--target=")
        || argument.starts_with("--profile=")
        || argument.starts_with("--features=")
        || argument.starts_with("--message-format=")
}

fn is_update_target_argument(argument: &OsString) -> bool {
    let argument = argument.to_string_lossy();
    matches!(
        argument.as_ref(),
        "--all-targets"
            | "--lib"
            | "--bins"
            | "--bin"
            | "--examples"
            | "--example"
            | "--tests"
            | "--test"
            | "--benches"
            | "--bench"
    ) || argument.starts_with("--bin=")
        || argument.starts_with("--example=")
        || argument.starts_with("--test=")
        || argument.starts_with("--bench=")
}

/// Builds one selected executable while retaining Cargo's machine-readable
/// artifact records for bundle assembly.
pub(crate) fn build_target(
    prepared: &crate::PreparedProject,
    target: &crate::SelectedTarget,
    options: &CargoOptions,
) -> Result<CargoOutput, Error> {
    options.validate()?;
    let mut command = command_for(prepared, CargoOperation::Build, options, false, false);
    command.args([
        "--manifest-path",
        &prepared.cargo_manifest_path().display().to_string(),
    ]);
    append_target_selection(&mut command, prepared, target);
    command.args([
        "--message-format",
        options
            .message_format
            .as_deref()
            .filter(|format| format.starts_with("json"))
            .unwrap_or("json-render-diagnostics"),
    ]);
    let output = run_command(command, CargoOperation::Build);
    let sync = prepared.sync_staged_lock();
    match (output, sync) {
        (Ok(output), Ok(())) => Ok(output),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) | (Err(_), Err(error)) => Err(error),
    }
}

fn append_target_selection(
    command: &mut Command,
    prepared: &crate::PreparedProject,
    target: &crate::SelectedTarget,
) {
    // Cargo only permits `--features` for the selected workspace package. A
    // root feature may nevertheless activate an optional dependency whose
    // binary we need to build. Select both packages through the root graph so
    // those root features remain effective while `--bin` still names the
    // dependency executable. This also keeps the invocation valid for a
    // registry or Git package that is outside the robot workspace.
    if target.package_id != prepared.cargo_root_package().id.to_string() {
        command.args([
            "--package",
            prepared.cargo_root_package().id.to_string().as_str(),
        ]);
    }
    command.args(["--package", target.package_id.as_str()]);
    command.args(["--bin", target.target.as_str()]);
    // The selected binary's `required-features` gate makes the target
    // eligible only when those features are already enabled. The runtime
    // cleanup consolidated each service into a package with empty defaults,
    // so the binary needs `runtime` (and any other declared features) to be
    // passed to `--features` or Cargo silently skips it and the project's
    // artifact stream records no executable. Auto-merge the gate into any
    // `--features` already on the command so the project's selection stays
    // the source of truth while still respecting an explicit caller list.
    if !target.required_features.is_empty() {
        let mut features = collect_existing_features(command);
        for feature in &target.required_features {
            if !features.iter().any(|existing| existing == feature) {
                features.push(feature.clone());
            }
        }
        command.args(["--features", &features.join(",")]);
    }
}

/// Pull every comma-separated feature already present on `command` so the
/// auto-merge in `append_target_selection` does not duplicate an explicit
/// caller list.
fn collect_existing_features(command: &Command) -> Vec<String> {
    let arguments: Vec<std::ffi::OsString> = command
        .get_args()
        .map(|argument| argument.to_os_string())
        .collect();
    let mut features = Vec::new();
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        let value = argument.to_string_lossy();
        if value == "--features" {
            if let Some(next) = arguments.get(index + 1) {
                for piece in next.to_string_lossy().split(',') {
                    let trimmed = piece.trim();
                    if !trimmed.is_empty() && !features.iter().any(|f| f == trimmed) {
                        features.push(trimmed.to_owned());
                    }
                }
                index += 2;
                continue;
            }
        } else if let Some(rest) = value.strip_prefix("--features=") {
            for piece in rest.split(',') {
                let trimmed = piece.trim();
                if !trimmed.is_empty() && !features.iter().any(|f| f == trimmed) {
                    features.push(trimmed.to_owned());
                }
            }
        }
        index += 1;
    }
    features
}

/// Extracts one selected executable from Cargo's JSON compiler-artifact stream.
pub(crate) fn artifact_path(
    stdout: &[u8],
    target: &crate::SelectedTarget,
) -> Result<std::path::PathBuf, Error> {
    let mut executable = None;
    for line in stdout.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let message =
            serde_json::from_slice::<Message>(line).map_err(|error| Error::ArtifactCapture {
                package: target.package.clone(),
                target: target.target.clone(),
                message: format!("invalid Cargo JSON message: {error}"),
            })?;
        if let Message::CompilerArtifact(artifact) = message
            && artifact.package_id.to_string() == target.package_id
            && artifact.target.name == target.target
            && artifact.target.is_bin()
        {
            executable = artifact.executable.map(|path| path.into_std_path_buf());
        }
    }
    executable.ok_or_else(|| Error::ArtifactCapture {
        package: target.package.clone(),
        target: target.target.clone(),
        message: "no compiler-artifact executable matched the selected package".to_owned(),
    })
}

fn run_command(mut command: Command, operation: CargoOperation) -> Result<CargoOutput, Error> {
    let arguments = command.get_args().map(OsString::from).collect::<Vec<_>>();
    let output = command.output().map_err(|source| Error::CargoSpawn {
        operation: operation.as_str().to_owned(),
        source,
    })?;
    if !output.status.success() {
        return Err(Error::CargoCommand {
            operation: operation.as_str().to_owned(),
            status: status_string(output.status),
            stdout: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(CargoOutput {
        arguments,
        status: output.status,
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

fn status_string(status: ExitStatus) -> String {
    status
        .code()
        .map(|code| code.to_string())
        .unwrap_or_else(|| "terminated by signal".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_existing_features_reads_space_and_equals_forms() {
        let mut command = Command::new("cargo");
        command.args(["--features", "scenario,imu"]);
        command.args(["--features=vision"]);
        let features = collect_existing_features(&command);
        assert_eq!(
            features,
            vec!["scenario".to_owned(), "imu".to_owned(), "vision".to_owned()]
        );
    }

    #[test]
    fn collect_existing_features_dedupes_overlapping_entries() {
        let mut command = Command::new("cargo");
        command.args(["--features", "scenario,imu"]);
        command.args(["--features=scenario"]);
        let features = collect_existing_features(&command);
        assert_eq!(features, vec!["scenario".to_owned(), "imu".to_owned()]);
    }
}
