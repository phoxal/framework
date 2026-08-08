//! Authored manifest readers and deterministic source-to-canonical compilation.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use phoxal_model::AssetId;
use phoxal_model::compiler::RobotParts;
use phoxal_model::identity::{CapabilityId, ComponentInstanceId, ComponentTypeId, LinkId, RobotId};

use source::SourceError;

pub mod build_requirements;
pub mod bundle;
pub mod schema;
pub mod source;

// The authored URDF DTO stays private: `phoxal-model` owns the normalized
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
/// `robot_root` is the base for the manifest's `robot.structure` and
/// `router.config` paths; in an authored project it is the project root, and in
/// a finalized bundle it is `<bundle>/assets`.
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

    /// A declared path resolves outside the tree it must stay inside.
    #[error("{} must stay below {}", path.display(), root.display())]
    Escapes { path: PathBuf, root: PathBuf },

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

    /// Instance parameters claim a different capability kind than the component
    /// type declares.
    #[error(
        "component instance '{instance}' parameter '{capability_id}' kind '{authored}' does not \
         match '{declared}'"
    )]
    CapabilityKindMismatch {
        instance: String,
        capability_id: String,
        authored: phoxal_model::component::capability::CapabilityKind,
        declared: phoxal_model::component::capability::CapabilityKind,
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
        source: phoxal_model::ModelError,
    },

    /// A runtime asset tree could not be read.
    #[error("failed to compile runtime assets below {}: {source}", root.display())]
    Assets {
        root: PathBuf,
        #[source]
        source: AssetError,
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
        source: phoxal_model::ModelError,
    },

    #[error("duplicate compiled asset '{id}'", id = id.as_str())]
    Duplicate { id: AssetId },

    #[error("canonical model references missing compiled asset '{id}'", id = id.as_str())]
    Missing { id: AssetId },
}

/// The complete source-to-runtime compilation result.
#[derive(Debug, Clone)]
pub struct CompiledProject {
    robot: phoxal_model::Robot,
    participants: ParticipantDeclarations,
    assets: CompiledAssets,
}

/// Normalized participant declarations kept outside the canonical robot.
#[derive(Debug, Clone, Default)]
pub struct ParticipantDeclarations(Vec<Participant>);

/// One selected project participant, independent of process launch policy.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Participant {
    pub id: String,
    pub kind: ParticipantKind,
    pub component_instance: Option<String>,
    pub config: Option<serde_json::Value>,
}

/// What a participant is.
///
/// This must round-trip every kind a real participant binary declares in its
/// embedded metadata, which is why `Brain` is here even though a brain is never
/// declared under `robot.yaml` `services:` - the authored grammar owns that
/// exclusion, through `RESERVED_BRAIN_ID`, and a deserializable enum is the
/// wrong place to express it.
///
/// The canonical definition is `phoxal_runtime_contract::ParticipantKind`.
/// This crate cannot depend on it (that edge is forbidden), so the two are
/// coupled by convention and pinned by a test.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ParticipantKind {
    Service,
    Driver,
    Simulator,
    Brain,
}

/// Deterministic compiled runtime assets.
#[derive(Debug, Clone, Default)]
pub struct CompiledAssets(BTreeMap<AssetId, Vec<u8>>);

impl SourceSet {
    /// Compile authored YAML/URDF sources after the caller resolved component
    /// roots.
    ///
    /// # Errors
    ///
    /// Returns the first [`CompileError`] the sources violate, named by the
    /// stage that owns the invariant.
    pub fn compile(self) -> Result<CompiledProject, CompileError> {
        let project_root = canonicalize(&self.project_root)?;
        let robot_manifest = canonicalize(&self.robot_manifest)?;
        if !robot_manifest.starts_with(&project_root) {
            return Err(CompileError::Escapes {
                path: robot_manifest,
                root: project_root,
            });
        }
        let source::robot::Manifest::V0(manifest) = source::robot::Manifest::load(&robot_manifest)
            .map_err(|source| CompileError::Document { source })?;

        let resolved = ResolvedSources {
            robot_manifest,
            robot_root: project_root.clone(),
            component_roots: self.component_roots,
        };
        let (robot, participants) = resolved.compile_model(&manifest)?;
        let assets = resolved.compile_assets(&project_root, &manifest, &robot)?;

        Ok(CompiledProject {
            robot,
            participants,
            assets,
        })
    }
}

