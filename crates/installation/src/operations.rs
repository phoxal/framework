//! Bounded host operations for an artifact-only deployment.
//!
//! The operations in this module deliberately stop at the host boundary.
//! They install and select immutable artifacts, invoke native systemd
//! lifecycle primitives, and expose bounded journal output.
//! Domain readiness still comes from the public supervisor session, and
//! motion re-arm remains an explicit runtime action rather than an inferred
//! consequence of a process becoming active.

use std::fmt;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use crate::{
    Activation, IdentityUpdate, Installation, InstallationError, InstalledRelease, ReleaseId,
    SERVICE_UNIT_FILE, ServiceConfigError, configure_systemd_service,
};

/// Default number of journal lines returned by [`SystemdService::logs`].
pub const DEFAULT_LOG_LINES: u32 = 200;
/// Default byte bound applied to one journal response.
pub const DEFAULT_MAX_LOG_BYTES: usize = 256 * 1024;

/// A bounded request for service logs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LogQuery {
    lines: u32,
    max_bytes: usize,
}

impl LogQuery {
    /// Create a bounded log request.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceControlError::InvalidLogQuery`] when either bound is
    /// zero.
    pub fn new(lines: u32, max_bytes: usize) -> Result<Self, ServiceControlError> {
        if lines == 0 || max_bytes == 0 {
            return Err(ServiceControlError::InvalidLogQuery { lines, max_bytes });
        }
        Ok(Self { lines, max_bytes })
    }

    /// Maximum number of lines requested from the journal.
    #[must_use]
    pub fn lines(self) -> u32 {
        self.lines
    }

    /// Maximum number of bytes accepted from the journal.
    #[must_use]
    pub fn max_bytes(self) -> usize {
        self.max_bytes
    }
}

impl Default for LogQuery {
    fn default() -> Self {
        Self {
            lines: DEFAULT_LOG_LINES,
            max_bytes: DEFAULT_MAX_LOG_BYTES,
        }
    }
}

/// Process-level state reported by the native service manager.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProcessState {
    /// The supervisor process is not running.
    Inactive,
    /// The supervisor process is starting.
    Activating,
    /// The supervisor process has reported process readiness.
    Active,
    /// The supervisor process is stopping.
    Deactivating,
    /// The service manager observed a failed process activation or exit.
    Failed,
    /// The service manager returned an unrecognized active state.
    Unknown,
}

impl ProcessState {
    fn from_systemd(value: &str) -> Self {
        match value {
            "inactive" => Self::Inactive,
            "activating" => Self::Activating,
            "active" => Self::Active,
            "deactivating" => Self::Deactivating,
            "failed" => Self::Failed,
            _ => Self::Unknown,
        }
    }
}

/// Process readiness and native service-manager details.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceStatus {
    /// Coarse process state.
    pub state: ProcessState,
    /// Native service-manager sub-state, such as `running` or `dead`.
    pub sub_state: String,
    /// Native service-manager result, such as `success` or `exit-code`.
    pub result: String,
}

impl ServiceStatus {
    /// Whether the supervisor process has reached native process readiness.
    #[must_use]
    pub fn process_ready(&self) -> bool {
        self.state == ProcessState::Active
    }
}

/// Bounded logs returned by a host service manager.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceLogs {
    /// Raw journal bytes, limited by the corresponding [`LogQuery`].
    pub bytes: Vec<u8>,
}

/// Combined artifact and process state exposed by the host boundary.
///
/// The status intentionally does not claim domain readiness.
/// Callers must query the public supervisor session for domain state before
/// treating the deployed graph as ready.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeploymentStatus {
    /// Immutable release selected by the active symlink.
    pub active_release: Option<ReleaseId>,
    /// Native process state for the supervisor service.
    pub process: ServiceStatus,
}

/// Native lifecycle and log operations required by a deployment host.
///
/// Implementations may wrap systemd or a deterministic fixture.
/// Implementations must keep command output bounded when serving logs.
pub trait ServiceControl {
    /// Service unit controlled by this implementation.
    fn unit(&self) -> &str;

