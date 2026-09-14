//! Project discovery, authored composition validation, and Cargo orchestration.
//!
//! This crate owns the source-development boundary used by `cargo-phoxal`.
//! It deliberately does not depend on the Runtime SDK, supervisor, service
//! implementations, simulator, registry client, or the archived CLI.

pub mod artifact;
mod bundle;
mod cargo;
mod discovery;
mod document;
mod error;
mod file_lock;
mod preparation;
mod publication;
pub mod scenario;
mod selection;
mod simulation;
mod submission;
mod validation;

pub use artifact::{
    ArtifactContract, ArtifactSummary, DescriptorInfo, DescriptorSummary, InputKind, InputRecord,
    OutputKind, OutputRecord, PortKind, PortSignature, RuntimeRecord, validate_connected_endpoints,
};
pub use bundle::{
    BUNDLE_SCHEMA, BundleActuationBinding, BundleArtifact, BundleCargoInvocation, BundleComponent,
    BundleEnvironment, BundleExecutable, BundleFile, BundleGitSource, BundleManifest,
    BundleModelClosure, BundleNativeTool, BundlePackage, BundleProvenance, BundleResource,
    BundleSimulation, BundleSimulationProvider, BundleSource, BundleSourceClosure,
    BundleSourceFile, BundleSourceKind, BundleSupervisor, BundleToolchain, CompiledBundle,
    LocalIdentity, LocalRunPlan, LocalSimulationPlan, SimulationModelFacts,
    SimulationProviderBinding, digest_source_files,
};
pub use cargo::{CargoOperation, CargoOptions, CargoOutput, CargoSelection, LockMode};
pub use discovery::ProjectLayout;
pub use document::{
    BrainSelection, COMPONENT_SCHEMA, CapabilityDeclaration, ComponentDocument, ComponentInstance,
    ComponentModel, ConnectionSources, NativeTarget, NativeTargetKind, PortReference,
    PortReferenceError, ROBOT_SCHEMA, RobotDocument, RobotSection, ServiceSelection, is_identifier,
};
pub use error::{
    DiscoveryError, Error, PublicationError, SourceError, ValidationError, ValidationErrors,
};
pub use preparation::{PreparationChange, ScenarioPreparationChange};
pub use scenario::{DiscoveredScenario, generate_harness_source};
pub use publication::{
    PUBLICATION_SCHEMA, PublicationFile, PublicationKind, PublicationOptions, PublicationResult,
    PublicationSourceProvenance, prepare_publication,
};
pub use selection::{
    PackageSource, SelectedComponent, SelectedDriver, SelectedService, SelectedTarget,
    SourceSelection, TargetRole, resolve_sources,
};
pub use simulation::{
    DEFAULT_SIMULATOR_BINARY, DEFAULT_SIMULATOR_PACKAGE, DEFAULT_SIMULATOR_VERSION,
    SIMULATION_PROTOCOL, SimulationBound, SimulationCleanup, SimulationPresentation,
    SimulationRunOptions, SimulationRunReport, SimulatorArtifactSummary,
};
pub use submission::{DeviceAuthorization, SubmissionResult, submit_publication};

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

/// A discovered project with its authored document and root Cargo manifest.
#[derive(Debug, Clone)]
pub struct Project {
    layout: ProjectLayout,
    document: RobotDocument,
}

impl Project {
    /// Discovers and loads a project from a directory, file, or nested source path.
    pub fn discover(start: impl AsRef<Path>) -> Result<Self, Error> {
        let layout = ProjectLayout::discover(start)?;
        Self::from_layout(layout)
    }

    /// Loads a project from already discovered canonical paths.
    pub fn from_layout(layout: ProjectLayout) -> Result<Self, Error> {
        let text = std::fs::read_to_string(layout.robot_manifest()).map_err(|source| {
            Error::ReadRobot {
                path: layout.robot_manifest().to_owned(),
                source,
            }
        })?;
        let document = document::parse_and_validate(&text, layout.robot_manifest())?;
        let manifest_text = std::fs::read_to_string(layout.cargo_manifest()).map_err(|source| {
            Error::ReadManifest {
                path: layout.cargo_manifest().to_owned(),
                source,
            }
        })?;
        let manifest = toml::from_str::<toml::Value>(&manifest_text).map_err(|source| {
            Error::ParseManifest {
                path: layout.cargo_manifest().to_owned(),
                source,
            }
        })?;
        if !manifest.get("package").is_some_and(toml::Value::is_table) {
            return Err(Error::VirtualManifest {
                path: layout.cargo_manifest().to_owned(),
            });
        }
        Ok(Self { layout, document })
    }

