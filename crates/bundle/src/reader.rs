//! The one bundle reader, used by the supervisor and by every participant.

use std::path::Path;

use phoxal_model::identity::RobotId;
use phoxal_model::manifest::ManifestDocument;
use phoxal_model::{AssetId, Robot};

use crate::{BundleError, BundleRoot, ParticipantAssets, read_manifest_document};

/// An opened bundle: its manifest, and access to its assets.
///
/// Opening one parses `manifest.json` and does nothing else. A participant not
/// named in the manifest opens the bundle exactly as one that is - the manifest
/// is the robot model plus, for those that have one, their own configuration -
/// so there is no selection step and no way for a launched process to be refused
/// by the bundle it was pointed at.
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
    /// Returns the same failures as [`ParticipantAssets::read`].
    pub fn asset(&self, id: &AssetId) -> Result<Vec<u8>, BundleError> {
        self.assets.read(id)
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