    /// Make one generated absolute unit path discoverable by systemd.
    ///
    /// # Errors
    ///
    /// Returns a typed host-control error when the service manager cannot link
    /// the unit path.
    fn link(&mut self, unit_path: &Path) -> Result<(), ServiceControlError>;

    /// Ask the service manager to reload generated unit files.
    ///
    /// # Errors
    ///
    /// Returns a typed host-control error when the service manager cannot
    /// reload its unit configuration.
    fn reload(&mut self) -> Result<(), ServiceControlError>;

    /// Start the supervisor service.
    ///
    /// # Errors
    ///
    /// Returns a typed host-control error when the service manager cannot
    /// start the unit.
    fn start(&mut self) -> Result<(), ServiceControlError>;

    /// Stop the supervisor service and wait for the service manager command to
    /// complete.
    ///
    /// # Errors
    ///
    /// Returns a typed host-control error when the service manager cannot stop
    /// the unit.
    fn stop(&mut self) -> Result<(), ServiceControlError>;

    /// Read process-level service status.
    ///
    /// # Errors
    ///
    /// Returns a typed host-control error when status cannot be read or
    /// decoded.
    fn status(&mut self) -> Result<ServiceStatus, ServiceControlError>;

    /// Read bounded service logs.
    ///
    /// # Errors
    ///
    /// Returns a typed host-control error when logs cannot be read or exceed
    /// the requested bound.
    fn logs(&mut self, query: LogQuery) -> Result<ServiceLogs, ServiceControlError>;
}

/// Native systemd implementation of [`ServiceControl`].
#[derive(Clone, Debug)]
pub struct SystemdService {
    unit: String,
    systemctl: PathBuf,
    journalctl: PathBuf,
}

impl SystemdService {
    /// Use `systemctl` and `journalctl` from the host PATH.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceControlError::InvalidUnit`] for an unsafe unit name.
    pub fn new(unit: impl Into<String>) -> Result<Self, ServiceControlError> {
        Self::with_commands(
            unit,
            PathBuf::from("systemctl"),
            PathBuf::from("journalctl"),
        )
    }

    /// Construct a service manager with explicit command paths.
    ///
    /// Explicit paths make fixture-backed acceptance tests deterministic while
    /// preserving the native systemd command boundary in production.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceControlError::InvalidUnit`] for an unsafe unit name.
    pub fn with_commands(
        unit: impl Into<String>,
        systemctl: impl Into<PathBuf>,
        journalctl: impl Into<PathBuf>,
    ) -> Result<Self, ServiceControlError> {
        let unit = unit.into();
        if !valid_unit(&unit) {
            return Err(ServiceControlError::InvalidUnit { unit });
        }
        Ok(Self {
            unit,
            systemctl: systemctl.into(),
            journalctl: journalctl.into(),
        })
    }

    /// Path used to invoke `systemctl`.
    #[must_use]
    pub fn systemctl(&self) -> &Path {
        &self.systemctl
    }

    /// Path used to invoke `journalctl`.
    #[must_use]
    pub fn journalctl(&self) -> &Path {
        &self.journalctl
    }

    fn run(&self, program: &Path, arguments: &[String]) -> Result<Output, ServiceControlError> {
        Command::new(program)
            .args(arguments)
            .output()
            .map_err(|source| ServiceControlError::Spawn {
                program: program.to_owned(),
                source,
            })
    }

