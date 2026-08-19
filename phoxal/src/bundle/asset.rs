//! Participant-facing asset reads.

use std::io::Read;

use crate::model::AssetId;

use crate::bundle::{ASSETS_DIR, BundleError, BundlePath, BundleRoot, open_bundle_file};

/// Reads the files below `<bundle>/assets`.
///
/// There is no declared asset set to consult: an [`AssetId`] is already a
/// validated relative forward-slash path with no `.` or `..` segment, and
/// The bundle path type validates the joined path again, so a read cannot name
/// anything outside `assets/`. That pair of checks is the whole fence.
#[derive(Clone, Debug)]
pub struct ParticipantAssets {
    root: BundleRoot,
}

impl ParticipantAssets {
    pub(crate) const fn new(root: BundleRoot) -> Self {
        Self { root }
    }

    pub(crate) fn relocate(&mut self, path: std::path::PathBuf) {
        self.root.relocate(path);
    }

    /// Read one asset out of the bundle.
    ///
    /// # Errors
    ///
    /// Returns a bundle error when the bundle carries no such asset, or when
    /// it carries one that cannot be read.
    pub fn read(&self, id: &AssetId) -> Result<Vec<u8>, BundleError> {
        let path = Self::path(id)?;
        let mut file = open_bundle_file(&self.root, &path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|source| BundleError::ReadFile {
                path: path.filesystem_path(self.root.path()),
                source,
            })?;
        Ok(bytes)
    }

    /// Where one logical asset sits in the bundle.
    pub(crate) fn path(id: &AssetId) -> Result<BundlePath, BundleError> {
        Ok(BundlePath::new(format!("{ASSETS_DIR}/{}", id.as_str()))?)
    }
}
