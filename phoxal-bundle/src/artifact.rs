//! Indexed executable artifact records.

use std::path::Path;

use phoxal_runtime_contract::identity::ParticipantArtifactId;
use phoxal_runtime_contract::metadata::ParticipantContract;
use serde::{Deserialize, Serialize};

use crate::{BIN_DIR, BundleError, BundlePath, DocumentError, Sha256Digest};

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
