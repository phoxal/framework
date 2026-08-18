//! Authored manifest readers and deterministic source-to-canonical compilation.
//!
//! Authored documents are written in a *source language*, and the `schema:` tag
//! at the head of one names which generation of that language it is written in.
//! A schema tag is never a framework compatibility identity: it negotiates
//! nothing between binaries, no runtime reads it, and `FrameworkVersion`
//! remains the only negotiated compatibility identity.
//!
//! A generation's syntax ends at one boundary. [`source`] owns the versioned
//! DTOs and their rules; each one normalizes into `normalized`, the
//! version-independent form; and the compiler in this module reads only that.
//! So a new source generation is a new DTO plus a new `normalize` - never a
//! second copy of the compiler.
//!
//! [`SourceSet::compile`] takes the official service set as an argument. Which
//! services are official is a build-tooling fact that changes with the packages
//! the CLI resolves, so the framework holds no list of its own; the caller's set
//! is merged with the authored `services:` map, and an authored entry wins.
//! Where the compiled robot is written - `manifest.json` beside `assets/` and
//! `bin/` - is [`crate::bundle`]. Runtime processes compile none of this.
//!
//! # What a release owes an author
//!
//! This is not a wire surface - no two binaries negotiate over a `robot.yaml` -
//! so the contract-surface comparison cannot see a grammar change here at all.
//! The promise it does carry is directional: a newer compatible framework keeps
//! reading every document an older compatible framework accepted, with the same
//! meaning, and is free to accept more.
//!
//! `cargo xtask compatibility report` gates that. It compiles a corpus of the
//! repository's authored projects through both this reader and the published
//! one, and calls a document that stopped compiling, or that compiles to a
//! different canonical model, source-breaking. The remedy lives in the versioned
//! DTO's `normalize`, which is the only place a generation's syntax and defaults
//! are owned; see `xtask/README.md` rule 8.
//!
//! Both readers are asked through [`probe`], the one entry that exists for that
//! checker. It is deliberately tiny and **stays source-compatible across
//! trains**, because the checker compiles a single program against two crate
//! sets at once: if the entry moved, no one program would compile on both sides
//! and the whole leg would go quiet.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::model::AssetId;
use crate::model::compiler::RobotParts;
use crate::model::identity::{
    CapabilityId, ComponentInstanceId, ComponentTypeId, LinkId, RobotId, ServiceId,
};

use source::SourceError;

pub mod build_requirements;
pub mod schema;
pub mod source;

mod normalized;

// A second source generation, proved end to end without shipping one.
#[cfg(test)]
mod source_generation_proof;

// The authored URDF DTO stays private: `crate::model` owns the normalized
// structure a caller is meant to read. Only the failure vocabulary is public,
// because it appears inside `CompileError` and a caller has to be able to name
// what it matched.
mod urdf_dto;
pub use urdf_dto::{JointEnd, StructuralKind, UrdfError};

/// Exact authored inputs after package/workspace component resolution.
#[derive(Debug, Clone)]
pub struct SourceSet {
    pub project_root: PathBuf,
    pub robot_manifest: PathBuf,
    pub component_roots: BTreeMap<String, PathBuf>,
}

/// Where one robot's documents actually live, for authored projects and
/// finalized bundles alike.
///
/// `robot_root` is the base for the manifest's `robot.structure` path; in an
/// authored project it is the project root, and in a finalized bundle it is
/// `<bundle>/assets`.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedSources {
    robot_manifest: PathBuf,
    robot_root: PathBuf,
    component_roots: BTreeMap<String, PathBuf>,
}

