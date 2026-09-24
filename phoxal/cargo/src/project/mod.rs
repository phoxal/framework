//! Project discovery, authored composition validation, and Cargo orchestration.
//!
//! This module is the source-development boundary used by `cargo-phoxal`.
//! It deliberately does not depend on the Runtime SDK, supervisor, service
//! implementations, simulator, registry client, or the archived CLI.

pub mod artifact;
mod bundle;
mod cargo;
mod discovery;
mod document;
#[allow(dead_code)]
mod error;
mod file_lock;
pub(crate) mod participant;
#[allow(dead_code)]
mod preparation;
#[allow(dead_code)]
mod publication;
#[allow(dead_code)]
pub mod scenario;
#[allow(dead_code)]
mod selection;
mod simulation;
mod submission;
mod validation;

#[cfg(test)]
mod tests;

pub use bundle::{CompiledBundle, SimulationModelFacts};
pub use cargo::{CargoOperation, CargoOptions, CargoOutput, CargoSelection, LockMode};
pub use discovery::ProjectLayout;
pub use document::RobotDocument;
pub use error::{DiscoveryError, Error, PublicationError, SourceError};
pub use publication::{
    PublicationKind, PublicationOptions, PublicationResult, prepare_publication,
};
pub use selection::{SelectedTarget, SourceSelection};
pub use simulation::{
    SimulationBound, SimulationPresentation, SimulationRunOptions, SimulationRunReport,
    install_simulator, simulator_status, uninstall_simulator,
};
pub use submission::{SubmissionResult, submit_publication};

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

    /// Prepares the authored Cargo graph and resolves all explicit sources.
    ///
    /// Ordinary preparation may add the known mandatory supervisor dependency
    /// and let Cargo update the owning workspace lock.  Locked and frozen
    /// preparation refuses that addition before changing either file.
    pub fn prepare(&self, options: &CargoOptions) -> Result<PreparedProject, Error> {
        self.prepare_with(options, |_, _, _| Ok(()))
            .map(|(prepared, ())| prepared)
    }

    /// Stage authored inputs before resolving, allowing an explicit update to
    /// repair a lockfile that cannot resolve the newly authored dependencies.
    fn prepare_with<T>(
        &self,
        options: &CargoOptions,
        before_resolution: impl FnOnce(&Path, &Path, Option<&Path>) -> Result<T, Error>,
    ) -> Result<(PreparedProject, T), Error> {
        participant::prepare(&self.layout, options)?;
        let result = before_resolution(self.layout.cargo_manifest(), self.layout.root(), None)?;
        let metadata = cargo::load_metadata_at(
            self.layout.cargo_manifest(),
            self.layout.root(),
            None,
            options,
        )?;
        reject_direct_targetless_git(&metadata)?;
        let cargo_sources =
            selection::resolve_prepared_sources(&self.layout, &self.document, &metadata, options)?;
        let cargo_root_package = metadata
            .root_package()
            .cloned()
            .ok_or(SourceError::MissingBrain)?;
        Ok((
            PreparedProject {
                layout: self.layout.clone(),
                document: self.document.clone(),
                metadata: metadata.clone(),
                cargo_metadata: metadata,
                cargo_root_package,
                cargo_sources,
                local_source: None,
            },
            result,
        ))
    }

    /// Provisions the independent simulator application, probes its native
    /// model contract, builds the simulation bundle, and runs one finite
    /// simulation with bounded supervisor cleanup.
    pub fn run_simulation(
        &self,
        options: &CargoOptions,
        request: &SimulationRunOptions,
    ) -> Result<SimulationRunReport, Error> {
        simulation::run(self, options, request, None)
    }

    /// Run one validated scenario program through the controlled simulation.
    pub fn run_scenario_simulation(
        &self,
        options: &CargoOptions,
        request: &SimulationRunOptions,
        program: &phoxal::scenario::__internal::Program,
    ) -> Result<SimulationRunReport, Error> {
        simulation::run(self, options, request, Some(program))
    }

    /// Probe the requested scene with the simulator and return its
    /// model identity and quantum. Used by the case-host protocol
    /// (plan §9) so the tool can hand the probed quantum to the
    /// harness before the harness builds its `Program`.
    pub fn probe_simulation_scene(
        &self,
        options: &CargoOptions,
        request: &SimulationRunOptions,
    ) -> Result<SimulationModelFacts, Error> {
        simulation::probe_simulation_scene(self, options, request)
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

/// A validated project and the Cargo graph used for its selected sources.
#[derive(Debug, Clone)]
pub struct PreparedProject {
    layout: ProjectLayout,
    document: RobotDocument,
    metadata: cargo_metadata::Metadata,
    cargo_metadata: cargo_metadata::Metadata,
    cargo_root_package: cargo_metadata::Package,
    cargo_sources: SourceSelection,
    local_source: Option<Arc<publication::LocalProjectSource>>,
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

    pub(crate) fn cargo_metadata(&self) -> &cargo_metadata::Metadata {
        &self.cargo_metadata
    }

    pub(crate) fn cargo_root_package(&self) -> &cargo_metadata::Package {
        &self.cargo_root_package
    }

    pub(crate) fn cargo_sources(&self) -> &SourceSelection {
        &self.cargo_sources
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

    /// Validates the authored project, prepares its selected APIs, and runs
    /// Cargo check without building runtime executables.
    pub fn check(&self, options: &CargoOptions) -> Result<Vec<CargoOutput>, Error> {
        self.run(CargoOperation::Check, options)
    }

    /// Builds and atomically publishes the complete selected executable bundle.
    ///
    /// The output is a source-side compiled directory containing the selected
    /// brain, service, and component-driver binaries plus the exact supervisor
    /// executable and inspectable manifest records.
    pub fn build_bundle(
        &self,
        options: &CargoOptions,
        output: impl AsRef<Path>,
    ) -> Result<CompiledBundle, Error> {
        bundle::assemble_with_inputs(self, options, output, false, None, None)
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
        let RobotDocument::V0 { robot, .. } = &self.document;
        self.metadata
            .target_directory
            .as_std_path()
            .join("phoxal")
            .join(&robot.id)
            .join("bundle")
    }

    /// Builds and publishes a bundle carrying the complete controlled
    /// simulation contract supplied by the independent native application.
    pub fn build_simulation_bundle(
        &self,
        options: &CargoOptions,
        output: impl AsRef<Path>,
        facts: &SimulationModelFacts,
    ) -> Result<CompiledBundle, Error> {
        bundle::assemble_with_inputs(self, options, output, true, Some(facts), None)
    }

    /// Builds the immutable controlled-simulation bundle while validating one
    /// run-only experiment graph against it. The returned bundle never embeds
    /// the experiment or its virtual producers.
    pub(crate) fn build_simulation_bundle_for_run(
        &self,
        options: &CargoOptions,
        output: impl AsRef<Path>,
        facts: &SimulationModelFacts,
        program: &phoxal::scenario::__internal::Program,
    ) -> Result<CompiledBundle, Error> {
        bundle::assemble_with_inputs(
            self,
            options,
            output,
            true,
            Some(facts),
            Some(bundle::SimulationRunInput {
                program,
                fixture_instance_id: "scenario",
            }),
        )
    }

    pub(crate) fn build_probe_bundle_for_run(
        &self,
        options: &CargoOptions,
        output: impl AsRef<Path>,
        program: &phoxal::scenario::__internal::Program,
    ) -> Result<CompiledBundle, Error> {
        bundle::assemble_with_inputs(
            self,
            options,
            output,
            true,
            None,
            Some(bundle::SimulationRunInput {
                program,
                fixture_instance_id: "scenario",
            }),
        )
    }

    pub(crate) fn build_probe_bundle(
        &self,
        options: &CargoOptions,
        output: impl AsRef<Path>,
    ) -> Result<CompiledBundle, Error> {
        bundle::assemble_with_inputs(self, options, output, true, None, None)
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
