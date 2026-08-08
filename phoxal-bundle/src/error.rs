//! Bundle document, selection, and I/O failures.

use std::path::PathBuf;

use phoxal_model::component::capability::MotorCommand;
use phoxal_model::identity::{CapabilityRef, ComponentInstanceId};
use phoxal_model::{AssetId, Clock};
use phoxal_runtime_contract::identity::{ParticipantArtifactId, ParticipantId};
use phoxal_runtime_contract::metadata::ParticipantRequirement;

use crate::{BundlePath, BundlePathError, DigestError, ParticipantClock, Sha256Digest};

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
