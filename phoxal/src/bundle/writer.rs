//! Final bundle staging and atomic publication.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::model::AssetId;
use crate::model::manifest::ManifestDocument;

use crate::bundle::{
    ASSETS_DIR, BIN_DIR, BundleError, BundlePath, BundleRoot, MANIFEST_FILE, ParticipantAssets,
    RuntimeBundle, copy_executable_source, create_staging_root, ensure_staging_directory,
    prepare_publish_parent, publish_staging_root, reject_existing_target, write_new_file,
};

/// A build-tool-facing writer for the explicit final assembly boundary.
pub struct BundleWriter;

impl BundleWriter {
    /// Write one bundle: the manifest, the assets, and the binaries.
    ///
    /// The bundle is assembled in a private sibling directory and only then
    /// renamed onto its final name, so the target is either absent or a complete
    /// bundle.
    ///
    /// `binaries` maps the bundle-relative destination - `bin/brain`,
    /// `bin/<service-id>`, `bin/<component-type>` - to the executable to copy
    /// there. Nothing is hashed and nothing is recorded: the launcher derives
    /// the executable from the id it is launching, and integrity is the
    /// archive's job.
    ///
    /// # Errors
    ///
    /// Returns [`BundleError::TargetExists`] when `root` already exists,
    /// [`BundleError::NotExecutable`] or [`BundleError::UnsupportedEntry`] when a
    /// supplied binary is not a runnable file, and [`BundleError::ReadFile`] for
    /// any other I/O failure. A failed write leaves no staging directory behind.
    pub fn write(
        root: impl AsRef<Path>,
        manifest: &ManifestDocument,
        assets: &BTreeMap<AssetId, Vec<u8>>,
        binaries: &BTreeMap<BundlePath, PathBuf>,
    ) -> Result<RuntimeBundle, BundleError> {
        let publish_target = prepare_publish_parent(root.as_ref())?;
        reject_existing_target(&publish_target)?;
        let staging_path = create_staging_root(&publish_target)?;
        let staged = BundleRoot::open(&staging_path)?;
        let written = stage(&staged, manifest, assets, binaries);
        let bundle = match written {
            Ok(bundle) => bundle,
            Err(error) => {
                let _ = std::fs::remove_dir_all(&staging_path);
                return Err(error);
            }
        };
        if let Err(error) = publish_staging_root(staged.path(), &publish_target) {
            let _ = std::fs::remove_dir_all(&staging_path);
            return Err(error);
        }
        Ok(bundle.relocated(publish_target))
    }
}

fn stage(
    root: &BundleRoot,
    manifest: &ManifestDocument,
    assets: &BTreeMap<AssetId, Vec<u8>>,
    binaries: &BTreeMap<BundlePath, PathBuf>,
) -> Result<RuntimeBundle, BundleError> {
    ensure_staging_directory(root, ASSETS_DIR)?;
    ensure_staging_directory(root, BIN_DIR)?;
    for (id, bytes) in assets {
        write_new_file(root, &ParticipantAssets::path(id)?, bytes)?;
    }
    for (destination, source) in binaries {
        copy_executable_source(root, source, destination)?;
    }
    let json = serde_json::to_vec_pretty(manifest)?;
    write_new_file(root, &BundlePath::new(MANIFEST_FILE)?, &json)?;
    RuntimeBundle::open(root.path())
}
