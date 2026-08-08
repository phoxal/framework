//! Participant-facing asset capability.

use std::collections::{BTreeMap, BTreeSet};

use phoxal_model::AssetId;
use serde::{Deserialize, Serialize};

use crate::{
    ASSETS_DIR, BundleError, BundlePath, BundleRoot, DocumentError, Sha256Digest, open_bundle_file,
    read_and_verify,
};

/// The integrity index for every participant-readable asset.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssetIndex {
    pub(crate) entries: Vec<AssetRecord>,
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

    pub(crate) fn validate(&self) -> Result<(), DocumentError> {
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
    pub(crate) id: AssetId,
    pub(crate) path: BundlePath,
    pub(crate) size_bytes: u64,
    pub(crate) digest: Sha256Digest,
}

impl AssetRecord {
    #[must_use]
    pub const fn id(&self) -> &AssetId {
        &self.id
    }

    #[must_use]
    pub const fn path(&self) -> &BundlePath {
        &self.path
    }

    #[must_use]
    pub const fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    #[must_use]
    pub const fn digest(&self) -> Sha256Digest {
        self.digest
    }
}

/// Participant-readable, digest-checked asset access.
#[derive(Clone, Debug)]
pub struct ParticipantAssets {
    root: BundleRoot,
    entries: BTreeMap<AssetId, AssetRecord>,
}

impl ParticipantAssets {
    pub(crate) fn new(root: BundleRoot, index: &AssetIndex) -> Self {
        Self {
            root,
            entries: index
                .entries
                .iter()
                .map(|entry| (entry.id.clone(), entry.clone()))
                .collect(),
        }
    }

    pub(crate) fn relocate(&mut self, path: std::path::PathBuf) {
        self.root.relocate(path);
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
        let path = entry.path.filesystem_path(self.root.path());
        let mut file = open_bundle_file(&self.root, &entry.path)?;
        read_and_verify(&mut file, &path, entry.digest, Some(entry.size_bytes))
    }
}
