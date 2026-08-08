//! The persisted runtime bundle boundary.
//!
//! `phoxal-manifest` compiles authored YAML/URDF into canonical model facts;
//! this crate owns the artifact that remains after that source tree is gone.
//! A runtime process reads only `runtime.json`, the indexed files below
//! `assets/`, and the selected binary below `bin/`. It never invokes the source
//! compiler and never discovers a participant from a catalog.
//!
//! ```text
//! <bundle>/
//! ├── runtime.json
//! ├── assets/
//! └── bin/
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use phoxal_model::component::capability::MotorCommand;
use phoxal_model::identity::{CapabilityRef, ComponentInstanceId};
use phoxal_model::{AssetId, Clock, Robot};
pub use phoxal_runtime_contract::identity::ParticipantArtifactId;
use phoxal_runtime_contract::identity::ParticipantId;
use phoxal_runtime_contract::metadata::{ParticipantContract, ParticipantRequirement};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod path;
pub use path::{BundlePath, BundlePathError, DigestError, Sha256Digest};
mod asset;
pub use asset::ParticipantAssets;
mod reader;
pub use reader::{ParticipantBundle, ParticipantRuntimeInputs, RuntimeBundle};

/// The only schema tag currently readable by this framework train.
pub const RUNTIME_SCHEMA: &str = "phoxal/runtime-bundle/v0";
/// The persisted document filename at the bundle root.
pub const RUNTIME_FILE: &str = "runtime.json";
/// The participant-readable asset directory.
pub const ASSETS_DIR: &str = "assets";
/// The supervisor-only binary directory.
pub const BIN_DIR: &str = "bin";

/// The scheduler policy persisted for one runtime participant instance.
///
/// This belongs to the compiled runtime bundle because it is a runtime
/// selection fact, not a process-contract/launch parser type.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParticipantClock {
    /// Follow the host's boot-anchored real clock.
    Real,
    /// Follow the simulation world clock supplied by the runtime.
    Simulation,
    /// Do not schedule robot-time steps.
    Clockless,
}

impl fmt::Display for ParticipantClock {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Real => "real",
            Self::Simulation => "simulation",
            Self::Clockless => "clockless",
        })
    }
}

/// A schema-tagged persisted runtime document.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "schema", deny_unknown_fields)]
pub enum RuntimeDocument {
    /// The first runtime bundle schema. Older/future schemas are refused
    /// rather than guessed at by a runtime process.
    #[serde(rename = "phoxal/runtime-bundle/v0")]
    V0(Runtime),
}

impl<'de> Deserialize<'de> for RuntimeDocument {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "schema", deny_unknown_fields)]
        enum Wire {
            #[serde(rename = "phoxal/runtime-bundle/v0")]
            V0(Runtime),
        }

        match Wire::deserialize(deserializer)? {
            Wire::V0(runtime) => Ok(Self::new(runtime)),
        }
    }
}

impl RuntimeDocument {
    /// Wrap one already-validated runtime document.
    #[must_use]
    pub const fn new(runtime: Runtime) -> Self {
        Self::V0(runtime)
    }

    /// The runtime payload.
    #[must_use]
    pub const fn runtime(&self) -> &Runtime {
        match self {
            Self::V0(runtime) => runtime,
        }
    }

    /// The canonical robot identity persisted by this document.
    #[must_use]
    pub fn robot_id(&self) -> &phoxal_model::identity::RobotId {
        self.runtime().robot.id()
    }

    /// The canonical compiled robot.
    #[must_use]
    pub fn robot(&self) -> &Robot {
        &self.runtime().robot
    }

    /// The final participant set, in persisted order.
    #[must_use]
    pub fn participants(&self) -> &[RuntimeParticipant] {
        &self.runtime().participants
    }

    /// The reusable executable artifacts selected by participant instances.
    #[must_use]
    pub fn artifacts(&self) -> &BTreeMap<ParticipantArtifactId, BinaryReference> {
        &self.runtime().artifacts
    }

    /// Find the exact persisted participant selected by a process boundary.
    pub fn participant(&self, id: &ParticipantId) -> Result<&RuntimeParticipant, SelectionError> {
        self.participants()
            .iter()
            .find(|participant| participant.id == *id)
            .ok_or_else(|| SelectionError::Unknown {
                requested: id.clone(),
            })
    }
}

/// The persisted final runtime graph and all framework-owned runtime facts.
#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Runtime {
    /// The complete canonical model. Its `id` is the sole persisted RobotId;
    /// there is no namespace or duplicate top-level identity field.
    robot: Robot,
    /// The reusable staged executables and their embedded compatibility
    /// contracts. Multiple participant instances may point to one entry.
    artifacts: BTreeMap<ParticipantArtifactId, BinaryReference>,
    /// The exact process instances the executor must launch, in final
    /// persisted form.
    participants: Vec<RuntimeParticipant>,
    /// The participant-readable asset index and integrity facts.
    assets: AssetIndex,
    /// Optional supervisor router configuration, kept as an indexed asset.
    router: Option<RuntimeRouterConfig>,
}

impl<'de> Deserialize<'de> for Runtime {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            robot: Robot,
            artifacts: BTreeMap<ParticipantArtifactId, BinaryReference>,
            participants: Vec<RuntimeParticipant>,
            assets: AssetIndex,
            router: Option<RuntimeRouterConfig>,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.robot,
            wire.artifacts,
            wire.participants,
            wire.assets,
            wire.router,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl Runtime {
    /// Construct the complete in-memory runtime document, validating its
    /// cross-field invariants exactly once.
    pub fn new(
        robot: Robot,
        artifacts: BTreeMap<ParticipantArtifactId, BinaryReference>,
        participants: Vec<RuntimeParticipant>,
        assets: AssetIndex,
        router: Option<RuntimeRouterConfig>,
    ) -> Result<Self, DocumentError> {
        let runtime = Self {
            robot,
            artifacts,
            participants,
            assets,
            router,
        };
        runtime.validate()?;
        Ok(runtime)
    }

    /// The canonical compiled robot.
    #[must_use]
    pub const fn robot(&self) -> &Robot {
        &self.robot
    }

    /// The reusable executable artifacts retained by this runtime.
    #[must_use]
    pub fn artifacts(&self) -> &BTreeMap<ParticipantArtifactId, BinaryReference> {
        &self.artifacts
    }

    /// The final participant set, in persisted order.
    #[must_use]
    pub fn participants(&self) -> &[RuntimeParticipant] {
        &self.participants
    }

    /// The participant-readable asset index.
    #[must_use]
    pub const fn assets(&self) -> &AssetIndex {
        &self.assets
    }

    /// Optional router configuration selected by build tooling.
    #[must_use]
    pub const fn router(&self) -> Option<&RuntimeRouterConfig> {
        self.router.as_ref()
    }

    fn validate(&self) -> Result<(), DocumentError> {
        let mut ids = BTreeSet::new();
        let mut artifact_paths = BTreeSet::new();
        for (id, artifact) in &self.artifacts {
            artifact.validate(id)?;
            if !artifact_paths.insert(artifact.path.clone()) {
                return Err(DocumentError::DuplicateBinary {
                    path: artifact.path.clone(),
                });
            }
        }
        for participant in &self.participants {
            let artifact = self.artifacts.get(&participant.artifact).ok_or_else(|| {
                DocumentError::UnknownArtifact {
                    participant: participant.id.clone(),
                    artifact: participant.artifact.clone(),
                }
            })?;
            participant.validate(&self.robot, artifact)?;
            if !ids.insert(participant.id.clone()) {
                return Err(DocumentError::DuplicateParticipant {
                    id: participant.id.clone(),
                });
            }
        }
        self.assets.validate()?;
        if let Some(router) = &self.router {
            router.validate(&self.assets)?;
        }
        Ok(())
    }
}

/// One exact process entry in the final runtime graph.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeParticipant {
    /// The topology identity selected by the process launch.
    id: ParticipantId,
    /// The reusable artifact selected for this instance.
    artifact: ParticipantArtifactId,
    /// The already-compiled participant configuration. `None` means JSON
    /// `null`, not a request to consult authored configuration.
    config: Option<serde_json::Value>,
    /// An optional typed component-instance binding for a driver/simulator.
    component: Option<ComponentInstanceId>,
    /// The scheduler policy selected at build time.
    clock: ParticipantClock,
}

impl RuntimeParticipant {
    /// Construct one final participant instance selected by build tooling.
    #[must_use]
    pub fn new(
        id: ParticipantId,
        artifact: ParticipantArtifactId,
        config: Option<serde_json::Value>,
        component: Option<ComponentInstanceId>,
        clock: ParticipantClock,
    ) -> Self {
        Self {
            id,
            artifact,
            config,
            component,
            clock,
        }
    }

    /// The persisted participant instance identity.
    #[must_use]
    pub const fn id(&self) -> &ParticipantId {
        &self.id
    }

    /// The reusable artifact selected for this instance.
    #[must_use]
    pub const fn artifact(&self) -> &ParticipantArtifactId {
        &self.artifact
    }

    /// The compiled participant configuration, if any.
    #[must_use]
    pub fn config(&self) -> Option<&serde_json::Value> {
        self.config.as_ref()
    }

    /// The canonical component instance bound to this participant, if any.
    #[must_use]
    pub const fn component(&self) -> Option<&ComponentInstanceId> {
        self.component.as_ref()
    }

    /// The scheduler policy selected for this participant.
    #[must_use]
    pub const fn clock(&self) -> ParticipantClock {
        self.clock
    }

    /// Validate participant facts against the compiled robot.
    pub fn validate(&self, robot: &Robot, artifact: &BinaryReference) -> Result<(), DocumentError> {
        if !artifact.path.starts_with_directory(BIN_DIR) {
            return Err(DocumentError::ArtifactOutsideBin {
                artifact: self.artifact.clone(),
                path: artifact.path.clone(),
            });
        }
        if let Some(component) = &self.component
            && robot.component_instance(component.as_str()).is_none()
        {
            return Err(DocumentError::UnknownComponent {
                participant: self.id.clone(),
                component_instance: component.clone(),
            });
        }
        match (robot.clock(), self.clock) {
            (Clock::Real, ParticipantClock::Simulation) => {
                return Err(DocumentError::ClockMismatch {
                    participant: self.id.clone(),
                    robot: Clock::Real,
                    participant_clock: self.clock,
                });
            }
            (Clock::Simulated, ParticipantClock::Real) => {
                return Err(DocumentError::ClockMismatch {
                    participant: self.id.clone(),
                    robot: Clock::Simulated,
                    participant_clock: self.clock,
                });
            }
            _ => {}
        }
        let null = serde_json::Value::Null;
        let config = self.config.as_ref().unwrap_or(&null);
        let validator =
            jsonschema::validator_for(&artifact.contract.config_schema).map_err(|error| {
                DocumentError::InvalidConfigSchema {
                    participant: self.id.clone(),
                    error: error.to_string(),
                }
            })?;
        if let Err(error) = validator.validate(config) {
            return Err(DocumentError::InvalidConfig {
                participant: self.id.clone(),
                error: error.to_string(),
            });
        }
        validate_requirement(&artifact.contract, &self.id, robot)?;
        Ok(())
    }
}