/// Source compiler failure, classified by the stage that owns the invariant.
#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    /// A path the caller supplied does not resolve.
    #[error("failed to resolve compiler input {}: {source}", path.display())]
    Input {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The caller resolved no root for a component type the robot mounts.
    #[error("no resolved component root for type '{component_type}'")]
    UnresolvedComponentRoot { component_type: String },

    /// An authored document failed to load.
    #[error("failed to compile {} document: {source}", source.kind())]
    Document {
        #[source]
        source: SourceError,
    },

    /// A component type's documents failed to load.
    #[error("failed to compile component type '{component_type}' at {}: {source}", root.display())]
    Component {
        component_type: String,
        root: PathBuf,
        #[source]
        source: Box<CompileError>,
    },

    /// A URDF structure document is not a usable structure.
    #[error("failed to compile structure document {}: {source}", path.display())]
    Structure {
        path: PathBuf,
        #[source]
        source: UrdfError,
    },

    /// A component instance references a component type the robot never loads.
    #[error("component instance '{instance}' references unresolved type '{component_type}'")]
    UnknownComponentType {
        instance: String,
        component_type: String,
    },

    /// Instance parameters name a capability the component type never declares.
    #[error(
        "component instance '{instance}' parameters reference unknown capability \
         '{capability_id}'"
    )]
    UnknownCapability {
        instance: String,
        capability_id: String,
    },

    /// Instance roles name a capability the resolved component document does
    /// not declare.
    #[error(
        "component instance '{instance}' role assignments reference unknown capability \
         '{capability_id}'"
    )]
    UnknownRoleCapability {
        instance: String,
        capability_id: String,
    },

    /// Instance parameters claim a different capability kind than the component
    /// type declares.
    #[error(
        "component instance '{instance}' parameter '{capability_id}' kind '{authored}' does not \
         match '{declared}'"
    )]
    CapabilityKindMismatch {
        instance: String,
        capability_id: String,
        authored: crate::model::component::capability::CapabilityKind,
        declared: crate::model::component::capability::CapabilityKind,
    },

    /// An authored value and its canonical counterpart disagree on their
    /// shared wire shape. This is a defect in this crate, not in the document:
    /// the two are serde-compatible by construction.
    #[error("failed to normalize authored {authored} into its canonical form: {source}")]
    Transcode {
        authored: &'static str,
        #[source]
        source: serde_json::Error,
    },

    /// The normalized inputs do not assemble into a valid canonical robot.
    #[error("failed to construct canonical robot from {}: {source}", path.display())]
    CanonicalModel {
        path: PathBuf,
        #[source]
        source: crate::model::ModelError,
    },

    /// A runtime asset tree could not be read.
    #[error("failed to compile runtime assets below {}: {source}", root.display())]
    Assets {
        root: PathBuf,
        #[source]
        source: AssetError,
    },

    /// An authored driver block is not representable as the JSON configuration
    /// the manifest hands to a driver process. This is a defect in this crate,
    /// not in the document: the block is serde-serializable by construction.
    #[error("failed to normalize the driver block of component instance '{instance}': {source}")]
    DriverConfig {
        instance: String,
        #[source]
        source: serde_json::Error,
    },
}

/// Why a runtime asset tree is not compilable.
#[derive(Debug, thiserror::Error)]
pub enum AssetError {
    #[error("failed to read {}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A symlink can point outside the source tree, so the whole tree is
    /// refused rather than the link being followed or silently skipped.
    #[error("asset source tree contains forbidden symlink {}", path.display())]
    ForbiddenSymlink { path: PathBuf },

    #[error("unsupported asset source entry {}", path.display())]
    UnsupportedEntry { path: PathBuf },

    #[error("asset source entry name is not UTF-8: {}", path.display())]
    NotUtf8 { path: PathBuf },

    #[error("invalid logical asset id: {source}")]
    Id {
        #[source]
        source: crate::model::ModelError,
    },

    #[error("duplicate compiled asset '{id}'", id = id.as_str())]
    Duplicate { id: AssetId },

    #[error("canonical model references missing compiled asset '{id}'", id = id.as_str())]
    Missing { id: AssetId },
}

