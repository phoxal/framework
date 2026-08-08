//! Participant-facing asset capability.

use std::collections::BTreeMap;

use phoxal_model::AssetId;

use crate::{AssetIndex, AssetRecord, BundleError, BundleRoot, open_bundle_file, read_and_verify};

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