/// A staged reusable executable and its canonical artifact contract.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BinaryReference {
    /// A normalized bundle-relative path under `bin/`.
    path: BundlePath,
    /// The exact bytes staged at `path`.
    digest: Sha256Digest,
    /// The exact byte length staged at `path`.
    size_bytes: u64,
    /// The embedded process-contract facts the binary declared.
    contract: ParticipantContract,
}

impl BinaryReference {
    /// Construct a reference for an already-built executable.
    ///
    /// The bytes are hashed from the source file rather than accepted from an
    /// in-memory copy: the same executable is what the writer later stages.
    pub fn from_file(
        path: BundlePath,
        contract: ParticipantContract,
        source: impl AsRef<Path>,
    ) -> Result<Self, BundleError> {
        let source = source.as_ref();
        let file = std::fs::File::open(source).map_err(|source_error| BundleError::ReadFile {
            path: source.to_path_buf(),
            source: source_error,
        })?;
        let size_bytes = file
            .metadata()
            .map_err(|source_error| BundleError::ReadFile {
                path: source.to_path_buf(),
                source: source_error,
            })?
            .len();
        Ok(Self {
            path,
            digest: Sha256Digest::from_reader(file).map_err(|source_error| {
                BundleError::ReadFile {
                    path: source.to_path_buf(),
                    source: source_error,
                }
            })?,
            size_bytes,
            contract,
        })
    }

    /// The normalized bundle-relative binary path.
    #[must_use]
    pub const fn path(&self) -> &BundlePath {
        &self.path
    }

    /// The digest of the exact staged binary bytes.
    #[must_use]
    pub const fn digest(&self) -> Sha256Digest {
        self.digest
    }

    /// The exact byte length of the staged executable.
    #[must_use]
    pub const fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    /// The embedded process contract declared by this artifact.
    #[must_use]
    pub const fn contract(&self) -> &ParticipantContract {
        &self.contract
    }
}

impl BinaryReference {
    fn validate(&self, id: &ParticipantArtifactId) -> Result<(), DocumentError> {
        if self.contract.id != *id {
            return Err(DocumentError::ArtifactContractMismatch {
                artifact: id.clone(),
                contract: self.contract.id.clone(),
            });
        }
        if !self.path.starts_with_directory(BIN_DIR) {
            return Err(DocumentError::ArtifactOutsideBin {
                artifact: id.clone(),
                path: self.path.clone(),
            });
        }
        Ok(())
    }
}

/// Validate one artifact's static topology requirement against the canonical
/// robot. Instance identity is used only for a useful diagnostic; it never
/// participates in artifact-contract matching.
fn validate_requirement(
    contract: &ParticipantContract,
    participant: &ParticipantId,
    robot: &Robot,
) -> Result<(), DocumentError> {
    let Some(requirement) = contract.requirement else {
        return Ok(());
    };
    match requirement {
        ParticipantRequirement::DifferentialDriveVelocity => {
            let phoxal_model::robot::KinematicConfig::Differential {
                left_actuators,
                right_actuators,
                ..
            } = robot.motion().kinematic()
            else {
                return Err(DocumentError::RequirementKinematicsMismatch {
                    participant: participant.clone(),
                    requirement,
                    actual: robot.motion().kinematic().kind(),
                });
            };
            validate_drive_side(participant, "left_actuators", left_actuators, robot)?;
            validate_drive_side(participant, "right_actuators", right_actuators, robot)
        }
    }
}

fn validate_drive_side(
    participant: &ParticipantId,
    side: &'static str,
    actuators: &[CapabilityRef],
    robot: &Robot,
) -> Result<(), DocumentError> {
    if actuators.is_empty() {
        return Err(DocumentError::RequirementActuatorListEmpty {
            participant: participant.clone(),
            side,
        });
    }
    for reference in actuators {
        let (motor, _) = robot.require_motor(reference).map_err(|error| {
            DocumentError::RequirementActuatorInvalid {
                participant: participant.clone(),
                actuator: reference.clone(),
                error: error.to_string(),
            }
        })?;
        if motor.command != MotorCommand::Velocity {
            return Err(DocumentError::RequirementMotorModeMismatch {
                participant: participant.clone(),
                actuator: reference.clone(),
                expected: MotorCommand::Velocity,
                actual: motor.command,
            });
        }
    }
    Ok(())
}

/// A normalized reference to optional router configuration.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeRouterConfig {
    /// The config is an indexed asset, never an arbitrary filesystem path.
    path: BundlePath,
}

impl RuntimeRouterConfig {
    /// Construct router configuration pointing at one bundle asset.
    #[must_use]
    pub const fn new(path: BundlePath) -> Self {
        Self { path }
    }

    /// The indexed asset path containing the router configuration.
    #[must_use]
    pub const fn path(&self) -> &BundlePath {
        &self.path
    }

    fn validate(&self, assets: &AssetIndex) -> Result<(), DocumentError> {
        if !self.path.starts_with_directory(ASSETS_DIR) {
            return Err(DocumentError::RouterOutsideAssets {
                path: self.path.clone(),
            });
        }
        if !assets.entries.iter().any(|entry| entry.path == self.path) {
            return Err(DocumentError::RouterMissingAsset {
                path: self.path.clone(),
            });
        }
        Ok(())
    }
}

/// The integrity index for every participant-readable asset.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssetIndex {
    entries: Vec<AssetRecord>,
}

impl AssetIndex {
    /// Build an index for compiled logical assets. The writer places each
    /// logical id below `assets/` and records its byte length and digest.
    pub fn from_bytes(assets: &BTreeMap<AssetId, Vec<u8>>) -> Result<Self, DocumentError> {
        let entries = assets
            .iter()
            .map(|(id, bytes)| {
                Ok(AssetRecord {
                    id: id.clone(),
                    path: BundlePath::new(format!("{ASSETS_DIR}/{}", id.as_str()))?,
                    size_bytes: bytes.len() as u64,
                    digest: Sha256Digest::of(bytes),
                })
            })
            .collect::<Result<Vec<_>, DocumentError>>()?;
        let index = Self { entries };
        index.validate()?;
        Ok(index)
    }

    /// Every indexed participant-readable asset, in deterministic order.
    #[must_use]
    pub fn entries(&self) -> &[AssetRecord] {
        &self.entries
    }

    fn validate(&self) -> Result<(), DocumentError> {
        let mut ids = BTreeSet::new();
        let mut paths = BTreeSet::new();
        for entry in &self.entries {
            if !entry.path.starts_with_directory(ASSETS_DIR) {
                return Err(DocumentError::AssetOutsideAssets {
                    path: entry.path.clone(),
                });
            }
            let expected = format!("{ASSETS_DIR}/{}", entry.id.as_str());
            if entry.path.as_str() != expected {
                return Err(DocumentError::AssetPathMismatch {
                    id: entry.id.clone(),
                    path: entry.path.clone(),
                });
            }
            if !ids.insert(entry.id.clone()) {
                return Err(DocumentError::DuplicateAssetId {
                    id: entry.id.clone(),
                });
            }
            if !paths.insert(entry.path.clone()) {
                return Err(DocumentError::DuplicateAssetPath {
                    path: entry.path.clone(),
                });
            }
        }
        Ok(())
    }
}

/// One indexed asset and its expected bytes.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssetRecord {
    id: AssetId,
    path: BundlePath,
    size_bytes: u64,
    digest: Sha256Digest,
}

impl AssetRecord {
    /// The logical asset identity.
    #[must_use]
    pub const fn id(&self) -> &AssetId {
        &self.id
    }

    /// The normalized indexed bundle path.
    #[must_use]
    pub const fn path(&self) -> &BundlePath {
        &self.path
    }

    /// The expected byte length.
    #[must_use]
    pub const fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    /// The expected SHA-256 digest.
    #[must_use]
    pub const fn digest(&self) -> Sha256Digest {
        self.digest
    }
}

/// A bundle root pinned to one directory object for the lifetime of a load.
///
/// The path is retained only for diagnostics and compatibility with the
/// public `root()` accessor. All trusted file opens use the descriptor on Unix;
/// they never re-resolve this path after acquisition.
#[derive(Clone)]
pub(crate) struct BundleRoot {
    path: PathBuf,
    #[cfg(unix)]
    fd: Arc<std::os::fd::OwnedFd>,
}

impl fmt::Debug for BundleRoot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BundleRoot")
            .field("path", &self.path)
            .finish()
    }
}

impl BundleRoot {
    pub(crate) fn open(requested: &Path) -> Result<Self, BundleError> {
        #[cfg(unix)]
        {
            use std::ffi::CString;
            use std::os::fd::FromRawFd;

            let requested_c =
                CString::new(requested.as_os_str().as_encoded_bytes()).map_err(|_| {
                    BundleError::UnsupportedEntry {
                        path: requested.to_path_buf(),
                    }
                })?;
            let fd = unsafe {
                // SAFETY: the CString is NUL-free. O_NOFOLLOW applies to the
                // requested root itself, and O_DIRECTORY prevents pinning a
                // non-directory object.
                libc::open(
                    requested_c.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(root_open_error(requested));
            }
            let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
            Ok(Self {
                path: requested.to_path_buf(),
                fd: Arc::new(fd),
            })
        }
        #[cfg(not(unix))]
        {
            Err(BundleError::UnsupportedSecureOpen {
                path: requested.to_path_buf(),
            })
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

pub(crate) fn read_runtime_document(root: &BundleRoot) -> Result<RuntimeDocument, BundleError> {
    let runtime_path = root.path().join(RUNTIME_FILE);
    let mut runtime_file =
        open_bundle_file(root, &BundlePath::new(RUNTIME_FILE)?).map_err(|error| match error {
            BundleError::ReadFile { path, source } => BundleError::ReadDocument { path, source },
            BundleError::MissingFile { path } => BundleError::ReadDocument {
                path,
                source: std::io::Error::new(std::io::ErrorKind::NotFound, RUNTIME_FILE),
            },
            other => other,
        })?;
    let mut bytes = Vec::new();
    runtime_file
        .read_to_end(&mut bytes)
        .map_err(|source| BundleError::ReadDocument {
            path: runtime_path,
            source,
        })?;
    serde_json::from_slice(&bytes).map_err(BundleError::from)
}

#[cfg(unix)]
pub(crate) fn require_layout_directories(root: &BundleRoot) -> Result<(), BundleError> {
    for directory in [ASSETS_DIR, BIN_DIR] {
        let path = root.path().join(directory);
        if open_relative_directory(root, directory, &path)?.is_none() {
            return Err(BundleError::MissingFile { path });
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn require_layout_directories(root: &BundleRoot) -> Result<(), BundleError> {
    Err(BundleError::UnsupportedSecureOpen {
        path: root.path().to_path_buf(),
    })
}

/// Why a persisted runtime document is not a valid execution plan.
#[derive(Clone, Debug, thiserror::Error)]
pub enum DocumentError {
    #[error("duplicate participant id '{id}'")]
    DuplicateParticipant { id: ParticipantId },
    #[error("duplicate participant binary path '{path}'")]
    DuplicateBinary { path: BundlePath },
    #[error("participant '{participant}' references unknown artifact '{artifact}'")]
    UnknownArtifact {
        participant: ParticipantId,
        artifact: ParticipantArtifactId,
    },
    #[error("artifact '{artifact}' contract names '{contract}'")]
    ArtifactContractMismatch {
        artifact: ParticipantArtifactId,
        contract: ParticipantArtifactId,
    },
    #[error("artifact '{artifact}' path is outside bin/: {path}")]
    ArtifactOutsideBin {
        artifact: ParticipantArtifactId,
        path: BundlePath,
    },
    #[error("participant '{participant}' names unknown component instance '{component_instance}'")]
    UnknownComponent {
        participant: ParticipantId,
        component_instance: ComponentInstanceId,
    },
    #[error(
        "participant '{participant}' clock {participant_clock:?} conflicts with robot clock {robot:?}"
    )]
    ClockMismatch {
        participant: ParticipantId,
        robot: Clock,
        participant_clock: ParticipantClock,
    },
    #[error(
        "participant '{participant}' requires {requirement:?}, but the robot uses {actual} kinematics"
    )]
    RequirementKinematicsMismatch {
        participant: ParticipantId,
        requirement: ParticipantRequirement,
        actual: phoxal_model::robot::KinematicKind,
    },
    #[error("participant '{participant}' requires at least one {side} actuator")]
    RequirementActuatorListEmpty {
        participant: ParticipantId,
        side: &'static str,
    },
    #[error(
        "participant '{participant}' requirement could not resolve actuator '{actuator}': {error}"
    )]
    RequirementActuatorInvalid {
        participant: ParticipantId,
        actuator: CapabilityRef,
        error: String,
    },
    #[error(
        "participant '{participant}' actuator '{actuator}' is configured for {actual:?}, but the binary requires {expected:?}"
    )]
    RequirementMotorModeMismatch {
        participant: ParticipantId,
        actuator: CapabilityRef,
        expected: MotorCommand,
        actual: MotorCommand,
    },
    #[error("participant '{participant}' has an invalid config schema: {error}")]
    InvalidConfigSchema {
        participant: ParticipantId,
        error: String,
    },
    #[error("participant '{participant}' config does not match its binary schema: {error}")]
    InvalidConfig {
        participant: ParticipantId,
        error: String,
    },
    #[error("asset id '{id}' is persisted at the wrong path '{path}'", id = id.as_str())]
    AssetPathMismatch { id: AssetId, path: BundlePath },
    #[error("asset path is outside assets/: {path}")]
    AssetOutsideAssets { path: BundlePath },
    #[error("duplicate asset id '{id}'", id = id.as_str())]
    DuplicateAssetId { id: AssetId },
    #[error("duplicate asset path '{path}'")]
    DuplicateAssetPath { path: BundlePath },
    #[error("router config is outside assets/: {path}")]
    RouterOutsideAssets { path: BundlePath },
    #[error("router config is not present in the asset index: {path}")]
    RouterMissingAsset { path: BundlePath },
    #[error("supplied asset bytes do not match the persisted asset index")]
    AssetIndexMismatch,
    #[error("supplied binary bytes do not match the persisted participant set")]
    BinaryIndexMismatch,
    #[error("bundle path is not valid: {0}")]
    Path(#[from] BundlePathError),
}

