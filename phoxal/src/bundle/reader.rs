//! The supervisor's local bundle reader.

use std::path::Path;

use crate::model::identity::RobotId;
use crate::model::manifest::ManifestDocument;
use crate::model::{AssetId, Robot};

use crate::bundle::{BundleError, BundleRoot, ParticipantAssets, read_manifest_document};

/// An opened bundle: its manifest, and access to its assets.
///
/// Opening one parses `manifest.json` and does nothing else. It is only for a
/// process that owns a local bundle root, such as the supervisor or explicit
/// in-process harness. Launched participants receive their model and assets
/// through the supervisor instead of opening this directory themselves.
#[derive(Clone, Debug)]
pub struct RuntimeBundle {
    root: BundleRoot,
    manifest: ManifestDocument,
    assets: ParticipantAssets,
}

impl RuntimeBundle {
    /// Open one installed bundle.
    ///
    /// # Errors
    ///
    /// Returns [`BundleError::Root`] or [`BundleError::NotDirectory`] when
    /// `root` is not a directory, [`BundleError::ReadManifest`] when
    /// `manifest.json` cannot be read, and [`BundleError::ManifestJson`] when it
    /// is not a document this train understands.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, BundleError> {
        let root = BundleRoot::open(root.as_ref())?;
        let manifest = read_manifest_document(&root)?;
        Ok(Self {
            assets: ParticipantAssets::new(root.clone()),
            root,
            manifest,
        })
    }

    /// The bundle root path, retained for diagnostics and for launching.
    #[must_use]
    pub fn root(&self) -> &Path {
        self.root.path()
    }

    /// The persisted document, tag included.
    #[must_use]
    pub const fn manifest(&self) -> &ManifestDocument {
        &self.manifest
    }

    /// The compiled robot the manifest carries.
    #[must_use]
    pub const fn robot(&self) -> &Robot {
        self.manifest.robot()
    }

    /// The sole persisted robot identity.
    #[must_use]
    pub const fn robot_id(&self) -> &RobotId {
        self.robot().id()
    }

    /// Read one asset out of `<bundle>/assets`.
    ///
    /// # Errors
    ///
    /// Returns the same failures as local [`ParticipantAssets::read`].
    pub fn asset(&self, id: &AssetId) -> Result<Vec<u8>, BundleError> {
        self.assets.read_local(id)
    }

    /// The asset reader, for a consumer that keeps it beyond this value.
    #[must_use]
    pub const fn assets(&self) -> &ParticipantAssets {
        &self.assets
    }

    pub(crate) fn relocated(mut self, path: std::path::PathBuf) -> Self {
        self.root.relocate(path.clone());
        self.assets.relocate(path);
        self
    }
}