/// The complete source compilation result handed to build tooling.
///
/// There is nothing here beside the robot and its assets. What used to be a
/// parallel service list and driver list is inside the robot: a service is a
/// `services` entry with its configuration, and a driver is a `components` entry
/// whose `driver` block is present. Build tooling reads the process set off the
/// robot rather than off a second list that could disagree with it.
#[derive(Debug, Clone)]
pub struct CompiledProject {
    robot: crate::model::Robot,
    assets: CompiledAssets,
}

/// Deterministic compiled runtime assets.
#[derive(Debug, Clone, Default)]
pub struct CompiledAssets(BTreeMap<AssetId, Vec<u8>>);

impl SourceSet {
    /// Compile authored YAML/URDF sources after the caller resolved component
    /// roots.
    ///
    /// `official_services` is the set of framework-owned services this robot
    /// runs. The framework does not know that set: which services are official
    /// is a build-tooling fact that changes with the packages the CLI resolves,
    /// so hardcoding it here would put the CLI's catalogue inside the compiler.
    /// The caller supplies it, and it is merged with the authored `services:`
    /// map - an official service the document also configures keeps the authored
    /// configuration.
    ///
    /// # Errors
    ///
    /// Returns the first [`CompileError`] the sources violate, named by the
    /// stage that owns the invariant.
    pub fn compile(
        self,
        official_services: impl IntoIterator<Item = ServiceId>,
    ) -> Result<CompiledProject, CompileError> {
        let project_root = canonicalize(&self.project_root)?;
        let robot_manifest = canonicalize(&self.robot_manifest)?;
        // The authored generation ends here: everything past this line reads
        // the normalized robot and cannot tell which schema tag produced it.
        let manifest = source::robot::Manifest::load(&robot_manifest)
            .map_err(|source| CompileError::Document { source })?
            .normalize()?;

        let resolved = ResolvedSources {
            robot_manifest,
            robot_root: project_root.clone(),
            component_roots: self.component_roots,
        };
        let robot = resolved.compile_model(&manifest, official_services)?;
        let assets = resolved.compile_assets(&project_root, &manifest, &robot)?;

        Ok(CompiledProject { robot, assets })
    }
}

/// What this reader makes of one authored project, as one JSON document.
///
/// This is the stable entry point `cargo xtask compatibility` reads the
/// authored-source leg through. That leg compiles **one** probe program against
/// two crate sets - the published train and this workspace - and a difference
/// in the answer is only a difference in the reader if the two were asked the
/// same question in the same words. So the program may name nothing but this
/// function, and this function **must stay source-compatible across trains**:
/// renaming it, changing its parameters, or reshaping the document it returns
/// changes the checker's instrument rather than the compiler it measures. A
/// train that has to move it is re-baselining the source leg, and the checker
/// reports the older side as having no probe entry until the moved one is
/// itself published.
///
/// The document is `{"accepted": true, "canonical": ...}` when the project
/// compiles and `{"accepted": false, "error": "..."}` when it does not. The
/// canonical half is the persisted manifest document plus the identity and byte
/// length of every compiled asset: what the comparison is about is which assets
/// the compiler decided the model references, not the contents of a mesh.
///
/// `official_services` is the caller's official service set, exactly as
/// [`SourceSet::compile`] takes it.
#[must_use]
pub fn probe(sources: SourceSet, official_services: &[ServiceId]) -> serde_json::Value {
    let rejected = |error: String| serde_json::json!({"accepted": false, "error": error});
    let compiled = match sources.compile(official_services.iter().cloned()) {
        Ok(compiled) => compiled,
        Err(error) => return rejected(error.to_string()),
    };
    let (document, assets) = compiled.into_document();
    let assets = assets
        .iter()
        .map(|(id, bytes)| serde_json::json!({"bytes": bytes.len(), "id": id.as_str()}))
        .collect::<Vec<_>>();
    // A compiled document is serde-serializable by construction, so this arm is
    // unreachable in practice. It is written as a refusal rather than a panic
    // because the probe's whole job is to answer, and a checker that saw a
    // crash could not tell a defect here from a broken build.
    match serde_json::to_value(&document) {
        Ok(robot) => serde_json::json!({
            "accepted": true,
            "canonical": {"assets": assets, "robot": robot},
        }),
        Err(error) => rejected(format!(
            "failed to render the compiled manifest document: {error}"
        )),
    }
}

