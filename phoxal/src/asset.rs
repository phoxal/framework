//! Validated access to assets compiled into a runtime bundle.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

/// A normalized, forward-slash logical asset identifier.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct AssetId(String);

impl AssetId {
    /// Validate and construct a logical asset identifier.
    pub fn new(value: impl Into<String>) -> crate::Result<Self> {
        let value = value.into();
        if value.is_empty()
            || value.starts_with('/')
            || value.contains('\\')
            || value
                .split('/')
                .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
        {
            anyhow::bail!("invalid logical asset id '{value}'");
        }
        Ok(Self(value))
    }

    /// The normalized forward-slash representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Read-only resolver for the declared assets below `<bundle>/assets`.
#[derive(Clone, Debug)]
pub struct AssetResolver {
    paths: BTreeMap<AssetId, PathBuf>,
}

impl AssetResolver {
    pub(crate) fn discover(root: PathBuf) -> crate::Result<Self> {
        if !root.exists() {
            return Ok(Self {
                paths: BTreeMap::new(),
            });
        }
        let canonical_root = std::fs::canonicalize(&root).map_err(|error| {
            anyhow::anyhow!(
                "failed to resolve compiled asset root {}: {error}",
                root.display()
            )
        })?;
        let mut paths = BTreeMap::new();
        discover_assets(&canonical_root, &canonical_root, &mut paths)?;
        Ok(Self { paths })
    }

    /// Read a declared asset.
    pub fn read(&self, id: &AssetId) -> crate::Result<Vec<u8>> {
        Ok(std::fs::read(self.path(id)?)?)
    }

    /// Open a declared asset.
    pub fn open(&self, id: &AssetId) -> crate::Result<std::fs::File> {
        Ok(std::fs::File::open(self.path(id)?)?)
    }

    /// Resolve a declared asset without permitting traversal or symlink escape.
    pub fn path(&self, id: &AssetId) -> crate::Result<&Path> {
        self.paths
            .get(id)
            .map(PathBuf::as_path)
            .ok_or_else(|| anyhow::anyhow!("undeclared asset '{}'", id.as_str()))
    }

    /// The validated logical identifiers available in this bundle.
    pub fn ids(&self) -> impl ExactSizeIterator<Item = &AssetId> {
        self.paths.keys()
    }
}

fn discover_assets(
    root: &Path,
    directory: &Path,
    paths: &mut BTreeMap<AssetId, PathBuf>,
) -> crate::Result<()> {
    let mut entries = std::fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let source = entry.path();
        let metadata = std::fs::symlink_metadata(&source)?;
        if metadata.file_type().is_symlink() {
            anyhow::bail!(
                "compiled asset tree contains forbidden symlink {}",
                source.display()
            );
        }
        if metadata.is_dir() {
            discover_assets(root, &source, paths)?;
            continue;
        }
        if !metadata.is_file() {
            anyhow::bail!(
                "compiled asset tree contains unsupported entry {}",
                source.display()
            );
        }
        let relative = source
            .strip_prefix(root)
            .map_err(|_| anyhow::anyhow!("compiled asset escaped root: {}", source.display()))?;
        if relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        {
            anyhow::bail!(
                "compiled asset path is not normalized: {}",
                relative.display()
            );
        }
        let logical = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        let id = AssetId::new(logical)?;
        if paths.insert(id.clone(), source).is_some() {
            anyhow::bail!("duplicate compiled asset '{}'", id.as_str());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_ids_reject_non_normal_paths() {
        for value in [
            "",
            "/absolute",
            "../secret",
            "a/../b",
            "a\\b",
            "a//b",
            "a/./b",
        ] {
            assert!(AssetId::new(value).is_err(), "{value}");
        }
        assert_eq!(
            AssetId::new("meshes/base.stl").unwrap().as_str(),
            "meshes/base.stl"
        );
    }

    #[test]
    fn a_bundle_without_assets_has_an_empty_resolver() {
        let bundle = tempfile::tempdir().unwrap();
        std::fs::write(bundle.path().join("robot.json"), b"{}").unwrap();
        let resolver = AssetResolver::discover(bundle.path().join("assets")).unwrap();
        assert_eq!(resolver.ids().len(), 0);
    }
}
