//! Authored manifest readers and deterministic source-to-canonical compilation.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
pub use phoxal_model::AssetId;

pub mod behavior;
pub mod source;
mod structure;

/// Exact authored inputs after package/workspace component resolution.
#[derive(Debug, Clone)]
pub struct SourceSet {
    pub project_root: PathBuf,
    pub robot_manifest: PathBuf,
    pub component_roots: BTreeMap<String, PathBuf>,
}

/// Source compiler failure classified by the stage that owns the invariant.
#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    #[error("invalid compiler input at {}: {source:#}", path.display())]
    Input {
        path: PathBuf,
        #[source]
        source: anyhow::Error,
    },
    #[error("failed to compile robot document {}: {source:#}", path.display())]
    Robot {
        path: PathBuf,
        #[source]
        source: anyhow::Error,
    },
    #[error(
        "failed to compile component document '{component_type}' at {}: {source:#}",
        path.display()
    )]
    Component {
        component_type: String,
        path: PathBuf,
        #[source]
        source: anyhow::Error,
    },
    #[error("failed to compile structure document {}: {source:#}", path.display())]
    Structure {
        path: PathBuf,
        #[source]
        source: anyhow::Error,
    },
    #[error("failed to construct canonical robot from {}: {source}", path.display())]
    CanonicalModel {
        path: PathBuf,
        #[source]
        source: phoxal_model::ModelError,
    },
    #[error("failed to normalize authored robot {}: {source:#}", path.display())]
    Normalize {
        path: PathBuf,
        #[source]
        source: anyhow::Error,
    },
    #[error("failed to compile participant declarations from {}: {source:#}", path.display())]
    Participants {
        path: PathBuf,
        #[source]
        source: anyhow::Error,
    },
    #[error("failed to compile runtime assets below {}: {source:#}", path.display())]
    Assets {
        path: PathBuf,
        #[source]
        source: anyhow::Error,
    },
    #[error("failed to compile behavior documents below {}: {source:#}", path.display())]
    Behavior {
        path: PathBuf,
        #[source]
        source: anyhow::Error,
    },
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

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ParticipantKind {
    Service,
    Driver,
    Simulator,
    Tool,
}

/// Deterministic compiled runtime assets.
#[derive(Debug, Clone, Default)]
pub struct CompiledAssets(BTreeMap<AssetId, Vec<u8>>);

/// Compile authored YAML/URDF sources after the caller resolved component roots.
pub fn compile(sources: SourceSet) -> Result<CompiledProject, CompileError> {
    compile_inner(sources)
}