/// Every logical asset the canonical model actually references.
pub(crate) fn referenced_asset_ids(robot: &phoxal_model::Robot) -> BTreeSet<AssetId> {
    let mut ids = robot
        .structure()
        .asset_ids()
        .cloned()
        .collect::<BTreeSet<_>>();
    for instance in robot.components() {
        // A validated robot never mounts an instance of a type it did not
        // load, so an absent component type is not reachable here.
        if let Some(component) = robot.component_for_instance(instance.id().as_str()) {
            ids.extend(component.structure().asset_ids().cloned());
        }
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

    /// Build the canonical model and participant declarations from documents
    /// that are already resolved on disk.
    fn compile_model(
        &self,
        manifest: &source::robot::v0::Manifest,
    ) -> Result<(phoxal_model::Robot, ParticipantDeclarations), CompileError> {
        let mut component_types = BTreeMap::new();
        let mut simulation_types = BTreeMap::new();
        for component_type in manifest.used_component_types() {
            let root = self.component_root(component_type)?.clone();
            let (component, simulation) = self
                .compile_component_type(component_type, &root)
                .map_err(|source| CompileError::Component {
                    component_type: component_type.to_string(),
                    root,
                    source: Box::new(source),
                })?;
            let type_id = self.identity(ComponentTypeId::new(component_type))?;
            component_types.insert(type_id.clone(), component);
            if let Some(simulation) = simulation {
                simulation_types.insert(type_id, simulation);
            }
        }

        let mut component_instances = BTreeMap::new();
        for (id, authored) in &manifest.robot.components {
            let component = component_types
                .get(authored.component.as_str())
                .ok_or_else(|| CompileError::UnknownComponentType {
                    instance: id.clone(),
                    component_type: authored.component.clone(),
                })?;
            let mut direction_signs = BTreeMap::new();
            for (capability_id, parameters) in &authored.parameters {
                let declared = component.capability(capability_id).ok_or_else(|| {
                    CompileError::UnknownCapability {
                        instance: id.clone(),
                        capability_id: capability_id.clone(),
                    }
                })?;
                if declared.kind() != parameters.kind() {
                    return Err(CompileError::CapabilityKindMismatch {
                        instance: id.clone(),
                        capability_id: capability_id.clone(),
                        authored: parameters.kind(),
                        declared: declared.kind(),
                    });
                }
                direction_signs.insert(
                    self.identity(CapabilityId::new(capability_id))?,
                    parameters.direction_sign(),
                );
            }
            let instance_id = self.identity(ComponentInstanceId::new(id))?;
            component_instances.insert(
                instance_id.clone(),
                phoxal_model::compiler::component_instance(
                    instance_id,
                    self.identity(ComponentTypeId::new(&authored.component))?,
                    LinkId::new(&authored.mount_link),
                    direction_signs,
                ),
            );
        }

        let structure_path = self.robot_relative(&manifest.robot.structure)?;
        let structure = urdf_dto::Structure::load(&structure_path)
            .and_then(|structure| structure.into_canonical(None))
            .map_err(|source| CompileError::Structure {
                path: structure_path,
                source,
            })?;

        let robot = phoxal_model::compiler::robot(RobotParts {
            id: self.identity(RobotId::new(&manifest.robot.id))?,
            clock: manifest.clock.into(),
            kinematic: manifest.robot.kinematic.clone(),
            motion_limits: manifest.robot.motion_limits,
            component_instances,
            component_types,
            simulation_types,
            structure,
        })
        .map_err(|source| CompileError::CanonicalModel {
            path: self.robot_manifest.clone(),
            source,
        })?;

        let participants = self.compile_participants(manifest)?;
        Ok((robot, participants))
    }

    /// Load and normalize one component type's documents.
    fn compile_component_type(
        &self,
        component_type: &str,
        configured_root: &Path,
    ) -> Result<
        (
            phoxal_model::component::Component,
            Option<phoxal_model::simulation::Simulation>,
        ),
        CompileError,
    > {
        let root = canonicalize(configured_root)?;
        let authored = source::component::Manifest::load(&root)
            .map_err(|source| CompileError::Document { source })?;
        authored
            .validate_as(component_type)
            .map_err(|errors| CompileError::Document {
                source: SourceError::Invalid {
                    origin: source::Origin::File(root.join("component.yaml")),
                    violations: source::Violations::Component(errors),
                },
            })?;
        let source::component::Manifest::V0(authored) = authored;

        let structure_path = root.join("structure.urdf");
        let structure = urdf_dto::Structure::load(&structure_path)
            .and_then(|structure| structure.into_canonical_fragment(component_type))
            .map_err(|source| CompileError::Structure {
                path: structure_path,
                source,
            })?;
        let component = phoxal_model::compiler::component(
            Self::transcode(&authored.capabilities, "component capabilities")?,
            structure,
        );

        let simulation_path = root.join("simulation.yaml");
        if !simulation_path.is_file() {
            return Ok((component, None));
        }
        let source::simulation::Manifest::V0(authored) =
            source::simulation::Manifest::load(&simulation_path)
                .map_err(|source| CompileError::Document { source })?;
        let links = authored
            .links
            .into_iter()
            .map(|(id, link)| (LinkId::new(id), link.contact_material))
            .collect();
        let simulation = phoxal_model::compiler::simulation(
            Self::transcode(&authored.capabilities, "simulation capabilities")?,
            links,
        );
        Ok((component, Some(simulation)))
    }

    /// Adopt an authored capability map into its canonical counterpart.
    ///
    /// The two shapes are serde-compatible by construction: the authored DTO's
    /// job is to add defaults and permissive spellings on the way in, and the
    /// canonical value is what is left once those are resolved. Going through
    /// JSON is what keeps that "same wire, different obligations" relationship
    /// explicit rather than hiding it in a hand-written field-by-field copy that
    /// would silently drift.
    fn transcode<T: serde::Serialize, U: serde::de::DeserializeOwned>(
        authored: &T,
        what: &'static str,
    ) -> Result<U, CompileError> {
        let value = serde_json::to_value(authored).map_err(|source| CompileError::Transcode {
            authored: what,
            source,
        })?;
        serde_json::from_value(value).map_err(|source| CompileError::Transcode {
            authored: what,
            source,
        })
    }

    /// Attribute an identifier rejection to the document that carried it.
    fn identity<T>(&self, result: Result<T, phoxal_model::ModelError>) -> Result<T, CompileError> {
        result.map_err(|source| CompileError::CanonicalModel {
            path: self.robot_manifest.clone(),
            source,
        })
    }

    /// Resolve a manifest-declared path against the robot root, refusing any
    /// path that leaves it.
    fn robot_relative(&self, relative: &Path) -> Result<PathBuf, CompileError> {
        let resolved = canonicalize(&self.robot_root.join(relative))?;
        let root = canonicalize(&self.robot_root)?;
        if resolved.starts_with(&root) {
            Ok(resolved)
        } else {
            Err(CompileError::Escapes {
                path: resolved,
                root,
            })
        }
    }

    fn compile_participants(
        &self,
        manifest: &source::robot::v0::Manifest,
    ) -> Result<ParticipantDeclarations, CompileError> {
        // `services` cannot contain the reserved `brain` identity here: every
        // entry into this compiler validates the authored document first, and
        // `Manifest::validate` owns that rejection.
        let mut participants = manifest
            .services
            .iter()
            .map(|(id, service)| Participant {
                id: id.clone(),
                kind: ParticipantKind::Service,
                component_instance: None,
                config: service.config.clone(),
            })
            .collect::<Vec<_>>();
        for (instance, component) in &manifest.robot.components {
            if let Some(driver) = &component.driver {
                participants.push(Participant {
                    id: component.component.clone(),
                    kind: ParticipantKind::Driver,
                    component_instance: Some(instance.clone()),
                    config: Some(Self::transcode(driver, "driver configuration")?),
                });
            }
            if self
                .component_root(&component.component)?
                .join("simulation.yaml")
                .is_file()
            {
                participants.push(Participant {
                    id: component.component.clone(),
                    kind: ParticipantKind::Simulator,
                    component_instance: Some(instance.clone()),
                    config: None,
                });
            }
        }
        // Every constructor above is keyed by a distinct authored map: `services`
        // by service id, and the driver/simulator declarations by component
        // instance. The `(kind, id, component_instance)` triple is therefore
        // unique by construction, so there is no duplicate check to run here.
        participants.sort_by(|left, right| {
            (
                left.kind as u8,
                left.id.as_str(),
                left.component_instance.as_deref(),
            )
                .cmp(&(
                    right.kind as u8,
                    right.id.as_str(),
                    right.component_instance.as_deref(),
                ))
        });
        Ok(ParticipantDeclarations(participants))
    }

    /// Stage every mesh tree the project owns, then prove the canonical model
    /// references nothing that was not staged.
    fn compile_assets(
        &self,
        project_root: &Path,
        manifest: &source::robot::v0::Manifest,
        robot: &phoxal_model::Robot,
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
    pub const fn robot(&self) -> &phoxal_model::Robot {
        &self.robot
    }

    #[must_use]
    pub const fn participants(&self) -> &ParticipantDeclarations {
        &self.participants
    }

    #[must_use]
    pub const fn assets(&self) -> &CompiledAssets {
        &self.assets
    }

    #[must_use]
    pub fn into_parts(self) -> (phoxal_model::Robot, ParticipantDeclarations, CompiledAssets) {
        (self.robot, self.participants, self.assets)
    }
}

impl ParticipantDeclarations {
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Participant> {
        self.0.iter()
    }

    #[must_use]
    pub fn into_vec(self) -> Vec<Participant> {
        self.0
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
    use super::ParticipantKind;

    #[test]
    fn asset_ids_are_normalized() {
        for invalid in ["", "/a", "../a", "a/../b", "a\\b", "a//b"] {
            assert!(phoxal_model::AssetId::new(invalid).is_err(), "{invalid}");
        }
        assert_eq!(
            phoxal_model::AssetId::new("meshes/base.stl")
                .unwrap()
                .as_str(),
            "meshes/base.stl"
        );
    }

    /// A participant binary's embedded metadata serializes its kind with the
    /// canonical `phoxal_runtime_contract::ParticipantKind` names. This crate
    /// must not depend on that crate, so the coupling is pinned here: adding a
    /// kind there without adding it below makes a real binary's metadata
    /// undeserializable, and this test is what says so.
    #[test]
    fn every_canonical_participant_kind_round_trips() {
        const CANONICAL: [&str; 4] = ["service", "driver", "simulator", "brain"];
        for name in CANONICAL {
            let json = format!("\"{name}\"");
            let kind: ParticipantKind = serde_json::from_str(&json)
                .unwrap_or_else(|error| panic!("'{name}' must deserialize: {error}"));
            assert_eq!(serde_json::to_string(&kind).unwrap(), json);
        }
        assert!(serde_json::from_str::<ParticipantKind>("\"tool\"").is_err());
    }
}
