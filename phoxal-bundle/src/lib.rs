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
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use phoxal_model::component::capability::MotorCommand;
use phoxal_model::identity::{CapabilityRef, ComponentInstanceId};
use phoxal_model::{AssetId, Clock, Robot};
use phoxal_runtime_contract::identity::{ParticipantId, ParticipantIdError};
use phoxal_runtime_contract::launch::ClockMode;
use phoxal_runtime_contract::metadata::{
    ParticipantKind, ParticipantRequirement, ParticipantSchemas,
};
use phoxal_runtime_contract::version::RobotApi;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The only schema tag currently readable by this framework train.
pub const RUNTIME_SCHEMA: &str = "phoxal/runtime-bundle/v0";
/// The persisted document filename at the bundle root.
pub const RUNTIME_FILE: &str = "runtime.json";
/// The participant-readable asset directory.
pub const ASSETS_DIR: &str = "assets";
/// The supervisor-only binary directory.
pub const BIN_DIR: &str = "bin";

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
            Wire::V0(runtime) => Self::new(runtime).map_err(serde::de::Error::custom),
        }
    }
}

impl RuntimeDocument {
    /// Construct and validate one runtime document.
    pub fn new(runtime: Runtime) -> Result<Self, DocumentError> {
        runtime.validate()?;
        Ok(Self::V0(runtime))
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

    /// Validate all typed invariants without touching the filesystem.
    pub fn validate(&self) -> Result<(), DocumentError> {
        self.runtime().validate()
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

    /// Replace the asset index after a build tool has staged its compiled
    /// assets. This does not write anything and intentionally does not infer
    /// the final participant set.
    pub fn with_assets(mut self, assets: AssetIndex) -> Result<Self, DocumentError> {
        match &mut self {
            Self::V0(runtime) => runtime.assets = assets,
        }
        self.validate()?;
        Ok(self)
    }
}

/// The persisted final runtime graph and all framework-owned runtime facts.
#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Runtime {
    /// The complete canonical model. Its `id` is the sole persisted RobotId;
    /// there is no namespace or duplicate top-level identity field.
    pub robot: Robot,
    /// The exact processes the executor must launch, in final persisted form.
    pub participants: Vec<RuntimeParticipant>,
    /// The participant-readable asset index and integrity facts.
    pub assets: AssetIndex,
    /// Optional supervisor router configuration, kept as an indexed asset.
    pub router: Option<RuntimeRouterConfig>,
}

impl<'de> Deserialize<'de> for Runtime {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            robot: Robot,
            participants: Vec<RuntimeParticipant>,
            assets: AssetIndex,
            router: Option<RuntimeRouterConfig>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let runtime = Self {
            robot: wire.robot,
            participants: wire.participants,
            assets: wire.assets,
            router: wire.router,
        };
        runtime.validate().map_err(serde::de::Error::custom)?;
        Ok(runtime)
    }
}