/// Every logical asset the canonical model actually references.
pub(crate) fn referenced_asset_ids(robot: &crate::model::Robot) -> BTreeSet<AssetId> {
    let mut ids = robot
        .structure()
        .asset_ids()
        .cloned()
        .collect::<BTreeSet<_>>();
    for component in robot.components() {
        ids.extend(component.component_type().structure().asset_ids().cloned());
    }
    ids
}

fn canonicalize(path: &Path) -> Result<PathBuf, CompileError> {
    path.canonicalize().map_err(|source| CompileError::Input {
        path: path.to_path_buf(),
        source,
    })
}

impl ResolvedSources {
    fn component_root(&self, component_type: &str) -> Result<&PathBuf, CompileError> {
        self.component_roots.get(component_type).ok_or_else(|| {
            CompileError::UnresolvedComponentRoot {
                component_type: component_type.to_string(),
            }
        })
    }

    /// Build the canonical model from documents that are already resolved on
    /// disk.
    fn compile_model(
        &self,
        manifest: &normalized::Robot,
        official_services: impl IntoIterator<Item = ServiceId>,
    ) -> Result<crate::model::Robot, CompileError> {
        let mut component_types = BTreeMap::new();
        for component_type in manifest.used_component_types() {
            let root = self.component_root(component_type)?.clone();
            let component =
                self.compile_component_type(component_type, &root)
                    .map_err(|source| CompileError::Component {
                        component_type: component_type.to_string(),
                        root,
                        source: Box::new(source),
                    })?;
            component_types.insert(
                self.identity(ComponentTypeId::new(component_type))?,
                component,
            );
        }

        let mut components = BTreeMap::new();
        for (id, authored) in &manifest.instances {
            let component = component_types
                .get(authored.component_type.as_str())
                .ok_or_else(|| CompileError::UnknownComponentType {
                    instance: id.clone(),
                    component_type: authored.component_type.clone(),
                })?;
            let mut direction_signs = BTreeMap::new();
            let mut roles = BTreeMap::new();
            for (capability_id, authored_roles) in &authored.roles {
                if component.capability(capability_id).is_none() {
                    return Err(CompileError::UnknownRoleCapability {
                        instance: id.clone(),
                        capability_id: capability_id.clone(),
                    });
                }
                let canonical_id = self.identity(CapabilityId::new(capability_id))?;
                roles.insert(canonical_id, authored_roles.clone());
            }
            for (capability_id, parameters) in &authored.parameters {
                let declared = component.capability(capability_id).ok_or_else(|| {
                    CompileError::UnknownCapability {
                        instance: id.clone(),
                        capability_id: capability_id.clone(),
                    }
                })?;
                if declared.kind() != parameters.kind {
                    return Err(CompileError::CapabilityKindMismatch {
                        instance: id.clone(),
                        capability_id: capability_id.clone(),
                        authored: parameters.kind,
                        declared: declared.kind(),
                    });
                }
                direction_signs.insert(
                    self.identity(CapabilityId::new(capability_id))?,
                    parameters.direction_sign,
                );
            }
            // The driver block is kept in every mode. Whether a hardware driver
            // is started is a launch decision the CLI makes; the bundle always
            // says how the component would be driven.
            let driver = authored
                .driver
                .as_ref()
                .map(serde_json::to_value)
                .transpose()
                .map_err(|source| CompileError::DriverConfig {
                    instance: id.clone(),
                    source,
                })?;
            components.insert(
                self.identity(ComponentInstanceId::new(id))?,
                crate::model::compiler::component_instance(
                    self.identity(ComponentTypeId::new(&authored.component_type))?,
                    LinkId::new(&authored.mount_link),
                    direction_signs,
                    roles,
                    driver,
                ),
            );
        }

        let structure_path = self.robot_relative(&manifest.structure)?;
        let structure = urdf_dto::Structure::load(&structure_path)
            .and_then(|structure| structure.into_canonical(None))
            .map_err(|source| CompileError::Structure {
                path: structure_path,
                source,
            })?;

        crate::model::compiler::robot(RobotParts {
            id: self.identity(RobotId::new(&manifest.id))?,
            kinematic: manifest.kinematic.clone(),
            motion_limits: manifest.motion_limits,
            services: self.compile_services(manifest, official_services)?,
            components,
            component_types,
            structure,
        })
        .map_err(|source| CompileError::CanonicalModel {
            path: self.robot_manifest.clone(),
            source,
        })
    }

