//! Canonical immutable runtime robot model.
//!
//! The model is constructed from finalized sources at load time; it has no
//! persisted wire form of its own.

pub mod asset;
pub mod component;
pub mod robot;
pub mod simulation;
pub mod structure;

pub use asset::{AssetError, AssetId, ParticipantAssetResolver};
pub use robot::{Clock, Robot, RobotIdentity};

/// Compiler linkage that is intentionally absent from the runtime model API.
#[doc(hidden)]
pub mod __private {
    /// Construct a validated structure from the manifest compiler's normalized
    /// JSON value. Runtime consumers load a complete [`crate::Robot`] instead.
    pub fn structure_from_compiler_value(
        document: serde_json::Value,
    ) -> Result<crate::structure::Structure, crate::ModelError> {
        crate::structure::Structure::from_compiler_value(document)
    }
}

/// Invalid canonical robot semantics.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("{0}")]
    Invalid(String),
}
