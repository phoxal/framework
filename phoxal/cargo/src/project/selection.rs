//! Exact participants prepared outside the robot Cargo graph.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::project::document::{
    BrainSelection, ComponentDocument, RobotDocument, ValidateComponentDocument,
};
use crate::project::error::SourceError;
use crate::project::participant;
use crate::project::{CargoOptions, Error, ProjectLayout};
use cargo_metadata::{DependencyKind, Metadata, Package, PackageId, Target};

/// A role selected by `robot.yaml` and resolved through the root Cargo graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TargetRole {
    /// The required robot-local application binary.
    Brain,
    /// A behavioral service instance.
    Service,
    /// A mounted component definition.
    Component,
    /// A component-owned executable driver.
    Driver,
    /// The mandatory supervisor executable.
    Supervisor,
}

impl std::fmt::Display for TargetRole {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Brain => "brain",
            Self::Service => "service",
            Self::Component => "component",
            Self::Driver => "driver",
            Self::Supervisor => "supervisor",
        })
    }
}

/// Where Cargo obtained a selected package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageSource {
    /// A package in the robot's local Cargo workspace.
    Local { manifest_path: PathBuf },
    /// A pinned Git package.
    Git { source: String },
    /// A registry package, including the configured Phoxal registry.
    Registry { source: String },
    /// A future or custom Cargo source that this reader preserves verbatim.
    Other { source: String },
}

/// One Cargo target selected for execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedTarget {
    /// Package identity returned by Cargo.
    pub package_id: String,
    /// Package name.
    pub package: String,
    /// Target name passed to Cargo's `--bin` selector.
    pub target: String,
    /// Main source file reported by Cargo.
    pub source_path: PathBuf,
    /// Features required by the target.
    pub required_features: Vec<String>,
    /// Root dependency key used to activate required features for a selected
    /// dependency target. Root-local targets leave this unset.
    pub feature_dependency: Option<String>,
}

/// One selected service package and its executable/library targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedService {
    /// `robot.yaml` instance identity.
    pub instance: String,
    /// Exact Cargo dependency key authored in `robot.yaml` or inferred from its
    /// shorthand service entry.
    pub dependency_key: String,
    /// Cargo package identity.
    pub package_id: String,
    /// Cargo package name.
    pub package: String,
    /// Package source and provenance class.
    pub source: PackageSource,
    /// Service executable selected for assembly.
    pub binary: SelectedTarget,
}

/// One selected component package. Components may be passive and therefore do
/// not need an executable target in the project graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedComponent {
    /// `robot.components.<id>` instance identity.
    pub instance: String,
    /// Exact Cargo dependency key from the component instance.
    pub dependency_key: String,
    /// Cargo package identity.
    pub package_id: String,
    /// Cargo package name.
    pub package: String,
    /// Package source and provenance class.
    pub source: PackageSource,
    /// Persistent site in the parent robot model receiving this instance.
    pub mount_site: String,
    /// Parsed component-owned semantic and native binding declaration.
    pub definition: ComponentDocument,
    /// The component-owned driver selected by this instance, when its
    /// authored `driver` block requests a real process.
    pub driver: Option<SelectedDriver>,
}

/// One component-owned driver selected through a resolved Cargo dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedDriver {
    /// Component-local or root-visible dependency key selecting the driver.
    pub dependency_key: String,
    /// Cargo package identity.
    pub package_id: String,
    /// Cargo package name.
    pub package: String,
    /// Package source and provenance class.
    pub source: PackageSource,
    /// Executable selected for this mounted component instance.
    pub binary: SelectedTarget,
}

/// The complete Cargo-backed selection made by one validated robot document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSelection {
    /// The mandatory root-local brain binary.
    pub brain: SelectedTarget,
    /// The mandatory supervisor executable resolved from the root graph.
    pub supervisor: SelectedTarget,
    /// Explicit behavioral service selections in authored map order.
    pub services: BTreeMap<String, SelectedService>,
    /// Mounted component package selections in authored map order.
    pub components: BTreeMap<String, SelectedComponent>,
}