    fn run_logs(
        &self,
        arguments: &[String],
        maximum: usize,
    ) -> Result<Vec<u8>, ServiceControlError> {
        let mut child = Command::new(&self.journalctl)
            .args(arguments)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|source| ServiceControlError::Spawn {
                program: self.journalctl.clone(),
                source,
            })?;
        let Some(mut stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ServiceControlError::ReadOutput {
                program: self.journalctl.clone(),
                source: io::Error::other("journal command did not provide stdout"),
            });
        };
        let mut bytes = Vec::with_capacity(maximum.min(8 * 1024));
        let mut buffer = [0_u8; 8 * 1024];
        loop {
            let count = match stdout.read(&mut buffer) {
                Ok(count) => count,
                Err(source) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ServiceControlError::ReadOutput {
                        program: self.journalctl.clone(),
                        source,
                    });
                }
            };
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
            if bytes.len() > maximum {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ServiceControlError::LogLimitExceeded {
                    unit: self.unit.clone(),
                    bytes: bytes.len(),
                    maximum,
                });
            }
        }
        let status = child
            .wait()
            .map_err(|source| ServiceControlError::ReadOutput {
                program: self.journalctl.clone(),
                source,
            })?;
        if !status.success() {
            return Err(ServiceControlError::CommandFailed {
                program: self.journalctl.clone(),
                unit: self.unit.clone(),
                status: status.to_string(),
                stderr: String::new(),
            });
        }
        Ok(bytes)
    }

    fn check(&self, program: &Path, output: Output) -> Result<Output, ServiceControlError> {
        if output.status.success() {
            Ok(output)
        } else {
            Err(ServiceControlError::CommandFailed {
                program: program.to_owned(),
                unit: self.unit.clone(),
                status: output.status.to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            })
        }
    }
}

impl ServiceControl for SystemdService {
    fn unit(&self) -> &str {
        &self.unit
    }

    fn link(&mut self, unit_path: &Path) -> Result<(), ServiceControlError> {
        if !unit_path.is_absolute() {
            return Err(ServiceControlError::InvalidUnitPath {
                path: unit_path.to_owned(),
            });
        }
        let unit_path = unit_path
            .to_str()
            .ok_or_else(|| ServiceControlError::InvalidUnitPath {
                path: unit_path.to_owned(),
            })?;
        let output = self.run(
            &self.systemctl,
            &[String::from("link"), unit_path.to_owned()],
        )?;
        self.check(&self.systemctl, output).map(|_| ())
    }

    fn reload(&mut self) -> Result<(), ServiceControlError> {
        let output = self.run(&self.systemctl, &[String::from("daemon-reload")])?;
        self.check(&self.systemctl, output).map(|_| ())
    }

    fn start(&mut self) -> Result<(), ServiceControlError> {
        let output = self.run(&self.systemctl, &[String::from("start"), self.unit.clone()])?;
        self.check(&self.systemctl, output).map(|_| ())
    }

    fn stop(&mut self) -> Result<(), ServiceControlError> {
        let output = self.run(&self.systemctl, &[String::from("stop"), self.unit.clone()])?;
        self.check(&self.systemctl, output).map(|_| ())
    }

    fn status(&mut self) -> Result<ServiceStatus, ServiceControlError> {
        let output = self.run(
            &self.systemctl,
            &[
                String::from("show"),
                String::from("--property=ActiveState"),
                String::from("--property=SubState"),
                String::from("--property=Result"),
                String::from("--value"),
                self.unit.clone(),
            ],
        )?;
        let output = self.check(&self.systemctl, output)?;
        parse_systemd_status(&self.unit, &output.stdout)
    }

    fn logs(&mut self, query: LogQuery) -> Result<ServiceLogs, ServiceControlError> {
        let bytes = self.run_logs(
            &[
                String::from("--unit"),
                self.unit.clone(),
                String::from("--no-pager"),
                String::from("--output=cat"),
                String::from("--lines"),
                query.lines.to_string(),
            ],
            query.max_bytes,
        )?;
        Ok(ServiceLogs { bytes })
    }
}

/// Installation and host lifecycle operations for one deployment root.
pub struct DeploymentOperations<C> {
    installation: Installation,
    control: C,
}