/// Why an exact participant selection failed.
#[derive(Clone, Debug, thiserror::Error)]
pub enum SelectionError {
    #[error("runtime bundle has no participant '{requested}'")]
    Unknown { requested: ParticipantId },
    #[error("participant '{participant}' references missing artifact '{artifact}'")]
    MissingArtifact {
        participant: ParticipantId,
        artifact: ParticipantArtifactId,
    },
}

/// Why reading or validating a bundle failed.
#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    #[error("failed to resolve bundle root {}: {source}", path.display())]
    Root {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("bundle root is not a directory: {0}")]
    NotDirectory(PathBuf),
    #[error("bundle target already exists: {0}")]
    TargetExists(PathBuf),
    #[error("secure no-follow opening is unsupported for bundle file {path}")]
    UnsupportedSecureOpen { path: PathBuf },
    #[error("atomic no-replace publication is unsupported for bundle target {path}")]
    UnsupportedAtomicPublish { path: PathBuf },
    #[error("failed to read runtime document {}: {source}", path.display())]
    ReadDocument {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("runtime document is not valid JSON: {0}")]
    DocumentJson(#[from] serde_json::Error),
    #[error(transparent)]
    Document(#[from] DocumentError),
    #[error("bundle contains forbidden symlink {path}")]
    ForbiddenSymlink { path: PathBuf },
    #[error("bundle contains unsupported filesystem entry {path}")]
    UnsupportedEntry { path: PathBuf },
    #[error("bundle executable is not executable: {path}")]
    NotExecutable { path: PathBuf },
    #[error("bundle contains unexpected file {path}")]
    UnexpectedFile { path: PathBuf },
    #[error("bundle contains unindexed directory {path}")]
    UnindexedDirectory { path: PathBuf },
    #[error("bundle is missing required file {path}")]
    MissingFile { path: PathBuf },
    #[error("failed to read bundle file {path}: {source}", path = path.display())]
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("bundle file {path} changed after indexing: expected {expected}, found {actual}")]
    Integrity {
        path: PathBuf,
        expected: Sha256Digest,
        actual: Sha256Digest,
    },
    #[error("bundle file {path} has size {actual}, expected {expected}")]
    Size {
        path: PathBuf,
        expected: u64,
        actual: u64,
    },
    #[error("asset '{id}' is not declared by runtime.json", id = id.as_str())]
    UndeclaredAsset { id: AssetId },
    #[error(transparent)]
    Path(#[from] BundlePathError),
    #[error(transparent)]
    Digest(#[from] DigestError),
    #[error(transparent)]
    Selection(#[from] SelectionError),
}

/// A build-tool-facing writer for the explicit final assembly boundary.
pub struct BundleWriter;

impl BundleWriter {
    /// Write a document and the exact staged bytes it references.
    ///
    /// The caller supplies the final `RuntimeDocument`; this method never
    /// discovers participants, consults a catalog, or derives launch policy.
    /// It writes only `runtime.json`, `assets/`, and `bin/`, then reopens the
    /// result through the same reader the executor uses.
    pub fn write(
        root: impl AsRef<Path>,
        document: &RuntimeDocument,
        assets: &BTreeMap<AssetId, Vec<u8>>,
        binaries: &BTreeMap<BundlePath, PathBuf>,
    ) -> Result<RuntimeBundle, BundleError> {
        Self::write_inner(root, document, assets, binaries, publish_staging_root)
    }

    fn write_inner<F>(
        root: impl AsRef<Path>,
        document: &RuntimeDocument,
        assets: &BTreeMap<AssetId, Vec<u8>>,
        binaries: &BTreeMap<BundlePath, PathBuf>,
        publish: F,
    ) -> Result<RuntimeBundle, BundleError>
    where
        F: FnOnce(&Path, &Path) -> Result<(), BundleError>,
    {
        let expected_assets = &document.runtime().assets;
        let supplied_assets = AssetIndex::from_bytes(assets)?;
        if supplied_assets.entries != expected_assets.entries {
            return Err(BundleError::Document(DocumentError::AssetIndexMismatch));
        }

        let expected_binaries = document
            .artifacts()
            .values()
            .map(|artifact| artifact.path.clone())
            .collect::<BTreeSet<_>>();
        let supplied_binaries = binaries.keys().cloned().collect::<BTreeSet<_>>();
        if expected_binaries != supplied_binaries {
            return Err(BundleError::Document(DocumentError::BinaryIndexMismatch));
        }
        let requested_root = root.as_ref();
        let publish_target = prepare_publish_parent(requested_root)?;
        reject_existing_target(&publish_target)?;
        let root = create_staging_root(&publish_target)?;
        let staged = (|| {
            ensure_staging_directory(&root, &root.join(ASSETS_DIR))?;
            ensure_staging_directory(&root, &root.join(BIN_DIR))?;
            for (id, bytes) in assets {
                let path = root
                    .join(ASSETS_DIR)
                    .join(id.as_str().split('/').collect::<PathBuf>());
                write_new_file(&root, &path, bytes)?;
            }
            for (path, source) in binaries {
                let artifact = document
                    .artifacts()
                    .values()
                    .find(|artifact| artifact.path == *path)
                    .ok_or_else(|| BundleError::MissingFile {
                        path: path.filesystem_path(&root),
                    })?;
                copy_executable_source(
                    &root,
                    source,
                    &path.filesystem_path(&root),
                    artifact.digest,
                    artifact.size_bytes,
                )?;
            }
            let json = serde_json::to_vec_pretty(document)?;
            write_new_file(&root, &root.join(RUNTIME_FILE), &json)
        })();
        if let Err(error) = staged {
            let _ = std::fs::remove_dir_all(&root);
            return Err(error);
        }
        if let Err(error) = publish(&root, &publish_target) {
            let _ = std::fs::remove_dir_all(&root);
            return Err(error);
        }
        RuntimeBundle::open_verified(&publish_target)
    }
}

fn write_new_file(root: &Path, path: &Path, bytes: &[u8]) -> Result<(), BundleError> {
    ensure_staging_ancestors(root, path)?;
    let parent = path.parent().ok_or_else(|| BundleError::ReadFile {
        path: path.to_path_buf(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "file has no parent"),
    })?;
    ensure_staging_directory(root, parent)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = options.open(path).map_err(|source| BundleError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    std::io::Write::write_all(&mut file, bytes).map_err(|source| BundleError::ReadFile {
        path: path.to_path_buf(),
        source,
    })
}

fn copy_executable_source(
    root: &Path,
    source: &Path,
    destination: &Path,
    expected_digest: Sha256Digest,
    expected_size: u64,
) -> Result<(), BundleError> {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    let source_metadata =
        std::fs::symlink_metadata(source).map_err(|source_error| BundleError::ReadFile {
            path: source.to_path_buf(),
            source: source_error,
        })?;
    if source_metadata.file_type().is_symlink() || !source_metadata.is_file() {
        return Err(BundleError::UnsupportedEntry {
            path: source.to_path_buf(),
        });
    }
    #[cfg(unix)]
    if source_metadata.permissions().mode() & 0o111 == 0 {
        return Err(BundleError::NotExecutable {
            path: source.to_path_buf(),
        });
    }

    ensure_staging_ancestors(root, destination)?;
    let mut input = std::fs::File::open(source).map_err(|source_error| BundleError::ReadFile {
        path: source.to_path_buf(),
        source: source_error,
    })?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|source_error| BundleError::ReadFile {
            path: destination.to_path_buf(),
            source: source_error,
        })?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = input
            .read(&mut buffer)
            .map_err(|source_error| BundleError::ReadFile {
                path: source.to_path_buf(),
                source: source_error,
            })?;
        if count == 0 {
            break;
        }
        std::io::Write::write_all(&mut output, &buffer[..count]).map_err(|source_error| {
            BundleError::ReadFile {
                path: destination.to_path_buf(),
                source: source_error,
            }
        })?;
        hasher.update(&buffer[..count]);
        total = total
            .checked_add(count as u64)
            .ok_or_else(|| BundleError::Size {
                path: destination.to_path_buf(),
                expected: expected_size,
                actual: u64::MAX,
            })?;
    }
    let actual = Sha256Digest(hasher.finalize().into());
    if total != expected_size {
        return Err(BundleError::Size {
            path: destination.to_path_buf(),
            expected: expected_size,
            actual: total,
        });
    }
    if actual != expected_digest {
        return Err(BundleError::Integrity {
            path: destination.to_path_buf(),
            expected: expected_digest,
            actual,
        });
    }
    #[cfg(unix)]
    {
        std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o755)).map_err(
            |source_error| BundleError::ReadFile {
                path: destination.to_path_buf(),
                source: source_error,
            },
        )?;
    }
    Ok(())
}

