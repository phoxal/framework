//! Reading a finalized runtime bundle.
//!
//! A finalized bundle is the one artifact both the supervisor and every
//! participant process read at runtime. It carries no compiled model document:
//! the canonical [`phoxal_model::Robot`] is built here, in memory, from the same
//! finalized documents the operator can inspect.
//!
//! ```text
//! <bundle>/
//!   robot.yaml                         finalized robot/v0: no `extends`, explicit `clock`
//!   assets/
//!     robot/structure.urdf             `robot.structure` resolves below `assets/`
//!     robot/meshes/...                 logical asset id `robot/meshes/...`
//!     components/<component-type>/
//!       component.yaml
//!       structure.urdf
//!       simulation.yaml                optional
//!       meshes/...                     logical asset id `components/<type>/meshes/...`
//!     router/config.json5              optional, `router.config` resolves below `assets/`
//!   bin/...                            participant executables, never participant-readable
//! ```
//!
//! Loading is pure: no source discovery, no Cargo or registry access, and no
//! mutation of the bundle.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use phoxal_model::{AssetId, ParticipantAssetResolver, Robot};

use crate::{CompileError, ParticipantDeclarations, ResolvedSources, referenced_asset_ids, source};

/// The finalized robot document, at the bundle root.
const ROBOT_FILE: &str = "robot.yaml";
/// The single root every participant-readable path lives below.
const ASSETS_DIR: &str = "assets";
/// The directory holding component definitions, keyed by component type.
const COMPONENTS_DIR: &str = "components";

/// A failure loading a finalized bundle.
#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    #[error("failed to resolve bundle root {}: {source}", path.display())]
    Root {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read finalized robot document {}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "finalized robot document {} still declares `extends`: a bundle carries one fully \
         resolved document",
        path.display()
    )]
    UnresolvedExtends { path: PathBuf },
    #[error("invalid finalized robot document {}: {source:#}", path.display())]
    Manifest {
        path: PathBuf,
        #[source]
        source: anyhow::Error,
    },
    #[error(transparent)]
    Compile(#[from] CompileError),
    #[error("failed to index bundle assets: {0}")]
    Assets(#[from] phoxal_model::AssetError),
    #[error("bundle is missing asset '{id}' referenced by the canonical model", id = id.as_str())]
    MissingAsset { id: AssetId },
    #[error("bundle contains forbidden symlink {}", path.display())]
    ForbiddenSymlink { path: PathBuf },
    #[error("bundle contains unsupported entry {}", path.display())]
    UnsupportedEntry { path: PathBuf },
    #[error(
        "bundle file {} changed after it was indexed: the index is the fence, so a path that no \
         longer matches what was fenced is refused rather than served",
        path.display()
    )]
    ChangedAfterIndex { path: PathBuf },
    #[error("bundle path {} is not valid UTF-8", path.display())]
    NotUtf8 { path: PathBuf },
}

/// One loaded finalized bundle.
#[derive(Debug)]
pub struct FinalizedBundle {
    robot: Robot,
    participants: ParticipantDeclarations,
    assets: ParticipantAssetResolver,
    router_config: Option<PathBuf>,
}

impl FinalizedBundle {
    /// Load the canonical model, participant declarations, and participant
    /// asset fence from a finalized bundle root.
    pub fn load(root: impl AsRef<Path>) -> Result<Self, BundleError> {
        let root = root.as_ref();
        let root = root.canonicalize().map_err(|source| BundleError::Root {
            path: root.to_path_buf(),
            source,
        })?;
        let robot_manifest = root.join(ROBOT_FILE);
        let assets_root = root.join(ASSETS_DIR);

        let text =
            std::fs::read_to_string(&robot_manifest).map_err(|source| BundleError::Read {
                path: robot_manifest.clone(),
                source,
            })?;
        if declares_extends(&text) {
            return Err(BundleError::UnresolvedExtends {
                path: robot_manifest,
            });
        }
        let source::robot::Manifest::V0(manifest) = source::robot::parse_from_string(&text)
            .map_err(|source| BundleError::Manifest {
                path: robot_manifest.clone(),
                source,
            })?;
        manifest
            .validate()
            .map_err(|errors| BundleError::Manifest {
                path: robot_manifest.clone(),
                source: source::robot::validation_error(
                    &robot_manifest.display().to_string(),
                    errors,
                ),
            })?;

        let resolved = ResolvedSources {
            component_roots: manifest
                .used_component_types()
                .into_iter()
                .map(|component_type| {
                    (
                        component_type.to_string(),
                        assets_root.join(COMPONENTS_DIR).join(component_type),
                    )
                })
                .collect(),
            robot_manifest,
            robot_root: assets_root.clone(),
        };
        let (robot, participants) = resolved.compile_model(&manifest)?;

        let assets = ParticipantAssetResolver::discover(assets_root)?;
        let declared = assets
            .ids()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        for id in referenced_asset_ids(&robot).map_err(|source| CompileError::CanonicalModel {
            path: resolved.robot_manifest.clone(),
            source,
        })? {
            if !declared.contains(&id) {
                return Err(BundleError::MissingAsset { id });
            }
        }

        let router_config = manifest
            .router
            .config
            .as_ref()
            .map(|relative| resolved.robot_relative(relative))
            .transpose()?;

        Ok(Self {
            robot,
            participants,
            assets,
            router_config,
        })
    }

