use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, ExitStatus};

use cargo_metadata::{CargoOpt, Message, Metadata, MetadataCommand};

use crate::error::Error;

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
    /// Cargo compiler output format.
    pub message_format: Option<String>,
    /// Additional Cargo arguments before a test delimiter.
    pub cargo_args: Vec<OsString>,
    /// Arguments after Cargo's `--` test delimiter.
    pub test_args: Vec<OsString>,
}

impl Default for CargoOptions {
    fn default() -> Self {
        Self {
            lock: LockMode::Unlocked,
            offline: false,
            target: None,
            profile: None,
            features: Vec::new(),
            all_features: false,
            no_default_features: false,
            message_format: None,
            cargo_args: Vec::new(),
            test_args: Vec::new(),
        }
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

    fn append_common(&self, command: &mut Command, include_message_format: bool) {
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
        if include_message_format && let Some(message_format) = &self.message_format {
            command.args(["--message-format", message_format]);
        }
        command.args(&self.cargo_args);
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
}

impl CargoOperation {
    /// The Cargo subcommand spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Build => "build",
            Self::Test => "test",
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

/// Invokes Cargo metadata with the same lock/offline policy as the operation.
pub(crate) fn load_metadata(manifest: &Path, options: &CargoOptions) -> Result<Metadata, Error> {
    options.validate()?;
    let mut command = MetadataCommand::new();
    command
        .manifest_path(manifest)
        .features(CargoOpt::SomeFeatures(options.features.clone()));
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
            let root = &prepared.root_package;
            let mut command = command_for(prepared, operation, options, true);
            command.args(["--package", root.id.to_string().as_str()]);
            command.args([
                "--manifest-path",
                &prepared.layout.cargo_manifest().display().to_string(),
            ]);
            command.args(["--"]);
            command.args(&options.test_args);
            outputs.push(run_command(command, operation)?);
        }
        CargoOperation::Check | CargoOperation::Build => {
            for target in prepared.execution_targets() {
                let mut command = command_for(prepared, operation, options, true);
                command.args([
                    "--manifest-path",
                    &prepared.layout.cargo_manifest().display().to_string(),
                ]);
                append_target_selection(&mut command, prepared, target);
                outputs.push(run_command(command, operation)?);
            }
        }
    }
    Ok(outputs)
}

fn command_for(
    prepared: &crate::PreparedProject,
    operation: CargoOperation,
    options: &CargoOptions,
    include_message_format: bool,
) -> Command {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let mut command = Command::new(cargo);
    command.current_dir(prepared.layout.root());
    command.arg(operation.as_str());
    options.append_common(&mut command, include_message_format);
    command
}

/// Builds one selected executable while retaining Cargo's machine-readable
/// artifact records for bundle assembly.
pub(crate) fn build_target(
    prepared: &crate::PreparedProject,
    target: &crate::SelectedTarget,
    options: &CargoOptions,
) -> Result<CargoOutput, Error> {
    options.validate()?;
    let mut command = command_for(prepared, CargoOperation::Build, options, false);
    command.args([
        "--manifest-path",
        &prepared.layout().cargo_manifest().display().to_string(),
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
    run_command(command, CargoOperation::Build)
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
    if target.package_id != prepared.root_package.id.to_string() {
        command.args(["--package", prepared.root_package.id.to_string().as_str()]);
    }
    command.args(["--package", target.package_id.as_str()]);
    command.args(["--bin", target.target.as_str()]);
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
