//! Bundle I/O failures.

use std::path::PathBuf;

use crate::bundle::BundlePathError;

/// Why reading or writing a bundle failed.
///
/// Every variant is an I/O or layout fact. There is no "this bundle is not a
/// valid execution plan" family any more: the manifest either parses into a
/// validated [`crate::model::Robot`] or it does not, and the model owns that
/// judgment.
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
    #[error("failed to read bundle manifest {}: {source}", path.display())]
    ReadManifest {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("bundle manifest is not a readable phoxal document: {0}")]
    ManifestJson(#[from] serde_json::Error),
    #[error("bundle contains unsupported filesystem entry {path}")]
    UnsupportedEntry { path: PathBuf },
    #[error("bundle executable is not executable: {path}")]
    NotExecutable { path: PathBuf },
    #[error("bundle is missing required file {path}")]
    MissingFile { path: PathBuf },
    #[error("failed to read bundle file {path}: {source}", path = path.display())]
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Path(#[from] BundlePathError),
}