    /// The canonical runtime model this bundle describes.
    #[must_use]
    pub fn robot(&self) -> &Robot {
        &self.robot
    }

    /// Take ownership of the canonical model, leaving the resolvers behind.
    #[must_use]
    pub fn into_robot(self) -> Robot {
        self.robot
    }

    /// The participants this bundle declares.
    #[must_use]
    pub fn participants(&self) -> &ParticipantDeclarations {
        &self.participants
    }

    /// The participant-facing asset fence. This is the only bundle access a
    /// participant is ever handed.
    #[must_use]
    pub fn assets(&self) -> &ParticipantAssetResolver {
        &self.assets
    }

    /// Split into the two values a participant runner binds.
    #[must_use]
    pub fn into_participant_inputs(self) -> (Robot, ParticipantAssetResolver) {
        (self.robot, self.assets)
    }

    /// The Zenoh router configuration this bundle carries, when it declares one.
    #[must_use]
    pub fn router_config(&self) -> Option<&Path> {
        self.router_config.as_deref()
    }
}

/// Whether a finalized document still carries a non-empty `extends`.
///
/// This runs before the typed parse so the diagnostic names the real problem:
/// the DTO's own `extends` rejection would otherwise report it as a parser
/// misuse.
fn declares_extends(text: &str) -> bool {
    let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(text) else {
        return false;
    };
    value
        .get("extends")
        .and_then(serde_yaml::Value::as_sequence)
        .is_some_and(|parents| !parents.is_empty())
}

/// Why a requested bundle path was refused before any filesystem access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum InvalidBundlePath {
    #[error("bundle path is empty")]
    Empty,
    #[error("bundle path is absolute")]
    Absolute,
    #[error("bundle path traverses out of the bundle")]
    Traversal,
    #[error("bundle path is not normalized")]
    NotNormalized,
}

/// A validated, normalized forward-slash path inside a bundle.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BundlePath(String);