fn prepare_publish_parent(root: &Path) -> Result<PathBuf, BundleError> {
    let parent = root.parent().unwrap_or_else(|| Path::new("."));
    // A host may intentionally expose its temporary directory through a
    // compatibility symlink (for example macOS `/var`). Resolve that parent
    // once; the bundle target itself is still refused when it is a symlink.
    let canonical_parent = parent
        .canonicalize()
        .map_err(|source| BundleError::ReadFile {
            path: parent.to_path_buf(),
            source,
        })?;
    let metadata =
        std::fs::symlink_metadata(&canonical_parent).map_err(|source| BundleError::ReadFile {
            path: canonical_parent.clone(),
            source,
        })?;
    if !metadata.is_dir() {
        return Err(BundleError::NotDirectory(canonical_parent));
    }
    let name = root.file_name().ok_or_else(|| BundleError::ReadFile {
        path: root.to_path_buf(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid bundle name"),
    })?;
    Ok(canonical_parent.join(name))
}

fn reject_existing_target(root: &Path) -> Result<(), BundleError> {
    match std::fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(BundleError::ForbiddenSymlink {
            path: root.to_path_buf(),
        }),
        Ok(metadata) if !metadata.is_dir() => Err(BundleError::NotDirectory(root.to_path_buf())),
        Ok(metadata) if metadata.is_dir() => {
            if let Some(path) = find_symlink(root)? {
                Err(BundleError::ForbiddenSymlink { path })
            } else {
                Err(BundleError::TargetExists(root.to_path_buf()))
            }
        }
        Ok(_) => Err(BundleError::TargetExists(root.to_path_buf())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(BundleError::ReadFile {
            path: root.to_path_buf(),
            source,
        }),
    }
}

fn find_symlink(directory: &Path) -> Result<Option<PathBuf>, BundleError> {
    for entry in std::fs::read_dir(directory).map_err(|source| BundleError::ReadFile {
        path: directory.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| BundleError::ReadFile {
            path: directory.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let metadata =
            std::fs::symlink_metadata(&path).map_err(|source| BundleError::ReadFile {
                path: path.clone(),
                source,
            })?;
        if metadata.file_type().is_symlink() {
            return Ok(Some(path));
        }
        if metadata.is_dir()
            && let Some(path) = find_symlink(&path)?
        {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

fn create_staging_root(target: &Path) -> Result<PathBuf, BundleError> {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| BundleError::ReadFile {
            path: target.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid bundle name"),
        })?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    for attempt in 0..100u32 {
        let staged = parent.join(format!(
            ".{name}.staging-{}-{stamp}-{attempt}",
            std::process::id()
        ));
        match std::fs::create_dir(&staged) {
            Ok(()) => return Ok(staged),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(BundleError::ReadFile {
                    path: staged,
                    source,
                });
            }
        }
    }
    Err(BundleError::ReadFile {
        path: parent.to_path_buf(),
        source: std::io::Error::new(std::io::ErrorKind::AlreadyExists, "staging name exhausted"),
    })
}

fn ensure_staging_directory(root: &Path, directory: &Path) -> Result<(), BundleError> {
    let relative = directory
        .strip_prefix(root)
        .map_err(|_| BundleError::UnsupportedEntry {
            path: directory.to_path_buf(),
        })?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(BundleError::UnsupportedEntry {
                path: directory.to_path_buf(),
            });
        };
        current.push(name);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(BundleError::ForbiddenSymlink { path: current });
            }
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return Err(BundleError::UnsupportedEntry { path: current }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&current).map_err(|source| BundleError::ReadFile {
                    path: current.clone(),
                    source,
                })?;
            }
            Err(source) => {
                return Err(BundleError::ReadFile {
                    path: current,
                    source,
                });
            }
        }
    }
    Ok(())
}

fn ensure_staging_ancestors(root: &Path, path: &Path) -> Result<(), BundleError> {
    let parent = path.parent().ok_or_else(|| BundleError::UnsupportedEntry {
        path: path.to_path_buf(),
    })?;
    ensure_staging_directory(root, parent)
}

