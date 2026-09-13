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
mod preparation;
mod publication;
mod selection;
mod submission;
mod validation;

pub use artifact::{
    ArtifactContract, ArtifactSummary, DescriptorInfo, DescriptorSummary, InputKind, InputRecord,
    OutputKind, OutputRecord, PortKind, PortSignature, RuntimeRecord, validate_connected_endpoints,
};
pub use bundle::{
    BUNDLE_SCHEMA, BundleArtifact, BundleComponent, BundleExecutable, BundleFile, BundleManifest,
    BundleModelClosure, BundlePackage, BundleProvenance, BundleResource, CompiledBundle,
    LocalIdentity, LocalRunPlan, LocalSimulationPlan,
};
pub use cargo::{CargoOperation, CargoOptions, CargoOutput, LockMode};
pub use discovery::ProjectLayout;
pub use document::{
    BrainSelection, ComponentInstance, ConnectionSources, PortReference, PortReferenceError,
    ROBOT_SCHEMA, RobotDocument, RobotSection, ServiceSelection, is_identifier,
};
pub use error::{
    DiscoveryError, Error, PublicationError, SourceError, ValidationError, ValidationErrors,
};
pub use preparation::PreparationChange;
pub use publication::{
    PUBLICATION_SCHEMA, PublicationFile, PublicationKind, PublicationOptions, PublicationResult,
    prepare_publication,
};
pub use selection::{
    PackageSource, SelectedComponent, SelectedDriver, SelectedService, SelectedTarget,
    SourceSelection, TargetRole, resolve_sources,
};
pub use submission::{DeviceAuthorization, SubmissionResult, submit_publication};

use std::path::{Path, PathBuf};
use std::process::Command;

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
        let preparation = preparation::ensure_required_dependencies(&self.layout, options)?;
        let metadata = match cargo::load_metadata(self.layout.cargo_manifest(), options) {
            Ok(metadata) => metadata,
            Err(error) => return rollback_preparation(preparation, error),
        };
        let sources = match resolve_sources(&self.document, &metadata, self.layout.cargo_manifest())
        {
            Ok(sources) => sources,
            Err(error) => return rollback_preparation(preparation, error.into()),
        };
        let root_package = match metadata.root_package().cloned() {
            Some(package) => package,
            None => return rollback_preparation(preparation, SourceError::MissingBrain.into()),
        };
        Ok(PreparedProject {
            layout: self.layout.clone(),
            document: self.document.clone(),
            metadata,
            root_package,
            sources,
            preparation,
        })
    }
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

/// A validated project and the Cargo graph used for its selected sources.
#[derive(Debug, Clone)]
pub struct PreparedProject {
    layout: ProjectLayout,
    document: RobotDocument,
    metadata: cargo_metadata::Metadata,
    root_package: cargo_metadata::Package,
    sources: SourceSelection,
    preparation: preparation::ManifestTransaction,
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

    /// Returns the workspace root owning the retained Cargo.lock.
    #[must_use]
    pub fn cargo_workspace_root(&self) -> &Path {
        self.metadata.workspace_root.as_std_path()
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

    /// Returns all explicit Cargo-backed source selections.
    #[must_use]
    pub fn sources(&self) -> &SourceSelection {
        &self.sources
    }

    /// Returns the automatic dependency additions made during preparation.
    #[must_use]
    pub fn preparation_changes(&self) -> &[PreparationChange] {
        self.preparation.changes()
    }

    /// Runs one supported Cargo source-development operation.
    pub fn run(
        &self,
        operation: CargoOperation,
        options: &CargoOptions,
    ) -> Result<Vec<CargoOutput>, Error> {
        cargo::run(self, operation, options)
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
    /// brain, service, and component-driver binaries plus inspectable manifest
    /// and provenance records. The selected supervisor is built through the
    /// same graph but is launched separately by `run_local`.
    pub fn build_bundle(
        &self,
        options: &CargoOptions,
        output: impl AsRef<Path>,
    ) -> Result<CompiledBundle, Error> {
        self.build_supervisor(options)?;
        bundle::assemble(self, options, output)
    }

    /// Builds the exact supervisor binary selected through the root Cargo
    /// graph and returns Cargo's reported executable path.
    pub fn build_supervisor(&self, options: &CargoOptions) -> Result<PathBuf, Error> {
        let output = cargo::build_target(self, &self.sources.supervisor, options)?;
        let executable = cargo::artifact_path(&output.stdout, &self.sources.supervisor)?;
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
        let supervisor = self.build_supervisor(options)?;
        let bundle = bundle::assemble(self, options, output)?;
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

    /// Prepares a local simulation launch without provisioning or launching an
    /// independent simulator application.
    pub fn local_simulation_plan(
        &self,
        options: &CargoOptions,
        output: impl AsRef<Path>,
        scope: impl Into<String>,
        supervisor_id: impl Into<String>,
    ) -> Result<LocalSimulationPlan, Error> {
        let identity = LocalIdentity::new(scope, supervisor_id)?;
        let bundle = self.build_bundle(options, output)?;
        Ok(LocalSimulationPlan { bundle, identity })
    }

    pub(crate) fn assembly_targets(&self) -> Vec<(String, &SelectedTarget)> {
        let mut targets = vec![(String::from("brain"), &self.sources.brain)];
        targets.extend(
            self.sources
                .services
                .iter()
                .map(|(instance, service)| (instance.clone(), &service.binary)),
        );
        targets.extend(
            self.sources
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
        let mut targets = vec![&self.sources.brain];
        targets.extend(
            self.sources
                .services
                .values()
                .map(|service| &service.binary),
        );
        targets.extend(
            self.sources
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
        if self.sources.services.contains_key(instance) {
            return "service".to_owned();
        }
        if self
            .sources
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
