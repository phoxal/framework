//! Canonical compiled world bundle storage and validation.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::model::asset::AssetId;
use crate::model::world::{World, WorldDigest, WorldProgressError};

const WORLD_FILE: &str = "world.json";
const ASSETS_DIRECTORY: &str = "assets";
const ARCHIVE_MAGIC: &[u8] = b"phoxal-world-bundle-v0\0";

/// A canonical expanded world plus every asset byte it can reach.
#[derive(Clone, Debug)]
pub struct WorldBundle {
    world: World,
    assets: BTreeMap<AssetId, Vec<u8>>,
    canonical_archive: Vec<u8>,
    digest: WorldDigest,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "schema", deny_unknown_fields)]
enum WorldDocument {
    #[serde(rename = "phoxal/world-bundle/v0")]
    V0(World),
}

impl WorldBundle {
    pub(crate) fn from_compiler(
        world: World,
        assets: BTreeMap<AssetId, Vec<u8>>,
    ) -> Result<Self, WorldBundleError> {
        world.validate_intrinsic()?;
        validate_assets(&world, &assets)?;
        let canonical_archive = canonical_archive(&world, &assets)?;
        let digest = WorldDigest::of(&canonical_archive);
        Ok(Self {
            world,
            assets,
            canonical_archive,
            digest,
        })
    }

    /// Open and validate one inspectable `world.json` plus `assets/` bundle.
    ///
    /// # Errors
    ///
    /// Returns [`WorldBundleError`] for I/O, document, path, or closure failures.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, WorldBundleError> {
        let root = root
            .as_ref()
            .canonicalize()
            .map_err(|source| WorldBundleError::Io {
                path: root.as_ref().to_path_buf(),
                source,
            })?;
        validate_root_layout(&root)?;
        let document_path = root.join(WORLD_FILE);
        let document = std::fs::read(&document_path).map_err(|source| WorldBundleError::Io {
            path: document_path.clone(),
            source,
        })?;
        let WorldDocument::V0(world) =
            serde_json::from_slice(&document).map_err(|source| WorldBundleError::Document {
                path: document_path,
                source,
            })?;
        let assets = read_assets(&root.join(ASSETS_DIRECTORY))?;
        Self::from_compiler(world, assets)
    }

    /// Atomically write this bundle into a new target directory.
    ///
    /// # Errors
    ///
    /// Returns [`WorldBundleError::TargetExists`] when `root` already exists or an I/O error while staging.
    pub fn write(&self, root: impl AsRef<Path>) -> Result<(), WorldBundleError> {
        let root = root.as_ref();
        if root.exists() {
            return Err(WorldBundleError::TargetExists(root.to_path_buf()));
        }
        let parent = root.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent).map_err(|source| WorldBundleError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
        let staging = tempfile::Builder::new()
            .prefix(".phoxal-world-")
            .tempdir_in(parent)
            .map_err(|source| WorldBundleError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        let assets_root = staging.path().join(ASSETS_DIRECTORY);
        std::fs::create_dir(&assets_root).map_err(|source| WorldBundleError::Io {
            path: assets_root.clone(),
            source,
        })?;
        let document = serde_json::to_vec_pretty(&WorldDocument::V0(self.world.clone()))?;
        let document_path = staging.path().join(WORLD_FILE);
        std::fs::write(&document_path, document).map_err(|source| WorldBundleError::Io {
            path: document_path,
            source,
        })?;
        for (id, bytes) in &self.assets {
            let path = asset_path(&assets_root, id)?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|source| WorldBundleError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            std::fs::write(&path, bytes).map_err(|source| WorldBundleError::Io { path, source })?;
        }
        std::fs::rename(staging.path(), root).map_err(|source| WorldBundleError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        Ok(())
    }

    #[must_use]
    pub const fn world(&self) -> &World {
        &self.world
    }

    #[must_use]
    pub const fn digest(&self) -> WorldDigest {
        self.digest
    }

    pub fn assets(&self) -> impl ExactSizeIterator<Item = (&AssetId, &[u8])> {
        self.assets.iter().map(|(id, bytes)| (id, bytes.as_slice()))
    }

    #[must_use]
    pub fn asset(&self, id: &AssetId) -> Option<&[u8]> {
        self.assets.get(id).map(Vec::as_slice)
    }

    /// Deterministic bytes over which [`WorldDigest`] is computed.
    #[must_use]
    pub fn canonical_archive(&self) -> &[u8] {
        &self.canonical_archive
    }
}

fn validate_assets(
    world: &World,
    assets: &BTreeMap<AssetId, Vec<u8>>,
) -> Result<(), WorldBundleError> {
    let referenced = world.referenced_assets();
    let present = assets.keys().cloned().collect::<BTreeSet<_>>();
    if referenced != present {
        return Err(WorldBundleError::AssetClosure {
            referenced: referenced
                .into_iter()
                .map(|id| id.as_str().to_owned())
                .collect(),
            present: present
                .into_iter()
                .map(|id| id.as_str().to_owned())
                .collect(),
        });
    }
    for (id, bytes) in assets {
        let expected = format!("sha256/{:x}.glb", Sha256::digest(bytes));
        if id.as_str() != expected {
            return Err(WorldBundleError::AssetDigestMismatch {
                asset: id.as_str().to_owned(),
                expected,
            });
        }
        super::glb::validate_closed(bytes).map_err(|source| WorldBundleError::InvalidAsset {
            asset: id.as_str().to_owned(),
            detail: source.to_string(),
        })?;
    }
    Ok(())
}

fn validate_root_layout(root: &Path) -> Result<(), WorldBundleError> {
    let mut world = false;
    let mut assets = false;
    let entries = std::fs::read_dir(root)
        .and_then(Iterator::collect::<std::io::Result<Vec<_>>>)
        .map_err(|source| WorldBundleError::Io {
            path: root.to_path_buf(),
            source,
        })?;
    for entry in entries {
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(|source| WorldBundleError::Io {
            path: path.clone(),
            source,
        })?;
        match entry.file_name().to_str() {
            Some(WORLD_FILE) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                world = true;
            }
            Some(ASSETS_DIRECTORY) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                assets = true;
            }
            _ => {
                return Err(WorldBundleError::Invalid(format!(
                    "world bundle contains an unsupported root entry: {}",
                    path.display()
                )));
            }
        }
    }
    if !world || !assets {
        return Err(WorldBundleError::Invalid(
            "world bundle must contain exactly world.json and assets/".to_owned(),
        ));
    }
    Ok(())
}

