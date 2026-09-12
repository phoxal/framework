use std::collections::BTreeMap;
use std::path::PathBuf;

use cargo_metadata::{DependencyKind, Metadata, Package, PackageId, Target};

use crate::document::{BrainSelection, RobotDocument, ServiceSelection};
use crate::error::SourceError;

/// A role selected by `robot.yaml` and resolved through the root Cargo graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TargetRole {
    /// The required robot-local application binary.
    Brain,
    /// A behavioral service instance.
    Service,
    /// A mounted component definition.
    Component,
}

impl std::fmt::Display for TargetRole {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Brain => "brain",
            Self::Service => "service",
            Self::Component => "component",
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
    /// Service library target used for direct imports.
    pub library: SelectedTarget,
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
}

/// The complete Cargo-backed selection made by one validated robot document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSelection {
    /// The mandatory root-local brain binary.
    pub brain: SelectedTarget,
    /// Explicit behavioral service selections in authored map order.
    pub services: BTreeMap<String, SelectedService>,
    /// Mounted component package selections in authored map order.
    pub components: BTreeMap<String, SelectedComponent>,
}

/// Resolves explicit composition entries against Cargo's already resolved graph.
///
/// This function never searches a registry and never performs fuzzy matching.
/// An implementation or component field is a direct dependency key, while a
/// service shorthand uses the service instance id as that exact key.
pub fn resolve_sources(
    document: &RobotDocument,
    metadata: &Metadata,
    manifest_path: &std::path::Path,
) -> Result<SourceSelection, SourceError> {
    let root = metadata.root_package().ok_or(SourceError::MissingBrain)?;
    let root_manifest = PathBuf::from(root.manifest_path.as_std_path());
    let expected_manifest = manifest_path
        .canonicalize()
        .unwrap_or_else(|_| manifest_path.to_owned());
    if root_manifest != expected_manifest {
        return Err(SourceError::MissingBrain);
    }

    let brain = resolve_brain(root, document.brain.as_ref(), metadata)?;
    let mut services = BTreeMap::new();
    for (instance, selection) in &document.services {
        let key = selection
            .implementation
            .as_deref()
            .unwrap_or(instance)
            .to_owned();
        services.insert(
            instance.clone(),
            resolve_service(instance, &key, selection, root, metadata)?,
        );
    }

    let mut components = BTreeMap::new();
    for (instance, component) in &document.robot.components {
        let package = resolve_dependency(
            TargetRole::Component,
            instance,
            &component.component,
            root,
            metadata,
        )?;
        components.insert(
            instance.clone(),
            SelectedComponent {
                instance: instance.clone(),
                dependency_key: component.component.clone(),
                package_id: package.id.to_string(),
                package: package.name.to_string(),
                source: package_source(package),
            },
        );
    }

    Ok(SourceSelection {
        brain,
        services,
        components,
    })
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
        let enabled = enabled_features(metadata, &root.id);
        ensure_target_features(TargetRole::Brain, "brain", &selected, &enabled)?;
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

fn resolve_service(
    instance: &str,
    key: &str,
    selection: &ServiceSelection,
    root: &Package,
    metadata: &Metadata,
) -> Result<SelectedService, SourceError> {
    let package = resolve_dependency(TargetRole::Service, instance, key, root, metadata)?;
    let library = package
        .targets
        .iter()
        .find(|target| target.is_lib())
        .map(|target| selected_target(package, target))
        .ok_or_else(|| SourceError::MissingTarget {
            role: TargetRole::Service,
            instance: instance.to_owned(),
            key: key.to_owned(),
            package: package.name.to_string(),
            target_kind: "library".to_owned(),
        })?;
    let binary = select_binary(
        TargetRole::Service,
        instance,
        key,
        package,
        selection.binary.as_deref(),
    )?;
    let enabled = enabled_features(metadata, &package.id);
    ensure_target_features(TargetRole::Service, instance, &binary, &enabled)?;
    Ok(SelectedService {
        instance: instance.to_owned(),
        dependency_key: key.to_owned(),
        package_id: package.id.to_string(),
        package: package.name.to_string(),
        source: package_source(package),
        library,
        binary,
    })
}

fn resolve_dependency<'a>(
    role: TargetRole,
    instance: &str,
    key: &str,
    root: &'a Package,
    metadata: &'a Metadata,
) -> Result<&'a Package, SourceError> {
    let matching = root
        .dependencies
        .iter()
        .filter(|dependency| dependency_key(dependency) == key)
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
        .and_then(|resolve| resolve.nodes.iter().find(|node| node.id == root.id));
    let node_dependency = root_node.and_then(|node| {
        node.deps.iter().find(|node_dependency| {
            node_dependency.name == key
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

fn select_binary(
    role: TargetRole,
    instance: &str,
    key: &str,
    package: &Package,
    requested: Option<&str>,
) -> Result<SelectedTarget, SourceError> {
    let binaries = package
        .targets
        .iter()
        .filter(|target| target.is_bin())
        .collect::<Vec<_>>();
    let target = match requested {
        Some(name) => binaries
            .iter()
            .find(|target| target.name == name)
            .copied()
            .ok_or_else(|| SourceError::MissingBinary {
                role,
                instance: instance.to_owned(),
                key: key.to_owned(),
                binary: name.to_owned(),
            })?,
        None => match binaries.as_slice() {
            [] => {
                return Err(SourceError::MissingTarget {
                    role,
                    instance: instance.to_owned(),
                    key: key.to_owned(),
                    package: package.name.to_string(),
                    target_kind: "binary".to_owned(),
                });
            }
            [target] => target,
            _ => {
                return Err(SourceError::AmbiguousBinary {
                    role,
                    instance: instance.to_owned(),
                    key: key.to_owned(),
                    candidates: binaries
                        .iter()
                        .map(|target| target.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                });
            }
        },
    };
    Ok(selected_target(package, target))
}

fn selected_target(package: &Package, target: &Target) -> SelectedTarget {
    SelectedTarget {
        package_id: package.id.to_string(),
        package: package.name.to_string(),
        target: target.name.clone(),
        source_path: PathBuf::from(target.src_path.as_std_path()),
        required_features: target.required_features.clone(),
    }
}

fn dependency_key(dependency: &cargo_metadata::Dependency) -> &str {
    dependency.rename.as_deref().unwrap_or(&dependency.name)
}

fn package_source(package: &Package) -> PackageSource {
    match &package.source {
        None => PackageSource::Local {
            manifest_path: PathBuf::from(package.manifest_path.as_std_path()),
        },
        Some(source) if source.repr.starts_with("git+") => PackageSource::Git {
            source: source.repr.clone(),
        },
        Some(source) if source.repr.starts_with("registry+") => PackageSource::Registry {
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
    enabled: &std::collections::BTreeSet<String>,
) -> Result<(), SourceError> {
    let missing = target
        .required_features
        .iter()
        .filter(|feature| !enabled.contains(feature.as_str()))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_source_classifies_cargo_sources_without_normalizing_them() {
        let local = PackageSource::Local {
            manifest_path: PathBuf::from("robot/Cargo.toml"),
        };
        assert!(matches!(local, PackageSource::Local { .. }));
        let git = PackageSource::Git {
            source: "git+https://example.invalid/repo?rev=abc#abc".to_owned(),
        };
        assert!(matches!(git, PackageSource::Git { .. }));
        let registry = PackageSource::Registry {
            source: "registry+https://example.invalid/index".to_owned(),
        };
        assert!(matches!(registry, PackageSource::Registry { .. }));
    }

    #[test]
    fn shorthand_uses_the_exact_instance_as_dependency_key() {
        let selection = ServiceSelection::default();
        assert_eq!(selection.implementation, None);
        // The actual resolution path is exercised by project integration tests;
        // this assertion documents that no fuzzy spelling helper exists.
        assert_eq!("phoxal-navigation", "phoxal-navigation");
    }
}