pub(crate) fn resolve_prepared_sources(
    layout: &ProjectLayout,
    document: &RobotDocument,
    metadata: &Metadata,
    options: &CargoOptions,
) -> Result<SourceSelection, Error> {
    let RobotDocument::V0 {
        brain: authored_brain,
        services: authored_services,
        robot,
        ..
    } = document;
    let root = metadata.root_package().ok_or(SourceError::MissingBrain)?;
    let brain = resolve_brain(root, authored_brain.as_ref(), metadata)?;
    let supervisor = resolve_supervisor(root, metadata)?;
    let installed = participant::selected_installations(layout, document, options)?;
    let mut services = BTreeMap::new();
    for instance in authored_services.keys() {
        let selected = installed
            .get(instance)
            .ok_or_else(|| Error::ArtifactCapture {
                package: instance.clone(),
                target: instance.clone(),
                message: "participant installation missing".to_owned(),
            })?;
        let binary = installed_target(selected);
        services.insert(
            instance.clone(),
            SelectedService {
                instance: instance.clone(),
                dependency_key: selected.package.clone(),
                package_id: binary.package_id.clone(),
                package: selected.package.clone(),
                source: selected.source.clone(),
                binary,
            },
        );
    }
    let mut components = BTreeMap::new();
    for (instance, component) in &robot.components {
        if component.driver.is_none() {
            let package = resolve_dependency(
                TargetRole::Component,
                instance,
                &component.package,
                root,
                metadata,
            )?;
            if package.name != component.package || package.version.to_string() != component.version
            {
                return Err(Error::ArtifactInvalid {
                    path: layout.robot_manifest().to_owned(),
                    message: format!(
                        "passive component {instance} resolves to {} {}, expected {} {}",
                        package.name, package.version, component.package, component.version
                    ),
                });
            }
            let source_root = package
                .manifest_path
                .as_std_path()
                .parent()
                .ok_or_else(|| Error::ArtifactInvalid {
                    path: package.manifest_path.as_std_path().to_owned(),
                    message: "passive component manifest has no parent".to_owned(),
                })?;
            let definition = load_component_definition(instance, source_root)?;
            components.insert(
                instance.clone(),
                SelectedComponent {
                    instance: instance.clone(),
                    dependency_key: component.package.clone(),
                    package_id: package.id.to_string(),
                    package: component.package.clone(),
                    source: package_source(package),
                    mount_site: component.mount_site.clone(),
                    definition,
                    driver: None,
                },
            );
            continue;
        }
        let selected = installed
            .get(instance)
            .ok_or_else(|| Error::ArtifactCapture {
                package: component.package.clone(),
                target: component.package.clone(),
                message: "component installation missing".to_owned(),
            })?;
        let definition = load_installed_component_definition(instance, selected)?;
        let binary = installed_target(selected);
        components.insert(
            instance.clone(),
            SelectedComponent {
                instance: instance.clone(),
                dependency_key: component.package.clone(),
                package_id: binary.package_id.clone(),
                package: component.package.clone(),
                source: selected.source.clone(),
                mount_site: component.mount_site.clone(),
                definition,
                driver: component.driver.as_ref().map(|_| SelectedDriver {
                    dependency_key: component.package.clone(),
                    package_id: binary.package_id.clone(),
                    package: component.package.clone(),
                    source: selected.source.clone(),
                    binary,
                }),
            },
        );
    }
    Ok(SourceSelection {
        brain,
        supervisor,
        services,
        components,
    })
}

fn installed_target(selected: &participant::InstalledSelection) -> SelectedTarget {
    use sha2::{Digest, Sha256};
    let identity = Sha256::digest(selected.executable.to_string_lossy().as_bytes());
    SelectedTarget {
        package_id: format!(
            "{} {} (prepared:{:x})",
            selected.package, selected.version, identity
        ),
        package: selected.package.clone(),
        target: selected.binary.clone(),
        source_path: selected.executable.clone(),
        required_features: Vec::new(),
        feature_dependency: None,
    }
}

fn load_installed_component_definition(
    instance: &str,
    selected: &participant::InstalledSelection,
) -> Result<ComponentDocument, Error> {
    load_component_definition(instance, &selected.source_root)
}