fn canonical_archive(
    world: &World,
    assets: &BTreeMap<AssetId, Vec<u8>>,
) -> Result<Vec<u8>, WorldBundleError> {
    let document = serde_json::to_vec(&WorldDocument::V0(world.clone()))?;
    let mut archive = Vec::new();
    archive.extend_from_slice(ARCHIVE_MAGIC);
    append_archive_entry(&mut archive, WORLD_FILE.as_bytes(), &document)?;
    for (id, bytes) in assets {
        let name = format!("{ASSETS_DIRECTORY}/{}", id.as_str());
        append_archive_entry(&mut archive, name.as_bytes(), bytes)?;
    }
    Ok(archive)
}

fn append_archive_entry(
    archive: &mut Vec<u8>,
    name: &[u8],
    bytes: &[u8],
) -> Result<(), WorldBundleError> {
    let name_len = u64::try_from(name.len()).map_err(|_| WorldBundleError::ArchiveTooLarge)?;
    let bytes_len = u64::try_from(bytes.len()).map_err(|_| WorldBundleError::ArchiveTooLarge)?;
    archive.extend_from_slice(&name_len.to_be_bytes());
    archive.extend_from_slice(name);
    archive.extend_from_slice(&bytes_len.to_be_bytes());
    archive.extend_from_slice(bytes);
    Ok(())
}

fn asset_path(root: &Path, id: &AssetId) -> Result<PathBuf, WorldBundleError> {
    let path = root.join(id.as_str());
    if !path.starts_with(root) {
        return Err(WorldBundleError::InvalidAssetPath(id.as_str().to_owned()));
    }
    Ok(path)
}

fn read_assets(root: &Path) -> Result<BTreeMap<AssetId, Vec<u8>>, WorldBundleError> {
    if !root.is_dir() {
        return Err(WorldBundleError::Invalid(format!(
            "world bundle is missing {}",
            root.display()
        )));
    }
    let mut assets = BTreeMap::new();
    read_asset_directory(root, root, &mut assets)?;
    Ok(assets)
}

fn read_asset_directory(
    root: &Path,
    current: &Path,
    assets: &mut BTreeMap<AssetId, Vec<u8>>,
) -> Result<(), WorldBundleError> {
    let mut entries = std::fs::read_dir(current)
        .and_then(Iterator::collect::<std::io::Result<Vec<_>>>)
        .map_err(|source| WorldBundleError::Io {
            path: current.to_path_buf(),
            source,
        })?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(|source| WorldBundleError::Io {
            path: path.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink() {
            return Err(WorldBundleError::Invalid(format!(
                "world bundle asset is a forbidden symlink: {}",
                path.display()
            )));
        }
        if metadata.is_dir() {
            read_asset_directory(root, &path, assets)?;
            continue;
        }
        if !metadata.is_file() {
            return Err(WorldBundleError::Invalid(format!(
                "world bundle contains an unsupported asset entry: {}",
                path.display()
            )));
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| WorldBundleError::InvalidAssetPath(path.display().to_string()))?;
        let relative = relative
            .to_str()
            .ok_or_else(|| WorldBundleError::InvalidAssetPath(path.display().to_string()))?
            .replace(std::path::MAIN_SEPARATOR, "/");
        let id = AssetId::new(relative)
            .map_err(|_| WorldBundleError::InvalidAssetPath(path.display().to_string()))?;
        let bytes = std::fs::read(&path).map_err(|source| WorldBundleError::Io {
            path: path.clone(),
            source,
        })?;
        assets.insert(id, bytes);
    }
    Ok(())
}

/// A compiled world bundle that is not closed and canonical.
#[derive(Debug, thiserror::Error)]
pub enum WorldBundleError {
    #[error("world bundle target already exists: {}", .0.display())]
    TargetExists(PathBuf),
    #[error("world bundle I/O failed at {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("world bundle document {} is invalid: {source}", path.display())]
    Document {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("world bundle JSON could not be encoded: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid world bundle: {0}")]
    Invalid(String),
    #[error("invalid world bundle asset path '{0}'")]
    InvalidAssetPath(String),
    #[error("world bundle asset '{asset}' is not a closed GLB v2 file: {detail}")]
    InvalidAsset { asset: String, detail: String },
    #[error("world bundle asset closure differs: referenced {referenced:?}, present {present:?}")]
    AssetClosure {
        referenced: Vec<String>,
        present: Vec<String>,
    },
    #[error("world bundle asset '{asset}' does not match its byte digest; expected '{expected}'")]
    AssetDigestMismatch { asset: String, expected: String },
    #[error(transparent)]
    Progress(#[from] WorldProgressError),
    #[error("world bundle canonical archive exceeds supported length")]
    ArchiveTooLarge,
}