impl<C> DeploymentOperations<C>
where
    C: ServiceControl,
{
    /// Bind artifact installation to one native host service controller.
    #[must_use]
    pub fn new(installation: Installation, control: C) -> Self {
        Self {
            installation,
            control,
        }
    }

    /// Immutable artifact store used by these operations.
    #[must_use]
    pub fn installation(&self) -> &Installation {
        &self.installation
    }

    /// Service controller used by these operations.
    #[must_use]
    pub fn control(&self) -> &C {
        &self.control
    }

    /// Mutable service controller, useful for host-specific diagnostics.
    pub fn control_mut(&mut self) -> &mut C {
        &mut self.control
    }

    /// Install and verify an artifact, then persist its deployment identity.
    ///
    /// Installation does not activate or start the release.
    /// The archive is the only deployment input and the host does not need the
    /// source tree, `robot.yaml`, or Cargo.
    ///
    /// # Errors
    ///
    /// Returns a typed artifact, identity, or filesystem error.
    pub fn install(
        &mut self,
        archive: impl AsRef<Path>,
        checksum: impl AsRef<Path>,
        identity: IdentityUpdate,
    ) -> Result<InstalledRelease, OperationsError> {
        let installed = self.installation.install(archive, checksum)?;
        configure_systemd_service(self.installation.root(), identity)?;
        self.control
            .link(&self.installation.root().join(SERVICE_UNIT_FILE))?;
        self.control.reload()?;
        Ok(installed)
    }

    /// Start the supervisor and return process-level readiness.
    ///
    /// This does not imply that the runtime graph is domain-ready.
    /// Callers must query the public supervisor session separately.
    ///
    /// # Errors
    ///
    /// Returns a typed service-manager or status error.
    pub fn start(&mut self) -> Result<ServiceStatus, OperationsError> {
        self.control.start()?;
        Ok(self.control.status()?)
    }

    /// Stop the supervisor and return its resulting process-level status.
    ///
    /// # Errors
    ///
    /// Returns a typed service-manager or status error.
    pub fn stop(&mut self) -> Result<ServiceStatus, OperationsError> {
        self.control.stop()?;
        Ok(self.control.status()?)
    }

    /// Read immutable artifact and process state together.
    ///
    /// Domain readiness remains deliberately outside this host-level result.
    ///
    /// # Errors
    ///
    /// Returns a typed artifact or service-manager error.
    pub fn status(&mut self) -> Result<DeploymentStatus, OperationsError> {
        Ok(DeploymentStatus {
            active_release: self.installation.active_id()?,
            process: self.control.status()?,
        })
    }

    /// Read bounded supervisor logs from the native service journal.
    ///
    /// # Errors
    ///
    /// Returns a typed service-manager or output-bound error.
    pub fn logs(&mut self, query: LogQuery) -> Result<ServiceLogs, OperationsError> {
        Ok(self.control.logs(query)?)
    }

    /// Stop the old process, activate one exact release, and start it.
    ///
    /// A process-level activation failure leaves the candidate selected and
    /// stopped so recovery is explicit and observable.
    /// The returned error carries the previous release when one existed.
    ///
    /// # Errors
    ///
    /// Returns [`OperationsError::ActivationFailed`] when process readiness is
    /// not reached, or a typed artifact/control error before selection.
    pub fn activate_and_start(&mut self, id: &ReleaseId) -> Result<Activation, OperationsError> {
        let previous = self.installation.active_id()?;
        self.control.stop()?;
        let activation = self.installation.activate(id)?;
        self.finish_activation(activation, previous)
    }

    /// Explicitly select a known release, start it, and return the activation.
    ///
    /// This is the recovery operation after a failed activation or a failed
    /// domain readiness check.
    /// The caller must separately verify public domain readiness and must not
    /// infer motion re-arm from this process-level result.
    ///
    /// # Errors
    ///
    /// Returns [`OperationsError::ActivationFailed`] when process readiness is
    /// not reached, or a typed artifact/control error before selection.
    pub fn rollback_and_start(&mut self, id: &ReleaseId) -> Result<Activation, OperationsError> {
        let previous = self.installation.active_id()?;
        self.control.stop()?;
        let activation = self.installation.rollback(id)?;
        self.finish_activation(activation, previous)
    }

    /// Stop the service and remove the active selector.
    ///
    /// Use this when the failed activation had no previous release to restore.
    /// Immutable releases remain installed for a later explicit activation.
    ///
    /// # Errors
    ///
    /// Returns a typed service-manager or filesystem error.
    pub fn deactivate(&mut self) -> Result<Option<ReleaseId>, OperationsError> {
        self.control.stop()?;
        Ok(self.installation.deactivate()?)
    }

    fn finish_activation(
        &mut self,
        activation: Activation,
        previous: Option<ReleaseId>,
    ) -> Result<Activation, OperationsError> {
        if let Err(error) = self.control.start() {
            return Err(OperationsError::ActivationFailed {
                attempted: activation.active,
                previous,
                status: None,
                cause: error.to_string(),
            });
        }
        let status = match self.control.status() {
            Ok(status) => status,
            Err(error) => {
                return Err(OperationsError::ActivationFailed {
                    attempted: activation.active,
                    previous,
                    status: None,
                    cause: error.to_string(),
                });
            }
        };
        if !status.process_ready() {
            return Err(OperationsError::ActivationFailed {
                attempted: activation.active,
                previous,
                status: Some(Box::new(status)),
                cause: String::from("service did not reach process-ready state"),
            });
        }
        Ok(activation)
    }
}

