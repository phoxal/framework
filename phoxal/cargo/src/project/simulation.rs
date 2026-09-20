//! Independent simulator provisioning and local finite-run orchestration.
//!
//! A simulator is an application selected outside the robot Cargo graph. This
//! module keeps its own small Cargo project and lockfile, probes the native
//! application's explicit model facts, then asks the prepared robot project
//! to assemble a simulation bundle.

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::project::file_lock::ExclusiveFileLock;
use cargo_metadata::{Message, MetadataCommand};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::project::cargo::{CargoOptions, PHOXAL_REGISTRY_INDEX};
use crate::project::{CompiledBundle, Error, Project, SimulationModelFacts};
use phoxal::scenario::Program;

/// The official independently installed native simulation application.
pub const DEFAULT_SIMULATOR_PACKAGE: &str = "phoxal-simulator";
/// The first simulator package release selected by the framework tool.
pub const DEFAULT_SIMULATOR_VERSION: &str = "0.0.0-dev.1";
/// The binary target exposed by the official simulator package.
pub const DEFAULT_SIMULATOR_BINARY: &str = "phoxal-simulator";
const SELECTION_FILE: &str = "selection.json";
const PROVISION_LOCK: &str = "provision.lock";
const SIMULATOR_MANIFEST: &str = "Cargo.toml";
const SIMULATOR_LOCK: &str = "Cargo.lock";
const BUILD_ROOT: &str = "build";
const ARTIFACT_ROOT: &str = "artifacts";
const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);
const PROCESS_POLL: Duration = Duration::from_millis(25);
const MANAGED_MARKER: &str = ".managed-by-cargo-phoxal";
const MUJOCO_VERSION: &str = "3.12.0";
#[cfg(target_os = "macos")]
const MUJOCO_MACOS_URL: &str = "https://github.com/google-deepmind/mujoco/releases/download/3.12.0/mujoco-3.12.0-macos-universal2.dmg";
#[cfg(target_os = "macos")]
const MUJOCO_MACOS_SHA256: &str =
    "8410882d724c3637b935dc0482b0de90efed44b17bbcfcb4a165ee46285a4865";
#[cfg(target_os = "linux")]
const MUJOCO_LINUX_X86_64_URL: &str = "https://github.com/google-deepmind/mujoco/releases/download/3.12.0/mujoco-3.12.0-linux-x86_64.tar.gz";
#[cfg(target_os = "linux")]
const MUJOCO_LINUX_X86_64_SHA256: &str =
    "a9367911e6d5eaeade17c2197304687421c1fc932cdf7bcd4cb8cfaf0374dcb2";
#[cfg(target_os = "linux")]
const MUJOCO_LINUX_AARCH64_URL: &str = "https://github.com/google-deepmind/mujoco/releases/download/3.12.0/mujoco-3.12.0-linux-aarch64.tar.gz";
#[cfg(target_os = "linux")]
const MUJOCO_LINUX_AARCH64_SHA256: &str =
    "08fd5627a2ef7d5a42580c40e014ab2c1a644f082010c584ca361a3ed8cad838";

/// The presentation selected for a finite simulation run.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SimulationPresentation {
    /// Run without creating a presentation window.
    Headless,
    /// Open the simulator's interactive presentation.
    Desktop,
}

impl SimulationPresentation {
    /// The simulator command-line spelling.
    #[must_use]
    pub const fn flag(self) -> &'static str {
        match self {
            Self::Headless => "--headless",
            Self::Desktop => "--desktop",
        }
    }
}

/// One positive finite bound for a simulation command.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum SimulationBound {
    /// Advance exactly this many native quanta.
    Steps(u64),
    /// Advance exactly this many seconds after native quantum validation.
    Duration(f64),
}

impl SimulationBound {
    fn validate(self) -> Result<(), Error> {
        match self {
            Self::Steps(steps) if steps > 0 => Ok(()),
            Self::Steps(_) => Err(simulation_error("steps must be a positive integer")),
            Self::Duration(duration) if duration.is_finite() && duration > 0.0 => Ok(()),
            Self::Duration(_) => Err(simulation_error(
                "duration must be a positive finite number",
            )),
        }
    }

    fn validate_for_quantum(self, quantum_ns: u64) -> Result<(), Error> {
        if quantum_ns == 0 {
            return Err(simulation_error("simulation quantum must be positive"));
        }
        match self {
            Self::Steps(_) => Ok(()),
            Self::Duration(duration) => {
                let duration_ns = duration * 1_000_000_000.0;
                let quanta = duration_ns / quantum_ns as f64;
                let nearest = quanta.round();
                let error = (quanta - nearest).abs();
                let tolerance = f64::EPSILON * quanta.abs().max(1.0) * 16.0;
                if !quanta.is_finite() || nearest < 1.0 || error > tolerance {
                    return Err(simulation_error(format!(
                        "duration {duration} seconds is not an integral number of {quantum_ns}ns simulation quanta"
                    )));
                }
                Ok(())
            }
        }
    }
}

/// Inputs for one local simulation command.
#[derive(Clone, Debug, PartialEq)]
pub struct SimulationRunOptions {
    scene: PathBuf,
    presentation: SimulationPresentation,
    bound: SimulationBound,
    simulator_executable: Option<PathBuf>,
    simulator_package: String,
    simulator_version: String,
    simulator_binary: String,
    output: Option<PathBuf>,
    scope: String,
    supervisor_id: String,
    run_id: String,
    startup_timeout: Duration,
    cleanup_timeout: Duration,
    auto_run: bool,
}

impl SimulationRunOptions {
    /// Construct a request using the official simulator selection.
    pub fn new(
        scene: impl Into<PathBuf>,
        presentation: SimulationPresentation,
        bound: SimulationBound,
    ) -> Result<Self, Error> {
        bound.validate()?;
        Ok(Self {
            scene: scene.into(),
            presentation,
            bound,
            simulator_executable: None,
            simulator_package: DEFAULT_SIMULATOR_PACKAGE.to_owned(),
            simulator_version: DEFAULT_SIMULATOR_VERSION.to_owned(),
            simulator_binary: DEFAULT_SIMULATOR_BINARY.to_owned(),
            output: None,
            scope: "local".to_owned(),
            supervisor_id: "local".to_owned(),
            run_id: "local-simulation".to_owned(),
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
            cleanup_timeout: DEFAULT_CLEANUP_TIMEOUT,
            auto_run: false,
        })
    }

    /// The scene path supplied by the caller.
    #[must_use]
    pub fn scene(&self) -> &Path {
        &self.scene
    }

    /// Use an explicitly selected simulator executable.
    ///
    /// This is the injection seam for local development and deterministic
    /// process fixtures. The executable is checked as a regular file before
    /// it is launched and its digest is retained in the run report.
    #[must_use]
    pub fn with_simulator_executable(mut self, path: impl Into<PathBuf>) -> Self {
        self.simulator_executable = Some(path.into());
        self
    }

    /// Put the immutable robot simulation bundle at an explicit path.
    #[must_use]
    pub fn with_output(mut self, path: impl Into<PathBuf>) -> Self {
        self.output = Some(path.into());
        self
    }

    /// Set the explicit router namespace, supervisor identity, and run id.
    #[must_use]
    pub fn with_identity(
        mut self,
        scope: impl Into<String>,
        supervisor_id: impl Into<String>,
        run_id: impl Into<String>,
    ) -> Self {
        self.scope = scope.into();
        self.supervisor_id = supervisor_id.into();
        self.run_id = run_id.into();
        self
    }

    /// Start advancing immediately when the desktop presentation becomes ready.
    #[must_use]
    pub const fn with_auto_run(mut self) -> Self {
        self.auto_run = true;
        self
    }
}

/// The exact simulator artifact selected for a run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SimulatorArtifactSummary {
    /// Cargo package name.
    pub package: String,
    /// Exact Cargo package version.
    pub version: String,
    /// Selected binary target.
    pub binary: String,
    /// Acquisition source, registry or explicit injected path.
    pub source: String,
    /// Absolute executable path used by the launcher.
    pub executable: PathBuf,
    /// SHA-256 of the executable bytes.
    pub sha256: String,
    /// Exact standalone application Cargo.toml digest, when provisioned.
    pub cargo_manifest_sha256: Option<String>,
    /// Exact independent application Cargo.lock digest, when provisioned.
    pub cargo_lock_sha256: Option<String>,
}

/// Bounded cleanup evidence for a local simulation process pair.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SimulationCleanup {
    /// Whether the supervisor was asked to stop.
    pub supervisor_stop_requested: bool,
    /// Whether the supervisor exited before the cleanup deadline.
    pub supervisor_exited: bool,
    /// Whether a forced kill was required.
    pub supervisor_killed: bool,
    /// Human-readable cleanup diagnostic, if cleanup was incomplete.
    pub error: Option<String>,
}