    /// Returns the canonical source layout.
    #[must_use]
    pub fn layout(&self) -> &ProjectLayout {
        &self.layout
    }

    /// Returns the parsed authored document.
    #[must_use]
    pub fn document(&self) -> &RobotDocument {
        &self.document
    }

    /// Prepares the authored Cargo graph and resolves all explicit sources.
    ///
    /// Ordinary preparation may add the known mandatory supervisor dependency
    /// and let Cargo update the owning workspace lock.  Locked and frozen
    /// preparation refuses that addition before changing either file.
    pub fn prepare(&self, options: &CargoOptions) -> Result<PreparedProject, Error> {
        self.prepare_with(options, |_, _, _| Ok(()))
            .map(|(prepared, ())| prepared)
    }

    /// Run only the scenario preparation step. Useful for `cargo phoxal
    /// simulation scenario list/run` to materialise the test target and
    /// generated harness without a full Cargo graph resolution.
    pub fn prepare_scenarios(
        &self,
        options: &CargoOptions,
    ) -> Result<Vec<ScenarioPreparationChange>, Error> {
        preparation::prepare_scenario_target(&self.layout, options)
    }

    /// Stage authored inputs before resolving, allowing an explicit update to
    /// repair a lockfile that cannot resolve the newly authored dependencies.
    fn prepare_with<T>(
        &self,
        options: &CargoOptions,
        before_resolution: impl FnOnce(&Path, &Path, Option<&Path>) -> Result<T, Error>,
    ) -> Result<(PreparedProject, T), Error> {
        let mut preparation = preparation::ensure_required_dependencies(&self.layout, options)?;
        // Scenario preparation runs *inside* the same workspace lock and
        // manifest snapshot. Any later failure restores the manifest to its
        // pre-supervisor state, which also undoes the scenario additions.
        if let Err(error) = preparation::prepare_scenario_target_in_transaction(
            &self.layout,
            options,
            &mut preparation,
        ) {
            return rollback_preparation(preparation, error);
        }
        let local_source = match publication::prepare_local_project_source(&self.layout) {
            Ok(source) => source,
            Err(error) => return rollback_preparation(preparation, error),
        };
        let metadata_manifest = local_source
            .as_ref()
            .map_or_else(|| self.layout.cargo_manifest(), |source| source.manifest());
        let metadata_workdir = local_source
            .as_ref()
            .map_or_else(|| self.layout.root(), |source| source.cargo_workdir());
        let metadata_target = local_source.as_ref().map(|source| source.target_dir());
        let result = match before_resolution(metadata_manifest, metadata_workdir, metadata_target) {
            Ok(result) => result,
            Err(error) => return rollback_preparation(preparation, error),
        };
        let metadata = match cargo::load_metadata_at(
            metadata_manifest,
            metadata_workdir,
            metadata_target,
            options,
        ) {
            Ok(metadata) => metadata,
            Err(error) => return rollback_preparation(preparation, error),
        };
        if let Err(error) = reject_direct_targetless_git(&metadata) {
            return rollback_preparation(preparation, error);
        }
        let cargo_sources = match resolve_sources(&self.document, &metadata, metadata_manifest) {
            Ok(sources) => sources,
            Err(error) => return rollback_preparation(preparation, error.into()),
        };
        let cargo_root_package = match metadata.root_package().cloned() {
            Some(package) => package,
            None => return rollback_preparation(preparation, SourceError::MissingBrain.into()),
        };
        if let Some(source) = &local_source
            && let Err(error) = source.sync_lock()
        {
            return rollback_preparation(preparation, error);
        }
        let (logical_metadata, root_package, sources) = if let Some(source) = &local_source {
            let logical_metadata = match source.logical_metadata(&metadata) {
                Ok(metadata) => metadata,
                Err(error) => return rollback_preparation(preparation, error),
            };
            let root_package = match logical_metadata.root_package().cloned() {
                Some(package) => package,
                None => {
                    return rollback_preparation(preparation, SourceError::MissingBrain.into());
                }
            };
            (
                logical_metadata,
                root_package,
                logical_source_selection(&cargo_sources, source),
            )
        } else {
            (
                metadata.clone(),
                cargo_root_package.clone(),
                cargo_sources.clone(),
            )
        };
        let preparation_changes = preparation.commit();
        let logical_workspace_root = local_source.as_ref().map_or_else(
            || logical_metadata.workspace_root.as_std_path().to_owned(),
            |source| source.logical_workspace_root().to_owned(),
        );
        Ok((
            PreparedProject {
                layout: self.layout.clone(),
                document: self.document.clone(),
                metadata: logical_metadata,
                cargo_metadata: metadata,
                root_package,
                cargo_root_package,
                sources,
                cargo_sources,
                preparation_changes,
                local_source: local_source.map(Arc::new),
                logical_workspace_root,
            },
            result,
        ))
    }