fn compile_inner(sources: SourceSet) -> Result<CompiledProject, CompileError> {
    let project_root = sources
        .project_root
        .canonicalize()
        .with_context(|| {
            format!(
                "failed to resolve project root {}",
                sources.project_root.display()
            )
        })
        .map_err(|source| CompileError::Input {
            path: sources.project_root.clone(),
            source,
        })?;
    let robot_manifest = sources
        .robot_manifest
        .canonicalize()
        .with_context(|| {
            format!(
                "failed to resolve robot manifest {}",
                sources.robot_manifest.display()
            )
        })
        .map_err(|source| CompileError::Input {
            path: sources.robot_manifest.clone(),
            source,
        })?;
    if !robot_manifest.starts_with(&project_root) {
        return Err(CompileError::Input {
            path: robot_manifest.clone(),
            source: anyhow::anyhow!(
                "robot manifest must stay below project root {}",
                project_root.display()
            ),
        });
    }
    let manifest =
        source::robot::read_from_path(&robot_manifest).map_err(|source| CompileError::Robot {
            path: robot_manifest.clone(),
            source,
        })?;

    let mut component_types = BTreeMap::new();
    let mut simulation_types = BTreeMap::new();
    for component_type in manifest.used_component_types() {
        let configured_root = sources
            .component_roots
            .get(component_type)
            .cloned()
            .unwrap_or_else(|| PathBuf::from(component_type));
        let compiled: anyhow::Result<_> = (|| {
            let root = sources
                .component_roots
                .get(component_type)
                .with_context(|| {
                    format!("no resolved component root for authored type '{component_type}'")
                })?
                .canonicalize()
                .with_context(|| {
                    format!(
                        "failed to resolve component root for '{component_type}': {}",
                        configured_root.display()
                    )
                })?;
            let authored = source::component::read_from_dir(&root)
                .with_context(|| format!("failed to load component type '{component_type}'"))?;
            authored.validate_for_component(component_type)?;
            let capabilities =
                serde_json::from_value(serde_json::to_value(authored.capabilities)?)?;
            let structure_path = root.join("structure.urdf");
            let structure = structure::Structure::read_from_file(&structure_path)
                .with_context(|| {
                    format!(
                        "failed to read component structure {}",
                        structure_path.display()
                    )
                })?
                .into_canonical_fragment(component_type)?;
            let component = phoxal_model::component::Component::__new(capabilities, structure);
            let simulation = if root.join("simulation.yaml").is_file() {
                let authored = source::simulation::read_from_dir(&root).with_context(|| {
                    format!("failed to load simulation for component type '{component_type}'")
                })?;
                let capabilities =
                    serde_json::from_value(serde_json::to_value(authored.capabilities)?)?;
                let links = authored
                    .links
                    .into_iter()
                    .map(|(id, link)| (id, link.contact_material))
                    .collect();
                Some(phoxal_model::simulation::Simulation::__new(
                    capabilities,
                    links,
                ))
            } else {
                None
            };
            Ok((component, simulation))
        })();
        let (component, simulation) = compiled.map_err(|source| CompileError::Component {
            component_type: component_type.to_string(),
            path: configured_root,
            source,
        })?;
        component_types.insert(component_type.to_string(), component);
        if let Some(simulation) = simulation {
            simulation_types.insert(component_type.to_string(), simulation);
        }
    }

    let component_instances: anyhow::Result<BTreeMap<_, _>> = (|| {
        let mut component_instances = BTreeMap::new();
        for (id, authored) in &manifest.robot.components {
            let component = component_types.get(&authored.component).with_context(|| {
                format!(
                    "component instance '{id}' references unresolved type '{}'",
                    authored.component
                )
            })?;
            let mut direction_signs = BTreeMap::new();
            for (capability_id, parameters) in &authored.parameters {
                let capability = component.capability(capability_id).with_context(|| {
                    format!(
                        "component instance '{id}' parameters reference unknown capability '{capability_id}'"
                    )
                })?;
                if capability.kind_name() != parameters.kind_name() {
                    bail!(
                        "component instance '{id}' parameter '{capability_id}' kind '{}' does not match '{}'",
                        parameters.kind_name(),
                        capability.kind_name()
                    );
                }
                use source::robot::v0::capability::Parameters;
                let direction = match parameters {
                    Parameters::Motor(value) => value.direction_sign,
                    Parameters::Encoder(value) => value.direction_sign,
                    _ => 1,
                };
                direction_signs.insert(capability_id.clone(), direction);
            }
            component_instances.insert(
                id.clone(),
                phoxal_model::robot::ComponentInstance::__new(
                    id.clone(),
                    authored.component.clone(),
                    authored.mount_link.clone(),
                    direction_signs,
                ),
            );
        }
        Ok(component_instances)
    })();
    let component_instances = component_instances.map_err(|source| CompileError::Normalize {
        path: robot_manifest.clone(),
        source,
    })?;

    let structure_path = project_root.join(&manifest.robot.structure);
    let structure = (|| -> anyhow::Result<_> {
        structure::Structure::read_from_file(&structure_path)
            .with_context(|| {
                format!(
                    "failed to read robot structure {}",
                    structure_path.display()
                )
            })?
            .into_canonical(None)
    })()
    .map_err(|source| CompileError::Structure {
        path: structure_path,
        source,
    })?;
    let kinematic =
        serde_json::from_value(serde_json::to_value(&manifest.robot.kinematic).map_err(
            |error| CompileError::Normalize {
                path: robot_manifest.clone(),
                source: error.into(),
            },
        )?)
        .map_err(|error| CompileError::Normalize {
            path: robot_manifest.clone(),
            source: error.into(),
        })?;
    let motion_limits =
        serde_json::from_value(serde_json::to_value(manifest.robot.motion_limits).map_err(
            |error| CompileError::Normalize {
                path: robot_manifest.clone(),
                source: error.into(),
            },
        )?)
        .map_err(|error| CompileError::Normalize {
            path: robot_manifest.clone(),
            source: error.into(),
        })?;
    let robot = phoxal_model::Robot::__from_compiler(phoxal_model::robot::RobotParts {
        id: manifest.robot.id.clone(),
        namespace: manifest.robot.namespace.clone(),
        kinematic,
        motion_limits,
        component_instances,
        component_types,
        simulation_types,
        structure,
    })
    .map_err(|source| CompileError::CanonicalModel {
        path: robot_manifest.clone(),
        source,
    })?;

    let participants =
        compile_participants(&manifest, &sources.component_roots).map_err(|source| {
            CompileError::Participants {
                path: robot_manifest.clone(),
                source,
            }
        })?;
    let mut assets = CompiledAssets::default();
    collect_files(&project_root.join("meshes"), "meshes/robot", &mut assets).map_err(|source| {
        CompileError::Assets {
            path: project_root.clone(),
            source,
        }
    })?;
    for component_type in manifest.used_component_types() {
        let root = sources
            .component_roots
            .get(component_type)
            .with_context(|| {
                format!("no resolved component root for authored type '{component_type}'")
            })
            .map_err(|source| CompileError::Assets {
                path: project_root.clone(),
                source,
            })?;
        collect_files(
            &root.join("meshes"),
            &format!("meshes/components/{component_type}"),
            &mut assets,
        )
        .map_err(|source| CompileError::Assets {
            path: root.clone(),
            source,
        })?;
    }
    if manifest.behavior.is_some() {
        let behavior_root = project_root.join("behaviors");
        let catalog =
            behavior::compile(&project_root).map_err(|source| CompileError::Behavior {
                path: behavior_root,
                source,
            })?;
        assets
            .insert(
                AssetId::new("behavior/catalog.json").expect("static asset id is normalized"),
                catalog,
            )
            .map_err(|source| CompileError::Assets {
                path: project_root.join("assets"),
                source,
            })?;
    }
    for asset_id in robot.structure().asset_ids() {
        if !assets.0.contains_key(asset_id) {
            return Err(CompileError::Assets {
                path: project_root.clone(),
                source: anyhow::anyhow!(
                    "canonical model references missing compiled asset '{}'",
                    asset_id.as_str()
                ),
            });
        }
    }
    for instance in robot.components() {
        let component = robot
            .component_for_instance(instance.id())
            .map_err(|source| CompileError::CanonicalModel {
                path: robot_manifest.clone(),
                source,
            })?;
        for asset_id in component.structure().asset_ids() {
            if !assets.0.contains_key(asset_id) {
                return Err(CompileError::Assets {
                    path: project_root.clone(),
                    source: anyhow::anyhow!(
                        "canonical model references missing compiled asset '{}'",
                        asset_id.as_str()
                    ),
                });
            }
        }
    }

    Ok(CompiledProject {
        robot,
        participants,
        assets,
    })
}