fn load_component_definition(
    instance: &str,
    source_root: &std::path::Path,
) -> Result<ComponentDocument, Error> {
    let path = source_root.join("component.yaml");
    let text = std::fs::read_to_string(&path).map_err(|source| Error::ArtifactFile {
        path: path.clone(),
        source,
    })?;
    let definition: ComponentDocument =
        serde_yaml::from_str(&text).map_err(|source| Error::ArtifactInvalid {
            path: path.clone(),
            message: source.to_string(),
        })?;
    definition
        .validate()
        .map_err(|message| Error::ArtifactInvalid {
            path: path.clone(),
            message,
        })?;
    let ComponentDocument::V0 { model, .. } = &definition;
    if !source_root.join(&model.file).is_file() {
        return Err(Error::ArtifactInvalid {
            path,
            message: format!("component {instance} model is missing"),
        });
    }
    Ok(definition)
}

const SUPERVISOR_DEPENDENCY_KEY: &str = "phoxal-supervisor";
const SUPERVISOR_PACKAGE_NAME: &str = "phoxal-supervisor";
const SUPERVISOR_BINARY_NAME: &str = "phoxal-supervisor";

fn resolve_supervisor(root: &Package, metadata: &Metadata) -> Result<SelectedTarget, SourceError> {
    let package = resolve_dependency(
        TargetRole::Supervisor,
        "supervisor",
        SUPERVISOR_DEPENDENCY_KEY,
        root,
        metadata,
    )
    .map_err(|error| match error {
        SourceError::DependencyNotDeclared { .. }
        | SourceError::DependencyWrongKind { .. }
        | SourceError::DependencyUnresolved { .. } => SourceError::MissingSupervisor {
            key: SUPERVISOR_DEPENDENCY_KEY.to_owned(),
        },
        other => other,
    })?;
    if package.name != SUPERVISOR_PACKAGE_NAME {
        return Err(SourceError::MissingSupervisor {
            key: SUPERVISOR_DEPENDENCY_KEY.to_owned(),
        });
    }
    let target = package
        .targets
        .iter()
        .find(|target| target.is_bin() && target.name == SUPERVISOR_BINARY_NAME)
        .ok_or_else(|| SourceError::MissingTarget {
            role: TargetRole::Supervisor,
            instance: "supervisor".to_owned(),
            key: SUPERVISOR_DEPENDENCY_KEY.to_owned(),
            package: package.name.to_string(),
            target_kind: format!("binary '{SUPERVISOR_BINARY_NAME}'"),
        })?;
    let selected = selected_dependency_target(package, target, SUPERVISOR_DEPENDENCY_KEY);
    ensure_target_features(TargetRole::Supervisor, "supervisor", &selected, package)?;
    Ok(selected)
}

fn resolve_brain(
    root: &Package,
    selection: Option<&BrainSelection>,
    metadata: &Metadata,
) -> Result<SelectedTarget, SourceError> {
    let binaries = root
        .targets
        .iter()
        .filter(|target| target.is_bin())
        .collect::<Vec<_>>();
    if let Some(selection) = selection.and_then(|selection| selection.binary.as_deref()) {
        let target = binaries
            .iter()
            .find(|target| target.name == selection)
            .copied()
            .ok_or_else(|| SourceError::InvalidBrainBinary {
                binary: selection.to_owned(),
            })?;
        let selected = selected_target(root, target);
        ensure_target_features(TargetRole::Brain, "brain", &selected, root)?;
        return Ok(selected);
    }

    let enabled = enabled_features(metadata, &root.id);
    let eligible = binaries
        .iter()
        .filter(|target| {
            target
                .required_features
                .iter()
                .all(|feature| enabled.contains(feature))
        })
        .copied()
        .collect::<Vec<_>>();
    match eligible.as_slice() {
        [] if binaries.is_empty() => Err(SourceError::MissingBrain),
        [] => {
            let required = binaries
                .iter()
                .flat_map(|target| target.required_features.iter().cloned())
                .collect::<Vec<_>>();
            Err(SourceError::BrainFeatures {
                binary: binaries
                    .iter()
                    .map(|target| target.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                features: required.join(", "),
            })
        }
        [target] => Ok(selected_target(root, target)),
        _ => Err(SourceError::AmbiguousBrain {
            candidates: eligible
                .iter()
                .map(|target| target.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        }),
    }
}

fn resolve_dependency<'a>(
    role: TargetRole,
    instance: &str,
    key: &str,
    root: &'a Package,
    metadata: &'a Metadata,
) -> Result<&'a Package, SourceError> {
    resolve_package_dependency(role, instance, key, root, metadata)
}