    /// Runs an explicit Cargo update and validates the resulting Phoxal graph.
    ///
    /// The update is deliberately a project operation rather than a bare
    /// Cargo passthrough.  Cargo first resolves the caller's permitted update
    /// request, then a fresh metadata preparation and exact contract check
    /// must succeed before this method reports success.  Update-only trailing
    /// Cargo arguments are not replayed into the validation builds.
    pub fn update(&self, options: &CargoOptions) -> Result<Vec<CargoOutput>, Error> {
        cargo::validate_update_options(options)?;
        let (prepared, output) = self.prepare_with(options, |manifest, workdir, target| {
            cargo::update(manifest, workdir, target, options)
        })?;
        let validation_options = CargoOptions {
            cargo_args: Vec::new(),
            test_args: Vec::new(),
            selection: CargoSelection::default(),
            ..options.clone()
        };
        prepared.check(&validation_options)?;
        Ok(vec![output])
    }

    /// Provisions the independent simulator application, probes its native
    /// model contract, builds the simulation bundle, and runs one finite
    /// simulation with bounded supervisor cleanup.
    pub fn run_simulation(
        &self,
        options: &CargoOptions,
        request: &SimulationRunOptions,
    ) -> Result<SimulationRunReport, Error> {
        simulation::run(self, options, request)
    }
}

fn reject_direct_targetless_git(metadata: &cargo_metadata::Metadata) -> Result<(), Error> {
    let Some(resolve) = metadata.resolve.as_ref() else {
        return Ok(());
    };
    let Some(root) = resolve.root.as_ref() else {
        return Ok(());
    };
    let Some(node) = resolve.nodes.iter().find(|node| node.id == *root) else {
        return Ok(());
    };
    for dependency in &node.dependencies {
        let Some(package) = metadata
            .packages
            .iter()
            .find(|package| package.id == *dependency)
        else {
            continue;
        };
        if package
            .source
            .as_ref()
            .is_some_and(|source| source.repr.starts_with("git+"))
            && package.targets.is_empty()
        {
            return Err(SourceError::UnsupportedTargetlessGit {
                package: package.name.to_string(),
            }
            .into());
        }
    }
    Ok(())
}

fn rollback_preparation<T>(
    preparation: preparation::ManifestTransaction,
    error: Error,
) -> Result<T, Error> {
    match preparation.rollback() {
        Ok(()) => Err(error),
        Err(restore) => Err(restore),
    }
}

fn logical_source_selection(
    sources: &SourceSelection,
    source: &publication::LocalProjectSource,
) -> SourceSelection {
    SourceSelection {
        brain: logical_target(&sources.brain, source),
        supervisor: logical_target(&sources.supervisor, source),
        services: sources
            .services
            .iter()
            .map(|(instance, service)| {
                (
                    instance.clone(),
                    SelectedService {
                        instance: service.instance.clone(),
                        dependency_key: service.dependency_key.clone(),
                        package_id: source.logical_package_id(&service.package_id),
                        package: service.package.clone(),
                        source: logical_package_source(&service.source, source),
                        library: logical_target(&service.library, source),
                        binary: logical_target(&service.binary, source),
                    },
                )
            })
            .collect(),
        components: sources
            .components
            .iter()
            .map(|(instance, component)| {
                (
                    instance.clone(),
                    SelectedComponent {
                        instance: component.instance.clone(),
                        dependency_key: component.dependency_key.clone(),
                        package_id: source.logical_package_id(&component.package_id),
                        package: component.package.clone(),
                        source: logical_package_source(&component.source, source),
                        mount_site: component.mount_site.clone(),
                        definition: component.definition.clone(),
                        driver: component.driver.as_ref().map(|driver| SelectedDriver {
                            dependency_key: driver.dependency_key.clone(),
                            package_id: source.logical_package_id(&driver.package_id),
                            package: driver.package.clone(),
                            source: logical_package_source(&driver.source, source),
                            binary: logical_target(&driver.binary, source),
                        }),
                    },
                )
            })
            .collect(),
    }
}

