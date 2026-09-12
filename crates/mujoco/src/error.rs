//! Errors shared by the native model and artifact boundaries.

use std::path::PathBuf;

#[cfg(feature = "native")]
use crate::ModelIdentity;

/// Errors raised while validating a closed MJCF/resource artifact.
#[derive(Debug, thiserror::Error)]
pub enum ArtifactError {
    /// The model entry name is empty.
    #[error("model entry name must not be empty")]
    EmptyEntry,
    /// A resource name contains a path form that is not safe in a closed VFS.
    #[error("resource name {0:?} must be relative, normalized, and use '/' separators")]
    InvalidResourceName(String),
    /// A resource name contains a NUL byte.
    #[error("resource name {0:?} contains a NUL byte")]
    ResourceNameContainsNul(String),
    /// Two resources use the same normalized name.
    #[error("resource {0:?} appears more than once")]
    DuplicateResource(String),
    /// The declared model entry is not present in the closure.
    #[error("model entry {0:?} is not present in the resource closure")]
    EntryMissing(String),
    /// The model entry must be UTF-8 because MuJoCo parses MJCF XML text.
    #[error("model entry {path:?} is not valid UTF-8: {source}")]
    EntryNotUtf8 {
        /// Entry resource path.
        path: String,
        /// UTF-8 error from the standard library.
        source: std::str::Utf8Error,
    },
    /// A resource is larger than the configured per-resource limit.
    #[error("resource {name:?} is {actual} bytes, exceeding the {limit}-byte limit")]
    ResourceTooLarge {
        /// Resource path.
        name: String,
        /// Actual resource size.
        actual: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// The complete closure is larger than the configured limit.
    #[error("resource closure is {actual} bytes, exceeding the {limit}-byte limit")]
    ClosureTooLarge {
        /// Actual closure size.
        actual: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// Too many entries would make closure admission unbounded.
    #[error("resource closure has {actual} entries, exceeding the {limit}-entry limit")]
    TooManyResources {
        /// Actual entry count.
        actual: usize,
        /// Configured maximum.
        limit: usize,
    },
    /// A filesystem resource could not be read.
    #[error("cannot read model resource {path}: {source}")]
    Io {
        /// Resource path.
        path: PathBuf,
        /// Filesystem error.
        source: std::io::Error,
    },
    /// A model resource is neither a regular file nor a directory.
    #[error("model resource {0} is not a regular file")]
    UnsupportedFileType(PathBuf),
}

#[cfg(feature = "native")]
/// Errors raised while loading or compiling an MJCF artifact.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    /// The closed artifact failed validation.
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
    /// The selected API received a name that cannot be passed to MuJoCo.
    #[error("native object name contains a NUL byte")]
    NameContainsNul,
    /// MuJoCo rejected an operation.
    #[error("MuJoCo {operation} failed: {message}")]
    Native {
        /// Operation being performed.
        operation: &'static str,
        /// Native diagnostic text.
        message: String,
    },
    /// A native timestep is not a valid positive finite quantum.
    #[error("native model timestep must be finite and positive, got {0}")]
    InvalidTimestep(f64),
    /// A model handle came from a different compiled model.
    #[error("model handle belongs to {found}, but this model is {expected}")]
    ForeignHandle {
        /// Actual model identity carried by the handle.
        found: ModelIdentity,
        /// Model identity expected by this API.
        expected: ModelIdentity,
    },
    /// A model handle index no longer belongs to its model.
    #[error("{kind} handle index {index} is outside the model's {length}-element table")]
    InvalidHandleIndex {
        /// Native object kind.
        kind: &'static str,
        /// Requested index.
        index: usize,
        /// Number of elements in the table.
        length: usize,
    },
}

#[cfg(feature = "native")]
/// Errors raised by an owned non-authoritative MuJoCo computation workspace.
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    /// The native data allocation failed.
    #[error("cannot allocate MuJoCo workspace: {0}")]
    Allocation(String),
    /// The supplied values do not match a model state table.
    #[error("{name} has length {actual}, expected {expected}")]
    Length {
        /// State field name.
        name: &'static str,
        /// Supplied element count.
        actual: usize,
        /// Required element count.
        expected: usize,
    },
    /// A state value is not finite.
    #[error("{name}[{index}] must be finite, got {value}")]
    NonFinite {
        /// State field name.
        name: &'static str,
        /// Invalid element index.
        index: usize,
        /// Invalid value.
        value: f64,
    },
    /// MuJoCo rejected a workspace operation.
    #[error("MuJoCo workspace {operation} failed: {message}")]
    Native {
        /// Operation being performed.
        operation: &'static str,
        /// Native diagnostic text.
        message: String,
    },
}

#[cfg(feature = "native")]
/// Errors raised by authoritative fixed-step scene execution.
#[derive(Debug, thiserror::Error)]
pub enum SceneError {
    /// The scene's model could not be initialized.
    #[error(transparent)]
    Model(#[from] ModelError),
    /// The scene workspace could not be allocated or reset.
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
    /// The scene has failed and cannot be advanced, mutated, or reset.
    #[error("scene is terminally failed and must be replaced")]
    Failed,
    /// `advance` must request at least one native transition.
    #[error("advance count must be positive")]
    ZeroAdvance,
    /// Advancing would overflow the scene boundary identity.
    #[error("scene boundary overflow while advancing from {start} by {count}")]
    BoundaryOverflow {
        /// Starting boundary.
        start: u64,
        /// Requested count.
        count: u64,
    },
    /// MuJoCo returned a non-finite simulation timestamp.
    #[error("MuJoCo returned non-finite simulation time {0}")]
    NonFiniteTime(f64),
    /// Native time did not match the fixed source-authored quantum.
    #[error("native time {actual} differs from expected fixed-step time {expected}")]
    TimeMismatch {
        /// Native timestamp after the transition.
        actual: f64,
        /// Timestamp implied by the boundary and quantum.
        expected: f64,
    },
    /// An action value cannot enter the authoritative scene.
    #[error("control value at index {index} must be finite, got {value}")]
    NonFiniteControl {
        /// Control index.
        index: usize,
        /// Invalid value.
        value: f64,
    },
    /// A control index is outside the compiled model.
    #[error("control index {index} is outside the model's {length}-element control table")]
    ControlIndex {
        /// Requested control index.
        index: usize,
        /// Number of controls.
        length: usize,
    },
    /// A complete control vector has the wrong scalar length.
    #[error("control vector has length {actual}, expected {expected}")]
    ControlLength {
        /// Supplied scalar count.
        actual: usize,
        /// Required scalar count.
        expected: usize,
    },
    /// A control violates a native finite control range.
    #[error("control value {value} at index {index} is outside [{lower}, {upper}]")]
    ControlOutOfRange {
        /// Control index.
        index: usize,
        /// Requested value.
        value: f64,
        /// Lower bound.
        lower: f64,
        /// Upper bound.
        upper: f64,
    },
}