fn resolve_package_dependency<'a>(
    role: TargetRole,
    instance: &str,
    key: &str,
    owner: &'a Package,
    metadata: &'a Metadata,
) -> Result<&'a Package, SourceError> {
    let matching = owner
        .dependencies
        .iter()
        .filter(|dependency| dependency.name == key || dependency_key(dependency) == key)
        .collect::<Vec<_>>();
    let dependency = matching
        .iter()
        .find(|dependency| dependency.kind == DependencyKind::Normal)
        .copied()
        .or_else(|| matching.first().copied())
        .ok_or_else(|| SourceError::DependencyNotDeclared {
            role,
            instance: instance.to_owned(),
            key: key.to_owned(),
        })?;
    if dependency.kind != DependencyKind::Normal {
        return Err(SourceError::DependencyWrongKind {
            role,
            instance: instance.to_owned(),
            key: key.to_owned(),
            kind: dependency.kind.to_string(),
        });
    }

    let root_node = metadata
        .resolve
        .as_ref()
        .and_then(|resolve| resolve.nodes.iter().find(|node| node.id == owner.id));
    let node_dependency = root_node.and_then(|node| {
        node.deps.iter().find(|node_dependency| {
            node_dependency.name == key
                || node_dependency.name == dependency_key(dependency)
                || node_dependency.name == dependency.name
                || node_dependency.name == key.replace('-', "_")
        })
    });
    let package_id = node_dependency
        .map(|dependency| &dependency.pkg)
        .ok_or_else(|| SourceError::DependencyUnresolved {
            role,
            instance: instance.to_owned(),
            key: key.to_owned(),
        })?;
    metadata
        .packages
        .iter()
        .find(|package| package.id == *package_id)
        .ok_or_else(|| SourceError::DependencyUnresolved {
            role,
            instance: instance.to_owned(),
            key: key.to_owned(),
        })
}

fn selected_target(package: &Package, target: &Target) -> SelectedTarget {
    SelectedTarget {
        package_id: package.id.to_string(),
        package: package.name.to_string(),
        target: target.name.clone(),
        source_path: PathBuf::from(target.src_path.as_std_path()),
        required_features: target.required_features.clone(),
        feature_dependency: None,
    }
}

fn selected_dependency_target(
    package: &Package,
    target: &Target,
    dependency_key: &str,
) -> SelectedTarget {
    SelectedTarget {
        feature_dependency: Some(dependency_key.to_owned()),
        ..selected_target(package, target)
    }
}

fn dependency_key(dependency: &cargo_metadata::Dependency) -> &str {
    dependency.rename.as_deref().unwrap_or(&dependency.name)
}

fn package_source(package: &Package) -> PackageSource {
    match &package.source {
        None => PackageSource::Local {
            manifest_path: package.manifest_path.as_std_path().to_owned(),
        },
        Some(source) if source.repr.starts_with("git+") => PackageSource::Git {
            source: source.repr.clone(),
        },
        Some(source) if super::cargo::is_registry_source(&source.repr) => PackageSource::Registry {
            source: source.repr.clone(),
        },
        Some(source) => PackageSource::Other {
            source: source.repr.clone(),
        },
    }
}

fn enabled_features(
    metadata: &Metadata,
    package_id: &PackageId,
) -> std::collections::BTreeSet<String> {
    // The feature list is a resolve-node fact, but callers without the complete
    // graph still receive an empty set and the target's requirements remain
    // visible in the selected target metadata.
    metadata
        .resolve
        .as_ref()
        .and_then(|resolve| resolve.nodes.iter().find(|node| node.id == *package_id))
        .map(|node| {
            node.features
                .iter()
                .map(|feature| feature.as_ref().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn ensure_target_features(
    role: TargetRole,
    instance: &str,
    target: &SelectedTarget,
    package: &Package,
) -> Result<(), SourceError> {
    let missing = target
        .required_features
        .iter()
        .filter(|feature| !package.features.contains_key(feature.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(SourceError::UndefinedRequiredFeatures {
            role,
            instance: instance.to_owned(),
            binary: target.target.clone(),
            features: missing.join(", "),
        })
    }
}