    /// Every service this robot runs: the caller's official set, then the
    /// authored map on top of it.
    ///
    /// The authored entry wins, because an author who configures an official
    /// service is configuring the one that runs, not declaring a second.
    ///
    /// The authored map cannot contain the reserved `brain` identity here: every
    /// entry into this compiler validates the authored document first, and the
    /// document's own rules own that rejection.
    fn compile_services(
        &self,
        manifest: &normalized::Robot,
        official_services: impl IntoIterator<Item = ServiceId>,
    ) -> Result<BTreeMap<ServiceId, crate::model::robot::Service>, CompileError> {
        let mut services = official_services
            .into_iter()
            .map(|id| (id, crate::model::compiler::service(None)))
            .collect::<BTreeMap<_, _>>();
        for (id, config) in &manifest.services {
            services.insert(
                self.identity(ServiceId::new(id))?,
                crate::model::compiler::service(config.clone()),
            );
        }
        Ok(services)
    }

    /// Load and normalize one component type's documents.
    fn compile_component_type(
        &self,
        component_type: &str,
        configured_root: &Path,
    ) -> Result<crate::model::component::Component, CompileError> {
        let root = canonicalize(configured_root)?;
        let authored = source::component::Manifest::load(&root)
            .map_err(|source| CompileError::Document { source })?
            .normalize(component_type, &root)?;

        let structure_path = root.join("structure.urdf");
        let structure = urdf_dto::Structure::load(&structure_path)
            .and_then(|structure| structure.into_canonical_fragment(component_type))
            .map_err(|source| CompileError::Structure {
                path: structure_path,
                source,
            })?;
        let simulation_path = root.join("simulation.yaml");
        let simulation = if simulation_path.is_file() {
            let simulation = source::simulation::Manifest::load(&simulation_path)
                .map_err(|source| CompileError::Document { source })?
                .normalize()?;
            Some(crate::model::compiler::simulation(
                simulation.capabilities,
                simulation.links,
            ))
        } else {
            None
        };
        Ok(crate::model::compiler::component(
            authored.capabilities,
            structure,
            simulation,
        ))
    }

    /// Attribute an identifier rejection to the document that carried it.
    fn identity<T, E>(&self, result: Result<T, E>) -> Result<T, CompileError>
    where
        E: Into<crate::model::ModelError>,
    {
        result.map_err(|source| CompileError::CanonicalModel {
            path: self.robot_manifest.clone(),
            source: source.into(),
        })
    }

    /// Resolve a manifest-declared path against the robot root.
    fn robot_relative(&self, relative: &Path) -> Result<PathBuf, CompileError> {
        canonicalize(&self.robot_root.join(relative))
    }