impl BundlePath {
    /// Validate a requested path. Nothing is touched on disk here.
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidBundlePath> {
        let value = value.into();
        if value.is_empty() {
            return Err(InvalidBundlePath::Empty);
        }
        if value.starts_with('/') {
            return Err(InvalidBundlePath::Absolute);
        }
        for segment in value.split('/') {
            match segment {
                ".." => return Err(InvalidBundlePath::Traversal),
                "" | "." => return Err(InvalidBundlePath::NotNormalized),
                _ if segment.contains('\\') => return Err(InvalidBundlePath::NotNormalized),
                _ => {}
            }
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The outcome of one bundle file request.
///
/// Everything an operator can legitimately ask for - including asking for
/// something that is not there - is a variant here, not an error.
#[derive(Debug)]
#[non_exhaustive]
pub enum BundleFile {
    /// The indexed regular file, read in full.
    Found { path: BundlePath, bytes: Vec<u8> },
    /// No regular file is indexed at that path (a directory included).
    Missing,
    /// The request never named a servable path.
    InvalidPath(InvalidBundlePath),
    /// The file exists but exceeds the resolver's response limit.
    TooLarge { size_bytes: u64, limit_bytes: u64 },
}

/// A safe read-only index of every regular file in a finalized bundle.
///
/// This is the supervisor-only counterpart to
/// [`ParticipantAssetResolver`]: it serves the whole bundle, `bin/`
/// included, for the remote `bundle/get` capability. Participants never get one.
///
/// Indexing at construction is the fence: symlinks are refused outright, so no
/// indexed path can leave the canonical bundle root, and a request can only ever
/// select an already-validated entry.
#[derive(Clone, Debug)]
pub struct BundleResolver {
    files: BTreeMap<BundlePath, IndexedFile>,
    max_response_bytes: u64,
}

#[derive(Clone, Debug)]
struct IndexedFile {
    path: PathBuf,
    size_bytes: u64,
}

impl BundleResolver {
    /// Walk `root` once and index every regular file below it.
    pub fn index(root: impl AsRef<Path>, max_response_bytes: u64) -> Result<Self, BundleError> {
        let root = root.as_ref();
        let root = root.canonicalize().map_err(|source| BundleError::Root {
            path: root.to_path_buf(),
            source,
        })?;
        let mut files = BTreeMap::new();
        index_directory(&root, &root, &mut files)?;
        Ok(Self {
            files,
            max_response_bytes,
        })
    }

    /// Read one indexed file.
    ///
    /// Indexing established the fence - a regular file, of a known size, below
    /// the canonical root - but the read happens later, so the fence is
    /// re-checked here against the stored path before any bytes are read. An
    /// entry that has since become a symlink or a directory, or that no longer
    /// has the size it was indexed at, is refused as a hard failure: it is not
    /// something an operator can legitimately ask for, so it is not an outcome.
    pub fn get(&self, request: &str) -> Result<BundleFile, BundleError> {
        let path = match BundlePath::new(request) {
            Ok(path) => path,
            Err(reason) => return Ok(BundleFile::InvalidPath(reason)),
        };
        let Some(file) = self.files.get(&path) else {
            return Ok(BundleFile::Missing);
        };
        if file.size_bytes > self.max_response_bytes {
            return Ok(BundleFile::TooLarge {
                size_bytes: file.size_bytes,
                limit_bytes: self.max_response_bytes,
            });
        }
        let metadata =
            std::fs::symlink_metadata(&file.path).map_err(|source| BundleError::Read {
                path: file.path.clone(),
                source,
            })?;
        if !metadata.file_type().is_file() || metadata.len() != file.size_bytes {
            return Err(BundleError::ChangedAfterIndex {
                path: file.path.clone(),
            });
        }
        let bytes = std::fs::read(&file.path).map_err(|source| BundleError::Read {
            path: file.path.clone(),
            source,
        })?;
        Ok(BundleFile::Found { path, bytes })
    }

    /// Every servable path in this bundle.
    pub fn paths(&self) -> impl ExactSizeIterator<Item = &BundlePath> {
        self.files.keys()
    }
}

fn index_directory(
    root: &Path,
    directory: &Path,
    files: &mut BTreeMap<BundlePath, IndexedFile>,
) -> Result<(), BundleError> {
    let entries = std::fs::read_dir(directory)
        .and_then(std::iter::Iterator::collect::<std::io::Result<Vec<_>>>)
        .map_err(|source| BundleError::Read {
            path: directory.to_path_buf(),
            source,
        })?;
    for entry in entries {
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(|source| BundleError::Read {
            path: path.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink() {
            return Err(BundleError::ForbiddenSymlink { path });
        }
        if metadata.is_dir() {
            index_directory(root, &path, files)?;
            continue;
        }
        if !metadata.is_file() {
            return Err(BundleError::UnsupportedEntry { path });
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| BundleError::UnsupportedEntry { path: path.clone() })?;
        let mut segments = Vec::new();
        for component in relative.components() {
            let Component::Normal(segment) = component else {
                return Err(BundleError::UnsupportedEntry { path: path.clone() });
            };
            segments.push(
                segment
                    .to_str()
                    .ok_or_else(|| BundleError::NotUtf8 { path: path.clone() })?,
            );
        }
        let logical = BundlePath::new(segments.join("/"))
            .map_err(|_| BundleError::UnsupportedEntry { path: path.clone() })?;
        files.insert(
            logical,
            IndexedFile {
                path,
                size_bytes: metadata.len(),
            },
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle_with(files: &[(&str, &[u8])]) -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("temp bundle");
        for (relative, bytes) in files {
            let path = root.path().join(relative);
            std::fs::create_dir_all(path.parent().expect("a parent directory")).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        root
    }

    #[test]
    fn requested_paths_are_validated_before_any_lookup() {
        for (request, reason) in [
            ("", InvalidBundlePath::Empty),
            ("/etc/passwd", InvalidBundlePath::Absolute),
            ("bin/../../secret", InvalidBundlePath::Traversal),
            ("..", InvalidBundlePath::Traversal),
            ("bin//brain", InvalidBundlePath::NotNormalized),
            ("./robot.yaml", InvalidBundlePath::NotNormalized),
            ("bin\\brain", InvalidBundlePath::NotNormalized),
        ] {
            assert_eq!(BundlePath::new(request), Err(reason), "{request}");
        }
        assert_eq!(
            BundlePath::new("assets/robot/structure.urdf")
                .unwrap()
                .as_str(),
            "assets/robot/structure.urdf"
        );
    }

    #[test]
    fn the_whole_bundle_is_servable_including_binaries() {
        let root = bundle_with(&[
            ("robot.yaml", b"schema: robot/v0"),
            ("assets/robot/structure.urdf", b"<robot/>"),
            ("bin/brain", b"ELF"),
        ]);
        let resolver = BundleResolver::index(root.path(), 1024).expect("index succeeds");

        assert_eq!(
            resolver.paths().map(BundlePath::as_str).collect::<Vec<_>>(),
            ["assets/robot/structure.urdf", "bin/brain", "robot.yaml"]
        );
        let BundleFile::Found { bytes, .. } = resolver.get("bin/brain").expect("a readable file")
        else {
            panic!("bin/ is servable through the supervisor resolver");
        };
        assert_eq!(bytes, b"ELF");
    }

    #[test]
    fn expected_refusals_are_outcomes_rather_than_errors() {
        let root = bundle_with(&[("robot.yaml", b"schema: robot/v0"), ("bin/brain", b"ELF")]);
        let resolver = BundleResolver::index(root.path(), 2).expect("index succeeds");

        assert!(matches!(
            resolver.get("missing.txt").unwrap(),
            BundleFile::Missing
        ));
        // A directory is not a regular file, so it was never indexed.
        assert!(matches!(resolver.get("bin").unwrap(), BundleFile::Missing));
        assert!(matches!(
            resolver.get("../outside").unwrap(),
            BundleFile::InvalidPath(InvalidBundlePath::Traversal)
        ));
        assert!(matches!(
            resolver.get("/etc/passwd").unwrap(),
            BundleFile::InvalidPath(InvalidBundlePath::Absolute)
        ));
        assert!(matches!(
            resolver.get("bin/brain").unwrap(),
            BundleFile::TooLarge {
                size_bytes: 3,
                limit_bytes: 2
            }
        ));
    }

    #[test]
    fn a_file_that_changed_after_indexing_is_refused_rather_than_served() {
        let root = bundle_with(&[("bin/brain", b"ELF")]);
        let resolver = BundleResolver::index(root.path(), 1024).expect("index succeeds");
        let indexed = root.path().join("bin/brain");

        // Growing past the indexed size defeats the size bound the index
        // established, so the read must not happen at all.
        std::fs::write(&indexed, b"ELF and a great deal more").unwrap();
        let error = resolver
            .get("bin/brain")
            .expect_err("a changed file must fail");
        assert!(
            matches!(error, BundleError::ChangedAfterIndex { .. }),
            "{error}"
        );

        // Swapping the entry for a symlink defeats the symlink fence the same
        // way, even when the target happens to be the same size.
        std::fs::remove_file(&indexed).unwrap();
        let outside = root.path().join("outside.txt");
        std::fs::write(&outside, b"ELF").unwrap();
        std::os::unix::fs::symlink(&outside, &indexed).unwrap();
        let error = resolver
            .get("bin/brain")
            .expect_err("a swapped-in symlink must fail");
        assert!(
            matches!(error, BundleError::ChangedAfterIndex { .. }),
            "{error}"
        );
    }

    #[test]
    fn a_symlink_anywhere_in_the_bundle_refuses_the_whole_index() {
        let root = bundle_with(&[("robot.yaml", b"schema: robot/v0")]);
        let outside = root.path().join("outside.txt");
        std::fs::write(&outside, b"outside").unwrap();
        std::os::unix::fs::symlink(&outside, root.path().join("bin-link")).unwrap();

        let error = BundleResolver::index(root.path(), 1024).expect_err("a symlink must fail");
        assert!(
            matches!(error, BundleError::ForbiddenSymlink { .. }),
            "{error}"
        );
    }
}
