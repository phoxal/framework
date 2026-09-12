//! Closed MJCF/resource artifacts and deterministic identity material.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::ArtifactError;

const DEFAULT_MAX_RESOURCE_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_MAX_CLOSURE_BYTES: usize = 256 * 1024 * 1024;
const DEFAULT_MAX_RESOURCES: usize = 4096;

/// Admission limits for a closed native model/resource closure.
///
/// The limits are checked before native parsing so a malformed or oversized
/// source cannot make the native process allocate an unbounded amount of
/// memory through a model-loading path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResourceLimits {
    /// Maximum size of one resource in bytes.
    pub max_resource_bytes: usize,
    /// Maximum sum of resource sizes in bytes.
    pub max_closure_bytes: usize,
    /// Maximum number of resources in a closure.
    pub max_resources: usize,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_resource_bytes: DEFAULT_MAX_RESOURCE_BYTES,
            max_closure_bytes: DEFAULT_MAX_CLOSURE_BYTES,
            max_resources: DEFAULT_MAX_RESOURCES,
        }
    }
}

/// One named byte resource supplied to an MJCF closure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Resource {
    name: String,
    bytes: Vec<u8>,
}

impl Resource {
    /// Creates a resource with a normalized relative VFS name.
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactError`] when the name is absolute, contains `.` or
    /// `..` path components, uses backslashes, or contains a NUL byte.
    pub fn new(name: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Result<Self, ArtifactError> {
        let name = name.into();
        validate_resource_name(&name)?;
        Ok(Self {
            name,
            bytes: bytes.into(),
        })
    }

    /// Returns the normalized VFS name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the resource bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// An immutable, validated native model/resource closure.
///
/// The digest is computed from the entry name and every sorted resource name
/// and byte sequence with explicit length prefixes.
/// It is an artifact identity, not a claim that two native MuJoCo versions
/// will produce bit-identical compiled models.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClosedModel {
    entry: String,
    resources: Vec<Resource>,
    digest: [u8; 32],
}

impl ClosedModel {
    /// Builds and validates a closed model from named resources.
    ///
    /// The resource names are sorted for deterministic VFS insertion and
    /// digesting.
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactError`] for invalid names, duplicates, missing entry,
    /// invalid entry text, or an exceeded closure limit.
    pub fn new(
        entry: impl Into<String>,
        resources: impl IntoIterator<Item = Resource>,
    ) -> Result<Self, ArtifactError> {
        Self::with_limits(entry, resources, ResourceLimits::default())
    }

    /// Builds and validates a closed model with explicit admission limits.
    pub fn with_limits(
        entry: impl Into<String>,
        resources: impl IntoIterator<Item = Resource>,
        limits: ResourceLimits,
    ) -> Result<Self, ArtifactError> {
        let entry = entry.into();
        if entry.is_empty() {
            return Err(ArtifactError::EmptyEntry);
        }
        validate_resource_name(&entry).map_err(|error| match error {
            ArtifactError::InvalidResourceName(_) => {
                ArtifactError::InvalidResourceName(entry.clone())
            }
            ArtifactError::ResourceNameContainsNul(_) => {
                ArtifactError::ResourceNameContainsNul(entry.clone())
            }
            other => other,
        })?;

        let mut by_name = BTreeMap::new();
        let mut total_bytes = 0usize;
        for resource in resources {
            let name = resource.name.clone();
            if by_name.contains_key(&name) {
                return Err(ArtifactError::DuplicateResource(name));
            }
            if by_name.len() >= limits.max_resources {
                return Err(ArtifactError::TooManyResources {
                    actual: by_name.len() + 1,
                    limit: limits.max_resources,
                });
            }
            let size = resource.bytes.len();
            if size > limits.max_resource_bytes {
                return Err(ArtifactError::ResourceTooLarge {
                    name,
                    actual: size,
                    limit: limits.max_resource_bytes,
                });
            }
            total_bytes = total_bytes
                .checked_add(size)
                .ok_or(ArtifactError::ClosureTooLarge {
                    actual: usize::MAX,
                    limit: limits.max_closure_bytes,
                })?;
            by_name.insert(resource.name.clone(), resource);
        }

        if by_name.len() > limits.max_resources {
            return Err(ArtifactError::TooManyResources {
                actual: by_name.len(),
                limit: limits.max_resources,
            });
        }
        if total_bytes > limits.max_closure_bytes {
            return Err(ArtifactError::ClosureTooLarge {
                actual: total_bytes,
                limit: limits.max_closure_bytes,
            });
        }

        let entry_resource = by_name
            .get(&entry)
            .ok_or_else(|| ArtifactError::EntryMissing(entry.clone()))?;
        std::str::from_utf8(&entry_resource.bytes).map_err(|source| {
            ArtifactError::EntryNotUtf8 {
                path: entry.clone(),
                source,
            }
        })?;

        let resources: Vec<_> = by_name.into_values().collect();
        let digest = digest(&entry, &resources);
        Ok(Self {
            entry,
            resources,
            digest,
        })
    }

    /// Creates a closure containing one UTF-8 MJCF document named `model.xml`.
    pub fn from_xml(xml: impl AsRef<[u8]>) -> Result<Self, ArtifactError> {
        Self::new(
            "model.xml",
            [Resource::new("model.xml", xml.as_ref().to_vec())?],
        )
    }

    /// Reads one model directory into a closed resource closure.
    ///
    /// `root` is an explicit resource boundary.
    /// Every regular file below it is included, directory traversal is sorted
    /// by normalized path, and symlinks are refused so the closure cannot
    /// escape the selected root.
    pub fn from_directory(
        root: impl AsRef<Path>,
        entry: impl AsRef<Path>,
    ) -> Result<Self, ArtifactError> {
        Self::from_directory_with_limits(root, entry, ResourceLimits::default())
    }

    /// Reads one model directory with explicit closure admission limits.
    pub fn from_directory_with_limits(
        root: impl AsRef<Path>,
        entry: impl AsRef<Path>,
        limits: ResourceLimits,
    ) -> Result<Self, ArtifactError> {
        let root = root.as_ref();
        let root_metadata = fs::symlink_metadata(root).map_err(|source| ArtifactError::Io {
            path: root.to_owned(),
            source,
        })?;
        if root_metadata.file_type().is_symlink() {
            return Err(ArtifactError::InvalidResourceName(
                root.to_string_lossy().into_owned(),
            ));
        }
        let root = root.canonicalize().map_err(|source| ArtifactError::Io {
            path: root.to_owned(),
            source,
        })?;
        let root_metadata = fs::metadata(&root).map_err(|source| ArtifactError::Io {
            path: root.clone(),
            source,
        })?;
        if !root_metadata.is_dir() {
            return Err(ArtifactError::UnsupportedFileType(root));
        }
        let entry_path = entry.as_ref();
        let entry_name = normalized_relative_path(entry_path).map_err(|_| {
            ArtifactError::InvalidResourceName(entry_path.to_string_lossy().into_owned())
        })?;

        let mut files = Vec::new();
        let mut total_bytes = 0usize;
        collect_files(&root, &root, &mut files, limits, &mut total_bytes)?;
        let resources = files
            .into_iter()
            .map(|(name, path)| {
                let bytes = fs::read(&path).map_err(|source| ArtifactError::Io {
                    path: path.clone(),
                    source,
                })?;
                Resource::new(name, bytes)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Self::with_limits(entry_name, resources, limits)
    }

    /// Reads the parent directory of `path` as the explicit resource root.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, ArtifactError> {
        Self::from_file_with_limits(path, ResourceLimits::default())
    }

    /// Reads the parent resource root of `path` with explicit closure limits.
    pub fn from_file_with_limits(
        path: impl AsRef<Path>,
        limits: ResourceLimits,
    ) -> Result<Self, ArtifactError> {
        let path = path.as_ref();
        let file_name = path.file_name().ok_or_else(|| {
            ArtifactError::InvalidResourceName(path.to_string_lossy().into_owned())
        })?;
        let root = path.parent().unwrap_or_else(|| Path::new("."));
        Self::from_directory_with_limits(root, file_name, limits)
    }

    /// Returns the entry name passed to the native parser.
    #[must_use]
    pub fn entry(&self) -> &str {
        &self.entry
    }

    /// Returns resources in deterministic normalized-name order.
    pub fn resources(&self) -> impl ExactSizeIterator<Item = &Resource> {
        self.resources.iter()
    }

    /// Looks up one resource by normalized name.
    #[must_use]
    pub fn resource(&self, name: &str) -> Option<&Resource> {
        self.resources.iter().find(|resource| resource.name == name)
    }

    /// Returns the deterministic artifact digest.
    #[must_use]
    pub const fn digest(&self) -> [u8; 32] {
        self.digest
    }

    /// Returns the digest as lowercase hexadecimal.
    #[must_use]
    pub fn digest_hex(&self) -> String {
        self.digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

fn collect_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<(String, PathBuf)>,
    limits: ResourceLimits,
    total_bytes: &mut usize,
) -> Result<(), ArtifactError> {
    let mut entries = fs::read_dir(directory)
        .map_err(|source| ArtifactError::Io {
            path: directory.to_owned(),
            source,
        })?
        .map(|entry| {
            entry
                .map_err(|source| ArtifactError::Io {
                    path: directory.to_owned(),
                    source,
                })
                .and_then(|entry| {
                    let path = entry.path();
                    let file_type = entry.file_type().map_err(|source| ArtifactError::Io {
                        path: path.clone(),
                        source,
                    })?;
                    if file_type.is_symlink() {
                        return Err(ArtifactError::InvalidResourceName(
                            path.strip_prefix(root)
                                .unwrap_or(&path)
                                .to_string_lossy()
                                .into_owned(),
                        ));
                    }
                    let relative = path.strip_prefix(root).map_err(|_| {
                        ArtifactError::InvalidResourceName(path.to_string_lossy().into_owned())
                    })?;
                    let name = normalized_relative_path(relative).map_err(|_| {
                        ArtifactError::InvalidResourceName(relative.to_string_lossy().into_owned())
                    })?;
                    Ok((name, path, file_type))
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by(|left, right| left.0.cmp(&right.0));

    for (name, path, file_type) in entries {
        if file_type.is_dir() {
            collect_files(root, &path, files, limits, total_bytes)?;
        } else if !file_type.is_file() {
            return Err(ArtifactError::UnsupportedFileType(path));
        } else {
            let size = fs::metadata(&path)
                .map_err(|source| ArtifactError::Io {
                    path: path.clone(),
                    source,
                })?
                .len();
            let size = usize::try_from(size).unwrap_or(usize::MAX);
            if size > limits.max_resource_bytes {
                return Err(ArtifactError::ResourceTooLarge {
                    name,
                    actual: size,
                    limit: limits.max_resource_bytes,
                });
            }
            if files.len() >= limits.max_resources {
                return Err(ArtifactError::TooManyResources {
                    actual: files.len() + 1,
                    limit: limits.max_resources,
                });
            }
            *total_bytes = total_bytes
                .checked_add(size)
                .ok_or(ArtifactError::ClosureTooLarge {
                    actual: usize::MAX,
                    limit: limits.max_closure_bytes,
                })?;
            if *total_bytes > limits.max_closure_bytes {
                return Err(ArtifactError::ClosureTooLarge {
                    actual: *total_bytes,
                    limit: limits.max_closure_bytes,
                });
            }
            files.push((name, path));
        }
    }
    Ok(())
}

fn normalized_relative_path(path: &Path) -> Result<String, ()> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(());
    }
    if path.to_string_lossy().contains('\\') {
        return Err(());
    }
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                let part = part.to_str().ok_or(())?;
                if part.is_empty() || part.contains('\0') || part.contains('\\') {
                    return Err(());
                }
                parts.push(part);
            }
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => return Err(()),
        }
    }
    if parts.is_empty() {
        return Err(());
    }
    Ok(parts.join("/"))
}

