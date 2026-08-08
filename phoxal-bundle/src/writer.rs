//! Final bundle staging and atomic publication.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use phoxal_model::AssetId;

use crate::{
    ASSETS_DIR, AssetIndex, BinaryReference, BinarySource, BundleError, BundlePath, BundleRoot,
    DocumentError, RuntimeBundle, RuntimeDocument, copy_executable_source, create_staging_root,
    ensure_staging_directory, mark_staging_root_ready, prepare_publish_parent,
    publish_staging_root, reject_existing_target, write_new_file,
};

/// A build-tool-facing writer for the explicit final assembly boundary.
pub struct BundleWriter;

impl BundleWriter {
    /// Write a document and the exact staged executable sources it references.
    pub fn write(
        root: impl AsRef<Path>,
        document: &RuntimeDocument,
        assets: &BTreeMap<AssetId, Vec<u8>>,
        binaries: &BTreeMap<BundlePath, BinarySource>,
    ) -> Result<RuntimeBundle, BundleError> {
        write_inner(root, document, assets, binaries, publish_staging_root)
    }

    #[cfg(test)]
    pub(crate) fn write_inner<F>(
        root: impl AsRef<Path>,
        document: &RuntimeDocument,
        assets: &BTreeMap<AssetId, Vec<u8>>,
        binaries: &BTreeMap<BundlePath, BinarySource>,
        publish: F,
    ) -> Result<RuntimeBundle, BundleError>
    where
        F: FnOnce(&Path, &Path) -> Result<(), BundleError>,
    {
        write_inner(root, document, assets, binaries, publish)
    }
}

pub(crate) fn write_inner<F>(
    root: impl AsRef<Path>,
    document: &RuntimeDocument,
    assets: &BTreeMap<AssetId, Vec<u8>>,
    binaries: &BTreeMap<BundlePath, BinarySource>,
    publish: F,
) -> Result<RuntimeBundle, BundleError>
where
    F: FnOnce(&Path, &Path) -> Result<(), BundleError>,
{
    let expected_assets = document.runtime().assets();
    let supplied_assets = AssetIndex::from_bytes(assets)?;
    if supplied_assets.entries() != expected_assets.entries() {
        return Err(BundleError::Document(DocumentError::AssetIndexMismatch));
    }

    let expected_binaries = document
        .artifacts()
        .values()
        .map(|artifact| artifact.path().clone())
        .collect::<BTreeSet<_>>();
    let supplied_binaries = binaries.keys().cloned().collect::<BTreeSet<_>>();
    if expected_binaries != supplied_binaries {
        return Err(BundleError::Document(DocumentError::BinaryIndexMismatch));
    }
    let publish_target = prepare_publish_parent(root.as_ref())?;
    reject_existing_target(&publish_target)?;
    let staging_path = create_staging_root(&publish_target)?;
    let root = BundleRoot::open(&staging_path)?;
    let staged = (|| {
        ensure_staging_directory(&root, ASSETS_DIR)?;
        ensure_staging_directory(&root, crate::BIN_DIR)?;
        for (id, bytes) in assets {
            let path = BundlePath::new(format!("{ASSETS_DIR}/{}", id.as_str()))?;
            write_new_file(&root, &path, bytes)?;
        }
        for (path, source) in binaries {
            let artifact: &BinaryReference = document
                .artifacts()
                .values()
                .find(|artifact| artifact.path() == path)
                .ok_or_else(|| BundleError::MissingFile {
                    path: path.filesystem_path(root.path()),
                })?;
            copy_executable_source(
                &root,
                source,
                path,
                artifact.digest(),
                artifact.size_bytes(),
            )?;
        }
        let json = serde_json::to_vec_pretty(document)?;
        write_new_file(&root, &BundlePath::new(crate::RUNTIME_FILE)?, &json)?;
        mark_staging_root_ready(&root)?;
        RuntimeBundle::open_verified(root.path())
    })();
    let verified = match staged {
        Ok(verified) => verified,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staging_path);
            return Err(error);
        }
    };
    if let Err(error) = publish(root.path(), &publish_target) {
        let _ = std::fs::remove_dir_all(&staging_path);
        return Err(error);
    }
    Ok(verified.relocated(publish_target))
}