impl Runtime {
    /// Validate the complete in-memory runtime document.
    pub fn validate(&self) -> Result<(), DocumentError> {
        let mut ids = BTreeSet::new();
        let mut binary_paths = BTreeSet::new();
        for participant in &self.participants {
            participant.validate(&self.robot)?;
            if !ids.insert(participant.id.clone()) {
                return Err(DocumentError::DuplicateParticipant {
                    id: participant.id.clone(),
                });
            }
            if !binary_paths.insert(participant.binary.path.clone()) {
                return Err(DocumentError::DuplicateBinary {
                    path: participant.binary.path.clone(),
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
    pub id: ParticipantId,
    /// The role embedded in the selected binary.
    pub kind: ParticipantKind,
    /// The staged binary and the facts the builder read from it.
    pub binary: BinaryReference,
    /// Whether startup of this participant is required for the execution.
    pub startup: StartupRequirement,
    /// The already-compiled participant configuration. `None` means JSON
    /// `null`, not a request to consult authored configuration.
    pub config: Option<serde_json::Value>,
    /// An optional typed component-instance binding for a driver/simulator.
    pub binding: Option<ComponentBinding>,
    /// The scheduler policy selected at build time.
    pub clock: ClockMode,
}

impl RuntimeParticipant {
    /// Validate participant facts against the compiled robot.
    pub fn validate(&self, robot: &Robot) -> Result<(), DocumentError> {
        if self.binary.compatibility.participant_id != self.id {
            return Err(DocumentError::BinaryParticipantMismatch {
                participant: self.id.clone(),
                binary: self.binary.compatibility.participant_id.clone(),
            });
        }
        if self.binary.compatibility.kind != self.kind {
            return Err(DocumentError::BinaryKindMismatch {
                participant: self.id.clone(),
                declared: self.kind,
                binary: self.binary.compatibility.kind,
            });
        }
        if !self.binary.path.starts_with_directory(BIN_DIR) {
            return Err(DocumentError::BinaryOutsideBin {
                participant: self.id.clone(),
                path: self.binary.path.clone(),
            });
        }
        if let Some(binding) = &self.binding
            && robot
                .component_instance(binding.component_instance.as_str())
                .is_none()
        {
            return Err(DocumentError::UnknownBinding {
                participant: self.id.clone(),
                component_instance: binding.component_instance.clone(),
            });
        }
        match (robot.clock(), self.clock) {
            (Clock::Real, ClockMode::Simulation) => {
                return Err(DocumentError::ClockMismatch {
                    participant: self.id.clone(),
                    robot: Clock::Real,
                    participant_clock: self.clock,
                });
            }
            (Clock::Simulated, ClockMode::Real) => {
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
        let validator = jsonschema::validator_for(&self.binary.compatibility.config_schema)
            .map_err(|error| DocumentError::InvalidConfigSchema {
                participant: self.id.clone(),
                error: error.to_string(),
            })?;
        if let Err(error) = validator.validate(config) {
            return Err(DocumentError::InvalidConfig {
                participant: self.id.clone(),
                error: error.to_string(),
            });
        }
        self.binary
            .compatibility
            .validate_requirement(&self.id, robot)?;
        self.binary.build.validate()?;
        Ok(())
    }
}

/// A compiled component-instance binding.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentBinding {
    pub component_instance: ComponentInstanceId,
}

/// Build-time startup facts retained for the executor.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StartupRequirement {
    /// Whether the participant is required for the execution to be valid.
    pub required: bool,
    /// Whether the supervisor waits for this participant's Ready handoff.
    pub ready: bool,
}

/// A staged executable and the compatibility facts read by build tooling.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BinaryReference {
    /// A normalized bundle-relative path under `bin/`.
    pub path: BundlePath,
    /// The exact bytes staged at `path`.
    pub digest: Sha256Digest,
    /// The build/artifact facts needed to explain this binary.
    pub build: BuildFacts,
    /// The embedded process-contract facts the binary declared.
    pub compatibility: BinaryCompatibility,
}

impl BinaryReference {
    /// Construct a reference for already-built bytes.
    pub fn from_bytes(
        path: BundlePath,
        build: BuildFacts,
        compatibility: BinaryCompatibility,
        bytes: &[u8],
    ) -> Self {
        Self {
            path,
            digest: Sha256Digest::of(bytes),
            build,
            compatibility,
        }
    }
}

/// Facts from the build leg that selected and staged one executable.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildFacts {
    /// The Cargo package or user build unit that produced the binary.
    pub package: String,
    /// The target triple or other build target identity.
    pub target: String,
    /// The profile used to produce the staged bytes.
    pub profile: String,
}

impl BuildFacts {
    fn validate(&self) -> Result<(), DocumentError> {
        for (field, value) in [
            ("package", self.package.as_str()),
            ("target", self.target.as_str()),
            ("profile", self.profile.as_str()),
        ] {
            if value.is_empty() || value.bytes().any(|byte| byte.is_ascii_control()) {
                return Err(DocumentError::EmptyBuildFact {
                    field,
                    value: value.to_string(),
                });
            }
        }
        Ok(())
    }
}

/// Compatibility facts read from a participant binary's embedded metadata.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BinaryCompatibility {
    pub participant_id: ParticipantId,
    pub kind: ParticipantKind,
    pub api: RobotApi,
    pub schemas: ParticipantSchemas,
    /// Optional for compatibility with existing `phoxal/runtime-bundle/v0`
    /// documents. `None` means no additional static topology requirement.
    #[serde(default)]
    pub requirement: Option<ParticipantRequirement>,
    pub config_schema: serde_json::Value,
}

impl BinaryCompatibility {
    /// Validate the one topology requirement this binary declared against the
    /// already-canonical robot. This is deliberately a closed match over the
    /// requirement enum, not a package-name lookup or a generic service
    /// registry.
    fn validate_requirement(
        &self,
        participant: &ParticipantId,
        robot: &Robot,
    ) -> Result<(), DocumentError> {
        let Some(requirement) = self.requirement else {
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
    pub path: BundlePath,
}

impl RuntimeRouterConfig {
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
    pub entries: Vec<AssetRecord>,
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
    pub id: AssetId,
    pub path: BundlePath,
    pub size_bytes: u64,
    pub digest: Sha256Digest,
}

/// A normalized, traversal-proof bundle-relative path.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BundlePath(String);

impl BundlePath {
    /// Validate a forward-slash relative path.
    pub fn new(value: impl Into<String>) -> Result<Self, BundlePathError> {
        let value = value.into();
        if value.is_empty() {
            return Err(BundlePathError::Empty);
        }
        if value.starts_with('/') {
            return Err(BundlePathError::Absolute(value));
        }
        if value.contains('\\') {
            return Err(BundlePathError::NotNormalized(value));
        }
        let mut components = value.split('/');
        if components.any(|component| component.is_empty() || component == "." || component == "..")
        {
            return Err(BundlePathError::NotNormalized(value));
        }
        Ok(Self(value))
    }

    /// The normalized path string stored in JSON.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn starts_with_directory(&self, directory: &str) -> bool {
        self.0
            .strip_prefix(directory)
            .is_some_and(|rest| rest.starts_with('/') && rest.len() > 1)
    }

    fn filesystem_path(&self, root: &Path) -> PathBuf {
        root.join(self.0.split('/').collect::<PathBuf>())
    }
}

impl fmt::Display for BundlePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for BundlePath {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for BundlePath {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// A SHA-256 digest rendered as exactly 64 lowercase hexadecimal characters.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Sha256Digest([u8; 32]);

impl Sha256Digest {
    /// Hash one byte sequence.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    /// Parse the canonical JSON representation.
    pub fn parse(value: &str) -> Result<Self, DigestError> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(DigestError(value.to_string()));
        }
        let mut bytes = [0; 32];
        for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
            bytes[index] = (hex(pair[0])? << 4) | hex(pair[1])?;
        }
        Ok(Self(bytes))
    }

    /// Render the canonical lowercase hexadecimal representation.
    #[must_use]
    pub fn as_hex(self) -> String {
        let mut output = String::with_capacity(64);
        for byte in self.0 {
            output.push(hex_digit(byte >> 4));
            output.push(hex_digit(byte & 0x0f));
        }
        output
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.as_hex())
    }
}

impl Serialize for Sha256Digest {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.as_hex())
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

fn hex(value: u8) -> Result<u8, DigestError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(DigestError(String::from("non-hex digest"))),
    }
}

const fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        _ => (b'a' + value - 10) as char,
    }
}

/// A digest that was not the canonical lowercase SHA-256 spelling.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("digest must be 64 lowercase hexadecimal characters, got '{0}'")]
pub struct DigestError(String);

/// Why a bundle-relative path was rejected.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum BundlePathError {
    #[error("bundle path is empty")]
    Empty,
    #[error("bundle path is absolute: '{0}'")]
    Absolute(String),
    #[error("bundle path is not normalized: '{0}'")]
    NotNormalized(String),
}

/// Why a persisted runtime document is not a valid execution plan.
#[derive(Clone, Debug, thiserror::Error)]
pub enum DocumentError {
    #[error("duplicate participant id '{id}'")]
    DuplicateParticipant { id: ParticipantId },
    #[error("duplicate participant binary path '{path}'")]
    DuplicateBinary { path: BundlePath },
    #[error("participant '{participant}' binary metadata names '{binary}'")]
    BinaryParticipantMismatch {
        participant: ParticipantId,
        binary: ParticipantId,
    },
    #[error("participant '{participant}' declares kind {declared:?}, binary declares {binary:?}")]
    BinaryKindMismatch {
        participant: ParticipantId,
        declared: ParticipantKind,
        binary: ParticipantKind,
    },
    #[error("participant '{participant}' binary path is outside bin/: {path}")]
    BinaryOutsideBin {
        participant: ParticipantId,
        path: BundlePath,
    },
    #[error("participant '{participant}' binds unknown component instance '{component_instance}'")]
    UnknownBinding {
        participant: ParticipantId,
        component_instance: ComponentInstanceId,
    },
    #[error(
        "participant '{participant}' clock {participant_clock:?} conflicts with robot clock {robot:?}"
    )]
    ClockMismatch {
        participant: ParticipantId,
        robot: Clock,
        participant_clock: ClockMode,
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
    #[error("build fact '{field}' is empty or contains control characters: '{value}'")]
    EmptyBuildFact { field: &'static str, value: String },
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
    #[error("participant id is invalid: {0}")]
    InvalidId(#[from] ParticipantIdError),
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

/// Participant-readable, digest-checked asset access.
#[derive(Clone, Debug)]
pub struct ParticipantAssets {
    root: PathBuf,
    entries: BTreeMap<AssetId, AssetRecord>,
}

impl ParticipantAssets {
    fn new(root: PathBuf, index: &AssetIndex) -> Self {
        Self {
            root,
            entries: index
                .entries
                .iter()
                .map(|entry| (entry.id.clone(), entry.clone()))
                .collect(),
        }
    }

    /// Every logical asset declared by this runtime bundle.
    pub fn ids(&self) -> impl ExactSizeIterator<Item = &AssetId> {
        self.entries.keys()
    }

    /// Read a declared asset through one no-follow file descriptor and verify
    /// the bytes consumed from that same descriptor.
    pub fn read(&self, id: &AssetId) -> Result<Vec<u8>, BundleError> {
        let entry = self
            .entries
            .get(id)
            .ok_or_else(|| BundleError::UndeclaredAsset { id: id.clone() })?;
        let path = entry.path.filesystem_path(&self.root);
        let mut file = open_bundle_file(&self.root, &entry.path)?;
        let bytes = read_and_verify(&mut file, &path, entry.digest, Some(entry.size_bytes))?;
        Ok(bytes)
    }

    /// Open a declared asset after hashing the bytes from that same owned file
    /// descriptor. The returned handle is rewound and never re-resolves a
    /// pathname, so a later directory or leaf substitution cannot redirect it.
    pub fn open(&self, id: &AssetId) -> Result<std::fs::File, BundleError> {
        let entry = self
            .entries
            .get(id)
            .ok_or_else(|| BundleError::UndeclaredAsset { id: id.clone() })?;
        let path = entry.path.filesystem_path(&self.root);
        let mut verification = open_bundle_file(&self.root, &entry.path)?;
        let mut reader = verification
            .try_clone()
            .map_err(|source| BundleError::ReadFile {
                path: path.clone(),
                source,
            })?;
        let _ = read_and_verify(&mut reader, &path, entry.digest, Some(entry.size_bytes))?;
        verification
            .seek(SeekFrom::Start(0))
            .map_err(|source| BundleError::ReadFile { path, source })?;
        Ok(verification)
    }
}

/// One selected participant and the immutable runtime inputs it consumes.
#[derive(Clone, Debug)]
pub struct ParticipantRuntimeInputs {
    pub robot: Arc<Robot>,
    pub participant: RuntimeParticipant,
    pub assets: ParticipantAssets,
}

/// A loaded, integrity-checked runtime bundle.
#[derive(Clone, Debug)]
pub struct RuntimeBundle {
    root: PathBuf,
    document: RuntimeDocument,
    assets: ParticipantAssets,
}

impl RuntimeBundle {
    /// Open and validate one installed bundle.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, BundleError> {
        let requested = root.as_ref();
        if let Ok(metadata) = std::fs::symlink_metadata(requested)
            && metadata.file_type().is_symlink()
        {
            return Err(BundleError::ForbiddenSymlink {
                path: requested.to_path_buf(),
            });
        }
        let root = requested
            .canonicalize()
            .map_err(|source| BundleError::Root {
                path: requested.to_path_buf(),
                source,
            })?;
        if !root.is_dir() {
            return Err(BundleError::NotDirectory(root));
        }
        let runtime_path = root.join(RUNTIME_FILE);
        let mut runtime_file = open_bundle_file(&root, &BundlePath::new(RUNTIME_FILE)?).map_err(
            |error| match error {
                BundleError::ReadFile { path, source } => {
                    BundleError::ReadDocument { path, source }
                }
                BundleError::MissingFile { path } => BundleError::ReadDocument {
                    path,
                    source: std::io::Error::new(std::io::ErrorKind::NotFound, "runtime.json"),
                },
                other => other,
            },
        )?;
        let mut bytes = Vec::new();
        runtime_file
            .read_to_end(&mut bytes)
            .map_err(|source| BundleError::ReadDocument {
                path: runtime_path.clone(),
                source,
            })?;
        let document: RuntimeDocument = serde_json::from_slice(&bytes)?;
        document.validate()?;
        validate_layout(&root, document.runtime())?;
        Ok(Self {
            assets: ParticipantAssets::new(root.clone(), &document.runtime().assets),
            root,
            document,
        })
    }

    /// The canonical installed root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The validated persisted document.
    #[must_use]
    pub const fn document(&self) -> &RuntimeDocument {
        &self.document
    }

    /// The canonical robot loaded from runtime.json, with no source parser.
    #[must_use]
    pub fn robot(&self) -> &Robot {
        self.document.robot()
    }

    /// The sole persisted RobotId.
    #[must_use]
    pub fn robot_id(&self) -> &phoxal_model::identity::RobotId {
        self.document.robot_id()
    }

    /// The final persisted participant set.
    #[must_use]
    pub fn participants(&self) -> &[RuntimeParticipant] {
        self.document.participants()
    }

    /// Participant-readable digest-checked assets.
    #[must_use]
    pub const fn assets(&self) -> &ParticipantAssets {
        &self.assets
    }

    /// Select one exact participant record before opening any bus session.
    pub fn participant(&self, id: &ParticipantId) -> Result<&RuntimeParticipant, SelectionError> {
        self.document.participant(id)
    }

    /// Build one selected runtime-input object, cloning only immutable model
    /// data so it can outlive this loader handle inside a participant runner.
    pub fn participant_inputs(
        &self,
        id: &ParticipantId,
    ) -> Result<ParticipantRuntimeInputs, SelectionError> {
        Ok(ParticipantRuntimeInputs {
            robot: Arc::new(self.robot().clone()),
            participant: self.participant(id)?.clone(),
            assets: self.assets.clone(),
        })
    }
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
        binaries: &BTreeMap<BundlePath, Vec<u8>>,
    ) -> Result<RuntimeBundle, BundleError> {
        document.validate()?;
        let expected_assets = &document.runtime().assets;
        let supplied_assets = AssetIndex::from_bytes(assets)?;
        if supplied_assets.entries != expected_assets.entries {
            return Err(BundleError::Document(DocumentError::AssetIndexMismatch));
        }

        let expected_binaries = document
            .participants()
            .iter()
            .map(|participant| participant.binary.path.clone())
            .collect::<BTreeSet<_>>();
        let supplied_binaries = binaries.keys().cloned().collect::<BTreeSet<_>>();
        if expected_binaries != supplied_binaries {
            return Err(BundleError::Document(DocumentError::BinaryIndexMismatch));
        }
        for participant in document.participants() {
            let bytes =
                binaries
                    .get(&participant.binary.path)
                    .ok_or_else(|| BundleError::MissingFile {
                        path: participant.binary.path.filesystem_path(root.as_ref()),
                    })?;
            let digest = Sha256Digest::of(bytes);
            if digest != participant.binary.digest {
                return Err(BundleError::Integrity {
                    path: participant.binary.path.filesystem_path(root.as_ref()),
                    expected: participant.binary.digest,
                    actual: digest,
                });
            }
        }

        let requested_root = root.as_ref();
        let publish_target = prepare_publish_parent(requested_root)?;
        reject_existing_target(&publish_target)?;
        let root = create_staging_root(&publish_target)?;
        let staged = (|| {
            for (id, bytes) in assets {
                let path = root
                    .join(ASSETS_DIR)
                    .join(id.as_str().split('/').collect::<PathBuf>());
                write_new_file(&root, &path, bytes)?;
            }
            for (path, bytes) in binaries {
                write_new_file(&root, &path.filesystem_path(&root), bytes)?;
            }
            let json = serde_json::to_vec_pretty(document)?;
            write_new_file(&root, &root.join(RUNTIME_FILE), &json)
        })();
        if let Err(error) = staged {
            let _ = std::fs::remove_dir_all(&root);
            return Err(error);
        }
        if let Err(error) = publish_staging_root(&root, &publish_target) {
            let _ = std::fs::remove_dir_all(&root);
            return Err(error);
        }
        RuntimeBundle::open(&publish_target)
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

fn validate_layout(root: &Path, runtime: &Runtime) -> Result<(), BundleError> {
    let allowed = [RUNTIME_FILE, ASSETS_DIR, BIN_DIR];
    for entry in std::fs::read_dir(root).map_err(|source| BundleError::ReadFile {
        path: root.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| BundleError::ReadFile {
            path: root.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let metadata =
            std::fs::symlink_metadata(&path).map_err(|source| BundleError::ReadFile {
                path: path.clone(),
                source,
            })?;
        if metadata.file_type().is_symlink() {
            return Err(BundleError::ForbiddenSymlink { path });
        }
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| BundleError::UnsupportedEntry { path: path.clone() })?;
        if !allowed.contains(&name) {
            return Err(BundleError::UnexpectedFile { path });
        }
        if name != RUNTIME_FILE && !metadata.is_dir() {
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
        &root.join(ASSETS_DIR),
        &mut actual_assets,
        &mut actual_asset_directories,
    )?;
    reject_unindexed_directories(
        root,
        &actual_asset_directories,
        &expected_assets.keys().cloned().collect::<BTreeSet<_>>(),
    )?;
    if actual_assets != expected_assets.keys().cloned().collect::<BTreeSet<_>>() {
        if let Some(path) = actual_assets
            .difference(&expected_assets.keys().cloned().collect())
            .next()
        {
            return Err(BundleError::UnexpectedFile {
                path: root.join(path.as_str()),
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
                path: root.join(path.as_str()),
            });
        }
    }

    let expected_binaries = runtime
        .participants
        .iter()
        .map(|participant| participant.binary.path.clone())
        .collect::<BTreeSet<_>>();
    let mut actual_binaries = BTreeSet::new();
    let mut actual_binary_directories = BTreeSet::new();
    collect_files(
        root,
        &root.join(BIN_DIR),
        &mut actual_binaries,
        &mut actual_binary_directories,
    )?;
    reject_unindexed_directories(root, &actual_binary_directories, &expected_binaries)?;
    if actual_binaries != expected_binaries {
        if let Some(path) = actual_binaries.difference(&expected_binaries).next() {
            return Err(BundleError::UnexpectedFile {
                path: root.join(path.as_str()),
            });
        }
        if let Some(path) = expected_binaries.difference(&actual_binaries).next() {
            return Err(BundleError::MissingFile {
                path: root.join(path.as_str()),
            });
        }
    }
    for participant in &runtime.participants {
        verify_binary(root, &participant.binary)?;
    }
    Ok(())
}

fn collect_files(
    root: &Path,
    directory: &Path,
    paths: &mut BTreeSet<BundlePath>,
    directories: &mut BTreeSet<BundlePath>,
) -> Result<(), BundleError> {
    let directory_metadata = match std::fs::symlink_metadata(directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(BundleError::ReadFile {
                path: directory.to_path_buf(),
                source,
            });
        }
    };
    if directory_metadata.file_type().is_symlink() {
        return Err(BundleError::ForbiddenSymlink {
            path: directory.to_path_buf(),
        });
    }
    if !directory_metadata.is_dir() {
        return Err(BundleError::UnsupportedEntry {
            path: directory.to_path_buf(),
        });
    }
    let relative_directory =
        directory
            .strip_prefix(root)
            .map_err(|_| BundleError::UnsupportedEntry {
                path: directory.to_path_buf(),
            })?;
    if !relative_directory.as_os_str().is_empty() {
        let relative = relative_directory
            .to_str()
            .ok_or_else(|| BundleError::UnsupportedEntry {
                path: directory.to_path_buf(),
            })?
            .replace(std::path::MAIN_SEPARATOR, "/");
        directories.insert(BundlePath::new(relative)?);
    }
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
            return Err(BundleError::ForbiddenSymlink { path });
        }
        if metadata.is_dir() {
            collect_files(root, &path, paths, directories)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| BundleError::UnsupportedEntry { path: path.clone() })?;
            let mut valid = true;
            for component in relative.components() {
                if !matches!(component, Component::Normal(_)) {
                    valid = false;
                }
            }
            if !valid {
                return Err(BundleError::UnsupportedEntry { path });
            }
            let relative = relative
                .to_str()
                .ok_or_else(|| BundleError::UnsupportedEntry { path: path.clone() })?
                .replace(std::path::MAIN_SEPARATOR, "/");
            paths.insert(BundlePath::new(relative)?);
        } else {
            return Err(BundleError::UnsupportedEntry { path });
        }
    }
    Ok(())
}

fn reject_unindexed_directories(
    root: &Path,
    actual: &BTreeSet<BundlePath>,
    files: &BTreeSet<BundlePath>,
) -> Result<(), BundleError> {
    let mut expected = BTreeSet::new();
    for file in files {
        let mut components = file.0.split('/').collect::<Vec<_>>();
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

fn verify_binary(root: &Path, binary: &BinaryReference) -> Result<(), BundleError> {
    verify_digest_and_size(root, &binary.path, binary.digest, None)
}

fn verify_file(root: &Path, entry: &AssetRecord) -> Result<(), BundleError> {
    verify_digest_and_size(root, &entry.path, entry.digest, Some(entry.size_bytes))
}

fn verify_digest_and_size(
    root: &Path,
    bundle_path: &BundlePath,
    expected: Sha256Digest,
    expected_size: Option<u64>,
) -> Result<(), BundleError> {
    let path = bundle_path.filesystem_path(root);
    let file = open_bundle_file(root, bundle_path)?;
    let mut reader = file.try_clone().map_err(|source| BundleError::ReadFile {
        path: path.clone(),
        source,
    })?;
    let _ = read_and_verify(&mut reader, &path, expected, expected_size)?;
    Ok(())
}

fn read_and_verify(
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

fn open_bundle_file(root: &Path, path: &BundlePath) -> Result<std::fs::File, BundleError> {
    let filesystem_path = path.filesystem_path(root);
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

        let root_c = CString::new(root.as_os_str().as_encoded_bytes()).map_err(|_| {
            BundleError::UnsupportedEntry {
                path: filesystem_path.clone(),
            }
        })?;
        let root_fd = unsafe {
            // SAFETY: the CString is NUL-free and points at the requested
            // directory. O_NOFOLLOW prevents a substituted root symlink.
            libc::open(
                root_c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if root_fd < 0 {
            return Err(io_error_for_path(&filesystem_path));
        }
        let mut parent = unsafe { OwnedFd::from_raw_fd(root_fd) };
        let components = path.0.split('/').collect::<Vec<_>>();
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
    use phoxal_runtime_contract::metadata::ParticipantKind;
    use phoxal_runtime_contract::version::{
        BusAbi, ComponentSchema, LaunchAbi, RobotSchema, SimulationSchema,
    };

    type StagedBytes = (
        RuntimeDocument,
        BTreeMap<AssetId, Vec<u8>>,
        BTreeMap<BundlePath, Vec<u8>>,
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
        let binary_bytes = b"not an executable in the unit test".to_vec();
        let mut binaries = BTreeMap::new();
        binaries.insert(binary_path, binary_bytes.clone());
        let binary = BinaryReference::from_bytes(
            BundlePath::new("bin/drive").expect("binary path"),
            BuildFacts {
                package: "test".to_string(),
                target: "host".to_string(),
                profile: "debug".to_string(),
            },
            BinaryCompatibility {
                participant_id: ParticipantId::new("drive").expect("participant id"),
                kind: ParticipantKind::Service,
                api: RobotApi::V0_2,
                schemas: ParticipantSchemas {
                    bus: BusAbi::V0,
                    launch: LaunchAbi::V0,
                    robot: RobotSchema::V0,
                    component: ComponentSchema::V0,
                    simulation: SimulationSchema::V0,
                },
                requirement: None,
                config_schema: serde_json::json!({"type":"null"}),
            },
            &binary_bytes,
        );
        let participant = RuntimeParticipant {
            id: ParticipantId::new("drive").expect("participant id"),
            kind: ParticipantKind::Service,
            binary,
            startup: StartupRequirement {
                required: true,
                ready: true,
            },
            config: None,
            binding: None,
            clock: ClockMode::Real,
        };
        let index = AssetIndex::from_bytes(&assets).expect("asset index");
        let document = RuntimeDocument::new(Runtime {
            robot,
            participants: vec![participant],
            assets: index,
            router: None,
        })
        .expect("runtime document");
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
        runtime.participants[0].binary.compatibility.requirement =
            Some(ParticipantRequirement::DifferentialDriveVelocity);
        let document = RuntimeDocument::new(runtime).expect("requirement document is valid");
        (document, assets, binaries)
    }

    #[test]
    fn stock_drive_requirement_accepts_differential_velocity_motors() {
        let (document, _, _) =
            requirement_document(motor_robot(MotorCommand::Velocity, MotorCommand::Velocity));
        assert!(document.validate().is_ok());
    }

    #[test]
    fn stock_drive_requirement_rejects_non_differential_topologies() {
        for kind in [
            phoxal_model::robot::KinematicKind::Mecanum,
            phoxal_model::robot::KinematicKind::Omnidirectional,
            phoxal_model::robot::KinematicKind::Ackermann,
        ] {
            let (document, _, _) = {
                let (base, assets, binaries) = document();
                let RuntimeDocument::V0(mut runtime) = base;
                runtime.robot = topology_robot(kind);
                runtime.participants[0].binary.compatibility.requirement =
                    Some(ParticipantRequirement::DifferentialDriveVelocity);
                (RuntimeDocument::V0(runtime), assets, binaries)
            };
            assert!(matches!(
                document.validate(),
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
            runtime.participants[0].binary.compatibility.requirement =
                Some(ParticipantRequirement::DifferentialDriveVelocity);
            let document = RuntimeDocument::V0(runtime);
            assert!(matches!(
                document.validate(),
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
        assert_eq!(Sha256Digest::parse(&digest.as_hex()), Ok(digest));
        assert!(Sha256Digest::parse(&digest.as_hex().to_uppercase()).is_err());
    }

    #[test]
    fn runtime_json_is_tagged_strict_and_robot_id_is_persisted_once() {
        let (document, _, _) = document();
        let value = serde_json::to_value(&document).expect("document serializes");
        assert_eq!(value["schema"], RUNTIME_SCHEMA);
        assert_eq!(
            value["participants"][0]["binary"]["compatibility"]["requirement"],
            serde_json::Value::Null,
            "legacy-compatible documents omit static requirements"
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
    fn runtime_json_without_requirement_remains_legacy_compatible() {
        let (document, _, _) = document();
        let mut value = serde_json::to_value(&document).expect("document serializes");
        value["participants"][0]["binary"]["compatibility"]
            .as_object_mut()
            .expect("compatibility is an object")
            .remove("requirement");

        let decoded = serde_json::from_value::<RuntimeDocument>(value)
            .expect("v0 runtime documents without the additive field remain readable");
        let RuntimeDocument::V0(runtime) = decoded;
        assert_eq!(
            runtime.participants[0].binary.compatibility.requirement,
            None
        );
    }

    #[test]
    fn participant_config_is_validated_against_embedded_binary_schema() {
        let (document, _, _) = document();
        let RuntimeDocument::V0(mut runtime) = document;
        runtime.participants[0].config = Some(serde_json::json!({"unexpected": true}));
        assert!(matches!(
            RuntimeDocument::new(runtime),
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
            RuntimeBundle::open(&root),
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
            RuntimeBundle::open(&root),
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
            RuntimeBundle::open(&root),
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
            RuntimeBundle::open(&root),
            Err(BundleError::ForbiddenSymlink { .. })
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
            RuntimeBundle::open(&root),
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

    #[cfg(not(unix))]
    #[test]
    fn unsupported_platforms_fail_closed_for_asset_open() {
        let root = tempfile::tempdir().expect("bundle root");
        let path = root.path().join(ASSETS_DIR).join("asset");
        std::fs::create_dir_all(path.parent().expect("asset parent")).expect("asset directory");
        std::fs::write(&path, b"asset").expect("asset file");
        let bundle_path = BundlePath::new("assets/asset").expect("bundle path");
        assert!(matches!(
            open_bundle_file(root.path(), &bundle_path),
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
            RuntimeBundle::open(&root),
            Err(BundleError::UnexpectedFile { .. })
        ));
    }
}