fn logical_target(
    target: &SelectedTarget,
    source: &publication::LocalProjectSource,
) -> SelectedTarget {
    SelectedTarget {
        package_id: source.logical_package_id(&target.package_id),
        package: target.package.clone(),
        target: target.target.clone(),
        source_path: source.logical_path(&target.source_path),
        required_features: target.required_features.clone(),
    }
}

fn logical_package_source(
    package_source: &PackageSource,
    source: &publication::LocalProjectSource,
) -> PackageSource {
    match package_source {
        PackageSource::Local { manifest_path } => PackageSource::Local {
            manifest_path: source.logical_path(manifest_path),
        },
        PackageSource::Git { source: value } => PackageSource::Git {
            source: value.clone(),
        },
        PackageSource::Registry { source: value } => PackageSource::Registry {
            source: value.clone(),
        },
        PackageSource::Other { source: value } => PackageSource::Other {
            source: value.clone(),
        },
    }
}

/// A validated project and the Cargo graph used for its selected sources.
#[derive(Debug, Clone)]
pub struct PreparedProject {
    layout: ProjectLayout,
    document: RobotDocument,
    metadata: cargo_metadata::Metadata,
    cargo_metadata: cargo_metadata::Metadata,
    root_package: cargo_metadata::Package,
    cargo_root_package: cargo_metadata::Package,
    sources: SourceSelection,
    cargo_sources: SourceSelection,
    preparation_changes: Vec<PreparationChange>,
    local_source: Option<Arc<publication::LocalProjectSource>>,
    logical_workspace_root: PathBuf,
}

impl PreparedProject {
    /// Returns the project layout used for this preparation.
    #[must_use]
    pub fn layout(&self) -> &ProjectLayout {
        &self.layout
    }

    /// Returns the validated authored document.
    #[must_use]
    pub fn document(&self) -> &RobotDocument {
        &self.document
    }

    /// Returns Cargo's complete metadata graph.
    #[must_use]
    pub fn metadata(&self) -> &cargo_metadata::Metadata {
        &self.metadata
    }

    pub(crate) fn cargo_metadata(&self) -> &cargo_metadata::Metadata {
        &self.cargo_metadata
    }

    /// Returns the workspace root owning the retained Cargo.lock.
    #[must_use]
    pub fn cargo_workspace_root(&self) -> &Path {
        &self.logical_workspace_root
    }

    /// Returns the logical root Cargo.lock path.
    #[must_use]
    pub fn cargo_lock(&self) -> PathBuf {
        self.layout.cargo_lock(self.cargo_workspace_root())
    }

    /// Returns the root package selected as the mandatory brain source.
    #[must_use]
    pub fn root_package(&self) -> &cargo_metadata::Package {
        &self.root_package
    }

    pub(crate) fn cargo_root_package(&self) -> &cargo_metadata::Package {
        &self.cargo_root_package
    }

    /// Returns all explicit Cargo-backed source selections.
    #[must_use]
    pub fn sources(&self) -> &SourceSelection {
        &self.sources
    }

    pub(crate) fn cargo_sources(&self) -> &SourceSelection {
        &self.cargo_sources
    }

    /// Returns the automatic dependency additions made during preparation.
    #[must_use]
    pub fn preparation_changes(&self) -> &[PreparationChange] {
        &self.preparation_changes
    }

    pub(crate) fn cargo_manifest_path(&self) -> &Path {
        self.local_source
            .as_ref()
            .map_or_else(|| self.layout.cargo_manifest(), |source| source.manifest())
    }