fn valid_unit(unit: &str) -> bool {
    !unit.is_empty()
        && unit.len() <= 255
        && unit.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'@' | b'.' | b'_' | b'-' | b':')
        })
}

fn parse_systemd_status(unit: &str, bytes: &[u8]) -> Result<ServiceStatus, ServiceControlError> {
    let output = String::from_utf8_lossy(bytes);
    let mut values = output.lines();
    let active = values.next().map(str::trim).unwrap_or_default();
    let sub_state = values.next().map(str::trim).unwrap_or_default();
    let result = values.next().map(str::trim).unwrap_or_default();
    if active.is_empty() || sub_state.is_empty() || result.is_empty() {
        return Err(ServiceControlError::MalformedStatus {
            unit: unit.to_owned(),
            output: output.into_owned(),
        });
    }
    Ok(ServiceStatus {
        state: ProcessState::from_systemd(active),
        sub_state: sub_state.to_owned(),
        result: result.to_owned(),
    })
}

/// Failures at the artifact, service configuration, or host process boundary.
#[derive(Debug, thiserror::Error)]
pub enum OperationsError {
    /// Artifact installation or activation failed before process activation.
    #[error(transparent)]
    Installation(#[from] InstallationError),
    /// Generated host service configuration failed.
    #[error(transparent)]
    ServiceConfig(#[from] ServiceConfigError),
    /// A direct lifecycle, status, or log operation failed.
    #[error(transparent)]
    Service(#[from] ServiceControlError),
    /// A selected release did not reach process readiness.
    #[error("release {attempted} failed process activation: {cause}")]
    ActivationFailed {
        /// Candidate release selected when activation failed.
        attempted: ReleaseId,
        /// Release available for explicit recovery, if any.
        previous: Option<ReleaseId>,
        /// Process status observed before failure, if available.
        status: Option<Box<ServiceStatus>>,
        /// Host-level failure detail.
        cause: String,
    },
}

/// Host process-control failures.
#[derive(Debug, thiserror::Error)]
pub enum ServiceControlError {
    /// A unit name contains unsupported characters.
    #[error("invalid systemd unit name `{unit}`")]
    InvalidUnit { unit: String },
    /// A generated unit path was not absolute or valid host text.
    #[error("invalid generated systemd unit path {}", path.display())]
    InvalidUnitPath { path: PathBuf },
    /// A log request has an empty line or byte bound.
    #[error("log bounds must be non-zero, got lines={lines}, max_bytes={max_bytes}")]
    InvalidLogQuery { lines: u32, max_bytes: usize },
    /// A native command could not be started.
    #[error("failed to start host command {}: {source}", program.display())]
    Spawn {
        program: PathBuf,
        #[source]
        source: io::Error,
    },
    /// A native command's output stream could not be read.
    #[error("failed to read host command {}: {source}", program.display())]
    ReadOutput {
        program: PathBuf,
        #[source]
        source: io::Error,
    },
    /// A native command returned a non-success status.
    #[error("host command {} failed for {unit} with {status}: {stderr}", program.display())]
    CommandFailed {
        program: PathBuf,
        unit: String,
        status: String,
        stderr: String,
    },
    /// The status response did not contain the three required fields.
    #[error("systemd returned malformed status for {unit}: {output:?}")]
    MalformedStatus { unit: String, output: String },
    /// Journal output exceeded the caller's explicit bound.
    #[error("journal output for {unit} was {bytes} bytes, above bound {maximum}")]
    LogLimitExceeded {
        unit: String,
        bytes: usize,
        maximum: usize,
    },
    /// A fixture or alternate host adapter reported a typed backend failure.
    #[error("{operation} failed for {unit}: {detail}")]
    Backend {
        unit: String,
        operation: String,
        detail: String,
    },
}

impl fmt::Display for ProcessState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Inactive => "inactive",
            Self::Activating => "activating",
            Self::Active => "active",
            Self::Deactivating => "deactivating",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::{
        BUNDLE_DIR, DeploymentIdentity, MANIFEST_FILE, SERVICE_UNIT_FILE, SUPERVISOR_FILE,
        create_archive,
    };

    #[derive(Debug)]
    struct FixtureControl {
        unit: String,
        calls: Vec<&'static str>,
        linked_unit: Option<PathBuf>,
        state: ProcessState,
        fail_next_start: bool,
        logs: Vec<u8>,
    }

    impl FixtureControl {
        fn new() -> Self {
            Self {
                unit: String::from(SERVICE_UNIT_FILE),
                calls: Vec::new(),
                linked_unit: None,
                state: ProcessState::Inactive,
                fail_next_start: false,
                logs: b"supervisor started\nsupervisor stopped\n".to_vec(),
            }
        }

        fn status(&self) -> ServiceStatus {
            ServiceStatus {
                state: self.state.clone(),
                sub_state: match self.state {
                    ProcessState::Active => String::from("running"),
                    _ => String::from("dead"),
                },
                result: match self.state {
                    ProcessState::Failed => String::from("exit-code"),
                    _ => String::from("success"),
                },
            }
        }
    }

    impl ServiceControl for FixtureControl {
        fn unit(&self) -> &str {
            &self.unit
        }

        fn link(&mut self, unit_path: &Path) -> Result<(), ServiceControlError> {
            self.calls.push("link");
            self.linked_unit = Some(unit_path.to_owned());
            Ok(())
        }

        fn reload(&mut self) -> Result<(), ServiceControlError> {
            self.calls.push("reload");
            Ok(())
        }

        fn start(&mut self) -> Result<(), ServiceControlError> {
            self.calls.push("start");
            if self.fail_next_start {
                self.fail_next_start = false;
                self.state = ProcessState::Failed;
                return Err(ServiceControlError::Backend {
                    unit: self.unit.clone(),
                    operation: String::from("start"),
                    detail: String::from("fixture activation failure"),
                });
            }
            self.state = ProcessState::Active;
            Ok(())
        }

        fn stop(&mut self) -> Result<(), ServiceControlError> {
            self.calls.push("stop");
            self.state = ProcessState::Inactive;
            Ok(())
        }

        fn status(&mut self) -> Result<ServiceStatus, ServiceControlError> {
            self.calls.push("status");
            Ok(FixtureControl::status(self))
        }

        fn logs(&mut self, query: LogQuery) -> Result<ServiceLogs, ServiceControlError> {
            self.calls.push("logs");
            if self.logs.len() > query.max_bytes {
                return Err(ServiceControlError::LogLimitExceeded {
                    unit: self.unit.clone(),
                    bytes: self.logs.len(),
                    maximum: query.max_bytes,
                });
            }
            Ok(ServiceLogs {
                bytes: self.logs.clone(),
            })
        }
    }

    fn release(root: &Path, marker: &str) {
        fs::create_dir_all(root.join(BUNDLE_DIR).join("bin")).expect("bundle directories");
        fs::write(
            root.join(BUNDLE_DIR).join(MANIFEST_FILE),
            format!(r#"{{"marker":"{marker}"}}"#),
        )
        .expect("manifest");
        fs::write(root.join(BUNDLE_DIR).join("bin").join("brain"), b"brain").expect("brain");
        fs::write(root.join(SUPERVISOR_FILE), b"supervisor").expect("supervisor");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                root.join(SUPERVISOR_FILE),
                fs::Permissions::from_mode(0o755),
            )
            .expect("executable supervisor");
            fs::set_permissions(
                root.join(BUNDLE_DIR).join("bin").join("brain"),
                fs::Permissions::from_mode(0o755),
            )
            .expect("executable brain");
        }
    }

    fn archive(temp: &tempfile::TempDir, marker: &str) -> (PathBuf, PathBuf, ReleaseId) {
        let source = temp.path().join(format!("source-{marker}"));
        fs::create_dir(&source).expect("source");
        release(&source, marker);
        let archive = temp.path().join(format!("{marker}.build.phoxal"));
        let checksum = temp.path().join(format!("{marker}.sha256"));
        let id = create_archive(&source, &archive, &checksum).expect("archive");
        (archive, checksum, id)
    }

    #[test]
    fn artifact_only_procedure_covers_lifecycle_logs_failed_activation_and_recovery() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let installation = Installation::open(temp.path().join("installation")).expect("store");
        let (first_archive, first_checksum, first) = archive(&temp, "first");
        let (second_archive, second_checksum, second) = archive(&temp, "second");
        let control = FixtureControl::new();
        let mut operations = DeploymentOperations::new(installation, control);
        let identity = DeploymentIdentity::new("workshop", "rover-01").expect("identity");

        let first_installed = operations
            .install(
                &first_archive,
                &first_checksum,
                IdentityUpdate::Set(identity.clone()),
            )
            .expect("first install");
        assert_eq!(first_installed.id, first);
        assert_eq!(
            operations.status().expect("initial status").active_release,
            None
        );
        assert_eq!(operations.control().calls, ["link", "reload", "status"]);
        let expected_unit = operations.installation().root().join(SERVICE_UNIT_FILE);
        assert_eq!(
            operations.control().linked_unit.as_deref(),
            Some(expected_unit.as_path())
        );
        assert_eq!(
            crate::read_systemd_identity(
                operations
                    .installation()
                    .root()
                    .join(crate::SERVICE_UNIT_FILE),
            )
            .expect("identity"),
            Some(identity)
        );

        operations
            .install(&second_archive, &second_checksum, IdentityUpdate::Retain)
            .expect("second install");
        operations.activate_and_start(&first).expect("first start");
        assert_eq!(
            operations.status().expect("first status").active_release,
            Some(first.clone())
        );
        assert!(
            operations
                .status()
                .expect("first process")
                .process
                .process_ready()
        );

        operations.control_mut().fail_next_start = true;
        let failure = operations
            .activate_and_start(&second)
            .expect_err("second activation must fail");
        let OperationsError::ActivationFailed {
            attempted,
            previous,
            status,
            ..
        } = failure
        else {
            panic!("unexpected error");
        };
        assert_eq!(attempted, second);
        assert_eq!(previous, Some(first.clone()));
        assert_eq!(status, None);
        assert_eq!(
            operations.status().expect("failed status").active_release,
            Some(second)
        );
        assert_eq!(
            operations.status().expect("failed process").process.state,
            ProcessState::Failed
        );

        let logs = operations
            .logs(LogQuery::new(20, 256).expect("log query"))
            .expect("logs");
        assert!(String::from_utf8_lossy(&logs.bytes).contains("supervisor started"));
        operations
            .rollback_and_start(&first)
            .expect("explicit recovery");
        assert_eq!(
            operations
                .status()
                .expect("recovered status")
                .active_release,
            Some(first)
        );
        assert!(
            operations
                .status()
                .expect("recovered process")
                .process
                .process_ready()
        );
        assert_eq!(
            operations.control().calls,
            [
                "link", "reload", "status", "link", "reload", "stop", "start", "status", "status",
                "status", "stop", "start", "status", "status", "logs", "stop", "start", "status",
                "status", "status",
            ]
        );
    }

