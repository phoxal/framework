//! Canonical logical asset identities, and validated access to the assets
//! compiled into a runtime bundle.
//!
//! The resolver lives here rather than in the `phoxal` facade because it is not
//! participant-authoring surface: the supervisor serves declared assets itself,
//! and `phoxal-cli` deliberately does not depend on the facade. Fencing that
//! both ends must agree on belongs in the model both ends already share.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::ModelError;

/// A normalized, forward-slash logical asset identifier.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AssetId(String);

impl AssetId {
    /// Validate and construct a logical asset identifier.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::MalformedAssetId`] unless `value` is a relative,
    /// forward-slash path with no empty, `.` or `..` segment. That is what
    /// makes an id unable to name anything outside the asset root.
    pub fn new(value: impl Into<String>) -> Result<Self, ModelError> {
        let value = value.into();
        if value.is_empty()
            || value.starts_with('/')
            || value.contains('\\')
            || value
                .split('/')
                .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
        {
            return Err(ModelError::MalformedAssetId { value });
        }
        Ok(Self(value))
    }

    /// The normalized forward-slash representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for AssetId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AssetId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// A failure reading the compiled asset tree, or a request it refuses.
#[derive(Debug, thiserror::Error)]
pub enum AssetError {
    /// The declared asset root could not be resolved to a canonical path, so
    /// nothing below it can be fenced.
    #[error("failed to resolve compiled asset root {}: {source}", root.display())]
    UnresolvableRoot {
        root: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The tree contains a symlink. Following one could leave the root, so the
    /// whole tree is refused rather than the link inspected.
    #[error("compiled asset tree contains forbidden symlink {}", path.display())]
    ForbiddenSymlink { path: PathBuf },

    /// An entry is neither a regular file nor a directory.
    #[error("compiled asset tree contains unsupported entry {}", path.display())]
    UnsupportedEntry { path: PathBuf },

    /// A discovered entry is not below the root it was walked from.
    #[error("compiled asset escaped root: {}", path.display())]
    EscapedRoot { path: PathBuf },

    /// A discovered path contains a component that is not a plain name.
    #[error("compiled asset path is not normalized: {}", path.display())]
    UnnormalizedPath { path: PathBuf },

    /// A discovered path is not UTF-8, so it has no logical identity.
    #[error("compiled asset path is not UTF-8: {}", path.display())]
    NonUtf8Path { path: PathBuf },

    /// Two entries in the tree claim one logical identity.
    #[error("duplicate compiled asset '{}'", id.as_str())]
    DuplicateAsset { id: AssetId },

    /// A request named an asset this bundle does not declare.
    #[error("undeclared asset '{}'", id.as_str())]
    UndeclaredAsset { id: AssetId },

    /// A discovered path does not form a valid logical asset identity.
    #[error(transparent)]
    Identity(#[from] ModelError),

    /// The filesystem refused the operation.
    #[error("asset filesystem failure: {0}")]
    Io(#[from] std::io::Error),
}

/// Read-only resolver for the declared assets below `<bundle>/assets`.
///
/// This is the participant-facing half of bundle access: exactly the frozen
/// documents and meshes a participant may read. Serving arbitrary bundle files
/// (including `bin/`) is a separate supervisor-only capability.
///
/// Discovery is the fence: the tree is walked once, every entry validated into
/// an [`AssetId`], and only that map is ever consulted afterwards. A request
/// therefore cannot reach participant binaries, undeclared files, or anything
/// outside the asset root - not because a request is inspected for traversal,
/// but because no path outside the map is reachable at all.
#[derive(Clone, Debug, Default)]
pub struct ParticipantAssetResolver {
    paths: BTreeMap<AssetId, PathBuf>,
}

impl ParticipantAssetResolver {
    /// Walk `root` and validate every entry into the declared set.
    ///
    /// A missing root is an empty set, not an error: a bundle may legitimately
    /// declare no assets.
    pub fn discover(root: PathBuf) -> Result<Self, AssetError> {
        if !root.exists() {
            return Ok(Self::default());
        }
        let canonical_root =
            std::fs::canonicalize(&root).map_err(|source| AssetError::UnresolvableRoot {
                root: root.clone(),
                source,
            })?;
        let mut paths = BTreeMap::new();
        Self::discover_into(&canonical_root, &canonical_root, &mut paths)?;
        Ok(Self { paths })
    }

    /// Read a declared asset.
    pub fn read(&self, id: &AssetId) -> Result<Vec<u8>, AssetError> {
        Ok(std::fs::read(self.path(id)?)?)
    }

    /// Open a declared asset.
    pub fn open(&self, id: &AssetId) -> Result<std::fs::File, AssetError> {
        Ok(std::fs::File::open(self.path(id)?)?)
    }

    /// Resolve a declared asset without permitting traversal or symlink escape.
    pub fn path(&self, id: &AssetId) -> Result<&Path, AssetError> {
        self.paths
            .get(id)
            .map(PathBuf::as_path)
            .ok_or_else(|| AssetError::UndeclaredAsset { id: id.clone() })
    }

    /// The validated logical identifiers available in this bundle.
    pub fn ids(&self) -> impl ExactSizeIterator<Item = &AssetId> {
        self.paths.keys()
    }

    /// Walk one directory of the asset tree, validating every entry into the
    /// declared set.
    fn discover_into(
        root: &Path,
        directory: &Path,
        paths: &mut BTreeMap<AssetId, PathBuf>,
    ) -> Result<(), AssetError> {
        let mut entries = std::fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let source = entry.path();
            let metadata = std::fs::symlink_metadata(&source)?;
            if metadata.file_type().is_symlink() {
                return Err(AssetError::ForbiddenSymlink { path: source });
            }
            if metadata.is_dir() {
                Self::discover_into(root, &source, paths)?;
                continue;
            }
            if !metadata.is_file() {
                return Err(AssetError::UnsupportedEntry { path: source });
            }
            let relative = source
                .strip_prefix(root)
                .map_err(|_| AssetError::EscapedRoot {
                    path: source.clone(),
                })?;
            if relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
            {
                return Err(AssetError::UnnormalizedPath {
                    path: relative.to_path_buf(),
                });
            }
            let logical = relative
                .components()
                .map(|component| {
                    component
                        .as_os_str()
                        .to_str()
                        .ok_or_else(|| AssetError::NonUtf8Path {
                            path: relative.to_path_buf(),
                        })
                })
                .collect::<Result<Vec<_>, AssetError>>()?
                .join("/");
            let id = AssetId::new(logical)?;
            if paths.insert(id.clone(), source.clone()).is_some() {
                return Err(AssetError::DuplicateAsset { id });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_normalized_ids() {
        for invalid in ["", "/a", "../a", "a/../b", "a\\b", "a//b"] {
            assert!(AssetId::new(invalid).is_err(), "{invalid}");
        }
        assert_eq!(
            AssetId::new("meshes/base.stl").unwrap().as_str(),
            "meshes/base.stl"
        );
    }

    #[test]
    fn a_missing_root_is_an_empty_set_not_a_failure() {
        let resolver = ParticipantAssetResolver::discover(PathBuf::from("/nonexistent/assets"))
            .expect("a bundle may declare no assets");
        assert_eq!(resolver.ids().len(), 0);
    }

    #[test]
    fn only_discovered_assets_are_reachable() {
        let root = tempfile::tempdir().expect("temp dir");
        let assets = root.path().join("assets");
        std::fs::create_dir_all(assets.join("meshes")).unwrap();
        std::fs::write(assets.join("meshes/base.stl"), b"mesh").unwrap();
        // A sibling of the asset root, exactly like `bin/` in a real bundle:
        // reachable on disk, never through the resolver.
        std::fs::create_dir_all(root.path().join("bin")).unwrap();
        std::fs::write(root.path().join("bin/phoxal-service-drive"), b"secret").unwrap();

        let resolver = ParticipantAssetResolver::discover(assets).expect("discovery succeeds");
        assert_eq!(resolver.ids().len(), 1);
        assert_eq!(
            resolver
                .read(&AssetId::new("meshes/base.stl").unwrap())
                .unwrap(),
            b"mesh"
        );
        // Nothing outside the declared set resolves, and the fencing does not
        // depend on inspecting the request: these ids simply are not keys.
        for undeclared in ["robot.yaml", "meshes/other.stl", "bin/phoxal-service-drive"] {
            let id = AssetId::new(undeclared).expect("a syntactically valid id");
            assert!(
                matches!(resolver.path(&id), Err(AssetError::UndeclaredAsset { .. })),
                "{undeclared}"
            );
        }
    }

    #[test]
    fn a_symlink_in_the_tree_is_refused_outright() {
        let root = tempfile::tempdir().expect("temp dir");
        let assets = root.path().join("assets");
        std::fs::create_dir_all(&assets).unwrap();
        let outside = root.path().join("outside.txt");
        std::fs::write(&outside, b"outside").unwrap();
        std::os::unix::fs::symlink(&outside, assets.join("link.txt")).unwrap();

        let error =
            ParticipantAssetResolver::discover(assets).expect_err("a symlink must fail discovery");
        assert!(
            matches!(error, AssetError::ForbiddenSymlink { .. }),
            "{error}"
        );
        assert!(error.to_string().contains("forbidden symlink"), "{error}");
    }
}