/// Terminal evidence retained by one local finite simulation run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SimulationRunReport {
    /// Summary schema identifier.
    pub schema: String,
    /// Canonical scene resource path.
    pub scene: PathBuf,
    /// Exact simulator artifact used.
    pub simulator: SimulatorArtifactSummary,
    /// Compiled simulation bundle path.
    pub bundle: PathBuf,
    /// Explicit router namespace.
    pub scope: String,
    /// Explicit supervisor identity.
    pub supervisor_id: String,
    /// Explicit finite run identity.
    pub run_id: String,
    /// Whether the supervisor survived startup readiness.
    pub supervisor_ready: bool,
    /// Whether the simulator emitted the required provider-contract terminal
    /// evidence for the finite run.
    pub provider_contract_verified: bool,
    /// Simulator exit code, or none when it terminated by signal.
    pub simulator_exit_code: Option<i32>,
    /// Wall time spent inside the simulator process for this finite run.
    pub simulator_wall_time_ns: u64,
    /// Complete simulator standard output.
    pub simulator_stdout: String,
    /// Complete simulator standard error.
    pub simulator_stderr: String,
    /// Bounded supervisor cleanup evidence.
    pub cleanup: SimulationCleanup,
    /// Runtime-observed scenario evidence, when this was a scenario run.
    pub scenario: Option<ScenarioExecutionReport>,
    /// Parsed terminal evidence emitted by the native simulator.
    pub terminal: Option<SimulatorTerminalEvidence>,
}

/// Evidence observed by the supervisor while executing one scenario program.
///
/// Re-exported from `phoxal::artifact::simulation`. The framework module is
/// the source of truth; this alias keeps every existing
/// internal call site compiling unchanged.
pub use phoxal::artifact::simulation::ScenarioExecutionReport;

/// One runtime-acknowledged scenario action.
///
/// Re-exported from `phoxal::artifact::simulation`.
#[allow(unused_imports)]
pub use phoxal::artifact::simulation::ScenarioStepEvidence;

/// One typed capture stream drained by the supervisor.
///
/// Re-exported from `phoxal::artifact::simulation`.
#[allow(unused_imports)]
pub use phoxal::artifact::simulation::ScenarioCaptureEvidence;