    #[test]
    fn systemd_status_is_process_only_and_log_output_is_bounded() {
        let status =
            parse_systemd_status(SERVICE_UNIT_FILE, b"active\nrunning\nsuccess\n").expect("status");
        assert_eq!(status.state, ProcessState::Active);
        assert!(status.process_ready());
        assert!(matches!(
            parse_systemd_status(SERVICE_UNIT_FILE, b"active\n").expect_err("malformed"),
            ServiceControlError::MalformedStatus { unit, output }
                if unit == SERVICE_UNIT_FILE && output == "active\n"
        ));
        assert!(matches!(
            LogQuery::new(0, 10),
            Err(ServiceControlError::InvalidLogQuery { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn systemd_fixture_covers_native_start_stop_status_and_logs_commands() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("temporary directory");
        let calls = temp.path().join("calls");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\nif [ \"$1\" = \"show\" ]; then printf 'active\\nrunning\\nsuccess\\n'; fi\nif [ \"$1\" = \"--unit\" ]; then printf 'fixture log\\n'; fi\n",
            calls.display()
        );
        let systemctl = temp.path().join("systemctl");
        let journalctl = temp.path().join("journalctl");
        for command in [&systemctl, &journalctl] {
            fs::write(command, &script).expect("fixture command");
            fs::set_permissions(command, fs::Permissions::from_mode(0o755))
                .expect("fixture executable");
        }
        let mut service = SystemdService::with_commands(SERVICE_UNIT_FILE, systemctl, journalctl)
            .expect("service");
        let unit_path = temp.path().join("installation").join(SERVICE_UNIT_FILE);
        service.link(&unit_path).expect("link");
        service.reload().expect("reload");
        service.start().expect("start");
        assert!(service.status().expect("status").process_ready());
        assert_eq!(
            service
                .logs(LogQuery::new(4, 128).expect("log query"))
                .expect("logs")
                .bytes,
            b"fixture log\n"
        );
        service.stop().expect("stop");
        assert_eq!(
            fs::read_to_string(calls).expect("calls"),
            format!(
                "link\n{}\ndaemon-reload\nstart\n{SERVICE_UNIT_FILE}\nshow\n--property=ActiveState\n--property=SubState\n--property=Result\n--value\n{SERVICE_UNIT_FILE}\n--unit\n{SERVICE_UNIT_FILE}\n--no-pager\n--output=cat\n--lines\n4\nstop\n{SERVICE_UNIT_FILE}\n",
                unit_path.display()
            )
        );
    }

    #[test]
    fn first_failed_activation_can_be_deactivated_without_removing_the_release() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let installation = Installation::open(temp.path().join("installation")).expect("store");
        let (archive, checksum, id) = archive(&temp, "first");
        let mut operations = DeploymentOperations::new(installation, FixtureControl::new());
        operations
            .install(
                &archive,
                &checksum,
                IdentityUpdate::Set(
                    DeploymentIdentity::new("workshop", "rover-01").expect("identity"),
                ),
            )
            .expect("install");
        operations.control_mut().fail_next_start = true;
        let failure = operations
            .activate_and_start(&id)
            .expect_err("activation must fail");
        assert!(matches!(
            failure,
            OperationsError::ActivationFailed { previous: None, .. }
        ));
        assert_eq!(
            operations.deactivate().expect("deactivate"),
            Some(id.clone())
        );
        assert_eq!(operations.installation().active_id().expect("active"), None);
        assert!(operations.installation().release_path(&id).is_dir());
    }
}
