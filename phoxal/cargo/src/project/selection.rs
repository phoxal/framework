use std::collections::BTreeMap;
use std::path::PathBuf;

use cargo_metadata::{DependencyKind, Metadata, Package, PackageId, Target};

use crate::project::cargo;
use crate::project::document::{
    BrainSelection, ComponentDocument, RobotDocument, ServiceSelection, ValidateComponentDocument,
};
use crate::project::error::SourceError;
use crate::project::publication::{RuntimePackageRole, validate_runtime_package};
use crate::project::robot_api;

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

/// Resolves explicit composition entries against Cargo's already resolved graph.
///
/// This function never searches a registry and never performs fuzzy matching.
/// Services resolve through the generated `robot_api` dependency graph while
/// component fields remain direct root dependency keys.
pub fn resolve_sources(
    document: &RobotDocument,
    metadata: &Metadata,
    manifest_path: &std::path::Path,
) -> Result<SourceSelection, SourceError> {
    let RobotDocument::V0 {
        brain: authored_brain,
        services: authored_services,
        robot,
        ..
    } = document;
    let root = metadata.root_package().ok_or(SourceError::MissingBrain)?;
    let root_manifest = PathBuf::from(root.manifest_path.as_std_path());
    let expected_manifest = manifest_path
        .canonicalize()
        .unwrap_or_else(|_| manifest_path.to_owned());
    if root_manifest != expected_manifest {
        return Err(SourceError::MissingBrain);
    }

    let brain = resolve_brain(root, authored_brain.as_ref(), metadata)?;
    let supervisor = resolve_supervisor(root, metadata)?;
    let mut services = BTreeMap::new();
    for (instance, selection) in authored_services {
        let key = format!("service_{}", instance.replace('-', "_"));
        services.insert(
            instance.to_owned(),
            resolve_service(instance, &key, selection, root, metadata)?,
        );
    }

    let mut components = BTreeMap::new();
    for (instance, component) in &robot.components {
        let package = resolve_dependency(
            TargetRole::Component,
            instance,
            &component.component,
            root,
            metadata,
        )?;
        validate_dependency_role(
            TargetRole::Component,
            instance,
            &component.component,
            package,
            RuntimePackageRole::Component,
        )?;
        let driver =
            resolve_component_driver(instance, component, &component.component, package, metadata)?;
        let definition = load_component_definition(instance, &component.component, package)?;
        components.insert(
            instance.to_owned(),
            SelectedComponent {
                instance: instance.to_owned(),
                dependency_key: component.component.clone(),
                package_id: package.id.to_string(),
                package: package.name.to_string(),
                source: package_source(package),
                mount_site: component.mount_site.clone(),
                definition,
                driver,
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

fn load_component_definition(
    instance: &str,
    dependency_key: &str,
    package: &Package,
) -> Result<ComponentDocument, SourceError> {
    let root = PathBuf::from(package.manifest_path.as_std_path())
        .parent()
        .map(std::path::Path::to_path_buf)
        .ok_or_else(|| SourceError::InvalidPackageRole {
            role: TargetRole::Component,
            instance: instance.to_owned(),
            key: dependency_key.to_owned(),
            package: package.name.to_string(),
            message: "Cargo manifest has no package directory".to_owned(),
        })?;
    let path = root.join("component.yaml");
    let text = std::fs::read_to_string(&path).map_err(|error| SourceError::InvalidPackageRole {
        role: TargetRole::Component,
        instance: instance.to_owned(),
        key: dependency_key.to_owned(),
        package: package.name.to_string(),
        message: format!("cannot read {}: {error}", path.display()),
    })?;
    let definition: ComponentDocument =
        serde_yaml::from_str(&text).map_err(|error| SourceError::InvalidPackageRole {
            role: TargetRole::Component,
            instance: instance.to_owned(),
            key: dependency_key.to_owned(),
            package: package.name.to_string(),
            message: format!("cannot parse {}: {error}", path.display()),
        })?;
    definition
        .validate()
        .map_err(|message| SourceError::InvalidPackageRole {
            role: TargetRole::Component,
            instance: instance.to_owned(),
            key: dependency_key.to_owned(),
            package: package.name.to_string(),
            message: format!("{} is invalid: {message}", path.display()),
        })?;
    let ComponentDocument::V0 { model, .. } = &definition;
    let model = root.join(&model.file);
    if !model.is_file() {
        return Err(SourceError::InvalidPackageRole {
            role: TargetRole::Component,
            instance: instance.to_owned(),
            key: dependency_key.to_owned(),
            package: package.name.to_string(),
            message: format!(
                "native model entry {} is not a regular file",
                model.display()
            ),
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

fn resolve_component_driver(
    instance: &str,
    component: &crate::project::document::ComponentInstance,
    component_dependency_key: &str,
    component_package: &Package,
    metadata: &Metadata,
) -> Result<Option<SelectedDriver>, SourceError> {
    let Some(driver) = component.driver.as_ref() else {
        return Ok(None);
    };
    if !driver.is_object() {
        return Err(SourceError::DriverField {
            instance: instance.to_owned(),
            field: "driver".to_owned(),
            message: "must be a mapping when present".to_owned(),
        });
    }
    let dependency_key = driver_string(
        instance,
        driver,
        &["dependency", "implementation", "package"],
    )?;
    let binary_name = driver_string(instance, driver, &["binary", "target"])?;
    let (package, dependency_key) = match dependency_key {
        Some(key) => (
            resolve_package_dependency(
                TargetRole::Driver,
                instance,
                &key,
                component_package,
                metadata,
            )?,
            key,
        ),
        None => (component_package, component_dependency_key.to_owned()),
    };
    let binary = select_binary(
        TargetRole::Driver,
        instance,
        &dependency_key,
        package,
        binary_name.as_deref(),
    )?;
    ensure_target_features(TargetRole::Driver, instance, &binary, package)?;
    Ok(Some(SelectedDriver {
        dependency_key,
        package_id: package.id.to_string(),
        package: package.name.to_string(),
        source: package_source(package),
        binary,
    }))
}

fn driver_string(
    instance: &str,
    driver: &serde_json::Value,
    keys: &[&str],
) -> Result<Option<String>, SourceError> {
    let Some(object) = driver.as_object() else {
        return Ok(None);
    };
    for key in keys {
        if let Some(value) = object.get(*key) {
            let value = value.as_str().ok_or_else(|| SourceError::DriverField {
                instance: instance.to_owned(),
                field: (*key).to_owned(),
                message: "must be a string when present".to_owned(),
            })?;
            if value.is_empty() {
                return Err(SourceError::DriverField {
                    instance: instance.to_owned(),
                    field: (*key).to_owned(),
                    message: "must not be empty".to_owned(),
                });
            }
            return Ok(Some(value.to_owned()));
        }
    }
    Ok(None)
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

fn resolve_service(
    instance: &str,
    key: &str,
    selection: &ServiceSelection,
    root: &Package,
    metadata: &Metadata,
) -> Result<SelectedService, SourceError> {
    let api = resolve_dependency(
        TargetRole::Service,
        instance,
        robot_api::DEPENDENCY_KEY,
        root,
        metadata,
    )?;
    let package = resolve_package_dependency(TargetRole::Service, instance, key, api, metadata)?;
    validate_dependency_role(
        TargetRole::Service,
        instance,
        key,
        package,
        RuntimePackageRole::Service,
    )?;
    let mut library = package
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
    library.feature_dependency = None;
    let mut binary = select_binary(
        TargetRole::Service,
        instance,
        key,
        package,
        selection.binary.as_deref(),
    )?;
    binary.feature_dependency = None;
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
    resolve_package_dependency(role, instance, key, root, metadata)
}

fn validate_dependency_role(
    role: TargetRole,
    instance: &str,
    key: &str,
    package: &Package,
    expected: RuntimePackageRole,
) -> Result<(), SourceError> {
    let manifest = PathBuf::from(package.manifest_path.as_std_path());
    validate_runtime_package(&manifest, &package.name, expected).map_err(|error| {
        SourceError::InvalidPackageRole {
            role,
            instance: instance.to_owned(),
            key: key.to_owned(),
            package: package.name.to_string(),
            message: error.to_string(),
        }
    })
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
        .and_then(|resolve| resolve.nodes.iter().find(|node| node.id == owner.id));
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
    Ok(selected_dependency_target(package, target, key))
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
            manifest_path: PathBuf::from(package.manifest_path.as_std_path()),
        },
        Some(source) if source.repr.starts_with("git+") => PackageSource::Git {
            source: source.repr.clone(),
        },
        Some(source) if cargo::is_registry_source(&source.repr) => PackageSource::Registry {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `Package` from the JSON shape that `cargo metadata` emits so
    /// the resolution helpers can be exercised end-to-end without a live
    /// invocation. `cargo_metadata::Target` and `cargo_metadata::Package` are
    /// `#[non_exhaustive]`; deserializing the same shape cargo emits is the
    /// supported way to construct them in tests.
    fn service_package(name: &str, binary_required_features: &[&str]) -> Package {
        let library = serde_json::json!({
            "name": name,
            "kind": ["lib"],
            "crate_types": ["lib"],
            "required-features": [],
            "src_path": format!("services/{name}/lib.rs"),
            "edition": "2024",
            "doc": true,
            "doctest": true,
            "test": true,
        });
        let binary = serde_json::json!({
            "name": format!("phoxal-service-{name}"),
            "kind": ["bin"],
            "crate_types": ["bin"],
            "required-features": binary_required_features,
            "src_path": format!("services/{name}/src/main.rs"),
            "edition": "2024",
            "doc": true,
            "doctest": false,
            "test": false,
        });
        let package = serde_json::json!({
            "name": format!("phoxal-service-{name}"),
            "version": "0.68.0",
            "id": format!("path+http://example.invalid/{name}#0.68.0"),
            "license": null,
            "license_file": null,
            "description": null,
            "source": null,
            "dependencies": [],
            "targets": [library, binary],
            "features": binary_required_features
                .iter()
                .map(|feature| (feature.to_string(), Vec::<String>::new()))
                .collect::<BTreeMap<_, _>>(),
            "manifest_path": format!("services/{name}/Cargo.toml"),
            "categories": [],
            "keywords": [],
            "readme": null,
            "repository": null,
            "homepage": null,
            "documentation": null,
            "edition": "2024",
            "metadata": null,
            "links": null,
            "publish": null,
            "default_run": null,
            "rust_version": null,
            "authors": [],
        });
        serde_json::from_value(package).expect("service-shaped package fixture")
    }

    fn extra_binary(name: &str, required_features: &[&str]) -> cargo_metadata::Target {
        let target = serde_json::json!({
            "name": name,
            "kind": ["bin"],
            "crate_types": ["bin"],
            "required-features": required_features,
            "src_path": format!("services/motion/src/bin/{name}.rs"),
            "edition": "2024",
            "doc": true,
            "doctest": false,
            "test": false,
        });
        serde_json::from_value(target).expect("binary target fixture")
    }

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
    fn select_binary_picks_the_unique_target_when_none_is_requested() {
        let package = service_package("motion", &["runtime"]);
        let selected = select_binary(
            TargetRole::Service,
            "primary",
            "phoxal-service-motion",
            &package,
            None,
        )
        .expect("single binary target resolves by default");
        assert_eq!(selected.package, "phoxal-service-motion");
        assert_eq!(selected.target, "phoxal-service-motion");
        assert_eq!(selected.required_features, vec!["runtime".to_owned()]);
    }

    #[test]
    fn select_binary_matches_the_requested_target_when_others_exist() {
        let mut package = service_package("motion", &["runtime"]);
        package.targets.push(extra_binary(
            "phoxal-service-motion-headless",
            &["runtime", "headless"],
        ));
        let selected = select_binary(
            TargetRole::Service,
            "primary",
            "phoxal-service-motion",
            &package,
            Some("phoxal-service-motion-headless"),
        )
        .expect("explicit binary selection overrides the default");
        assert_eq!(selected.target, "phoxal-service-motion-headless");
        assert_eq!(
            selected.required_features,
            vec!["runtime".to_owned(), "headless".to_owned()]
        );
    }

    #[test]
    fn select_binary_rejects_when_multiple_binaries_exist_without_a_choice() {
        let mut package = service_package("motion", &["runtime"]);
        package
            .targets
            .push(extra_binary("phoxal-service-motion-alt", &[]));
        let error = select_binary(
            TargetRole::Service,
            "primary",
            "phoxal-service-motion",
            &package,
            None,
        )
        .expect_err("ambiguous binary without explicit selection must fail");
        assert!(
            matches!(error, SourceError::AmbiguousBinary { .. }),
            "expected AmbiguousBinary, got {error:?}"
        );
    }

    #[test]
    fn ensure_target_features_accepts_declared_features_before_build_activation() {
        let package = service_package("motion", &["runtime"]);
        let target = SelectedTarget {
            package_id: "phoxal-service-motion".to_owned(),
            package: "phoxal-service-motion".to_owned(),
            target: "phoxal-service-motion".to_owned(),
            source_path: PathBuf::from("services/motion/src/main.rs"),
            required_features: vec!["runtime".to_owned()],
            feature_dependency: Some("motion".to_owned()),
        };
        ensure_target_features(TargetRole::Service, "primary", &target, &package)
            .expect("feature is declared by the selected package");
    }

    #[test]
    fn ensure_target_features_reports_missing_required_features() {
        let package = service_package("motion", &["runtime"]);
        let target = SelectedTarget {
            package_id: "phoxal-service-motion".to_owned(),
            package: "phoxal-service-motion".to_owned(),
            target: "phoxal-service-motion".to_owned(),
            source_path: PathBuf::from("services/motion/src/main.rs"),
            required_features: vec!["runtime".to_owned(), "scenario".to_owned()],
            feature_dependency: Some("motion".to_owned()),
        };
        let error = ensure_target_features(TargetRole::Service, "primary", &target, &package)
            .expect_err("missing required feature must surface UndefinedRequiredFeatures");
        match error {
            SourceError::UndefinedRequiredFeatures { features, .. } => {
                assert_eq!(features, "scenario");
            }
            other => panic!("expected UndefinedRequiredFeatures, got {other:?}"),
        }
    }

    #[test]
    fn service_selection_default_has_no_source_or_binary_override() {
        let selection = ServiceSelection::default();
        assert!(selection.source.is_none());
        assert!(selection.binary.is_none());
    }
}