/// Publish the staged directory after the caller's advisory target check.
///
/// Another writer may create the target after `reject_existing_target` returns,
/// so this remains the security boundary and must use a kernel no-replace
/// primitive. It must never be changed to `std::fs::rename`, which replaces an
/// existing directory on POSIX and reintroduces the publication race.
fn publish_staging_root(staged: &Path, target: &Path) -> Result<(), BundleError> {
    let target_path = target.to_path_buf();
    let staged = std::ffi::CString::new(staged.as_os_str().as_encoded_bytes()).map_err(|_| {
        BundleError::UnsupportedEntry {
            path: staged.to_path_buf(),
        }
    })?;
    let target = std::ffi::CString::new(target.as_os_str().as_encoded_bytes()).map_err(|_| {
        BundleError::UnsupportedEntry {
            path: target.to_path_buf(),
        }
    })?;

    #[cfg(target_os = "linux")]
    {
        let result = unsafe {
            // SAFETY: both paths are NUL-free C strings. renameat2 performs
            // the destination existence check and rename as one syscall.
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                staged.as_ptr(),
                libc::AT_FDCWD,
                target.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        map_no_replace_result(result, target_path)
    }

    #[cfg(target_os = "macos")]
    {
        let result = unsafe {
            // SAFETY: both paths are NUL-free C strings. renameatx_np with
            // RENAME_EXCL performs the destination existence check and rename
            // as one kernel operation.
            libc::renameatx_np(
                libc::AT_FDCWD,
                staged.as_ptr(),
                libc::AT_FDCWD,
                target.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        map_no_replace_result(result as libc::c_long, target_path)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = staged;
        Err(BundleError::UnsupportedAtomicPublish { path: target_path })
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn map_no_replace_result(result: libc::c_long, target: PathBuf) -> Result<(), BundleError> {
    if result == 0 {
        return Ok(());
    }
    let source = std::io::Error::last_os_error();
    match source.raw_os_error() {
        Some(libc::EEXIST) => Err(BundleError::TargetExists(target)),
        Some(libc::ENOSYS | libc::EINVAL | libc::ENOTSUP) => {
            Err(BundleError::UnsupportedAtomicPublish { path: target })
        }
        _ => Err(BundleError::ReadFile {
            path: target,
            source,
        }),
    }
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BundleEntryKind {
    Directory,
    File,
    Symlink,
    Unsupported,
}

#[cfg(unix)]
pub(crate) fn validate_layout(root: &BundleRoot, runtime: &Runtime) -> Result<(), BundleError> {
    use std::os::fd::AsRawFd;

    let root_path = root.path();
    require_layout_directories(root)?;
    let root_fd = duplicate_directory(root.fd.as_raw_fd(), root_path)?;
    let allowed = [RUNTIME_FILE, ASSETS_DIR, BIN_DIR];
    for name in list_directory(root_fd.as_raw_fd(), root_path)? {
        let path = root_path.join(&name);
        let kind = entry_kind(root_fd.as_raw_fd(), &name, &path)?;
        if kind == BundleEntryKind::Symlink {
            return Err(BundleError::ForbiddenSymlink { path });
        }
        let name = name
            .to_str()
            .ok_or_else(|| BundleError::UnsupportedEntry { path: path.clone() })?;
        if !allowed.contains(&name) {
            return Err(BundleError::UnexpectedFile { path });
        }
        if name == RUNTIME_FILE && kind != BundleEntryKind::File {
            return Err(BundleError::UnsupportedEntry { path });
        }
        if name != RUNTIME_FILE && kind != BundleEntryKind::Directory {
            return Err(BundleError::UnsupportedEntry { path });
        }
    }

    let mut expected_assets = BTreeMap::new();
    for entry in &runtime.assets.entries {
        expected_assets.insert(entry.path.clone(), entry);
        verify_file(root, entry)?;
    }
    let mut actual_assets = BTreeSet::new();
    let mut actual_asset_directories = BTreeSet::new();
    collect_files(
        root,
        ASSETS_DIR,
        &mut actual_assets,
        &mut actual_asset_directories,
    )?;
    reject_unindexed_directories(
        root_path,
        &actual_asset_directories,
        &expected_assets.keys().cloned().collect::<BTreeSet<_>>(),
    )?;
    if actual_assets != expected_assets.keys().cloned().collect::<BTreeSet<_>>() {
        if let Some(path) = actual_assets
            .difference(&expected_assets.keys().cloned().collect())
            .next()
        {
            return Err(BundleError::UnexpectedFile {
                path: root_path.join(path.as_str()),
            });
        }
        if let Some(path) = expected_assets
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>()
            .difference(&actual_assets)
            .next()
        {
            return Err(BundleError::MissingFile {
                path: root_path.join(path.as_str()),
            });
        }
    }

    let expected_binaries = runtime
        .artifacts
        .values()
        .map(|artifact| artifact.path.clone())
        .collect::<BTreeSet<_>>();
    let mut actual_binaries = BTreeSet::new();
    let mut actual_binary_directories = BTreeSet::new();
    collect_files(
        root,
        BIN_DIR,
        &mut actual_binaries,
        &mut actual_binary_directories,
    )?;
    reject_unindexed_directories(root_path, &actual_binary_directories, &expected_binaries)?;
    if actual_binaries != expected_binaries {
        if let Some(path) = actual_binaries.difference(&expected_binaries).next() {
            return Err(BundleError::UnexpectedFile {
                path: root_path.join(path.as_str()),
            });
        }
        if let Some(path) = expected_binaries.difference(&actual_binaries).next() {
            return Err(BundleError::MissingFile {
                path: root_path.join(path.as_str()),
            });
        }
    }
    for artifact in runtime.artifacts.values() {
        verify_binary(root, artifact)?;
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn validate_layout(root: &BundleRoot, _runtime: &Runtime) -> Result<(), BundleError> {
    Err(BundleError::UnsupportedSecureOpen {
        path: root.path().to_path_buf(),
    })
}

#[cfg(unix)]
fn collect_files(
    root: &BundleRoot,
    relative_directory: &str,
    paths: &mut BTreeSet<BundlePath>,
    directories: &mut BTreeSet<BundlePath>,
) -> Result<(), BundleError> {
    use std::os::fd::AsRawFd;

    let directory_path = root.path().join(relative_directory);
    let Some(directory_fd) = open_relative_directory(root, relative_directory, &directory_path)?
    else {
        return Ok(());
    };
    collect_files_at(
        directory_fd.as_raw_fd(),
        &directory_path,
        relative_directory,
        paths,
        directories,
    )
}

#[cfg(unix)]
fn collect_files_at(
    directory_fd: libc::c_int,
    directory_path: &Path,
    relative_directory: &str,
    paths: &mut BTreeSet<BundlePath>,
    directories: &mut BTreeSet<BundlePath>,
) -> Result<(), BundleError> {
    use std::os::fd::AsRawFd;

    for name in list_directory(directory_fd, directory_path)? {
        let path = directory_path.join(&name);
        let relative = relative_path(relative_directory, &name, &path)?;
        match entry_kind(directory_fd, &name, &path)? {
            BundleEntryKind::Symlink => return Err(BundleError::ForbiddenSymlink { path }),
            BundleEntryKind::Directory => {
                directories.insert(BundlePath::new(relative.clone())?);
                let child = open_directory_child(directory_fd, &name, &path)?;
                collect_files_at(child.as_raw_fd(), &path, &relative, paths, directories)?;
            }
            BundleEntryKind::File => {
                paths.insert(BundlePath::new(relative)?);
            }
            BundleEntryKind::Unsupported => return Err(BundleError::UnsupportedEntry { path }),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn relative_path(parent: &str, name: &std::ffi::OsStr, path: &Path) -> Result<String, BundleError> {
    let name = name.to_str().ok_or_else(|| BundleError::UnsupportedEntry {
        path: path.to_path_buf(),
    })?;
    Ok(if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    })
}

#[cfg(unix)]
fn duplicate_directory(fd: libc::c_int, path: &Path) -> Result<std::os::fd::OwnedFd, BundleError> {
    use std::os::fd::FromRawFd;

    let duplicate = unsafe { libc::dup(fd) };
    if duplicate < 0 {
        return Err(BundleError::ReadFile {
            path: path.to_path_buf(),
            source: std::io::Error::last_os_error(),
        });
    }
    Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(duplicate) })
}

#[cfg(unix)]
fn open_relative_directory(
    root: &BundleRoot,
    relative: &str,
    path: &Path,
) -> Result<Option<std::os::fd::OwnedFd>, BundleError> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    let mut parent = duplicate_directory(root.fd.as_raw_fd(), root.path())?;
    for component in relative.split('/') {
        let component = CString::new(component).map_err(|_| BundleError::UnsupportedEntry {
            path: path.to_path_buf(),
        })?;
        let child = unsafe {
            // SAFETY: parent is an owned directory descriptor and component
            // is one NUL-free relative name. O_NOFOLLOW prevents a directory
            // substitution from redirecting the walk.
            libc::openat(
                parent.as_raw_fd(),
                component.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if child < 0 {
            let source = std::io::Error::last_os_error();
            if source.kind() == std::io::ErrorKind::NotFound {
                return Ok(None);
            }
            if source.raw_os_error() == Some(libc::ELOOP) || path_contains_symlink(path) {
                return Err(BundleError::ForbiddenSymlink {
                    path: path.to_path_buf(),
                });
            }
            return Err(BundleError::ReadFile {
                path: path.to_path_buf(),
                source,
            });
        }
        parent = unsafe { OwnedFd::from_raw_fd(child) };
    }
    Ok(Some(parent))
}

#[cfg(unix)]
fn open_directory_child(
    parent: libc::c_int,
    name: &std::ffi::OsStr,
    path: &Path,
) -> Result<std::os::fd::OwnedFd, BundleError> {
    use std::ffi::CString;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;

    let name = CString::new(name.as_bytes()).map_err(|_| BundleError::UnsupportedEntry {
        path: path.to_path_buf(),
    })?;
    let child = unsafe {
        // SAFETY: parent is a directory descriptor and name is a NUL-free
        // single component. O_NOFOLLOW prevents a substituted child directory.
        libc::openat(
            parent,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if child < 0 {
        let source = std::io::Error::last_os_error();
        if source.raw_os_error() == Some(libc::ELOOP) {
            return Err(BundleError::ForbiddenSymlink {
                path: path.to_path_buf(),
            });
        }
        return Err(BundleError::ReadFile {
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(unsafe { OwnedFd::from_raw_fd(child) })
}

#[cfg(unix)]
fn entry_kind(
    parent: libc::c_int,
    name: &std::ffi::OsStr,
    path: &Path,
) -> Result<BundleEntryKind, BundleError> {
    use std::ffi::CString;
    use std::mem::MaybeUninit;
    use std::os::unix::ffi::OsStrExt;

    let name = CString::new(name.as_bytes()).map_err(|_| BundleError::UnsupportedEntry {
        path: path.to_path_buf(),
    })?;
    let mut metadata = MaybeUninit::<libc::stat>::uninit();
    let result = unsafe {
        // SAFETY: metadata points to writable storage and name is a NUL-free
        // relative entry. AT_SYMLINK_NOFOLLOW classifies the entry itself.
        libc::fstatat(
            parent,
            name.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result < 0 {
        return Err(BundleError::ReadFile {
            path: path.to_path_buf(),
            source: std::io::Error::last_os_error(),
        });
    }
    let metadata = unsafe { metadata.assume_init() };
    let mode = metadata.st_mode & libc::S_IFMT;
    if mode == libc::S_IFLNK {
        Ok(BundleEntryKind::Symlink)
    } else if mode == libc::S_IFDIR {
        Ok(BundleEntryKind::Directory)
    } else if mode == libc::S_IFREG {
        Ok(BundleEntryKind::File)
    } else {
        Ok(BundleEntryKind::Unsupported)
    }
}

#[cfg(unix)]
fn list_directory(fd: libc::c_int, path: &Path) -> Result<Vec<std::ffi::OsString>, BundleError> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStringExt;

    let duplicate = unsafe { libc::dup(fd) };
    if duplicate < 0 {
        return Err(BundleError::ReadFile {
            path: path.to_path_buf(),
            source: std::io::Error::last_os_error(),
        });
    }
    let directory = unsafe { libc::fdopendir(duplicate) };
    if directory.is_null() {
        let source = std::io::Error::last_os_error();
        unsafe { libc::close(duplicate) };
        return Err(BundleError::ReadFile {
            path: path.to_path_buf(),
            source,
        });
    }

    let mut names = Vec::new();
    loop {
        reset_errno();
        let entry = unsafe { libc::readdir(directory) };
        if entry.is_null() {
            let source = std::io::Error::last_os_error();
            if source.raw_os_error().is_some_and(|error| error != 0) {
                unsafe { libc::closedir(directory) };
                return Err(BundleError::ReadFile {
                    path: path.to_path_buf(),
                    source,
                });
            }
            break;
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if name.to_bytes() != b"." && name.to_bytes() != b".." {
            names.push(std::ffi::OsString::from_vec(name.to_bytes().to_vec()));
        }
    }
    if unsafe { libc::closedir(directory) } != 0 {
        return Err(BundleError::ReadFile {
            path: path.to_path_buf(),
            source: std::io::Error::last_os_error(),
        });
    }
    Ok(names)
}

#[cfg(unix)]
fn reset_errno() {
    #[cfg(target_os = "linux")]
    unsafe {
        *libc::__errno_location() = 0;
    }
    #[cfg(target_os = "macos")]
    unsafe {
        *libc::__error() = 0;
    }
}

fn reject_unindexed_directories(
    root: &Path,
    actual: &BTreeSet<BundlePath>,
    files: &BTreeSet<BundlePath>,
) -> Result<(), BundleError> {
    let mut expected = BTreeSet::new();
    for file in files {
        let mut components = file.as_str().split('/').collect::<Vec<_>>();
        components.pop();
        for length in 1..=components.len() {
            expected.insert(BundlePath::new(components[..length].join("/"))?);
        }
    }
    if let Some(unindexed) = actual.difference(&expected).next() {
        return Err(BundleError::UnindexedDirectory {
            path: root.join(unindexed.as_str()),
        });
    }
    Ok(())
}

fn verify_binary(root: &BundleRoot, binary: &BinaryReference) -> Result<(), BundleError> {
    let path = binary.path.filesystem_path(root.path());
    let file = open_bundle_file(root, &binary.path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let metadata = file.metadata().map_err(|source| BundleError::ReadFile {
            path: path.clone(),
            source,
        })?;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(BundleError::NotExecutable { path });
        }
    }
    verify_open_file(file, &path, binary.digest, Some(binary.size_bytes))
}

fn verify_file(root: &BundleRoot, entry: &AssetRecord) -> Result<(), BundleError> {
    verify_digest_and_size(root, &entry.path, entry.digest, Some(entry.size_bytes))
}

fn verify_digest_and_size(
    root: &BundleRoot,
    bundle_path: &BundlePath,
    expected: Sha256Digest,
    expected_size: Option<u64>,
) -> Result<(), BundleError> {
    let path = bundle_path.filesystem_path(root.path());
    let file = open_bundle_file(root, bundle_path)?;
    verify_open_file(file, &path, expected, expected_size)
}

fn verify_open_file(
    mut file: std::fs::File,
    path: &Path,
    expected: Sha256Digest,
    expected_size: Option<u64>,
) -> Result<(), BundleError> {
    let metadata = file.metadata().map_err(|source| BundleError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(BundleError::UnsupportedEntry {
            path: path.to_path_buf(),
        });
    }
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|source| BundleError::ReadFile {
                path: path.to_path_buf(),
                source,
            })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        total = total
            .checked_add(count as u64)
            .ok_or_else(|| BundleError::Size {
                path: path.to_path_buf(),
                expected: expected_size.unwrap_or(u64::MAX),
                actual: u64::MAX,
            })?;
    }
    if let Some(expected_size) = expected_size
        && total != expected_size
    {
        return Err(BundleError::Size {
            path: path.to_path_buf(),
            expected: expected_size,
            actual: total,
        });
    }
    let actual = Sha256Digest(hasher.finalize().into());
    if actual != expected {
        return Err(BundleError::Integrity {
            path: path.to_path_buf(),
            expected,
            actual,
        });
    }
    Ok(())
}

pub(crate) fn read_and_verify(
    file: &mut std::fs::File,
    path: &Path,
    expected: Sha256Digest,
    expected_size: Option<u64>,
) -> Result<Vec<u8>, BundleError> {
    let metadata = file.metadata().map_err(|source| BundleError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(BundleError::UnsupportedEntry {
            path: path.to_path_buf(),
        });
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| BundleError::ReadFile {
            path: path.to_path_buf(),
            source,
        })?;
    let actual_size = bytes.len() as u64;
    if let Some(expected_size) = expected_size
        && actual_size != expected_size
    {
        return Err(BundleError::Size {
            path: path.to_path_buf(),
            expected: expected_size,
            actual: actual_size,
        });
    }
    let actual = Sha256Digest::of(&bytes);
    if actual != expected {
        return Err(BundleError::Integrity {
            path: path.to_path_buf(),
            expected,
            actual,
        });
    }
    Ok(bytes)
}

pub(crate) fn open_bundle_file(
    root: &BundleRoot,
    path: &BundlePath,
) -> Result<std::fs::File, BundleError> {
    let filesystem_path = path.filesystem_path(root.path());
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

        let root_fd = unsafe { libc::dup(root.fd.as_raw_fd()) };
        if root_fd < 0 {
            return Err(BundleError::ReadFile {
                path: filesystem_path,
                source: std::io::Error::last_os_error(),
            });
        }
        let mut parent = unsafe { OwnedFd::from_raw_fd(root_fd) };
        let components = path.as_str().split('/').collect::<Vec<_>>();
        for (index, component) in components.iter().enumerate() {
            let name = CString::new(*component).map_err(|_| BundleError::UnsupportedEntry {
                path: filesystem_path.clone(),
            })?;
            let flags = if index + 1 == components.len() {
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC
            } else {
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC
            };
            let fd = unsafe {
                // SAFETY: parent remains an owned directory fd and name is a
                // NUL-free single path component. O_NOFOLLOW applies at every
                // component, preventing directory and leaf substitution.
                libc::openat(parent.as_raw_fd(), name.as_ptr(), flags)
            };
            if fd < 0 {
                return Err(io_error_for_path(&filesystem_path));
            }
            parent = unsafe { OwnedFd::from_raw_fd(fd) };
        }
        Ok(std::fs::File::from(parent))
    }
    #[cfg(not(unix))]
    {
        // No std-only API can bind every component to a no-follow directory
        // handle on these targets. Refuse the access rather than converting an
        // lstat/open check into a false security guarantee.
        Err(BundleError::UnsupportedSecureOpen {
            path: filesystem_path,
        })
    }
}

#[cfg(unix)]
fn root_open_error(path: &Path) -> BundleError {
    let source = std::io::Error::last_os_error();
    if source.raw_os_error() == Some(libc::ENOTDIR) {
        BundleError::NotDirectory(path.to_path_buf())
    } else if source.raw_os_error() == Some(libc::ELOOP) {
        BundleError::ForbiddenSymlink {
            path: path.to_path_buf(),
        }
    } else {
        BundleError::Root {
            path: path.to_path_buf(),
            source,
        }
    }
}

#[cfg(unix)]
fn io_error_for_path(path: &Path) -> BundleError {
    let source = std::io::Error::last_os_error();
    if source.kind() == std::io::ErrorKind::NotFound {
        BundleError::MissingFile {
            path: path.to_path_buf(),
        }
    } else if source.raw_os_error() == Some(libc::ELOOP) || path_contains_symlink(path) {
        BundleError::ForbiddenSymlink {
            path: path.to_path_buf(),
        }
    } else {
        BundleError::ReadFile {
            path: path.to_path_buf(),
            source,
        }
    }
}

#[cfg(unix)]
fn path_contains_symlink(path: &Path) -> bool {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if std::fs::symlink_metadata(&current)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoxal_model::RobotBuilder;
    use phoxal_model::builder::Kinematics;
    use phoxal_model::component::capability::{Capability, Motor, MotorCommand, StructuralTarget};
    use phoxal_model::identity::JointId;
    use phoxal_runtime_contract::metadata::{
        ParticipantContract, ParticipantKind, ParticipantSchemas,
    };
    use phoxal_runtime_contract::version::{BusAbi, LaunchAbi, RobotApi, RuntimeSchema};

    type StagedBytes = (
        RuntimeDocument,
        BTreeMap<AssetId, Vec<u8>>,
        BTreeMap<BundlePath, PathBuf>,
    );

    fn document() -> StagedBytes {
        let robot = RobotBuilder::new("rover")
            .build()
            .expect("minimal robot is valid");
        let asset_id = AssetId::new("robot/structure.json").expect("asset id");
        let asset_bytes = b"compiled structure".to_vec();
        let mut assets = BTreeMap::new();
        assets.insert(asset_id, asset_bytes);
        let binary_path = BundlePath::new("bin/drive").expect("binary path");
        let binary_source = std::env::current_exe().expect("test binary path");
        let mut binaries = BTreeMap::new();
        binaries.insert(binary_path, binary_source.clone());
        let artifact_id = ParticipantArtifactId::new("drive").expect("artifact id");
        let binary = BinaryReference::from_file(
            BundlePath::new("bin/drive").expect("binary path"),
            ParticipantContract {
                id: artifact_id.clone(),
                kind: ParticipantKind::Service,
                api: RobotApi::V0_2,
                schemas: ParticipantSchemas {
                    bus: BusAbi::V0,
                    launch: LaunchAbi::V0,
                    runtime: RuntimeSchema::V0,
                },
                requirement: None,
                config_schema: serde_json::json!({"type":"null"}),
            },
            &binary_source,
        )
        .expect("test binary hashes");
        let participant = RuntimeParticipant::new(
            ParticipantId::new("drive").expect("participant id"),
            artifact_id.clone(),
            None,
            None,
            ParticipantClock::Real,
        );
        let mut artifacts = BTreeMap::new();
        artifacts.insert(artifact_id, binary);
        let index = AssetIndex::from_bytes(&assets).expect("asset index");
        let runtime =
            Runtime::new(robot, artifacts, vec![participant], index, None).expect("runtime");
        let document = RuntimeDocument::new(runtime);
        (document, assets, binaries)
    }

    fn motor_robot(left: MotorCommand, right: MotorCommand) -> Robot {
        let motor_type = |command| {
            move |motor: phoxal_model::builder::ComponentTypeBuilder| {
                motor.capability(
                    "spin",
                    Capability::Motor(Motor {
                        target: StructuralTarget::Joint {
                            id: JointId::new("axle"),
                        },
                        command,
                        gear_ratio: 1.0,
                        max_torque_nm: None,
                        max_velocity_radps: None,
                    }),
                )
            }
        };
        RobotBuilder::new("rover")
            .component_type("left_motor", motor_type(left))
            .component_type("right_motor", motor_type(right))
            .component("left_drive", "left_motor")
            .component("right_drive", "right_motor")
            .kinematics(Kinematics::Differential {
                left_actuators: &["left_drive.spin"],
                right_actuators: &["right_drive.spin"],
                left_encoders: &[],
                right_encoders: &[],
                wheel_radius_m: 0.1,
                wheel_base_m: 0.4,
            })
            .build()
            .expect("differential test robot is valid")
    }

    fn topology_robot(kind: phoxal_model::robot::KinematicKind) -> Robot {
        match kind {
            phoxal_model::robot::KinematicKind::Differential => {
                motor_robot(MotorCommand::Velocity, MotorCommand::Velocity)
            }
            phoxal_model::robot::KinematicKind::Mecanum => RobotBuilder::new("rover")
                .component_type("motor", |motor| motor.motor("spin", "axle"))
                .component("front_left", "motor")
                .component("front_right", "motor")
                .component("rear_left", "motor")
                .component("rear_right", "motor")
                .kinematics(Kinematics::Mecanum {
                    front_left_actuator: "front_left.spin",
                    front_right_actuator: "front_right.spin",
                    rear_left_actuator: "rear_left.spin",
                    rear_right_actuator: "rear_right.spin",
                    wheel_radius_m: 0.1,
                    wheel_base_m: 0.4,
                    track_m: 0.3,
                })
                .build()
                .expect("mecanum test robot is valid"),
            phoxal_model::robot::KinematicKind::Ackermann => RobotBuilder::new("rover")
                .component_type("motor", |motor| motor.motor("spin", "axle"))
                .component("drive", "motor")
                .kinematics(Kinematics::Ackermann {
                    steering_actuator: "drive.spin",
                    drive_actuator: "drive.spin",
                    steering_encoder: None,
                    drive_encoder: None,
                    wheel_base_m: 0.4,
                    track_m: 0.3,
                    max_steering_angle_rad: 0.6,
                })
                .build()
                .expect("ackermann test robot is valid"),
            phoxal_model::robot::KinematicKind::Omnidirectional => RobotBuilder::new("rover")
                .kinematics(Kinematics::Omnidirectional {
                    actuators: &[],
                    encoders: &[],
                })
                .build()
                .expect("omnidirectional test robot is valid"),
        }
    }

    fn requirement_document(robot: Robot) -> StagedBytes {
        let (document, assets, binaries) = document();
        let RuntimeDocument::V0(mut runtime) = document;
        runtime.robot = robot;
        runtime
            .artifacts
            .values_mut()
            .next()
            .unwrap()
            .contract
            .requirement = Some(ParticipantRequirement::DifferentialDriveVelocity);
        let document = RuntimeDocument::new(
            Runtime::new(
                runtime.robot,
                runtime.artifacts,
                runtime.participants,
                runtime.assets,
                runtime.router,
            )
            .expect("requirement runtime is valid"),
        );
        (document, assets, binaries)
    }

    #[test]
    fn stock_drive_requirement_accepts_differential_velocity_motors() {
        let (document, _, _) =
            requirement_document(motor_robot(MotorCommand::Velocity, MotorCommand::Velocity));
        assert_eq!(
            document.robot().motion().kinematic().kind(),
            phoxal_model::robot::KinematicKind::Differential
        );
    }

    #[test]
    fn stock_drive_requirement_rejects_non_differential_topologies() {
        for kind in [
            phoxal_model::robot::KinematicKind::Mecanum,
            phoxal_model::robot::KinematicKind::Omnidirectional,
            phoxal_model::robot::KinematicKind::Ackermann,
        ] {
            let runtime = {
                let (base, assets, binaries) = document();
                let RuntimeDocument::V0(mut runtime) = base;
                runtime.robot = topology_robot(kind);
                runtime
                    .artifacts
                    .values_mut()
                    .next()
                    .unwrap()
                    .contract
                    .requirement = Some(ParticipantRequirement::DifferentialDriveVelocity);
                let _ = (assets, binaries);
                runtime
            };
            assert!(matches!(
                Runtime::new(
                    runtime.robot,
                    runtime.artifacts,
                    runtime.participants,
                    runtime.assets,
                    runtime.router,
                ),
                Err(DocumentError::RequirementKinematicsMismatch { .. })
            ));
        }
    }

    #[test]
    fn stock_drive_requirement_rejects_each_nonvelocity_drive_side() {
        for (left, right, actuator) in [
            (
                MotorCommand::Position,
                MotorCommand::Velocity,
                "left_drive.spin",
            ),
            (
                MotorCommand::Velocity,
                MotorCommand::Torque,
                "right_drive.spin",
            ),
        ] {
            let (base, _, _) = document();
            let RuntimeDocument::V0(mut runtime) = base;
            runtime.robot = motor_robot(left, right);
            runtime
                .artifacts
                .values_mut()
                .next()
                .unwrap()
                .contract
                .requirement = Some(ParticipantRequirement::DifferentialDriveVelocity);
            assert!(matches!(
                Runtime::new(
                    runtime.robot,
                    runtime.artifacts,
                    runtime.participants,
                    runtime.assets,
                    runtime.router,
                ),
                Err(DocumentError::RequirementMotorModeMismatch { actuator: ref found, .. })
                    if found.to_string() == actuator
            ));
        }
    }

    #[test]
    fn paths_and_digests_are_strict() {
        for (value, valid) in [
            ("assets/mesh.obj", true),
            ("bin/brain", true),
            ("", false),
            ("/etc/passwd", false),
            ("assets/../bin/brain", false),
            ("assets//mesh.obj", false),
            ("assets\\mesh.obj", false),
        ] {
            assert_eq!(BundlePath::new(value).is_ok(), valid, "{value}");
        }
        let digest = Sha256Digest::of(b"hello");
        assert_eq!(
            Sha256Digest::from_reader(std::io::Cursor::new(b"hello")).expect("reader hashes"),
            digest
        );
        assert_eq!(Sha256Digest::parse(&digest.as_hex()), Ok(digest));
        assert!(Sha256Digest::parse(&digest.as_hex().to_uppercase()).is_err());
    }

    #[test]
    fn runtime_json_is_tagged_strict_and_robot_id_is_persisted_once() {
        let (document, _, _) = document();
        let value = serde_json::to_value(&document).expect("document serializes");
        assert_eq!(value["schema"], RUNTIME_SCHEMA);
        assert_eq!(
            value["artifacts"]["drive"]["contract"]["requirement"],
            serde_json::Value::Null,
            "runtime artifacts persist an explicit requirement value"
        );
        assert!(value.get("robot_id").is_none());
        let text = serde_json::to_string(&document).expect("document serializes");
        assert_eq!(text.matches("\"id\":\"rover\"").count(), 1);
        assert!(
            serde_json::from_str::<RuntimeDocument>(
                &text.replace("phoxal/runtime-bundle/v0", "phoxal/runtime-bundle/v1")
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<RuntimeDocument>(
                &text.replace("\"router\":null", "\"router\":null,\"source\":true")
            )
            .is_err()
        );

        let mut duplicate = serde_json::to_value(&document).expect("document serializes");
        let participants = duplicate["participants"]
            .as_array_mut()
            .expect("participants are an array");
        participants.push(participants[0].clone());
        assert!(
            serde_json::from_value::<RuntimeDocument>(duplicate).is_err(),
            "direct runtime.json decoding must validate the participant graph"
        );
    }

    #[test]
    fn multiple_participant_instances_may_share_one_artifact() {
        let (document, _, _) = document();
        let RuntimeDocument::V0(mut runtime) = document;
        let artifact = runtime.participants[0].artifact.clone();
        runtime.participants.push(RuntimeParticipant::new(
            ParticipantId::new("drive-rear").expect("participant id"),
            artifact.clone(),
            None,
            None,
            ParticipantClock::Real,
        ));

        let document = RuntimeDocument::new(
            Runtime::new(
                runtime.robot,
                runtime.artifacts,
                runtime.participants,
                runtime.assets,
                runtime.router,
            )
            .expect("shared artifact is valid"),
        );
        assert_eq!(document.artifacts().len(), 1);
        assert_eq!(document.participants().len(), 2);
        assert!(
            document
                .participants()
                .iter()
                .all(|participant| participant.artifact == artifact)
        );
    }

    #[test]
    fn participant_config_is_validated_against_embedded_binary_schema() {
        let (document, _, _) = document();
        let RuntimeDocument::V0(mut runtime) = document;
        runtime.participants[0].config = Some(serde_json::json!({"unexpected": true}));
        assert!(matches!(
            Runtime::new(
                runtime.robot,
                runtime.artifacts,
                runtime.participants,
                runtime.assets,
                runtime.router,
            ),
            Err(DocumentError::InvalidConfig { .. })
        ));
    }

    #[test]
    fn writer_and_reader_use_only_runtime_json_and_indexed_files() {
        let parent = tempfile::tempdir().expect("bundle parent");
        let root = parent.path().join("bundle");
        let (document, assets, binaries) = document();
        let loaded = BundleWriter::write(&root, &document, &assets, &binaries)
            .expect("bundle writes and reopens");
        assert_eq!(loaded.robot_id().as_str(), "rover");
        assert_eq!(loaded.participants().len(), 1);
        let id = AssetId::new("robot/structure.json").expect("asset id");
        assert_eq!(
            loaded.assets().read(&id).expect("asset reads"),
            b"compiled structure"
        );
        assert!(!root.join("robot.yaml").exists());
    }

    #[cfg(unix)]
    #[test]
    fn writer_stages_a_real_executable_with_canonical_mode() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;

        let parent = tempfile::tempdir().expect("bundle parent");
        let root = parent.path().join("bundle");
        let source = parent.path().join("probe-source");
        std::fs::write(&source, b"#!/bin/sh\nprintf staged\n").expect("probe source");
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o700))
            .expect("probe source mode");

        let (document, assets, _) = document();
        let RuntimeDocument::V0(mut runtime) = document;
        let artifact_id = ParticipantArtifactId::new("drive").expect("artifact id");
        let existing = runtime.artifacts.get(&artifact_id).expect("drive artifact");
        let reference =
            BinaryReference::from_file(existing.path.clone(), existing.contract.clone(), &source)
                .expect("probe reference");
        runtime.artifacts.insert(artifact_id, reference);
        let document = RuntimeDocument::new(
            Runtime::new(
                runtime.robot,
                runtime.artifacts,
                runtime.participants,
                runtime.assets,
                runtime.router,
            )
            .expect("runtime document"),
        );
        let binaries =
            BTreeMap::from([(BundlePath::new("bin/drive").expect("binary path"), source)]);

        BundleWriter::write(&root, &document, &assets, &binaries).expect("bundle writes");
        let staged = root.join("bin/drive");
        assert_eq!(
            std::fs::metadata(&staged)
                .expect("staged metadata")
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        let output = Command::new(&staged)
            .output()
            .expect("staged executable runs");
        assert!(output.status.success());
        assert_eq!(output.stdout, b"staged");
    }

    #[cfg(unix)]
    #[test]
    fn verified_open_rejects_a_non_executable_binary() {
        use std::os::unix::fs::PermissionsExt;

        let parent = tempfile::tempdir().expect("bundle parent");
        let root = parent.path().join("bundle");
        let (document, assets, binaries) = document();
        BundleWriter::write(&root, &document, &assets, &binaries).expect("bundle writes");
        let binary = root.join("bin/drive");
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o644))
            .expect("remove execute bit");
        assert!(matches!(
            RuntimeBundle::open_verified(&root),
            Err(BundleError::NotExecutable { .. })
        ));
    }

    #[test]
    fn verified_and_selected_open_require_both_layout_directories() {
        for directory in [ASSETS_DIR, BIN_DIR] {
            let parent = tempfile::tempdir().expect("bundle parent");
            let root = parent.path().join("bundle");
            let (document, assets, binaries) = document();
            BundleWriter::write(&root, &document, &assets, &binaries).expect("bundle writes");
            std::fs::remove_dir_all(root.join(directory))
                .expect("remove required layout directory");

            assert!(matches!(
                RuntimeBundle::open_verified(&root),
                Err(BundleError::MissingFile { .. })
            ));
            assert!(matches!(
                ParticipantBundle::open(
                    &root,
                    &ParticipantId::new("drive").expect("participant id")
                ),
                Err(BundleError::MissingFile { .. })
            ));
        }
    }

    #[test]
    fn selection_is_exact_and_happens_before_any_runtime_side_effect() {
        let parent = tempfile::tempdir().expect("bundle parent");
        let root = parent.path().join("bundle");
        let (document, assets, binaries) = document();
        let loaded =
            BundleWriter::write(&root, &document, &assets, &binaries).expect("bundle writes");
        let unknown = ParticipantId::new("missing").expect("valid unknown id");
        assert!(matches!(
            loaded.participant(&unknown),
            Err(SelectionError::Unknown { .. })
        ));
    }

    #[test]
    fn participant_open_skips_unrelated_artifact_hashes_but_full_open_does_not() {
        let parent = tempfile::tempdir().expect("bundle parent");
        let root = parent.path().join("bundle");
        let (document, assets, mut binaries) = document();
        let RuntimeDocument::V0(mut runtime) = document;
        let source = std::env::current_exe().expect("test binary path");
        let other_artifact = ParticipantArtifactId::new("other").expect("artifact id");
        let original = runtime
            .artifacts
            .get(&ParticipantArtifactId::new("drive").expect("artifact id"))
            .expect("drive artifact");
        let mut contract = original.contract.clone();
        contract.id = other_artifact.clone();
        let other_path = BundlePath::new("bin/other").expect("binary path");
        runtime.artifacts.insert(
            other_artifact.clone(),
            BinaryReference::from_file(other_path.clone(), contract, &source)
                .expect("other artifact"),
        );
        runtime.participants.push(RuntimeParticipant::new(
            ParticipantId::new("other").expect("participant id"),
            other_artifact,
            None,
            None,
            ParticipantClock::Real,
        ));
        binaries.insert(other_path, source);
        let document = RuntimeDocument::new(
            Runtime::new(
                runtime.robot,
                runtime.artifacts,
                runtime.participants,
                runtime.assets,
                runtime.router,
            )
            .expect("runtime document"),
        );
        BundleWriter::write(&root, &document, &assets, &binaries).expect("bundle writes");
        std::fs::write(root.join("bin/other"), b"tampered").expect("tamper other artifact");

        ParticipantBundle::open(&root, &ParticipantId::new("drive").expect("participant id"))
            .expect("selected participant does not hash unrelated artifact");
        assert!(matches!(
            RuntimeBundle::open_verified(&root),
            Err(BundleError::Integrity { .. } | BundleError::Size { .. })
        ));
    }

    #[test]
    fn a_mutated_indexed_asset_is_rejected_on_open_and_read() {
        let parent = tempfile::tempdir().expect("bundle parent");
        let root = parent.path().join("bundle");
        let (document, assets, binaries) = document();
        let loaded =
            BundleWriter::write(&root, &document, &assets, &binaries).expect("bundle writes");
        let id = AssetId::new("robot/structure.json").expect("asset id");
        std::fs::write(root.join("assets/robot/structure.json"), b"tampered")
            .expect("tamper asset");
        assert!(matches!(
            loaded.assets().read(&id),
            Err(BundleError::Integrity { .. } | BundleError::Size { .. })
        ));
        assert!(matches!(
            RuntimeBundle::open_verified(&root),
            Err(BundleError::Integrity { .. } | BundleError::Size { .. })
        ));
    }

    #[test]
    fn a_mutated_indexed_binary_is_rejected_on_open() {
        let parent = tempfile::tempdir().expect("bundle parent");
        let root = parent.path().join("bundle");
        let (document, assets, binaries) = document();
        BundleWriter::write(&root, &document, &assets, &binaries).expect("bundle writes");
        std::fs::write(root.join("bin/drive"), b"tampered").expect("tamper binary");
        assert!(matches!(
            RuntimeBundle::open_verified(&root),
            Err(BundleError::Integrity { .. } | BundleError::Size { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_bundle_files_are_rejected_before_runtime_use() {
        let parent = tempfile::tempdir().expect("bundle parent");
        let root = parent.path().join("bundle");
        let outside = tempfile::tempdir().expect("outside root");
        let (document, assets, binaries) = document();
        let loaded =
            BundleWriter::write(&root, &document, &assets, &binaries).expect("bundle writes");
        let asset = root.join("assets/robot/structure.json");
        std::fs::remove_file(&asset).expect("remove indexed asset");
        std::fs::write(outside.path().join("structure.json"), b"outside").expect("outside asset");
        std::os::unix::fs::symlink(outside.path().join("structure.json"), &asset)
            .expect("symlink asset");
        assert!(matches!(
            RuntimeBundle::open_verified(&root),
            Err(BundleError::ForbiddenSymlink { .. })
        ));
        assert!(matches!(
            loaded
                .assets()
                .read(&AssetId::new("robot/structure.json").expect("asset id")),
            Err(BundleError::ForbiddenSymlink { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn substituted_assets_directory_is_not_followed_by_asset_access() {
        let parent = tempfile::tempdir().expect("bundle parent");
        let root = parent.path().join("bundle");
        let outside = tempfile::tempdir().expect("outside root");
        let (document, assets, binaries) = document();
        let loaded =
            BundleWriter::write(&root, &document, &assets, &binaries).expect("bundle writes");
        let assets_dir = root.join(ASSETS_DIR);
        let moved_assets = outside.path().join(ASSETS_DIR);
        std::fs::rename(&assets_dir, &moved_assets).expect("move indexed assets");
        std::os::unix::fs::symlink(&moved_assets, &assets_dir).expect("symlink assets directory");
        assert!(matches!(
            loaded
                .assets()
                .read(&AssetId::new("robot/structure.json").expect("asset id")),
            Err(BundleError::ForbiddenSymlink { .. })
        ));
        assert!(matches!(
            RuntimeBundle::open_verified(&root),
            Err(BundleError::ForbiddenSymlink { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn pinned_root_cannot_be_redirected_by_root_symlink_substitution() {
        let parent = tempfile::tempdir().expect("bundle parent");
        let root = parent.path().join("bundle");
        let moved = parent.path().join("bundle-original");
        let outside = parent.path().join("outside");
        let bundle_path = BundlePath::new("assets/asset").expect("bundle path");
        std::fs::create_dir_all(root.join(ASSETS_DIR)).expect("bundle assets");
        std::fs::create_dir_all(outside.join(ASSETS_DIR)).expect("outside assets");
        std::fs::write(root.join("assets/asset"), b"pinned").expect("pinned asset");
        std::fs::write(outside.join("assets/asset"), b"redirected").expect("outside asset");

        let pinned = BundleRoot::open(&root).expect("pin bundle root");
        std::fs::rename(&root, &moved).expect("move original bundle");
        std::os::unix::fs::symlink(&outside, &root).expect("substitute root symlink");

        let mut file = open_bundle_file(&pinned, &bundle_path).expect("open pinned asset");
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).expect("read pinned asset");
        assert_eq!(bytes, b"pinned");
    }

    #[cfg(unix)]
    #[test]
    fn layout_validation_stays_on_pinned_tree_after_root_substitution() {
        let parent = tempfile::tempdir().expect("bundle parent");
        let root = parent.path().join("bundle");
        let moved = parent.path().join("bundle-original");
        let outside = parent.path().join("outside");
        let (document, assets, binaries) = document();
        let loaded =
            BundleWriter::write(&root, &document, &assets, &binaries).expect("bundle writes");
        drop(loaded);
        std::fs::write(root.join(ASSETS_DIR).join("unexpected"), b"extra")
            .expect("extra original file");

        // Build a pathname replacement containing the expected files but not
        // the extra file. A pathname-based validator would accept this tree;
        // the pinned descriptor must continue to observe the original.
        let RuntimeDocument::V0(runtime) = &document;
        std::fs::create_dir_all(outside.join(ASSETS_DIR).join("robot")).expect("outside assets");
        std::fs::create_dir_all(outside.join(BIN_DIR)).expect("outside binaries");
        std::fs::write(
            outside.join(RUNTIME_FILE),
            serde_json::to_vec_pretty(&document).expect("runtime json"),
        )
        .expect("outside runtime");
        for (id, bytes) in &assets {
            std::fs::write(
                outside
                    .join(ASSETS_DIR)
                    .join(id.as_str().split('/').collect::<PathBuf>()),
                bytes,
            )
            .expect("outside asset");
        }
        for (path, source) in &binaries {
            std::fs::copy(source, path.filesystem_path(&outside)).expect("outside binary");
        }

        let pinned = BundleRoot::open(&root).expect("pin original root");
        std::fs::rename(&root, &moved).expect("move original root");
        std::os::unix::fs::symlink(&outside, &root).expect("substitute root symlink");

        assert!(matches!(
            validate_layout(&pinned, runtime),
            Err(BundleError::UnexpectedFile { path }) if path.ends_with("assets/unexpected")
        ));
    }

    #[test]
    fn unindexed_empty_directories_are_rejected() {
        let parent = tempfile::tempdir().expect("bundle parent");
        let root = parent.path().join("bundle");
        let (document, assets, binaries) = document();
        BundleWriter::write(&root, &document, &assets, &binaries).expect("bundle writes");
        std::fs::create_dir(root.join(ASSETS_DIR).join("unused")).expect("empty directory");
        assert!(matches!(
            RuntimeBundle::open_verified(&root),
            Err(BundleError::UnindexedDirectory { .. })
        ));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn no_replace_publication_preserves_target_created_after_staging() {
        let parent = tempfile::tempdir().expect("publication parent");
        let staged = parent.path().join(".bundle.staging");
        let target = parent.path().join("bundle");
        std::fs::create_dir(&staged).expect("staging directory");
        std::fs::write(staged.join("new"), b"new bundle").expect("staged marker");

        // This target is created after staging, standing in for a concurrent
        // publisher winning the check-to-publish race.
        std::fs::create_dir(&target).expect("concurrent target");
        std::fs::write(target.join("sentinel"), b"existing bundle").expect("target sentinel");

        assert!(matches!(
            publish_staging_root(&staged, &target),
            Err(BundleError::TargetExists(path)) if path == target
        ));
        assert_eq!(
            std::fs::read(target.join("sentinel")).expect("sentinel remains"),
            b"existing bundle"
        );
        assert!(
            staged.exists(),
            "failed publication retains staging for cleanup"
        );
        assert!(!target.join("new").exists(), "target was never replaced");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn no_replace_publication_closes_the_preflight_race_for_all_target_types() {
        for target_kind in ["directory", "file", "symlink"] {
            let parent = tempfile::tempdir().expect("publication parent");
            let staged = parent.path().join(".bundle.staging");
            let target = parent.path().join("bundle");
            std::fs::create_dir(&staged).expect("staging directory");
            std::fs::write(staged.join("new"), b"new bundle").expect("staged marker");

            // Model the real writer sequence: the advisory preflight observes
            // an absent target, then a concurrent actor creates it immediately
            // before the no-replace publish syscall.
            assert!(matches!(reject_existing_target(&target), Ok(())));
            match target_kind {
                "directory" => {
                    std::fs::create_dir(&target).expect("concurrent directory");
                    std::fs::write(target.join("sentinel"), b"directory")
                        .expect("directory sentinel");
                }
                "file" => std::fs::write(&target, b"file").expect("concurrent file"),
                "symlink" => {
                    let outside = parent.path().join("outside");
                    std::fs::write(&outside, b"outside").expect("outside target");
                    std::os::unix::fs::symlink(&outside, &target).expect("concurrent symlink");
                }
                _ => unreachable!(),
            }

            assert!(matches!(
                publish_staging_root(&staged, &target),
                Err(BundleError::TargetExists(path)) if path == target
            ));
            assert!(
                staged.exists(),
                "failed publication retains staging for cleanup"
            );
            assert!(!target.join("new").exists(), "target was never replaced");
            match target_kind {
                "directory" => assert_eq!(
                    std::fs::read(target.join("sentinel")).expect("sentinel remains"),
                    b"directory"
                ),
                "file" => assert_eq!(std::fs::read(&target).expect("file remains"), b"file"),
                "symlink" => assert!(
                    std::fs::symlink_metadata(&target)
                        .expect("symlink remains")
                        .file_type()
                        .is_symlink()
                ),
                _ => unreachable!(),
            }
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn writer_tail_race_removes_staging_without_touching_new_targets() {
        for target_kind in ["directory", "file", "symlink"] {
            let parent = tempfile::tempdir().expect("bundle parent");
            let root = parent.path().join("bundle");
            let target = root
                .parent()
                .expect("bundle parent path")
                .canonicalize()
                .expect("canonical bundle parent")
                .join("bundle");
            let (document, assets, binaries) = document();
            let observed_staging = std::cell::RefCell::new(None);

            let result = BundleWriter::write_inner(
                &root,
                &document,
                &assets,
                &binaries,
                |staged, target| {
                    *observed_staging.borrow_mut() = Some(staged.to_path_buf());
                    match target_kind {
                        "directory" => {
                            std::fs::create_dir(target).expect("concurrent directory");
                            std::fs::write(target.join("sentinel"), b"directory")
                                .expect("directory sentinel");
                        }
                        "file" => std::fs::write(target, b"file").expect("concurrent file"),
                        "symlink" => {
                            let outside = parent.path().join("outside");
                            std::fs::write(&outside, b"outside").expect("outside target");
                            std::os::unix::fs::symlink(&outside, target)
                                .expect("concurrent symlink");
                        }
                        _ => unreachable!(),
                    }
                    publish_staging_root(staged, target)
                },
            );

            assert!(matches!(
                result,
                Err(BundleError::TargetExists(path)) if path == target
            ));
            let staging = observed_staging
                .into_inner()
                .expect("writer reached publication tail");
            assert!(
                !staging.exists(),
                "writer removes failed task-owned staging"
            );
            assert!(
                !target.join("runtime.json").exists(),
                "target was never replaced"
            );
            match target_kind {
                "directory" => assert_eq!(
                    std::fs::read(target.join("sentinel")).expect("sentinel remains"),
                    b"directory"
                ),
                "file" => assert_eq!(std::fs::read(&target).expect("file remains"), b"file"),
                "symlink" => assert!(
                    std::fs::symlink_metadata(&target)
                        .expect("symlink remains")
                        .file_type()
                        .is_symlink()
                ),
                _ => unreachable!(),
            }
        }
    }

    #[cfg(not(unix))]
    #[test]
    fn unsupported_platforms_fail_closed_for_asset_open() {
        let root = tempfile::tempdir().expect("bundle root");
        let path = root.path().join(ASSETS_DIR).join("asset");
        std::fs::create_dir_all(path.parent().expect("asset parent")).expect("asset directory");
        std::fs::write(&path, b"asset").expect("asset file");
        let bundle_path = BundlePath::new("assets/asset").expect("bundle path");
        assert!(matches!(
            BundleRoot::open(root.path()),
            Err(BundleError::UnsupportedSecureOpen { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn writer_rejects_existing_symlinked_target_before_writing() {
        let parent = tempfile::tempdir().expect("bundle parent");
        let outside = tempfile::tempdir().expect("outside root");
        let root = parent.path().join("bundle");
        std::fs::create_dir(&root).expect("existing root");
        std::os::unix::fs::symlink(outside.path(), root.join(ASSETS_DIR)).expect("assets symlink");
        let (document, assets, binaries) = document();
        assert!(matches!(
            BundleWriter::write(&root, &document, &assets, &binaries),
            Err(BundleError::ForbiddenSymlink { .. })
        ));
        assert!(!outside.path().join("robot").exists());
    }

    #[cfg(unix)]
    #[test]
    fn writer_rejects_symlinked_leaf_in_existing_target() {
        let parent = tempfile::tempdir().expect("bundle parent");
        let outside = tempfile::tempdir().expect("outside root");
        let root = parent.path().join("bundle");
        std::fs::create_dir_all(root.join(ASSETS_DIR).join("robot")).expect("existing tree");
        let leaf = root.join(ASSETS_DIR).join("robot/structure.json");
        std::os::unix::fs::symlink(outside.path().join("structure.json"), &leaf)
            .expect("leaf symlink");
        let (document, assets, binaries) = document();
        assert!(matches!(
            BundleWriter::write(&root, &document, &assets, &binaries),
            Err(BundleError::ForbiddenSymlink { .. })
        ));
        assert!(!outside.path().join("structure.json").exists());
    }

    #[test]
    fn old_source_documents_are_rejected_as_extra_bundle_truth() {
        let parent = tempfile::tempdir().expect("bundle parent");
        let root = parent.path().join("bundle");
        let (document, assets, binaries) = document();
        BundleWriter::write(&root, &document, &assets, &binaries).expect("bundle writes");
        std::fs::write(root.join("robot.yaml"), b"source truth").expect("old source");
        assert!(matches!(
            RuntimeBundle::open_verified(&root),
            Err(BundleError::UnexpectedFile { .. })
        ));
    }
}
