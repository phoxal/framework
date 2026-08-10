//! Indexed executable artifact records.

use std::fmt;
use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};

use phoxal_runtime_contract::identity::ParticipantArtifactId;
use phoxal_runtime_contract::metadata::ParticipantContract;
use serde::{Deserialize, Serialize};

use crate::{
    BIN_DIR, BundleError, BundlePath, DocumentError, Sha256Digest, open_executable_source,
};

/// One opened executable source. The writer hashes and copies the same open
/// handle, so the digest it records and the bytes it stages come from one file.
pub struct BinarySource {
    file: std::fs::File,
    path: PathBuf,
}

impl fmt::Debug for BinarySource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BinarySource")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl BinarySource {
    /// Open one executable source file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, BundleError> {
        let path = path.as_ref().to_path_buf();
        let file = open_executable_source(&path)?;
        Ok(Self { file, path })
    }

    /// The source pathname retained only for diagnostics.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn reader(&self) -> Result<std::fs::File, BundleError> {
        let mut file = self
            .file
            .try_clone()
            .map_err(|source| BundleError::ReadFile {
                path: self.path.clone(),
                source,
            })?;
        file.seek(SeekFrom::Start(0))
            .map_err(|source| BundleError::ReadFile {
                path: self.path.clone(),
                source,
            })?;
        Ok(file)
    }
}

/// A staged reusable executable and its canonical artifact contract.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BinaryReference {
    pub(crate) path: BundlePath,
    pub(crate) digest: Sha256Digest,
    pub(crate) size_bytes: u64,
    pub(crate) contract: ParticipantContract,
}

impl BinaryReference {
    /// Construct a reference for an already-built executable source.
    pub fn from_source(
        path: BundlePath,
        contract: ParticipantContract,
        source: &BinarySource,
    ) -> Result<Self, BundleError> {
        let file = source.reader()?;
        let size_bytes = file
            .metadata()
            .map_err(|source_error| BundleError::ReadFile {
                path: source.path.clone(),
                source: source_error,
            })?
            .len();
        Ok(Self {
            path,
            digest: Sha256Digest::from_reader(file).map_err(|source_error| {
                BundleError::ReadFile {
                    path: source.path.clone(),
                    source: source_error,
                }
            })?,
            size_bytes,
            contract,
        })
    }

    #[must_use]
    pub const fn path(&self) -> &BundlePath {
        &self.path
    }
    #[must_use]
    pub const fn digest(&self) -> Sha256Digest {
        self.digest
    }
    #[must_use]
    pub const fn size_bytes(&self) -> u64 {
        self.size_bytes
    }
    #[must_use]
    pub const fn contract(&self) -> &ParticipantContract {
        &self.contract
    }

    pub(crate) fn validate(&self, id: &ParticipantArtifactId) -> Result<(), DocumentError> {
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