fn validate_resource_name(name: &str) -> Result<(), ArtifactError> {
    if name.contains('\0') {
        return Err(ArtifactError::ResourceNameContainsNul(name.to_owned()));
    }
    normalized_relative_path(Path::new(name))
        .map(|_| ())
        .map_err(|_| ArtifactError::InvalidResourceName(name.to_owned()))
}

fn digest(entry: &str, resources: &[Resource]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    update_bytes(&mut hasher, entry.as_bytes());
    for resource in resources {
        update_bytes(&mut hasher, resource.name.as_bytes());
        update_bytes(&mut hasher, &resource.bytes);
    }
    hasher.finalize().into()
}

fn update_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const XML: &[u8] = br#"<mujoco><worldbody/></mujoco>"#;

    #[test]
    fn sorts_resources_and_computes_stable_digest() {
        let first = ClosedModel::new(
            "model.xml",
            [
                Resource::new("assets/floor.stl", b"floor".to_vec()).unwrap(),
                Resource::new("model.xml", XML.to_vec()).unwrap(),
            ],
        )
        .unwrap();
        let second = ClosedModel::new(
            "model.xml",
            [
                Resource::new("model.xml", XML.to_vec()).unwrap(),
                Resource::new("assets/floor.stl", b"floor".to_vec()).unwrap(),
            ],
        )
        .unwrap();

        assert_eq!(first, second);
        assert_eq!(
            first.resources().map(Resource::name).collect::<Vec<_>>(),
            ["assets/floor.stl", "model.xml",]
        );
        assert_eq!(first.digest_hex().len(), 64);
    }

    #[test]
    fn rejects_escape_and_duplicate_names() {
        assert!(matches!(
            Resource::new("../model.xml", XML),
            Err(ArtifactError::InvalidResourceName(_))
        ));
        assert!(matches!(
            Resource::new("assets\\mesh.stl", XML),
            Err(ArtifactError::InvalidResourceName(_))
        ));
        assert!(matches!(
            ClosedModel::new(
                "model.xml",
                [
                    Resource::new("model.xml", XML).unwrap(),
                    Resource::new("model.xml", XML).unwrap(),
                ]
            ),
            Err(ArtifactError::DuplicateResource(_))
        ));
    }

    #[test]
    fn rejects_missing_or_non_utf8_entry() {
        assert!(matches!(
            ClosedModel::new("", std::iter::empty::<Resource>()),
            Err(ArtifactError::EmptyEntry)
        ));
        assert!(matches!(
            ClosedModel::new("model.xml", [Resource::new("other.xml", XML).unwrap()]),
            Err(ArtifactError::EntryMissing(_))
        ));
        assert!(matches!(
            ClosedModel::new(
                "model.xml",
                [Resource::new("model.xml", vec![0xff]).unwrap()]
            ),
            Err(ArtifactError::EntryNotUtf8 { .. })
        ));
    }

    #[test]
    fn custom_limits_are_enforced_before_digesting() {
        let limits = ResourceLimits {
            max_resource_bytes: 2,
            max_closure_bytes: 2,
            max_resources: 1,
        };
        assert!(matches!(
            ClosedModel::with_limits(
                "model.xml",
                [Resource::new("model.xml", XML).unwrap()],
                limits,
            ),
            Err(ArtifactError::ResourceTooLarge { .. })
        ));
    }

    #[test]
    fn directory_closure_is_sorted_and_respects_limits_before_reading() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("assets")).unwrap();
        fs::write(root.path().join("model.xml"), XML).unwrap();
        fs::write(root.path().join("assets/floor.stl"), b"floor").unwrap();

        let first = ClosedModel::from_directory(root.path(), "model.xml").unwrap();
        let second = ClosedModel::from_file(root.path().join("model.xml")).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            first.resources().map(Resource::name).collect::<Vec<_>>(),
            ["assets/floor.stl", "model.xml"]
        );

        let limits = ResourceLimits {
            max_resource_bytes: 2,
            max_closure_bytes: 64,
            max_resources: 2,
        };
        assert!(matches!(
            ClosedModel::from_directory_with_limits(root.path(), "model.xml", limits),
            Err(ArtifactError::ResourceTooLarge { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn directory_closure_refuses_symlinks() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("model.xml"), XML).unwrap();
        std::os::unix::fs::symlink(root.path().join("model.xml"), root.path().join("alias.xml"))
            .unwrap();
        assert!(matches!(
            ClosedModel::from_directory(root.path(), "model.xml"),
            Err(ArtifactError::InvalidResourceName(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn directory_closure_refuses_a_symlinked_root() {
        let parent = tempfile::tempdir().unwrap();
        let actual = parent.path().join("actual");
        fs::create_dir(&actual).unwrap();
        fs::write(actual.join("model.xml"), XML).unwrap();
        let link = parent.path().join("link");
        std::os::unix::fs::symlink(&actual, &link).unwrap();

        assert!(matches!(
            ClosedModel::from_directory(&link, "model.xml"),
            Err(ArtifactError::InvalidResourceName(_))
        ));
    }
}