impl SimulationRunReport {
    /// Whether the simulator exited successfully and cleanup completed.
    #[must_use]
    pub fn success(&self) -> bool {
        self.simulator_exit_code == Some(0)
            && self.cleanup.error.is_none()
            && self.supervisor_ready
            && self.provider_contract_verified
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SimulatorSelection {
    schema: String,
    package: String,
    version: String,
    binary: String,
    source: String,
    executable: PathBuf,
    cargo_manifest: Option<PathBuf>,
    cargo_lock: Option<PathBuf>,
    executable_bytes: u64,
    executable_sha256: String,
    cargo_manifest_sha256: Option<String>,
    cargo_lock_sha256: Option<String>,
}

/// Result of inspecting the managed simulator installation.
#[derive(Debug, Clone, Serialize)]
pub struct SimulatorInstallationStatus {
    /// Whether a valid managed selection is installed.
    pub installed: bool,
    /// Managed installation root.
    pub root: PathBuf,
    /// Installed simulator package version.
    pub simulator_version: Option<String>,
    /// Managed MuJoCo version.
    pub mujoco_version: Option<String>,
    /// Selected simulator executable.
    pub executable: Option<PathBuf>,
}

#[derive(Debug)]
struct SimulatorArtifact {
    summary: SimulatorArtifactSummary,
    cargo_manifest: Option<PathBuf>,
    cargo_lock: Option<PathBuf>,
}

impl SimulatorArtifact {
    fn from_selection(selection: SimulatorSelection) -> Result<Self, Error> {
        if selection.schema != "phoxal/simulator-selection/v0" {
            return Err(simulation_error(format!(
                "unsupported simulator selection schema {}",
                selection.schema
            )));
        }
        ensure_regular_file(&selection.executable, "simulator executable")?;
        let digest = digest_file(&selection.executable)?;
        if digest.bytes != selection.executable_bytes
            || digest.sha256 != selection.executable_sha256
        {
            return Err(simulation_error(format!(
                "selected simulator executable {} changed after provisioning",
                selection.executable.display()
            )));
        }
        match (&selection.cargo_manifest, &selection.cargo_lock) {
            (Some(manifest), Some(lock)) => {
                ensure_regular_file(manifest, "simulator Cargo.toml")?;
                let actual = digest_file(manifest)?.sha256;
                if selection.cargo_manifest_sha256.as_deref() != Some(actual.as_str()) {
                    return Err(simulation_error(format!(
                        "selected simulator Cargo.toml {} changed after provisioning",
                        manifest.display()
                    )));
                }
                ensure_regular_file(lock, "simulator Cargo.lock")?;
                let actual = digest_file(lock)?.sha256;
                if selection.cargo_lock_sha256.as_deref() != Some(actual.as_str()) {
                    return Err(simulation_error(format!(
                        "selected simulator Cargo.lock {} changed after provisioning",
                        lock.display()
                    )));
                }
            }
            (None, None) => {
                if selection.source.starts_with("registry:") {
                    return Err(simulation_error(
                        "registry simulator selection is missing its standalone Cargo graph",
                    ));
                }
            }
            _ => {
                return Err(simulation_error(
                    "simulator selection must retain both Cargo.toml and Cargo.lock together",
                ));
            }
        }
        Ok(Self {
            summary: SimulatorArtifactSummary {
                package: selection.package,
                version: selection.version,
                binary: selection.binary,
                source: selection.source,
                executable: selection.executable,
                sha256: digest.sha256,
                cargo_manifest_sha256: selection.cargo_manifest_sha256,
                cargo_lock_sha256: selection.cargo_lock_sha256,
            },
            cargo_manifest: selection.cargo_manifest,
            cargo_lock: selection.cargo_lock,
        })
    }
}

/// Run one finite simulation from a prepared robot source project.
pub(crate) fn run(
    project: &Project,
    cargo_options: &CargoOptions,
    request: &SimulationRunOptions,
    scenario: Option<&Program>,
) -> Result<SimulationRunReport, Error> {
    cargo_options.validate()?;
    validate_request(request)?;

    // Provisioning happens before Project::prepare. A missing application in
    // locked or frozen mode therefore fails before the robot Cargo manifest or
    // its owning Cargo.lock can be changed by automatic supervisor setup.
    let scene = canonical_scene(request.scene())?;
    let simulator = provision(request)?;
    let prepared = project.prepare(cargo_options)?;
    let probe_output = probe_bundle_path(&prepared);
    let probe_bundle = match scenario {
        Some(program) => {
            prepared.build_scenario_probe_bundle(cargo_options, &probe_output, program)?
        }
        None => prepared.build_bundle(cargo_options, &probe_output)?,
    };
    let facts = probe(&simulator, &scene, probe_bundle.root(), request)?;
    request.bound.validate_for_quantum(facts.quantum_ns)?;
    let output = request.output.clone().unwrap_or_else(|| {
        prepared
            .default_bundle_path()
            .with_file_name("simulation-bundle")
    });
    let bundle = match scenario {
        Some(program) => {
            prepared.build_scenario_simulation_bundle(cargo_options, &output, &facts, program)?
        }
        None => prepared.build_simulation_bundle(cargo_options, &output, &facts)?,
    };
    launch(&simulator, &bundle, &scene, request, scenario.is_some())
}

/// Run only the simulator probe step. Used by the case-host protocol
/// (plan §9) so the tool can hand the probed quantum to the harness
/// before the harness builds its `Program`. The bundle that ships
/// with the probe is built from the project's regular target (no
/// scenario program embedded) — the lifecycle step that follows the
/// probe uses the harness's program, not this probe bundle.
pub fn probe_simulation_scene(
    project: &Project,
    cargo_options: &CargoOptions,
    request: &SimulationRunOptions,
) -> Result<SimulationModelFacts, Error> {
    cargo_options.validate()?;
    validate_request(request)?;
    let scene = canonical_scene(request.scene())?;
    let simulator = provision(request)?;
    let prepared = project.prepare(cargo_options)?;
    let probe_output = probe_bundle_path(&prepared);
    let probe_bundle = prepared.build_bundle(cargo_options, &probe_output)?;
    probe(&simulator, &scene, probe_bundle.root(), request)
}

fn validate_request(request: &SimulationRunOptions) -> Result<(), Error> {
    request.bound.validate()?;
    validate_identity_part("scope", &request.scope)?;
    validate_identity_part("supervisor_id", &request.supervisor_id)?;
    validate_identity_part("run_id", &request.run_id)?;
    if request.startup_timeout.is_zero() || request.cleanup_timeout.is_zero() {
        return Err(simulation_error(
            "simulation startup and cleanup timeouts must be positive",
        ));
    }
    if request.simulator_package.is_empty()
        || request.simulator_version.is_empty()
        || request.simulator_binary.is_empty()
    {
        return Err(simulation_error(
            "simulator package, version, and binary must be non-empty",
        ));
    }
    Ok(())
}

fn validate_identity_part(field: &str, value: &str) -> Result<(), Error> {
    if value.is_empty()
        || value.len() > 128
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        return Err(simulation_error(format!(
            "{field} must be 1-128 lowercase ASCII letters, digits, '-' or '_'"
        )));
    }
    Ok(())
}

fn canonical_scene(path: &Path) -> Result<PathBuf, Error> {
    let metadata = fs::symlink_metadata(path).map_err(|source| Error::ArtifactFile {
        path: path.to_owned(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(simulation_error(format!(
            "simulation scene {} must be a regular non-symlink file",
            path.display()
        )));
    }
    let extension = path.extension().and_then(OsStr::to_str).unwrap_or_default();
    if !matches!(extension, "xml" | "mjz") {
        return Err(simulation_error(format!(
            "simulation scene {} must end in .xml or .mjz",
            path.display()
        )));
    }
    path.canonicalize().map_err(|source| Error::ArtifactFile {
        path: path.to_owned(),
        source,
    })
}

fn probe_bundle_path(prepared: &crate::PreparedProject) -> PathBuf {
    prepared
        .default_bundle_path()
        .with_file_name("simulation-probe-bundle")
}

fn provision(request: &SimulationRunOptions) -> Result<SimulatorArtifact, Error> {
    if let Some(path) = &request.simulator_executable {
        ensure_regular_file(path, "explicit simulator executable")?;
        let digest = digest_file(path)?;
        return Ok(SimulatorArtifact {
            summary: SimulatorArtifactSummary {
                package: request.simulator_package.clone(),
                version: request.simulator_version.clone(),
                binary: request.simulator_binary.clone(),
                source: "explicit-executable".to_owned(),
                executable: path.canonicalize().map_err(|source| Error::ArtifactFile {
                    path: path.clone(),
                    source,
                })?,
                sha256: digest.sha256,
                cargo_manifest_sha256: None,
                cargo_lock_sha256: None,
            },
            cargo_manifest: None,
            cargo_lock: None,
        });
    }

    let root = simulator_store_root()?;
    provision_at(&root, request)
}

fn provision_at(root: &Path, request: &SimulationRunOptions) -> Result<SimulatorArtifact, Error> {
    let selection_path = root.join(SELECTION_FILE);
    if selection_path.is_file() {
        let selection = read_selection(&selection_path)?;
        return selected_artifact(selection, request);
    }
    Err(simulation_error(format!(
        "{} {} is not installed; run `cargo phoxal simulation install` before starting a simulation or pass --simulator",
        request.simulator_package, request.simulator_version
    )))
}

fn selected_artifact(
    selection: SimulatorSelection,
    request: &SimulationRunOptions,
) -> Result<SimulatorArtifact, Error> {
    if selection.package != request.simulator_package
        || selection.version != request.simulator_version
        || selection.binary != request.simulator_binary
    {
        return Err(simulation_error(format!(
            "stored simulator selection is {} {} {}, but this run requests {} {} {}",
            selection.package,
            selection.version,
            selection.binary,
            request.simulator_package,
            request.simulator_version,
            request.simulator_binary
        )));
    }
    SimulatorArtifact::from_selection(selection)
}

fn simulator_store_root() -> Result<PathBuf, Error> {
    if let Some(root) = std::env::var_os("PHOXAL_HOME") {
        if root.is_empty() {
            return Err(simulation_error("PHOXAL_HOME cannot be empty"));
        }
        return Ok(PathBuf::from(root).join("simulation"));
    }
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            simulation_error("cannot locate the user data directory; set PHOXAL_HOME")
        })?;
    #[cfg(target_os = "macos")]
    {
        Ok(PathBuf::from(home).join("Library/Application Support/Phoxal/simulation"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let data = std::env::var_os("XDG_DATA_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(home).join(".local/share"));
        Ok(data.join("phoxal/simulation"))
    }
}

/// Install or replace the managed MuJoCo distribution and simulator application.
pub fn install_simulator(
    options: &CargoOptions,
    mujoco_distribution: Option<&Path>,
    replace: bool,
) -> Result<SimulatorInstallationStatus, Error> {
    let root = simulator_store_root()?;
    let parent = root
        .parent()
        .ok_or_else(|| simulation_error("managed simulator root has no parent"))?;
    fs::create_dir_all(parent).map_err(|source| Error::ArtifactFile {
        path: parent.to_owned(),
        source,
    })?;
    let lock_path = parent.join(PROVISION_LOCK);
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|source| Error::ArtifactFile {
            path: lock_path.clone(),
            source,
        })?;
    let _lock = ExclusiveFileLock::try_acquire(lock).map_err(|error| {
        simulation_error(format!(
            "cannot acquire simulator installation lock {}: {error}",
            lock_path.display()
        ))
    })?;

    if root.exists() {
        if !root.join(MANAGED_MARKER).is_file() {
            return Err(simulation_error(format!(
                "refusing to replace unmanaged directory {}",
                root.display()
            )));
        }
        if !replace && root.join(SELECTION_FILE).is_file() {
            return simulator_status_at(&root);
        }
        fs::remove_dir_all(&root).map_err(|source| Error::ArtifactFile {
            path: root.clone(),
            source,
        })?;
    }
    fs::create_dir_all(&root).map_err(|source| Error::ArtifactFile {
        path: root.clone(),
        source,
    })?;
    fs::write(
        root.join(MANAGED_MARKER),
        "This directory is managed by cargo phoxal simulation install.\n",
    )
    .map_err(|source| Error::ArtifactFile {
        path: root.join(MANAGED_MARKER),
        source,
    })?;

    let request = SimulationRunOptions::new(
        PathBuf::from("managed-installation-placeholder.xml"),
        SimulationPresentation::Headless,
        SimulationBound::Steps(1),
    )?;
    let artifact = install_for_platform(&root, options, &request, mujoco_distribution)?;
    for transient in [root.join(BUILD_ROOT), root.join("native-link")] {
        if transient.is_dir() {
            fs::remove_dir_all(&transient).map_err(|source| Error::ArtifactFile {
                path: transient,
                source,
            })?;
        }
    }
    write_selection(&root, &artifact)?;
    simulator_status_at(&root)
}

/// Inspect the managed simulator without modifying it.
pub fn simulator_status() -> Result<SimulatorInstallationStatus, Error> {
    let root = simulator_store_root()?;
    simulator_status_at(&root)
}

fn simulator_status_at(root: &Path) -> Result<SimulatorInstallationStatus, Error> {
    let selection_path = root.join(SELECTION_FILE);
    if !selection_path.is_file() {
        return Ok(SimulatorInstallationStatus {
            installed: false,
            root: root.to_owned(),
            simulator_version: None,
            mujoco_version: None,
            executable: None,
        });
    }
    let selection = read_selection(&selection_path)?;
    let artifact = SimulatorArtifact::from_selection(selection)?;
    Ok(SimulatorInstallationStatus {
        installed: true,
        root: root.to_owned(),
        simulator_version: Some(artifact.summary.version),
        mujoco_version: Some(MUJOCO_VERSION.to_owned()),
        executable: Some(artifact.summary.executable),
    })
}

/// Remove the exact managed simulator installation.
pub fn uninstall_simulator() -> Result<PathBuf, Error> {
    let root = simulator_store_root()?;
    uninstall_simulator_at(&root)
}

fn uninstall_simulator_at(root: &Path) -> Result<PathBuf, Error> {
    if !root.exists() {
        return Ok(root.to_owned());
    }
    if !root.join(MANAGED_MARKER).is_file() {
        return Err(simulation_error(format!(
            "refusing to remove unmanaged directory {}",
            root.display()
        )));
    }
    fs::remove_dir_all(root).map_err(|source| Error::ArtifactFile {
        path: root.to_owned(),
        source,
    })?;
    Ok(root.to_owned())
}

fn write_selection(root: &Path, artifact: &SimulatorArtifact) -> Result<(), Error> {
    let executable_metadata =
        fs::metadata(&artifact.summary.executable).map_err(|source| Error::ArtifactFile {
            path: artifact.summary.executable.clone(),
            source,
        })?;
    let selection = SimulatorSelection {
        schema: "phoxal/simulator-selection/v0".to_owned(),
        package: artifact.summary.package.clone(),
        version: artifact.summary.version.clone(),
        binary: artifact.summary.binary.clone(),
        source: artifact.summary.source.clone(),
        executable: artifact.summary.executable.clone(),
        cargo_manifest: artifact.cargo_manifest.clone(),
        cargo_lock: artifact.cargo_lock.clone(),
        executable_bytes: executable_metadata.len(),
        executable_sha256: artifact.summary.sha256.clone(),
        cargo_manifest_sha256: artifact.summary.cargo_manifest_sha256.clone(),
        cargo_lock_sha256: artifact.summary.cargo_lock_sha256.clone(),
    };
    atomic_json(&root.join(SELECTION_FILE), &selection)
}

fn install_for_platform(
    root: &Path,
    options: &CargoOptions,
    request: &SimulationRunOptions,
    supplied_distribution: Option<&Path>,
) -> Result<SimulatorArtifact, Error> {
    #[cfg(target_os = "macos")]
    {
        install_macos(root, options, request, supplied_distribution)
    }
    #[cfg(target_os = "linux")]
    {
        return install_linux(root, options, request, supplied_distribution);
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (root, options, request, supplied_distribution);
        Err(simulation_error(
            "managed simulator installation currently supports macOS and Linux",
        ))
    }
}

#[cfg(target_os = "macos")]
fn install_macos(
    root: &Path,
    options: &CargoOptions,
    request: &SimulationRunOptions,
    supplied_distribution: Option<&Path>,
) -> Result<SimulatorArtifact, Error> {
    if let Some(distribution) = supplied_distribution {
        let distribution = distribution
            .canonicalize()
            .map_err(|source| Error::ArtifactFile {
                path: distribution.to_owned(),
                source,
            })?;
        return build_and_package_macos(root, options, request, &distribution);
    }
    if options.offline {
        return Err(simulation_error(
            "offline installation requires --mujoco-distribution",
        ));
    }
    let download = download_asset(MUJOCO_MACOS_URL, MUJOCO_MACOS_SHA256, "mujoco.dmg")?;
    let mount = tempfile::Builder::new()
        .prefix("phoxal-mujoco-mount-")
        .tempdir()
        .map_err(|source| Error::ArtifactFile {
            path: root.to_owned(),
            source,
        })?;
    run_command(
        Command::new("hdiutil")
            .args(["attach", "-readonly", "-nobrowse", "-mountpoint"])
            .arg(mount.path())
            .arg(&download.path),
        "mount MuJoCo disk image",
    )?;
    let result = build_and_package_macos(root, options, request, mount.path());
    let detach = run_command(
        Command::new("hdiutil").arg("detach").arg(mount.path()),
        "detach MuJoCo disk image",
    );
    match (result, detach) {
        (Ok(artifact), Ok(())) => Ok(artifact),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

#[cfg(target_os = "macos")]
fn build_and_package_macos(
    root: &Path,
    options: &CargoOptions,
    request: &SimulationRunOptions,
    distribution: &Path,
) -> Result<SimulatorArtifact, Error> {
    let framework = distribution.join("mujoco.framework/Versions/A");
    let native = framework.join(format!("libmujoco.{MUJOCO_VERSION}.dylib"));
    for required in [
        native.as_path(),
        &distribution.join("LICENSE"),
        &distribution.join("THIRD_PARTY_NOTICES"),
    ] {
        ensure_regular_file(required, "MuJoCo distribution file")?;
    }
    let link = root.join("native-link");
    fs::create_dir_all(&link).map_err(|source| Error::ArtifactFile {
        path: link.clone(),
        source,
    })?;
    copy_regular(&native, &link.join("libmujoco.dylib"))?;
    let artifact = build_registry_simulator(root, options, request, &link, None)?;
    package_macos(root, artifact, distribution, &native)
}

#[cfg(target_os = "linux")]
fn install_linux(
    root: &Path,
    options: &CargoOptions,
    request: &SimulationRunOptions,
    supplied_distribution: Option<&Path>,
) -> Result<SimulatorArtifact, Error> {
    let distribution = if let Some(distribution) = supplied_distribution {
        distribution
            .canonicalize()
            .map_err(|source| Error::ArtifactFile {
                path: distribution.to_owned(),
                source,
            })?
    } else {
        if options.offline {
            return Err(simulation_error(
                "offline installation requires --mujoco-distribution",
            ));
        }
        let (url, checksum) = match std::env::consts::ARCH {
            "x86_64" => (MUJOCO_LINUX_X86_64_URL, MUJOCO_LINUX_X86_64_SHA256),
            "aarch64" => (MUJOCO_LINUX_AARCH64_URL, MUJOCO_LINUX_AARCH64_SHA256),
            architecture => {
                return Err(simulation_error(format!(
                    "MuJoCo {MUJOCO_VERSION} has no managed Linux asset for {architecture}"
                )));
            }
        };
        let download = download_asset(url, checksum, "mujoco.tar.gz")?;
        let native_root = root.join("native");
        fs::create_dir_all(&native_root).map_err(|source| Error::ArtifactFile {
            path: native_root.clone(),
            source,
        })?;
        let archive_file = File::open(&download.path).map_err(|source| Error::ArtifactFile {
            path: download.path.clone(),
            source,
        })?;
        let decoder = flate2::read::GzDecoder::new(archive_file);
        let mut archive = tar::Archive::new(decoder);
        archive
            .unpack(&native_root)
            .map_err(|source| Error::ArtifactFile {
                path: native_root.clone(),
                source,
            })?;
        native_root.join(format!("mujoco-{MUJOCO_VERSION}"))
    };
    let link = distribution.join("lib");
    ensure_regular_file(
        &link.join(format!("libmujoco.so.{MUJOCO_VERSION}")),
        "MuJoCo shared library",
    )?;
    build_registry_simulator(root, options, request, &link, Some(&link))
}

struct DownloadedAsset {
    _directory: tempfile::TempDir,
    path: PathBuf,
}

fn download_asset(url: &str, expected_sha256: &str, name: &str) -> Result<DownloadedAsset, Error> {
    let directory = tempfile::Builder::new()
        .prefix("phoxal-mujoco-download-")
        .tempdir()
        .map_err(|source| Error::ArtifactFile {
            path: PathBuf::from(name),
            source,
        })?;
    let path = directory.path().join(name);
    let mut response = reqwest::blocking::get(url)
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|error| simulation_error(format!("cannot download {url}: {error}")))?;
    let mut file = File::create(&path).map_err(|source| Error::ArtifactFile {
        path: path.clone(),
        source,
    })?;
    std::io::copy(&mut response, &mut file).map_err(|source| Error::ArtifactFile {
        path: path.clone(),
        source,
    })?;
    file.sync_all().map_err(|source| Error::ArtifactFile {
        path: path.clone(),
        source,
    })?;
    let actual = digest_file(&path)?.sha256;
    if actual != expected_sha256 {
        return Err(simulation_error(format!(
            "downloaded MuJoCo asset checksum {actual} does not match {expected_sha256}"
        )));
    }
    Ok(DownloadedAsset {
        _directory: directory,
        path,
    })
}

fn read_selection(path: &Path) -> Result<SimulatorSelection, Error> {
    let bytes = fs::read(path).map_err(|source| Error::ArtifactFile {
        path: path.to_owned(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| Error::SimulationInvalid {
        message: format!(
            "cannot parse simulator selection {}: {source}",
            path.display()
        ),
    })
}

fn build_registry_simulator(
    root: &Path,
    options: &CargoOptions,
    request: &SimulationRunOptions,
    native_link_dir: &Path,
    runtime_library_dir: Option<&Path>,
) -> Result<SimulatorArtifact, Error> {
    let staging = tempfile::Builder::new()
        .prefix("phoxal-simulator-build-")
        .tempdir()
        .map_err(|source| Error::ArtifactFile {
            path: root.to_owned(),
            source,
        })?;
    let source_root = staging.path();
    let selector_manifest = source_root.join(SIMULATOR_MANIFEST);
    fs::write(&selector_manifest, simulator_manifest(request)).map_err(|source| {
        Error::ArtifactFile {
            path: selector_manifest.clone(),
            source,
        }
    })?;
    let selector_source_root = source_root.join("src");
    let selector_source = selector_source_root.join("main.rs");
    fs::create_dir_all(&selector_source_root).map_err(|source| Error::ArtifactFile {
        path: selector_source_root,
        source,
    })?;
    fs::write(&selector_source, "fn main() {}\n").map_err(|source| Error::ArtifactFile {
        path: selector_source,
        source,
    })?;
    let target_root = root.join(BUILD_ROOT);
    fs::create_dir_all(&target_root).map_err(|source| Error::ArtifactFile {
        path: target_root.clone(),
        source,
    })?;
    let selector_metadata = simulator_metadata(&selector_manifest, options, false)?;
    let package = selector_metadata
        .packages
        .iter()
        .find(|package| {
            package.name == request.simulator_package
                && package.version.to_string() == request.simulator_version
        })
        .ok_or_else(|| {
            simulation_error(format!(
                "registry did not resolve simulator package {} {}",
                request.simulator_package, request.simulator_version
            ))
        })?;
    let expected_source = format!(
        "registry+{}",
        PHOXAL_REGISTRY_INDEX
            .strip_prefix("sparse+")
            .unwrap_or(PHOXAL_REGISTRY_INDEX)
    );
    if package.source.as_ref().map(|source| source.repr.as_str()) != Some(expected_source.as_str())
    {
        return Err(simulation_error(format!(
            "simulator package {} {} did not resolve from the Phoxal registry",
            request.simulator_package, request.simulator_version
        )));
    }
    let package_root = PathBuf::from(package.manifest_path.as_std_path())
        .parent()
        .map(Path::to_owned)
        .ok_or_else(|| {
            simulation_error(format!(
                "simulator package {} has no source root",
                request.simulator_package
            ))
        })?;
    let application_root = source_root.join("application");
    copy_tree(&package_root, &application_root)?;
    let application_manifest = application_root.join(SIMULATOR_MANIFEST);
    let application_lock = application_root.join(SIMULATOR_LOCK);
    if !application_lock.is_file() {
        generate_application_lock(&application_manifest, options)?;
    }
    let metadata = simulator_metadata(&application_manifest, options, true)?;
    let package = metadata
        .packages
        .iter()
        .find(|package| {
            package.name == request.simulator_package
                && package.version.to_string() == request.simulator_version
        })
        .ok_or_else(|| {
            simulation_error(format!(
                "staged simulator source no longer resolves package {} {}",
                request.simulator_package, request.simulator_version
            ))
        })?;
    let package_id = package.id.to_string();
    let target = package
        .targets
        .iter()
        .find(|target| target.name == request.simulator_binary && target.is_bin())
        .ok_or_else(|| {
            simulation_error(format!(
                "simulator package {} {} has no binary target {}",
                request.simulator_package, request.simulator_version, request.simulator_binary
            ))
        })?;
    let mut command = Command::new(options.cargo_program());
    command.current_dir(&application_root);
    command.env("MUJOCO_DYNAMIC_LINK_DIR", native_link_dir);
    if let Some(runtime_library_dir) = runtime_library_dir {
        let inherited = std::env::var("RUSTFLAGS").unwrap_or_default();
        let rpath = format!("-C link-arg=-Wl,-rpath,{}", runtime_library_dir.display());
        command.env(
            "RUSTFLAGS",
            if inherited.is_empty() {
                rpath
            } else {
                format!("{inherited} {rpath}")
            },
        );
    }
    command.args([
        "build",
        "--manifest-path",
        &application_manifest.display().to_string(),
        "--bin",
        &target.name,
        "--target-dir",
        &target_root.display().to_string(),
        "--message-format",
        "json-render-diagnostics",
        "--config",
        &format!("registries.phoxal.index=\"{PHOXAL_REGISTRY_INDEX}\""),
    ]);
    // The standalone application owns the lock generated above.  The robot
    // lock policy controls whether this application may be provisioned, but
    // never replaces the application's own dependency graph.
    command.arg("--locked");
    if options.offline {
        command.arg("--offline");
    }
    let output = command.output().map_err(|source| Error::CargoSpawn {
        operation: "build simulator".to_owned(),
        source,
    })?;
    if !output.status.success() {
        return Err(Error::CargoCommand {
            operation: "build simulator".to_owned(),
            status: status_string(output.status),
            stdout: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    let executable = artifact_path(&output.stdout, &package_id, &target.name)?;
    ensure_regular_file(&executable, "built simulator executable")?;
    let digest = digest_file(&executable)?;
    let artifact_dir = root.join(ARTIFACT_ROOT).join(&digest.sha256);
    fs::create_dir_all(&artifact_dir).map_err(|source| Error::ArtifactFile {
        path: artifact_dir.clone(),
        source,
    })?;
    let artifact_path = artifact_dir.join(&request.simulator_binary);
    copy_regular(&executable, &artifact_path)?;
    make_executable(&artifact_path)?;
    ensure_regular_file(&application_lock, "simulator Cargo.lock")?;
    let retained_lock = artifact_dir.join(SIMULATOR_LOCK);
    copy_regular(&application_lock, &retained_lock)?;
    let retained_manifest = artifact_dir.join(SIMULATOR_MANIFEST);
    copy_regular(&application_manifest, &retained_manifest)?;
    let lock_digest = digest_file(&retained_lock)?.sha256;
    let manifest_digest = digest_file(&retained_manifest)?.sha256;
    Ok(SimulatorArtifact {
        summary: SimulatorArtifactSummary {
            package: request.simulator_package.clone(),
            version: request.simulator_version.clone(),
            binary: request.simulator_binary.clone(),
            source: format!("registry:{PHOXAL_REGISTRY_INDEX}"),
            executable: artifact_path,
            sha256: digest.sha256,
            cargo_manifest_sha256: Some(manifest_digest),
            cargo_lock_sha256: Some(lock_digest),
        },
        cargo_manifest: Some(retained_manifest),
        cargo_lock: Some(retained_lock),
    })
}

#[cfg(target_os = "macos")]
fn package_macos(
    root: &Path,
    mut artifact: SimulatorArtifact,
    distribution: &Path,
    native: &Path,
) -> Result<SimulatorArtifact, Error> {
    let app = root.join("Phoxal Simulator.app");
    let contents = app.join("Contents");
    let executable = contents.join("MacOS/phoxal-simulator");
    let resources = contents.join("Resources");
    let frameworks = contents.join("Frameworks");
    fs::create_dir_all(
        executable
            .parent()
            .ok_or_else(|| simulation_error("simulator application executable has no parent"))?,
    )
    .and_then(|()| fs::create_dir_all(&resources))
    .and_then(|()| fs::create_dir_all(&frameworks))
    .map_err(|source| Error::ArtifactFile {
        path: app.clone(),
        source,
    })?;
    copy_regular(&artifact.summary.executable, &executable)?;
    let native_name = format!("libmujoco.{MUJOCO_VERSION}.dylib");
    let bundled_native = frameworks.join(&native_name);
    copy_regular(native, &bundled_native)?;
    copy_regular(
        &distribution.join("LICENSE"),
        &resources.join("MUJOCO_LICENSE"),
    )?;
    copy_regular(
        &distribution.join("THIRD_PARTY_NOTICES"),
        &resources.join("MUJOCO_THIRD_PARTY_NOTICES"),
    )?;
    copy_regular(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../LICENSE"),
        &resources.join("PHOXAL_LICENSE"),
    )?;
    let plist = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\"><dict>\n\
         <key>CFBundleExecutable</key><string>phoxal-simulator</string>\n\
         <key>CFBundleIdentifier</key><string>org.phoxal.simulator</string>\n\
         <key>CFBundleName</key><string>Phoxal Simulator</string>\n\
         <key>CFBundlePackageType</key><string>APPL</string>\n\
         <key>CFBundleShortVersionString</key><string>0.0.0</string>\n\
         <key>CFBundleVersion</key><string>1</string>\n\
         <key>LSMinimumSystemVersion</key><string>13.0</string>\n\
         <key>PhoxalSimulatorVersion</key><string>{}</string>\n\
         <key>PhoxalMuJoCoVersion</key><string>{MUJOCO_VERSION}</string>\n\
         </dict></plist>\n",
        artifact.summary.version
    );
    fs::write(contents.join("Info.plist"), plist).map_err(|source| Error::ArtifactFile {
        path: contents.join("Info.plist"),
        source,
    })?;
    run_command(
        Command::new("install_name_tool")
            .args(["-add_rpath", "@executable_path/../Frameworks"])
            .arg(&executable),
        "add the simulator application rpath",
    )?;
    let dependency = format!("@rpath/mujoco.framework/Versions/A/libmujoco.{MUJOCO_VERSION}.dylib");
    run_command(
        Command::new("install_name_tool")
            .arg("-change")
            .arg(dependency)
            .arg(format!("@rpath/{native_name}"))
            .arg(&executable),
        "rewrite the simulator MuJoCo dependency",
    )?;
    run_command(
        Command::new("codesign")
            .args(["--force", "--sign", "-"])
            .arg(&bundled_native),
        "sign the bundled MuJoCo library",
    )?;
    run_command(
        Command::new("codesign")
            .args(["--force", "--sign", "-"])
            .arg(&executable),
        "sign the simulator executable",
    )?;
    run_command(
        Command::new("codesign")
            .args(["--force", "--deep", "--sign", "-"])
            .arg(&app),
        "sign the simulator application",
    )?;
    let digest = digest_file(&executable)?;
    artifact.summary.executable = executable;
    artifact.summary.sha256 = digest.sha256;
    Ok(artifact)
}

#[cfg(target_os = "macos")]
fn run_command(command: &mut Command, operation: &str) -> Result<(), Error> {
    let output = command.output().map_err(|source| Error::CargoSpawn {
        operation: operation.to_owned(),
        source,
    })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(Error::CargoCommand {
            operation: operation.to_owned(),
            status: status_string(output.status),
            stdout: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

fn generate_application_lock(manifest: &Path, options: &CargoOptions) -> Result<(), Error> {
    let mut command = Command::new(options.cargo_program());
    command.args([
        "generate-lockfile",
        "--manifest-path",
        &manifest.display().to_string(),
        "--config",
        &format!("registries.phoxal.index=\"{PHOXAL_REGISTRY_INDEX}\""),
    ]);
    if options.offline {
        command.arg("--offline");
    }
    let output = command.output().map_err(|source| Error::CargoSpawn {
        operation: "generate simulator lockfile".to_owned(),
        source,
    })?;
    if !output.status.success() {
        return Err(Error::CargoCommand {
            operation: "generate simulator lockfile".to_owned(),
            status: status_string(output.status),
            stdout: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(source).map_err(|source_error| Error::ArtifactFile {
        path: source.to_owned(),
        source: source_error,
    })?;
    if metadata.file_type().is_symlink() {
        return Err(simulation_error(format!(
            "simulator source tree contains a symlink at {}",
            source.display()
        )));
    }
    if metadata.is_file() {
        return copy_regular(source, destination);
    }
    if !metadata.is_dir() {
        return Err(simulation_error(format!(
            "simulator source tree entry {} is not a regular file or directory",
            source.display()
        )));
    }
    fs::create_dir_all(destination).map_err(|source_error| Error::ArtifactFile {
        path: destination.to_owned(),
        source: source_error,
    })?;
    let mut entries = fs::read_dir(source)
        .map_err(|source_error| Error::ArtifactFile {
            path: source.to_owned(),
            source: source_error,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source_error| Error::ArtifactFile {
            path: source.to_owned(),
            source: source_error,
        })?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        copy_tree(&entry.path(), &destination.join(entry.file_name()))?;
    }
    Ok(())
}

fn simulator_manifest(request: &SimulationRunOptions) -> String {
    format!(
        "[workspace]\nresolver = \"3\"\n\n[package]\nname = \"phoxal-simulator-selection\"\nversion = \"0.0.0\"\nedition = \"2024\"\npublish = false\n\n[dependencies]\n{} = {{ package = \"{}\", version = \"={}\", registry = \"phoxal\" }}\n",
        cargo_dependency_key(&request.simulator_package),
        request.simulator_package,
        request.simulator_version
    )
}

fn cargo_dependency_key(package: &str) -> String {
    let mut key = package
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || byte == b'_' {
                byte as char
            } else {
                '-'
            }
        })
        .collect::<String>();
    if key.is_empty() || key.as_bytes()[0].is_ascii_digit() {
        key.insert(0, '_');
    }
    key
}

fn simulator_metadata(
    manifest: &Path,
    options: &CargoOptions,
    locked: bool,
) -> Result<cargo_metadata::Metadata, Error> {
    let mut command = MetadataCommand::new();
    command
        .cargo_path(options.cargo_program())
        .manifest_path(manifest)
        .other_options(vec![
            "--config".to_owned(),
            format!("registries.phoxal.index=\"{PHOXAL_REGISTRY_INDEX}\""),
        ]);
    if options.offline {
        command.other_options(vec!["--offline".to_owned()]);
    }
    if locked {
        command.other_options(vec!["--locked".to_owned()]);
    }
    command.exec().map_err(|source| Error::CargoMetadata {
        manifest: manifest.to_owned(),
        source,
    })
}

fn probe(
    simulator: &SimulatorArtifact,
    scene: &Path,
    bundle: &Path,
    request: &SimulationRunOptions,
) -> Result<SimulationModelFacts, Error> {
    let mut command = Command::new(&simulator.summary.executable);
    command.args([
        "--probe",
        "--scene",
        &scene.display().to_string(),
        "--bundle",
        &bundle.display().to_string(),
        "--json",
    ]);
    command.arg(request.presentation.flag());
    let output = command
        .output()
        .map_err(|source| Error::SimulationInvalid {
            message: format!(
                "cannot start simulator probe {}: {source}",
                simulator.summary.executable.display()
            ),
        })?;
    if !output.status.success() {
        return Err(Error::SimulationInvalid {
            message: format!(
                "simulator probe failed ({}){}",
                status_string(output.status),
                diagnostic_output(&output.stderr)
            ),
        });
    }
    let facts: SimulationModelFacts = serde_json::from_slice(&output.stdout).map_err(|source| {
        simulation_error(format!(
            "simulator probe returned no valid SimulationModelFacts: {source}"
        ))
    })?;
    if facts.model_identity.is_empty() || facts.quantum_ns == 0 {
        return Err(simulation_error(
            "simulator probe returned incomplete model identity or quantum facts",
        ));
    }
    Ok(facts)
}

fn launch(
    simulator: &SimulatorArtifact,
    bundle: &CompiledBundle,
    scene: &Path,
    request: &SimulationRunOptions,
    scenario: bool,
) -> Result<SimulationRunReport, Error> {
    let supervisor_path = bundle.executable("supervisor");
    // Unix socket names must fit even when the source checkout path is long.
    // The private temporary directory is owned by this launcher and lives until cleanup.
    let readiness_directory = tempfile::Builder::new()
        .prefix("phoxal-sim-")
        .tempdir_in("/tmp")
        .map_err(|source| Error::ArtifactFile {
            path: std::env::temp_dir(),
            source,
        })?;
    let readiness_path = readiness_directory.path().join("ready.json");
    let scenario_result_path = readiness_directory.path().join("scenario-result.json");
    let endpoint = format!(
        "unixsock-stream/{}",
        readiness_directory.path().join("router.sock").display()
    );
    let mut supervisor_command = Command::new(&supervisor_path);
    supervisor_command.arg(bundle.root()).args([
        "--scope",
        &request.scope,
        "--supervisor-id",
        &request.supervisor_id,
        "--ready-file",
        &readiness_path.display().to_string(),
        "--listen",
        &endpoint,
    ]);
    if scenario {
        supervisor_command.args([
            "--scenario-result",
            &scenario_result_path.display().to_string(),
        ]);
    }
    let mut supervisor = supervisor_command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|source| Error::SupervisorLaunch {
            message: format!("cannot start {}: {source}", supervisor_path.display()),
        })?;
    let supervisor_ready =
        match wait_process_ready(&mut supervisor, &readiness_path, request.startup_timeout) {
            Ok(ready) => ready,
            Err(error) => {
                let cleanup = cleanup_process(&mut supervisor, request.cleanup_timeout);
                return Err(simulation_error(format!(
                    "{error}; cleanup: {}",
                    cleanup_diagnostic(&cleanup)
                )));
            }
        };
    if !supervisor_ready {
        let cleanup = cleanup_process(&mut supervisor, request.cleanup_timeout);
        return Err(Error::SupervisorLaunch {
            message: format!(
                "supervisor exited before readiness for bundle {}; see supervisor diagnostics above; cleanup: {}",
                bundle.root().display(),
                cleanup_diagnostic(&cleanup)
            ),
        });
    }

    let mut simulator_command = Command::new(&simulator.summary.executable);
    simulator_command
        .args([
            "--scene",
            &scene.display().to_string(),
            "--bundle",
            &bundle.root().display().to_string(),
            request.presentation.flag(),
            "--scope",
            &request.scope,
            "--supervisor-id",
            &request.supervisor_id,
            "--run-id",
            &request.run_id,
            "--connect",
            &endpoint,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match request.bound {
        SimulationBound::Steps(steps) => {
            simulator_command.args(["--steps", &steps.to_string()]);
        }
        SimulationBound::Duration(duration) => {
            simulator_command.args(["--duration", &duration.to_string()]);
        }
    }
    if request.auto_run {
        simulator_command.arg("--auto-run");
    }
    let simulator_started = Instant::now();
    let output = match simulator_command.output() {
        Ok(output) => output,
        Err(source) => {
            let cleanup = cleanup_process(&mut supervisor, request.cleanup_timeout);
            return Err(simulation_error(format!(
                "cannot start simulator {}: {source}; cleanup: {}",
                simulator.summary.executable.display(),
                cleanup_diagnostic(&cleanup)
            )));
        }
    };
    let simulator_wall_time_ns =
        u64::try_from(simulator_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    let terminal = terminal_evidence(&output.stdout);
    let provider_contract_verified = terminal
        .as_ref()
        .is_some_and(|evidence| terminal_evidence_verified(evidence, request.presentation));
    let cleanup = cleanup_process(&mut supervisor, request.cleanup_timeout);
    let scenario = if scenario {
        let bytes = fs::read(&scenario_result_path).map_err(|source| Error::ArtifactFile {
            path: scenario_result_path.clone(),
            source,
        })?;
        Some(serde_json::from_slice(&bytes).map_err(|source| {
            simulation_error(format!(
                "cannot parse scenario execution evidence {}: {source}",
                scenario_result_path.display()
            ))
        })?)
    } else {
        None
    };
    Ok(SimulationRunReport {
        schema: "phoxal/simulation-run/v0".to_owned(),
        scene: scene.to_owned(),
        simulator: simulator.summary.clone(),
        bundle: bundle.root().to_owned(),
        scope: request.scope.clone(),
        supervisor_id: request.supervisor_id.clone(),
        run_id: request.run_id.clone(),
        supervisor_ready,
        provider_contract_verified,
        simulator_exit_code: output.status.code(),
        simulator_wall_time_ns,
        simulator_stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        simulator_stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        cleanup,
        scenario,
        terminal,
    })
}

/// Native terminal evidence emitted by the simulator application.
///
/// Re-exported from `phoxal::artifact::simulation`. The framework module is
/// the source of truth.
pub use phoxal::artifact::simulation::SimulatorTerminalEvidence;

#[cfg(test)]
fn provider_contract_verified(stdout: &[u8], presentation: SimulationPresentation) -> bool {
    terminal_evidence(stdout)
        .as_ref()
        .is_some_and(|evidence| terminal_evidence_verified(evidence, presentation))
}

fn terminal_evidence(stdout: &[u8]) -> Option<SimulatorTerminalEvidence> {
    let line = stdout
        .split(|byte| *byte == b'\n')
        .rfind(|line| !line.is_empty())?;
    serde_json::from_slice::<SimulatorTerminalEvidence>(line).ok()
}

fn terminal_evidence_verified(
    evidence: &SimulatorTerminalEvidence,
    presentation: SimulationPresentation,
) -> bool {
    evidence.schema == "phoxal/simulation-run/v0"
        && evidence.provider_contract_verified
        && ((evidence.outcome == "success" && evidence.completed_steps == evidence.requested_steps)
            || (presentation == SimulationPresentation::Desktop
                && evidence.outcome == "stopped"
                && evidence.completed_steps <= evidence.requested_steps))
        && evidence.requested_steps > 0
}

#[derive(Deserialize)]
struct SupervisorReadiness {
    schema: String,
    execution: String,
}

fn wait_process_ready(
    child: &mut Child,
    readiness_path: &Path,
    timeout: Duration,
) -> Result<bool, Error> {
    let deadline = Instant::now() + timeout;
    loop {
        if readiness_path.is_file() {
            let bytes = fs::read(readiness_path).map_err(|source| Error::ArtifactFile {
                path: readiness_path.to_owned(),
                source,
            })?;
            let readiness: SupervisorReadiness =
                serde_json::from_slice(&bytes).map_err(|source| {
                    simulation_error(format!(
                        "supervisor readiness {} is invalid: {source}",
                        readiness_path.display()
                    ))
                })?;
            if readiness.schema != "phoxal/supervisor-ready/v0" || readiness.execution.is_empty() {
                return Err(simulation_error(format!(
                    "supervisor readiness {} has an unsupported or incomplete contract",
                    readiness_path.display()
                )));
            }
            return Ok(true);
        }
        match child.try_wait().map_err(|source| Error::SupervisorLaunch {
            message: format!("cannot inspect supervisor readiness: {source}"),
        })? {
            Some(_) => return Ok(false),
            None if Instant::now() >= deadline => return Ok(false),
            None => thread::sleep(PROCESS_POLL),
        }
    }
}

fn cleanup_process(child: &mut Child, timeout: Duration) -> SimulationCleanup {
    let mut result = SimulationCleanup {
        supervisor_stop_requested: true,
        supervisor_exited: false,
        supervisor_killed: false,
        error: None,
    };
    match child.try_wait() {
        Ok(Some(_)) => {
            result.supervisor_exited = true;
            return result;
        }
        Ok(None) => {}
        Err(error) => {
            result.error = Some(format!("cannot inspect supervisor cleanup: {error}"));
            return result;
        }
    }
    #[cfg(unix)]
    {
        // The supervisor owns a termination handler that performs orderly
        // Runtime, session, bus, and router shutdown.
        if unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) } != 0 {
            result.error = Some(format!(
                "cannot request orderly supervisor stop: {}",
                std::io::Error::last_os_error()
            ));
            return result;
        }
    }
    #[cfg(not(unix))]
    if let Err(error) = child.kill() {
        result.error = Some(format!("cannot stop supervisor: {error}"));
        return result;
    } else {
        result.supervisor_killed = true;
    }
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                result.supervisor_exited = true;
                return result;
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(PROCESS_POLL),
            Ok(None) => {
                if let Err(error) = child.kill() {
                    result.error = Some(format!(
                        "supervisor did not exit after orderly stop and forced kill failed: {error}"
                    ));
                    return result;
                }
                result.supervisor_killed = true;
                return match child.wait() {
                    Ok(_) => {
                        result.supervisor_exited = true;
                        result
                    }
                    Err(error) => {
                        result.error =
                            Some(format!("cannot reap supervisor after forced kill: {error}"));
                        result
                    }
                };
            }
            Err(error) => {
                result.error = Some(format!("cannot reap supervisor after kill: {error}"));
                return result;
            }
        }
    }
}

fn cleanup_diagnostic(cleanup: &SimulationCleanup) -> String {
    cleanup
        .error
        .clone()
        .unwrap_or_else(|| "supervisor cleanup completed".to_owned())
}

fn artifact_path(stdout: &[u8], package_id: &str, target: &str) -> Result<PathBuf, Error> {
    let mut executable = None;
    for line in stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let message = serde_json::from_slice::<Message>(line).map_err(|error| {
            simulation_error(format!("simulator Cargo emitted invalid JSON: {error}"))
        })?;
        if let Message::CompilerArtifact(artifact) = message
            && artifact.package_id.to_string() == package_id
            && artifact.target.name == target
            && artifact.target.is_bin()
        {
            executable = artifact.executable.map(|path| path.into_std_path_buf());
        }
    }
    executable.ok_or_else(|| {
        simulation_error(format!(
            "Cargo built simulator package {package_id} but emitted no executable target {target}"
        ))
    })
}

fn ensure_regular_file(path: &Path, label: &str) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(path).map_err(|source| Error::ArtifactFile {
        path: path.to_owned(),
        source,
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(simulation_error(format!(
            "{label} {} must be a regular non-symlink file",
            path.display()
        )));
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct FileDigest {
    bytes: u64,
    sha256: String,
}

fn digest_file(path: &Path) -> Result<FileDigest, Error> {
    let mut file = File::open(path).map_err(|source| Error::ArtifactFile {
        path: path.to_owned(),
        source,
    })?;
    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| Error::ArtifactFile {
                path: path.to_owned(),
                source,
            })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        bytes = bytes
            .checked_add(read as u64)
            .ok_or_else(|| simulation_error("artifact byte count overflowed"))?;
    }
    Ok(FileDigest {
        bytes,
        sha256: format!("{:x}", hasher.finalize()),
    })
}

fn copy_regular(from: &Path, to: &Path) -> Result<(), Error> {
    ensure_regular_file(from, "source artifact")?;
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::ArtifactFile {
            path: parent.to_owned(),
            source,
        })?;
    }
    fs::copy(from, to).map_err(|source| Error::ArtifactFile {
        path: to.to_owned(),
        source,
    })?;
    Ok(())
}

fn make_executable(path: &Path) -> Result<(), Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::metadata(path).map_err(|source| Error::ArtifactFile {
            path: path.to_owned(),
            source,
        })?;
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).map_err(|source| Error::ArtifactFile {
            path: path.to_owned(),
            source,
        })?;
    }
    Ok(())
}

fn atomic_json<T: Serialize>(path: &Path, value: &T) -> Result<(), Error> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = NamedTempFile::new_in(parent).map_err(|source| Error::ArtifactFile {
        path: parent.to_owned(),
        source,
    })?;
    let bytes = serde_json::to_vec_pretty(value).map_err(|source| Error::SimulationInvalid {
        message: format!("cannot serialize simulator selection: {source}"),
    })?;
    temporary
        .write_all(&bytes)
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|source| Error::ArtifactFile {
            path: path.to_owned(),
            source,
        })?;
    temporary
        .persist(path)
        .map_err(|error| Error::ArtifactFile {
            path: path.to_owned(),
            source: error.error,
        })?;
    Ok(())
}

fn status_string(status: ExitStatus) -> String {
    status.code().map_or_else(
        || "terminated by signal".to_owned(),
        |code| code.to_string(),
    )
}

fn diagnostic_output(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes).trim().to_owned();
    if text.is_empty() {
        String::new()
    } else {
        format!(": {text}")
    }
}

fn simulation_error(message: impl Into<String>) -> Error {
    Error::SimulationInvalid {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simulation_bound_rejects_zero_and_non_finite_values() {
        assert!(SimulationBound::Steps(0).validate().is_err());
        assert!(SimulationBound::Duration(0.0).validate().is_err());
        assert!(SimulationBound::Duration(f64::NAN).validate().is_err());
        assert!(SimulationBound::Steps(1).validate().is_ok());
    }

    #[test]
    fn duration_must_be_an_integral_number_of_native_quanta() {
        assert!(
            SimulationBound::Duration(0.03)
                .validate_for_quantum(10_000_000)
                .is_ok()
        );
        assert!(
            SimulationBound::Duration(0.025)
                .validate_for_quantum(10_000_000)
                .is_err()
        );
    }

    #[test]
    fn terminal_evidence_requires_the_provider_contract_marker() {
        let complete = br#"{"schema":"phoxal/simulation-run/v0","provider_contract_verified":true,"outcome":"success","completed_steps":2,"requested_steps":2}"#;
        assert!(provider_contract_verified(
            complete,
            SimulationPresentation::Headless
        ));
        let missing = br#"{"schema":"phoxal/simulation-run/v0","outcome":"success","completed_steps":2,"requested_steps":2}"#;
        assert!(!provider_contract_verified(
            missing,
            SimulationPresentation::Headless
        ));
        let incomplete = br#"{"schema":"phoxal/simulation-run/v0","provider_contract_verified":true,"outcome":"failed","completed_steps":1,"requested_steps":2}"#;
        assert!(!provider_contract_verified(
            incomplete,
            SimulationPresentation::Headless
        ));
    }

    #[test]
    fn desktop_stop_is_a_successful_control_outcome_but_not_a_headless_completion() {
        let stopped = br#"{"schema":"phoxal/simulation-run/v0","provider_contract_verified":true,"outcome":"stopped","completed_steps":0,"requested_steps":500}"#;
        assert!(provider_contract_verified(
            stopped,
            SimulationPresentation::Desktop
        ));
        assert!(!provider_contract_verified(
            stopped,
            SimulationPresentation::Headless
        ));
    }

    #[test]
    fn a_run_never_installs_a_missing_simulator_implicitly()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = tempfile::tempdir()?;
        let request = SimulationRunOptions::new(
            fixture.path().join("scene.xml"),
            SimulationPresentation::Headless,
            SimulationBound::Steps(1),
        )?;
        let root = fixture.path().join("managed");
        let error = provision_at(&root, &request)
            .expect_err("a run must require an explicit prior installation");
        assert!(matches!(
            error,
            Error::SimulationInvalid { message }
                if message.contains("cargo phoxal simulation install")
        ));
        assert!(!root.exists());
        Ok(())
    }

    #[test]
    fn a_valid_stored_selection_is_reused() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = tempfile::tempdir()?;
        let root = fixture.path().join("managed");
        let executable = root.join("artifacts/fake/simulator");
        if let Some(parent) = executable.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&executable, "fake simulator")?;
        let digest = digest_file(&executable)?;
        let selection_path = root.join("selection.json");
        let selection = SimulatorSelection {
            schema: "phoxal/simulator-selection/v0".to_owned(),
            package: DEFAULT_SIMULATOR_PACKAGE.to_owned(),
            version: DEFAULT_SIMULATOR_VERSION.to_owned(),
            binary: DEFAULT_SIMULATOR_BINARY.to_owned(),
            source: "fixture".to_owned(),
            executable: executable.clone(),
            cargo_manifest: None,
            cargo_lock: None,
            executable_bytes: digest.bytes,
            executable_sha256: digest.sha256.clone(),
            cargo_manifest_sha256: None,
            cargo_lock_sha256: None,
        };
        fs::write(&selection_path, serde_json::to_vec(&selection)?)?;
        let request = SimulationRunOptions::new(
            fixture.path().join("scene.xml"),
            SimulationPresentation::Headless,
            SimulationBound::Steps(1),
        )?;
        let artifact = provision_at(&root, &request)?;
        assert_eq!(artifact.summary.source, "fixture");
        assert_eq!(artifact.summary.sha256, digest.sha256);
        Ok(())
    }

    #[test]
    fn status_is_read_only_when_no_managed_installation_exists()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = tempfile::tempdir()?;
        let root = fixture.path().join("simulation");
        let status = simulator_status_at(&root)?;
        assert!(!status.installed);
        assert_eq!(status.root, root);
        assert!(!root.exists());
        Ok(())
    }

    #[test]
    fn uninstall_removes_only_a_marked_managed_directory() -> Result<(), Box<dyn std::error::Error>>
    {
        let fixture = tempfile::tempdir()?;
        let unmanaged = fixture.path().join("unmanaged");
        fs::create_dir(&unmanaged)?;
        let error = uninstall_simulator_at(&unmanaged)
            .expect_err("an unmanaged directory must never be removed");
        assert!(matches!(
            error,
            Error::SimulationInvalid { message }
                if message.contains("refusing to remove unmanaged directory")
        ));
        assert!(unmanaged.is_dir());

        let managed = fixture.path().join("managed");
        fs::create_dir(&managed)?;
        fs::write(managed.join(MANAGED_MARKER), "managed\n")?;
        assert_eq!(uninstall_simulator_at(&managed)?, managed);
        assert!(!managed.exists());
        Ok(())
    }

    #[test]
    fn simulator_manifest_uses_only_registry_coordinates() -> Result<(), Box<dyn std::error::Error>>
    {
        let request = SimulationRunOptions::new(
            "scene.xml",
            SimulationPresentation::Headless,
            SimulationBound::Steps(1),
        )?;
        let manifest = simulator_manifest(&request);
        assert!(manifest.contains("registry = \"phoxal\""));
        assert!(!manifest.contains("path ="));
        assert!(manifest.contains("version = \"=0.0.0-dev.1\""));
        Ok(())
    }

    #[test]
    fn artifact_path_requires_the_exact_package_and_binary() {
        let line = r#"{"reason":"compiler-artifact","package_id":"registry+https://example.invalid/#phoxal-simulator@0.0.0-dev.1","target":{"kind":["bin"],"crate_types":["bin"],"name":"phoxal-simulator","src_path":"/tmp/main.rs","edition":"2024","required-features":[]},"profile":{"opt_level":"0","debuginfo":2,"debug_assertions":true,"overflow_checks":true,"test":false,"panic":"unwind","incremental":true,"codegen-units":256,"rpath":false},"features":[],"filenames":[],"executable":"/tmp/simulator","fresh":false}"#;
        let path = artifact_path(
            line.as_bytes(),
            "registry+https://example.invalid/#phoxal-simulator@0.0.0-dev.1",
            "phoxal-simulator",
        );
        assert_eq!(path.ok(), Some(PathBuf::from("/tmp/simulator")));
    }

    #[cfg(unix)]
    #[test]
    fn readiness_requires_the_machine_contract_and_cleanup_is_orderly()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let readiness = directory.path().join("ready.json");
        fs::write(
            &readiness,
            br#"{"schema":"phoxal/supervisor-ready/v0","execution":"execution-1"}"#,
        )?;
        let mut child = Command::new("sleep").arg("10").spawn()?;
        assert!(wait_process_ready(
            &mut child,
            &readiness,
            Duration::from_secs(1)
        )?);
        let cleanup = cleanup_process(&mut child, Duration::from_secs(1));
        assert!(cleanup.supervisor_stop_requested);
        assert!(cleanup.supervisor_exited);
        assert!(!cleanup.supervisor_killed);
        assert!(cleanup.error.is_none());
        Ok(())
    }
}