fn compile_participants(
    manifest: &source::robot::v0::Manifest,
    component_roots: &BTreeMap<String, PathBuf>,
) -> anyhow::Result<ParticipantDeclarations> {
    let mut participants = Vec::new();
    participants.extend(manifest.services.iter().map(|(id, service)| Participant {
        id: id.clone(),
        kind: ParticipantKind::Service,
        component_instance: None,
        config: service.config.clone(),
    }));
    participants.extend(manifest.tools.iter().map(|(id, tool)| Participant {
        id: id.clone(),
        kind: ParticipantKind::Tool,
        component_instance: None,
        config: tool.config.clone(),
    }));
    if let Some(behavior) = &manifest.behavior {
        participants.push(Participant {
            id: "behavior".to_string(),
            kind: ParticipantKind::Service,
            component_instance: None,
            config: Some(serde_json::json!({
                "root": behavior.root,
                "autostart": behavior.autostart,
            })),
        });
    }
    for (instance, component) in &manifest.robot.components {
        if let Some(driver) = &component.driver {
            participants.push(Participant {
                id: component.component.clone(),
                kind: ParticipantKind::Driver,
                component_instance: Some(instance.clone()),
                config: Some(serde_json::to_value(driver)?),
            });
        }
        if component_roots
            .get(&component.component)
            .with_context(|| {
                format!(
                    "no resolved component root for authored type '{}'",
                    component.component
                )
            })?
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
    let mut seen = BTreeSet::new();
    for participant in &participants {
        let key = (
            participant.kind as u8,
            participant.id.as_str(),
            participant.component_instance.as_deref(),
        );
        if !seen.insert(key) {
            bail!(
                "duplicate participant declaration '{}'{}",
                participant.id,
                participant
                    .component_instance
                    .as_deref()
                    .map(|instance| format!(" for component '{instance}'"))
                    .unwrap_or_default()
            );
        }
    }
    Ok(ParticipantDeclarations(participants))
}

impl CompiledProject {
    #[must_use]
    pub fn robot(&self) -> &phoxal_model::Robot {
        &self.robot
    }

    #[must_use]
    pub fn participants(&self) -> &ParticipantDeclarations {
        &self.participants
    }

    #[must_use]
    pub fn assets(&self) -> &CompiledAssets {
        &self.assets
    }

    pub fn into_parts(self) -> (phoxal_model::Robot, ParticipantDeclarations, CompiledAssets) {
        (self.robot, self.participants, self.assets)
    }
}

impl ParticipantDeclarations {
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Participant> {
        self.0.iter()
    }

    pub fn into_vec(self) -> Vec<Participant> {
        self.0
    }
}

impl CompiledAssets {
    fn insert(&mut self, id: AssetId, bytes: Vec<u8>) -> anyhow::Result<()> {
        if self.0.insert(id.clone(), bytes).is_some() {
            bail!("duplicate compiled asset '{}'", id.as_str());
        }
        Ok(())
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&AssetId, &[u8])> {
        self.0.iter().map(|(id, bytes)| (id, bytes.as_slice()))
    }

    pub fn into_map(self) -> BTreeMap<AssetId, Vec<u8>> {
        self.0
    }
}

fn collect_files(
    source_root: &Path,
    staged_root: &str,
    output: &mut CompiledAssets,
) -> anyhow::Result<()> {
    if !source_root.is_dir() {
        return Ok(());
    }
    let mut entries = std::fs::read_dir(source_root)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let source = entry.path();
        let metadata = std::fs::symlink_metadata(&source)?;
        if metadata.file_type().is_symlink() {
            bail!(
                "asset source tree contains forbidden symlink {}",
                source.display()
            );
        }
        let name = entry.file_name().into_string().map_err(|name| {
            anyhow::anyhow!(
                "asset source entry name is not UTF-8 below {}: {:?}",
                source_root.display(),
                name
            )
        })?;
        let staged = format!("{staged_root}/{name}");
        if metadata.is_dir() {
            collect_files(&source, &staged, output)?;
        } else if metadata.is_file() {
            output.insert(AssetId::new(staged)?, std::fs::read(&source)?)?;
        } else {
            bail!("unsupported asset source entry {}", source.display());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_ids_are_normalized() {
        for invalid in ["", "/a", "../a", "a/../b", "a\\b", "a//b"] {
            assert!(AssetId::new(invalid).is_err(), "{invalid}");
        }
        assert_eq!(
            AssetId::new("meshes/base.stl").unwrap().as_str(),
            "meshes/base.stl"
        );
    }
}