    pub(crate) fn cargo_workdir(&self) -> &Path {
        self.local_source
            .as_ref()
            .map_or_else(|| self.layout.root(), |source| source.cargo_workdir())
    }

    pub(crate) fn cargo_target_dir(&self) -> Option<&Path> {
        self.local_source.as_ref().map(|source| source.target_dir())
    }

    pub(crate) fn authored_source_root(&self, staged: &Path) -> PathBuf {
        self.local_source.as_ref().map_or_else(
            || staged.to_owned(),
            |source| source.authored_source_root(staged),
        )
    }

    pub(crate) fn sync_staged_lock(&self) -> Result<(), Error> {
        if let Some(source) = &self.local_source {
            source.sync_lock()?
        }
        Ok(())
    }

    /// Runs one supported Cargo source-development operation.
    pub fn run(
        &self,
        operation: CargoOperation,
        options: &CargoOptions,
    ) -> Result<Vec<CargoOutput>, Error> {
        let outputs = cargo::run(self, operation, options);
        let sync = self.sync_staged_lock();
        match (outputs, sync) {
            (Ok(outputs), Ok(())) => Ok(outputs),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(error)) | (Err(_), Err(error)) => Err(error),
        }
    }

    /// Runs Cargo check and validates the exact compiled Runtime contracts and
    /// authored configuration for every selected execution target.
    pub fn check(&self, options: &CargoOptions) -> Result<Vec<CargoOutput>, Error> {
        let outputs = self.run(CargoOperation::Check, options)?;
        validation::validate_selected_contracts(self, options)?;
        self.build_supervisor(options)?;
        Ok(outputs)
    }

    /// Builds and atomically publishes the complete selected executable bundle.
    ///
    /// The output is a source-side compiled directory containing the selected
    /// brain, service, and component-driver binaries plus the exact supervisor
    /// executable and inspectable manifest and provenance records.
    pub fn build_bundle(
        &self,
        options: &CargoOptions,
        output: impl AsRef<Path>,
    ) -> Result<CompiledBundle, Error> {
        let build_inputs = bundle::capture_build_inputs(self)?;
        bundle::assemble_with_inputs(self, options, output, Some(&build_inputs), None)
    }

    /// Builds the exact supervisor binary selected through the root Cargo
    /// graph and returns Cargo's reported executable path.
    pub fn build_supervisor(&self, options: &CargoOptions) -> Result<PathBuf, Error> {
        let build_inputs = bundle::capture_build_inputs(self)?;
        let output = cargo::build_target(self, &self.cargo_sources.supervisor, options)?;
        bundle::verify_build_inputs(self, &build_inputs)?;
        let executable = cargo::artifact_path(&output.stdout, &self.cargo_sources.supervisor)?;
        let metadata =
            std::fs::symlink_metadata(&executable).map_err(|source| Error::ArtifactFile {
                path: executable.clone(),
                source,
            })?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(Error::ArtifactInvalid {
                path: executable,
                message: "Cargo reported a non-regular supervisor executable".to_owned(),
            });
        }
        Ok(executable)
    }

    /// Builds an immutable bundle and launches its selected supervisor in the
    /// isolated local namespace.
    pub fn run_local(
        &self,
        options: &CargoOptions,
        output: impl AsRef<Path>,
    ) -> Result<CompiledBundle, Error> {
        let bundle = self.build_bundle(options, output)?;
        let supervisor = bundle.executable("supervisor");
        let status = Command::new(&supervisor)
            .arg(bundle.root())
            .args(["--scope", "local", "--supervisor-id", "local"])
            .status()
            .map_err(|source| Error::SupervisorLaunch {
                message: format!("cannot start {}: {source}", supervisor.display()),
            })?;
        if !status.success() {
            let status = status.code().map_or_else(
                || "terminated by signal".to_owned(),
                |code| code.to_string(),
            );
            return Err(Error::SupervisorLaunch {
                message: format!("{} exited with status {status}", supervisor.display()),
            });
        }
        Ok(bundle)
    }

    /// Returns the default bundle path under Cargo's target directory.
    #[must_use]
    pub fn default_bundle_path(&self) -> PathBuf {
        self.metadata
            .target_directory
            .as_std_path()
            .join("phoxal")
            .join(&self.document.robot.id)
            .join("bundle")
    }

    /// Prepares a local hardware launch without claiming process readiness.
    pub fn local_run_plan(
        &self,
        options: &CargoOptions,
        output: impl AsRef<Path>,
        scope: impl Into<String>,
        supervisor_id: impl Into<String>,
    ) -> Result<LocalRunPlan, Error> {
        let identity = LocalIdentity::new(scope, supervisor_id)?;
        let bundle = self.build_bundle(options, output)?;
        Ok(LocalRunPlan { bundle, identity })
    }

    /// Prepares a local simulation launch from facts admitted by the
    /// independent native application.
    pub fn local_simulation_plan(
        &self,
        options: &CargoOptions,
        output: impl AsRef<Path>,
        scope: impl Into<String>,
        supervisor_id: impl Into<String>,
        facts: &SimulationModelFacts,
    ) -> Result<LocalSimulationPlan, Error> {
        let identity = LocalIdentity::new(scope, supervisor_id)?;
        let bundle = self.build_simulation_bundle(options, output, facts)?;
        Ok(LocalSimulationPlan { bundle, identity })
    }

    /// Builds and publishes a bundle carrying the complete controlled
    /// simulation contract supplied by the independent native application.
    pub fn build_simulation_bundle(
        &self,
        options: &CargoOptions,
        output: impl AsRef<Path>,
        facts: &SimulationModelFacts,
    ) -> Result<CompiledBundle, Error> {
        let build_inputs = bundle::capture_build_inputs(self)?;
        bundle::assemble_with_inputs(self, options, output, Some(&build_inputs), Some(facts))
    }

    pub(crate) fn assembly_targets(&self) -> Vec<(String, &SelectedTarget)> {
        let mut targets = vec![(String::from("brain"), &self.cargo_sources.brain)];
        targets.extend(
            self.cargo_sources
                .services
                .iter()
                .map(|(instance, service)| (instance.clone(), &service.binary)),
        );
        targets.extend(
            self.cargo_sources
                .components
                .iter()
                .filter_map(|(instance, component)| {
                    component
                        .driver
                        .as_ref()
                        .map(|driver| (instance.clone(), &driver.binary))
                }),
        );
        targets
    }

    pub(crate) fn execution_targets(&self) -> Vec<&SelectedTarget> {
        let mut targets = vec![&self.cargo_sources.brain];
        targets.extend(
            self.cargo_sources
                .services
                .values()
                .map(|service| &service.binary),
        );
        targets.extend(
            self.cargo_sources
                .components
                .values()
                .filter_map(|component| component.driver.as_ref().map(|driver| &driver.binary)),
        );
        targets
    }

    pub(crate) fn executable_role(&self, instance: &str) -> String {
        if instance == "brain" {
            return "brain".to_owned();
        }
        if self.cargo_sources.services.contains_key(instance) {
            return "service".to_owned();
        }
        if self
            .cargo_sources
            .components
            .get(instance)
            .and_then(|component| component.driver.as_ref())
            .is_some()
        {
            return "driver".to_owned();
        }
        "execution".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minimal_root_package_is_discovered_and_validated() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = tempfile::tempdir()?;
        std::fs::write(
            directory.path().join("robot.yaml"),
            "schema: phoxal/robot/v0\nrobot:\n  id: rover\n  model: model.xml\n  components: {}\nservices: {}\nconnections: {}\n",
        )?;
        std::fs::write(
            directory.path().join("Cargo.toml"),
            "[package]\nname = 'rover'\nversion = '0.1.0'\nedition = '2024'\n",
        )?;
        let project = Project::discover(directory.path())?;
        assert_eq!(project.document().robot.id, "rover");
        assert_eq!(project.layout().root(), directory.path().canonicalize()?);
        Ok(())
    }

    #[test]
    fn virtual_manifest_is_not_a_robot_brain_root() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        std::fs::write(
            directory.path().join("robot.yaml"),
            "robot:\n  id: rover\n  components: {}\n",
        )?;
        std::fs::write(
            directory.path().join("Cargo.toml"),
            "[workspace]\nmembers = []\n",
        )?;
        let error = Project::discover(directory.path()).expect_err("virtual root is invalid");
        assert!(matches!(error, Error::VirtualManifest { .. }));
        Ok(())
    }
}