    /// Stage every mesh tree the project owns, then prove the canonical model
    /// references nothing that was not staged.
    fn compile_assets(
        &self,
        project_root: &Path,
        manifest: &normalized::Robot,
        robot: &crate::model::Robot,
    ) -> Result<CompiledAssets, CompileError> {
        let mut assets = CompiledAssets::default();
        let mut collect = |root: &Path, staged: &str| -> Result<(), CompileError> {
            collect_files(&root.join("meshes"), staged, &mut assets).map_err(|source| {
                CompileError::Assets {
                    root: root.to_path_buf(),
                    source,
                }
            })
        };
        collect(project_root, "robot/meshes")?;
        for component_type in manifest.used_component_types() {
            let root = self.component_root(component_type)?.clone();
            collect(&root, &format!("components/{component_type}/meshes"))?;
        }
        for id in referenced_asset_ids(robot) {
            if !assets.0.contains_key(&id) {
                return Err(CompileError::Assets {
                    root: project_root.to_path_buf(),
                    source: AssetError::Missing { id },
                });
            }
        }
        Ok(assets)
    }
}

impl CompiledProject {
    #[must_use]
    pub const fn robot(&self) -> &crate::model::Robot {
        &self.robot
    }

    #[must_use]
    pub const fn assets(&self) -> &CompiledAssets {
        &self.assets
    }

    /// The document this project is persisted as.
    #[must_use]
    pub fn into_document(self) -> (crate::model::ManifestDocument, CompiledAssets) {
        (crate::model::ManifestDocument::new(self.robot), self.assets)
    }

    #[must_use]
    pub fn into_parts(self) -> (crate::model::Robot, CompiledAssets) {
        (self.robot, self.assets)
    }
}

impl CompiledAssets {
    fn insert(&mut self, id: AssetId, bytes: Vec<u8>) -> Result<(), AssetError> {
        if self.0.insert(id.clone(), bytes).is_some() {
            return Err(AssetError::Duplicate { id });
        }
        Ok(())
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&AssetId, &[u8])> {
        self.0.iter().map(|(id, bytes)| (id, bytes.as_slice()))
    }

    #[must_use]
    pub fn into_map(self) -> BTreeMap<AssetId, Vec<u8>> {
        self.0
    }
}

fn collect_files(
    source_root: &Path,
    staged_root: &str,
    output: &mut CompiledAssets,
) -> Result<(), AssetError> {
    if !source_root.is_dir() {
        return Ok(());
    }
    let read = |path: &Path| {
        std::fs::read_dir(path)
            .and_then(std::iter::Iterator::collect::<std::io::Result<Vec<_>>>)
            .map_err(|source| AssetError::Read {
                path: path.to_path_buf(),
                source,
            })
    };
    let mut entries = read(source_root)?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let source = entry.path();
        let metadata = std::fs::symlink_metadata(&source).map_err(|error| AssetError::Read {
            path: source.clone(),
            source: error,
        })?;
        if metadata.file_type().is_symlink() {
            return Err(AssetError::ForbiddenSymlink { path: source });
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            return Err(AssetError::NotUtf8 { path: source });
        };
        let staged = format!("{staged_root}/{name}");
        if metadata.is_dir() {
            collect_files(&source, &staged, output)?;
        } else if metadata.is_file() {
            let id = AssetId::new(staged).map_err(|source| AssetError::Id { source })?;
            let bytes = std::fs::read(&source).map_err(|error| AssetError::Read {
                path: source.clone(),
                source: error,
            })?;
            output.insert(id, bytes)?;
        } else {
            return Err(AssetError::UnsupportedEntry { path: source });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn asset_ids_are_normalized() {
        for invalid in ["", "/a", "../a", "a/../b", "a\\b", "a//b"] {
            assert!(crate::model::AssetId::new(invalid).is_err(), "{invalid}");
        }
        assert_eq!(
            crate::model::AssetId::new("meshes/base.stl")
                .unwrap()
                .as_str(),
            "meshes/base.stl"
        );
    }
}
