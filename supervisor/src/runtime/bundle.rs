//! Reading the supervisor's compiled manifest and robot document, and locating
//! the run directory that owns them.
//!
//! Opening a source bundle admits `manifest.json` and `robot.yaml`, validates
//! executable paths, and stops before launching anything. The supervisor
//! later launches only that admitted graph.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use phoxal::artifact::MethodShape;

/// The bundle directory's name inside a deployment release. The supervisor is
/// handed a bundle root and knows nothing about releases, but it does have to
/// find the run directory that owns the execution, and a release's bundle
/// always sits one level inside the release.
const RELEASE_BUNDLE_DIR: &str = "bundle";

/// Where a source project keeps its own release, relative to the project root.
const PROJECT_RELEASE_SUFFIX: [&str; 2] = [".phoxal", "release"];

/// Read the bundle at `root`.
#[derive(Debug)]
pub(crate) enum Bundle {
    /// The source compiler's executable bundle, which this supervisor owns.
    Source(SourceBundle),
}

impl Bundle {
    pub(crate) fn root(&self) -> &Path {
        match self {
            Self::Source(bundle) => bundle.root(),
        }
    }

    pub(crate) fn robot_id(&self) -> &str {
        match self {
            Self::Source(bundle) => match &bundle.manifest {
                SourceManifest::V0 { robot_id, .. } => robot_id,
            },
        }
    }

    /// Returns the immutable controlled-simulation definition carried by the
    /// source manifest, if any. A separate run specification can only be
    /// admitted against a bundle that carries this native scheduling contract.
    pub(crate) fn simulation(&self) -> Option<&SourceSimulation> {
        match self {
            Self::Source(bundle) => match &bundle.manifest {
                SourceManifest::V0 { simulation, .. } => simulation.as_ref(),
            },
        }
    }

    pub(crate) fn source(&self) -> Option<&SourceBundle> {
        match self {
            Self::Source(bundle) => Some(bundle),
        }
    }

    pub(crate) fn source_mut(&mut self) -> Option<&mut SourceBundle> {
        match self {
            Self::Source(bundle) => Some(bundle),
        }
    }
}

/// A source compiler bundle admitted for exact process launch.
#[derive(Debug, Clone)]
pub(crate) struct SourceBundle {
    root: PathBuf,
    manifest: SourceManifest,
    execution_connections: BTreeMap<String, serde_json::Value>,
}

impl SourceBundle {
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn executables(&self) -> impl Iterator<Item = &SourceExecutable> {
        match &self.manifest {
            SourceManifest::V0 { executables, .. } => executables.iter(),
        }
    }

    /// Return the exact artifact summary retained for one executable.
    pub(crate) fn artifact(&self, instance: &str) -> Option<&serde_json::Value> {
        match &self.manifest {
            SourceManifest::V0 { executables, .. } => executables
                .iter()
                .find(|executable| executable.instance == instance)
                .and_then(|executable| executable.artifact.as_ref()),
        }
    }

    pub(crate) fn connections(&self) -> &BTreeMap<String, serde_json::Value> {
        &self.execution_connections
    }

    /// Apply simulation-only source bindings to this in-memory execution
    /// graph. The immutable manifest and its bytes remain unchanged.
    pub(crate) fn apply_simulation_bindings(
        &mut self,
        bindings: &[phoxal::artifact::simulation_run::SimulationBinding],
    ) -> Result<()> {
        let mut seen = std::collections::BTreeSet::new();
        for binding in bindings {
            if binding.source_instance != "supervisor" {
                bail!(
                    "simulation binding for {}.{} has unsupported source instance `{}`",
                    binding.target_instance,
                    binding.signature.endpoint,
                    binding.source_instance
                );
            }
            if binding.signature.shape != MethodShape::Call
                || binding.signature.lease_valid_for_ms.is_none()
            {
                bail!(
                    "simulation binding for {}.{} is not a leased generated call",
                    binding.target_instance,
                    binding.signature.endpoint
                );
            }
            let target = format!("{}.{}", binding.target_instance, binding.signature.endpoint);
            if !seen.insert(target.clone()) {
                bail!("simulation run declares conflicting producers for `{target}`");
            }
            let replaces_authored_source = self.execution_connections.contains_key(&target);
            if binding.replaces_authored_source != replaces_authored_source {
                bail!(
                    "simulation binding for `{target}` records replaces_authored_source={}, but the immutable graph requires {}",
                    binding.replaces_authored_source,
                    replaces_authored_source
                );
            }
            let source = format!("{}.{}", binding.source_instance, binding.signature.endpoint);
            self.execution_connections
                .insert(target, serde_json::Value::String(source));
        }
        Ok(())
    }
    /// Return the immutable simulation contract carried by this source bundle.
    pub(crate) fn simulation(&self) -> Option<&SourceSimulation> {
        match &self.manifest {
            SourceManifest::V0 { simulation, .. } => simulation.as_ref(),
        }
    }
}

/// Open a source-side `bundle/v0`.
pub(crate) fn open(root: &Path) -> Result<Bundle> {
    let root = root.canonicalize().with_context(|| {
        format!(
            "phoxal-supervisor takes a compiled bundle directory; {} is not one",
            root.display()
        )
    })?;
    if !root.is_dir() {
        bail!(
            "compiled bundle root is not a directory: {}",
            root.display()
        );
    }
    let manifest_path = root.join(MANIFEST_FILE);
    let bytes = bounded_file(&manifest_path, MAX_MANIFEST_BYTES)?;
    let mut manifest = serde_json::from_slice::<SourceManifest>(&bytes)
        .with_context(|| format!("{} is not a supported compiled bundle", root.display()))?;
    let document_path = root.join("robot.yaml");
    let document_bytes = bounded_file(&document_path, MAX_MANIFEST_BYTES)?;
    let document =
        serde_yaml::from_slice::<SourceDocument>(&document_bytes).with_context(|| {
            format!(
                "cannot decode compiled robot.yaml {}",
                document_path.display()
            )
        })?;
    let SourceManifest::V0 { document: slot, .. } = &mut manifest;
    *slot = document;
    Ok(Bundle::Source(admit_source(root, manifest)?))
}

const MANIFEST_FILE: &str = "manifest.json";
const MAX_MANIFEST_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "schema", deny_unknown_fields)]
pub(crate) enum SourceManifest {
    #[serde(rename = "phoxal/bundle/v0")]
    V0 {
        robot_id: String,
        #[serde(skip, default = "empty_source_document")]
        document: SourceDocument,
        root_package: SourcePackage,
        target: String,
        profile: String,
        features: Vec<String>,
        executables: Vec<SourceExecutable>,
        components: Vec<SourceComponentRecord>,
        #[serde(default)]
        component_sources: BTreeMap<String, String>,
        #[serde(default)]
        model: Option<serde_json::Value>,
        #[serde(default)]
        simulation: Option<SourceSimulation>,
    },
}

fn empty_source_document() -> SourceDocument {
    SourceDocument::V0 {
        robot: SourceRobot {
            id: String::new(),
            model: None,
            components: BTreeMap::new(),
        },
        brain: None,
        services: BTreeMap::new(),
        connections: BTreeMap::new(),
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceSimulation {
    pub(crate) protocol: String,
    pub(crate) mode: String,
    pub(crate) model_identity: String,
    pub(crate) quantum_ns: u64,
    pub(crate) providers: Vec<SourceSimulationProvider>,
    pub(crate) actuation_bindings: Vec<SourceActuationBinding>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceSimulationProvider {
    pub(crate) rate_microhertz: u64,
    pub(crate) service_instance: String,
    pub(crate) port: String,
    pub(crate) service_fqn: String,
    pub(crate) method: String,
    pub(crate) shape: MethodShape,
    pub(crate) retained_latest: bool,
    pub(crate) lease_valid_for_ms: Option<u64>,
    pub(crate) input_fqn: String,
    pub(crate) payload_fqn: String,
    pub(crate) max_message_bytes: u32,
    pub(crate) max_buffered_items: u32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceActuationBinding {
    pub(crate) service_instance: String,
    pub(crate) port: String,
    pub(crate) payload_fqn: String,
    pub(crate) actuator_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "schema", deny_unknown_fields)]
pub(crate) enum SourceDocument {
    #[serde(rename = "phoxal/robot/v0")]
    V0 {
        robot: SourceRobot,
        #[serde(default)]
        brain: Option<serde_json::Value>,
        #[serde(default)]
        services: BTreeMap<String, SourceService>,
        #[serde(default)]
        connections: BTreeMap<String, serde_json::Value>,
    },
}

#[derive(Clone, Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceService {
    package: String,
    version: String,
    #[serde(default)]
    binary: Option<String>,
    #[serde(default)]
    source: Option<serde_json::Value>,
    #[serde(default)]
    config: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceRobot {
    id: String,
    #[serde(default)]
    model: Option<std::path::PathBuf>,
    #[serde(default)]
    components: BTreeMap<String, SourceComponent>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceComponent {
    package: String,
    version: String,
    mount_site: String,
    #[serde(default)]
    binary: Option<String>,
    #[serde(default)]
    source: Option<serde_json::Value>,
    #[serde(default)]
    driver: Option<serde_json::Value>,
    #[serde(default)]
    config: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourcePackage {
    id: String,
    name: String,
    source: String,
}

/// Supervisor projection of component package provenance. Physical models,
/// resources, and native binding semantics belong to the simulator and are
/// deliberately not interpreted by the hardware-capable supervisor.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct SourceComponentRecord {
    instance: String,
    dependency_key: String,
    package_id: String,
    package: String,
    source: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceExecutable {
    pub(crate) role: String,
    pub(crate) instance: String,
    package_id: String,
    package: String,
    target: String,
    pub(crate) path: String,
    #[serde(skip)]
    pub(crate) bytes: u64,
    #[serde(skip)]
    pub(crate) sha256: String,
    artifact: Option<serde_json::Value>,
}

impl SourceExecutable {
    pub(crate) fn path(&self) -> &Path {
        Path::new(&self.path)
    }

    pub(crate) fn instance(&self) -> &str {
        &self.instance
    }

    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        instance: impl Into<String>,
        path: impl Into<String>,
        bytes: u64,
        sha256: impl Into<String>,
    ) -> Self {
        let instance = instance.into();
        Self {
            role: if instance == "brain" {
                "brain".to_owned()
            } else {
                "service".to_owned()
            },
            instance,
            package_id: "fixture".to_owned(),
            package: "fixture".to_owned(),
            target: "fixture".to_owned(),
            path: path.into(),
            bytes,
            sha256: sha256.into(),
            artifact: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test_with_artifact(
        instance: impl Into<String>,
        artifact: serde_json::Value,
    ) -> Self {
        let instance = instance.into();
        Self {
            role: if instance == "brain" {
                "brain".to_owned()
            } else {
                "service".to_owned()
            },
            instance,
            package_id: "fixture".to_owned(),
            package: "fixture".to_owned(),
            target: "fixture".to_owned(),
            path: "bin/fixture".to_owned(),
            bytes: 1,
            sha256: "0".repeat(64),
            artifact: Some(artifact),
        }
    }
}

impl SourceBundle {
    #[cfg(test)]
    pub(crate) fn for_test(root: &Path, manifest: SourceManifest) -> Self {
        Self {
            root: root.to_owned(),
            execution_connections: execution_connections(&manifest)
                .expect("valid fixture connections"),
            manifest,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test_with_connections(
        root: &Path,
        mut manifest: SourceManifest,
        connections: BTreeMap<String, serde_json::Value>,
    ) -> Self {
        if let SourceManifest::V0 { document, .. } = &mut manifest
            && let SourceDocument::V0 {
                connections: document_connections,
                ..
            } = document
        {
            *document_connections = connections;
        }
        Self::for_test(root, manifest)
    }
}

#[cfg(test)]
impl SourceManifest {
    pub(crate) fn for_test(
        robot_id: impl Into<String>,
        executables: Vec<SourceExecutable>,
    ) -> Self {
        Self::V0 {
            robot_id: robot_id.into(),
            document: SourceDocument::V0 {
                robot: SourceRobot {
                    id: "fixture".to_owned(),
                    model: None,
                    components: BTreeMap::new(),
                },
                brain: None,
                services: BTreeMap::new(),
                connections: BTreeMap::new(),
            },
            root_package: SourcePackage {
                id: "fixture".to_owned(),
                name: "fixture".to_owned(),
                source: "local".to_owned(),
            },
            target: "host".to_owned(),
            profile: "dev".to_owned(),
            features: Vec::new(),
            executables,
            components: Vec::new(),
            component_sources: BTreeMap::new(),
            model: None,
            simulation: None,
        }
    }
}

fn admit_source(root: PathBuf, mut manifest: SourceManifest) -> Result<SourceBundle> {
    let SourceManifest::V0 {
        robot_id,
        document,
        executables,
        simulation,
        ..
    } = &manifest;
    let SourceDocument::V0 {
        robot: document_robot,
        ..
    } = document;
    validate_segment(robot_id, "robot_id")?;
    validate_source_document(&manifest)?;
    validate_source_metadata(&manifest)?;
    if let Some(simulation) = simulation.as_ref() {
        validate_source_simulation(simulation, &document_robot.components)?;
    }
    validate_simulation_executables(simulation.as_ref(), executables)?;
    if executables.is_empty() {
        bail!("source bundle contains no executable records");
    }
    let SourceManifest::V0 { executables, .. } = &mut manifest;
    let mut seen = std::collections::BTreeSet::new();
    let mut has_brain = false;
    for executable in executables.iter_mut() {
        if !matches!(executable.role.as_str(), "brain" | "service" | "driver") {
            bail!("unsupported executable role `{}`", executable.role);
        }
        validate_segment(&executable.instance, "executable instance")?;
        if !seen.insert(executable.instance.clone()) {
            bail!(
                "source bundle contains duplicate executable instance `{}`",
                executable.instance
            );
        }
        if executable.role == "brain" {
            if executable.instance != "brain" || has_brain {
                bail!("source bundle must contain exactly one executable brain");
            }
            has_brain = true;
        } else if executable.instance == "brain" {
            bail!("only the brain role may use executable instance `brain`");
        }
        if executable.package_id.is_empty() {
            bail!(
                "executable `{}` has an empty Cargo package identity",
                executable.instance
            );
        }
        if executable.package.is_empty() {
            bail!(
                "executable `{}` has an empty Cargo package name",
                executable.instance
            );
        }
        if executable.target.is_empty() {
            bail!(
                "executable `{}` has an empty Cargo target name",
                executable.instance
            );
        }
        if executable
            .artifact
            .as_ref()
            .is_some_and(|artifact| !artifact.is_object())
        {
            bail!(
                "executable `{}` has a non-object artifact contract",
                executable.instance
            );
        }
        let relative = safe_relative_path(&executable.path)?;
        let path = root.join(relative);
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("source bundle executable is missing: {}", path.display()))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            bail!(
                "source bundle executable is not a regular file: {}",
                path.display()
            );
        }
        let canonical = path.canonicalize().with_context(|| {
            format!(
                "cannot resolve source bundle executable: {}",
                path.display()
            )
        })?;
        if !canonical.starts_with(&root) {
            bail!(
                "source bundle executable escapes its root: {}",
                path.display()
            );
        }
        let (bytes, digest) = digest_file(&canonical)?;
        executable.bytes = bytes;
        executable.sha256 = digest;
    }
    if !has_brain {
        bail!("source bundle is missing executable instance `brain`");
    }
    let SourceManifest::V0 {
        document: source_document,
        ..
    } = &manifest;
    let SourceDocument::V0 {
        services: source_services,
        ..
    } = source_document;
    for (service, definition) in source_services {
        validate_segment(service, "service instance")?;
        if service == "brain" {
            bail!("source bundle services cannot contain `brain`");
        }
        if definition
            .config
            .as_ref()
            .is_some_and(serde_json::Value::is_null)
        {
            bail!("service `{service}` has an explicit null configuration");
        }
        if !seen.contains(service.as_str()) {
            bail!("service `{service}` has no executable record");
        }
    }
    let execution_connections = execution_connections(&manifest)?;
    Ok(SourceBundle {
        root,
        manifest,
        execution_connections,
    })
}

fn validate_simulation_executables(
    simulation: Option<&SourceSimulation>,
    executables: &[SourceExecutable],
) -> Result<()> {
    if simulation.is_some()
        && executables
            .iter()
            .any(|executable| executable.role == "driver")
    {
        bail!("controlled simulation bundles must exclude physical driver executables");
    }
    Ok(())
}

fn validate_source_simulation(
    simulation: &SourceSimulation,
    components: &BTreeMap<String, SourceComponent>,
) -> Result<()> {
    if simulation.protocol != "phoxal.simulation.v1" {
        bail!(
            "unsupported simulation protocol `{}`; expected phoxal.simulation.v1",
            simulation.protocol
        );
    }
    if simulation.mode != "controlled" {
        bail!(
            "unsupported simulation mode `{}`; expected controlled",
            simulation.mode
        );
    }
    if simulation.model_identity.is_empty()
        || simulation.model_identity.len() > 512
        || !simulation.model_identity.is_ascii()
        || simulation.model_identity.chars().any(char::is_whitespace)
    {
        bail!("simulation model identity is invalid");
    }
    if simulation.quantum_ns == 0 {
        bail!("simulation quantum must be positive");
    }
    if simulation.providers.is_empty() {
        bail!("simulation provider set must not be empty");
    }
    let physical_driver_instances = components
        .iter()
        .filter_map(|(instance, component)| component.driver.as_ref().map(|_| instance.as_str()))
        .collect::<std::collections::BTreeSet<_>>();
    let mut providers = std::collections::BTreeSet::new();
    for provider in &simulation.providers {
        validate_segment(
            &provider.service_instance,
            "simulation provider service instance",
        )?;
        validate_segment(&provider.port, "simulation provider port")?;
        if !physical_driver_instances.contains(provider.service_instance.as_str()) {
            bail!(
                "simulation provider `{}.{}` is not owned by a selected physical driver",
                provider.service_instance,
                provider.port
            );
        }
        if provider.shape != MethodShape::Observation {
            bail!(
                "simulation provider `{}.{}` must be an observation",
                provider.service_instance,
                provider.port
            );
        }
        if provider.input_fqn.is_empty()
            || provider.payload_fqn.is_empty()
            || provider.service_fqn.is_empty()
            || provider.method.is_empty()
        {
            bail!(
                "simulation provider `{}.{}` has an empty message identity",
                provider.service_instance,
                provider.port
            );
        }
        if provider.max_message_bytes == 0 || provider.max_buffered_items == 0 {
            bail!(
                "simulation provider `{}.{}` has a non-positive public bound",
                provider.service_instance,
                provider.port
            );
        }
        if !providers.insert((&provider.service_instance, &provider.port)) {
            bail!(
                "simulation provider `{}.{}` is duplicated",
                provider.service_instance,
                provider.port
            );
        }
    }
    if simulation.actuation_bindings.is_empty() {
        bail!("simulation actuation binding set must not be empty");
    }
    let mut bindings = std::collections::BTreeSet::new();
    let mut actuators = std::collections::BTreeSet::new();
    for binding in &simulation.actuation_bindings {
        validate_segment(
            &binding.service_instance,
            "simulation actuation service instance",
        )?;
        validate_segment(&binding.port, "simulation actuation port")?;
        if binding.payload_fqn.is_empty() || binding.actuator_ids.is_empty() {
            bail!(
                "simulation actuation `{}.{}` must carry a payload identity and native actuator IDs",
                binding.service_instance,
                binding.port
            );
        }
        if !bindings.insert((&binding.service_instance, &binding.port)) {
            bail!(
                "simulation actuation `{}.{}` is duplicated",
                binding.service_instance,
                binding.port
            );
        }
        for actuator in &binding.actuator_ids {
            if actuator.is_empty()
                || actuator.len() > 64
                || !actuator.is_ascii()
                || actuator.chars().any(char::is_whitespace)
            {
                bail!("simulation actuation contains an invalid native actuator ID");
            }
            if !actuators.insert(actuator) {
                bail!("native actuator `{actuator}` is mapped more than once");
            }
        }
    }
    Ok(())
}

/// The authored document keeps both deployment graphs. Native actuation
/// replaces a physical driver's input delivery, so no nonexistent driver may
/// appear in the supervisor's receiver acknowledgement roster.
fn execution_connections(manifest: &SourceManifest) -> Result<BTreeMap<String, serde_json::Value>> {
    let SourceManifest::V0 {
        document,
        executables,
        simulation,
        ..
    } = manifest;
    let SourceDocument::V0 {
        connections: authored_connections,
        ..
    } = document;
    let mut connections = BTreeMap::new();
    for (consumer, sources) in authored_connections {
        let (instance, _) = consumer
            .split_once('.')
            .with_context(|| format!("invalid connection consumer `{consumer}`"))?;
        if executables
            .iter()
            .any(|executable| executable.instance == instance)
        {
            connections.insert(consumer.clone(), sources.clone());
            continue;
        }
        let simulation = simulation
            .as_ref()
            .with_context(|| format!("connection consumer `{consumer}` has no executable"))?;
        if !simulation
            .providers
            .iter()
            .any(|provider| provider.service_instance == instance)
        {
            bail!("connection consumer `{consumer}` has no executable or native provider");
        }
        let sources = match sources {
            serde_json::Value::String(source) => vec![source.as_str()],
            serde_json::Value::Array(sources) => sources
                .iter()
                .map(|source| {
                    source
                        .as_str()
                        .context("connection source must be a string")
                })
                .collect::<Result<Vec<_>>>()?,
            _ => bail!("connection sources for `{consumer}` must be strings"),
        };
        if sources.is_empty()
            || sources.iter().any(|source| {
                !simulation.actuation_bindings.iter().any(|binding| {
                    source.split_once('.').is_some_and(|(instance, port)| {
                        binding.service_instance == instance && binding.port == port
                    })
                })
            })
        {
            bail!("native substitution does not cover driver input `{consumer}`");
        }
    }
    Ok(connections)
}

fn validate_source_document(manifest: &SourceManifest) -> Result<()> {
    let SourceManifest::V0 {
        document, robot_id, ..
    } = manifest;
    let SourceDocument::V0 {
        robot: document_robot,
        brain: document_brain,
        connections: document_connections,
        services: document_services,
        ..
    } = document;
    if document_robot.id != *robot_id {
        bail!(
            "bundle robot_id `{robot_id}` does not match document robot.id `{}`",
            document_robot.id
        );
    }
    validate_segment(&document_robot.id, "document robot.id")?;
    // The source schema rejects unknown fields. Native model interpretation
    // remains owned by the simulator; the supervisor validates execution
    // artifacts and graph routes below.
    let _ = (&document_robot.model, document_brain, document_connections);
    for (instance, component) in &document_robot.components {
        validate_segment(instance, "component instance")?;
        if component.package.is_empty() || component.version.is_empty() {
            bail!("component `{instance}` has an incomplete exact package selection");
        }
        if component.mount_site.is_empty() {
            bail!("component `{instance}` has an empty mount link");
        }
        let _ = (
            &component.binary,
            &component.source,
            &component.driver,
            &component.config,
        );
    }
    for (service, definition) in document_services {
        validate_segment(service, "service instance")?;
        if definition.package.is_empty() || definition.version.is_empty() {
            bail!("service `{service}` has an incomplete exact package selection");
        }
        if definition.binary.as_deref().is_some_and(str::is_empty) {
            bail!("service `{service}` has an empty binary target");
        }
        let _ = &definition.source;
    }
    Ok(())
}

fn validate_source_metadata(manifest: &SourceManifest) -> Result<()> {
    let SourceManifest::V0 {
        root_package,
        target,
        profile,
        features,
        components,
        ..
    } = manifest;
    if root_package.id.is_empty() || root_package.name.is_empty() || root_package.source.is_empty()
    {
        bail!("source bundle root package metadata is incomplete");
    }
    if target.is_empty() || profile.is_empty() {
        bail!("source bundle target and profile metadata must not be empty");
    }
    if features.iter().any(String::is_empty) {
        bail!("source bundle features must not contain empty names");
    }
    let mut seen = std::collections::BTreeSet::new();
    for component in components {
        validate_segment(&component.instance, "component instance")?;
        if !seen.insert(component.instance.as_str()) {
            bail!(
                "source bundle contains duplicate component instance `{}`",
                component.instance
            );
        }
        if component.dependency_key.is_empty()
            || component.package_id.is_empty()
            || component.package.is_empty()
            || component.source.is_empty()
        {
            bail!(
                "source bundle component `{}` metadata is incomplete",
                component.instance
            );
        }
    }
    Ok(())
}

fn bounded_file(path: &Path, maximum: usize) -> Result<Vec<u8>> {
    let link_metadata = fs::symlink_metadata(path)
        .with_context(|| format!("cannot inspect bundle manifest {}", path.display()))?;
    if link_metadata.file_type().is_symlink() {
        bail!(
            "bundle manifest must not be a symbolic link: {}",
            path.display()
        );
    }
    let file = fs::File::open(path)
        .with_context(|| format!("cannot read bundle manifest {}", path.display()))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        bail!("bundle manifest is not a regular file: {}", path.display());
    }
    let mut bytes = Vec::new();
    file.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        bail!("bundle manifest exceeds {maximum} bytes");
    }
    Ok(bytes)
}

fn validate_segment(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 64
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        bail!("{label} must be 1-64 lowercase ASCII letters, digits, '-' or '_'");
    }
    Ok(())
}

fn safe_relative_path(value: &str) -> Result<PathBuf> {
    let path = Path::new(value);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("executable path `{value}` is not bundle-relative");
    }
    Ok(path.to_owned())
}

fn digest_file(path: &Path) -> Result<(u64, String)> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes = bytes
            .checked_add(read as u64)
            .ok_or_else(|| anyhow::anyhow!("executable size overflows u64"))?;
        hasher.update(&buffer[..read]);
    }
    Ok((bytes, format!("{:x}", hasher.finalize())))
}

/// The root whose volatile run directory owns this bundle.
///
/// A bundle inside a deployment release is owned by whatever owns the release:
/// a project keeps its release at `<project>/.phoxal/release`, and every other
/// release - an installed one under `/var/phoxal`, or an extracted archive - is
/// its own root. A bare bundle root, run outside any release, owns itself.
pub(crate) fn owning_root(bundle_root: &Path) -> PathBuf {
    let Some(release_root) = strip_tail(bundle_root, &[RELEASE_BUNDLE_DIR]) else {
        return bundle_root.to_path_buf();
    };
    strip_tail(&release_root, &PROJECT_RELEASE_SUFFIX).unwrap_or(release_root)
}

/// `path` without `tail`, or `None` when it does not end with it.
fn strip_tail(path: &Path, tail: &[&str]) -> Option<PathBuf> {
    let mut components = path.components().rev();
    let found: Vec<_> = components
        .by_ref()
        .take(tail.len())
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    let expected: Vec<_> = tail.iter().rev().map(ToString::to_string).collect();
    (found == expected).then(|| components.rev().collect::<PathBuf>())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use sha2::Digest;

    use super::*;

    #[test]
    fn a_directory_without_a_manifest_is_not_a_bundle() {
        let dir = tempfile::tempdir().expect("temporary directory");
        std::fs::write(dir.path().join("robot.yaml"), "schema: phoxal/robot/v0\n")
            .expect("source fixture");
        let error = open(dir.path()).expect_err("authored YAML is not a compiled bundle");
        assert!(format!("{error:#}").contains("manifest.json"), "{error:#}");
    }

    /// The run directory a bundle's execution belongs to, for each shape a
    /// bundle root arrives in. The installed cases matter most: the unit starts
    /// `/var/phoxal/phoxal-supervisor /var/phoxal/bundle`, and the execution's
    /// socket and locks belong to the release, never to a directory inside the
    /// immutable release itself.
    #[test]
    fn a_bundle_is_owned_by_whatever_owns_the_release_it_sits_in() {
        assert_eq!(
            owning_root(Path::new("/work/rover/.phoxal/release/bundle")),
            Path::new("/work/rover")
        );
        assert_eq!(
            owning_root(Path::new("/var/phoxal/bundle")),
            Path::new("/var/phoxal")
        );
        assert_eq!(
            owning_root(Path::new("/var/lib/phoxal/releases/current/bundle")),
            Path::new("/var/lib/phoxal/releases/current")
        );
        // A bundle root that is not inside a release owns itself.
        assert_eq!(
            owning_root(Path::new("/var/lib/phoxal/releases/current")),
            Path::new("/var/lib/phoxal/releases/current")
        );
    }

    #[test]
    fn a_source_bundle_admits_the_manifest_and_present_executable() {
        let directory = tempfile::tempdir().expect("temporary source bundle");
        let bin = directory.path().join("bin");
        fs::create_dir(&bin).expect("bundle bin directory");
        let executable = bin.join("brain");
        let executable_bytes = b"#!/bin/sh\nexit 0\n";
        fs::write(&executable, executable_bytes).expect("bundle executable");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
            .expect("make bundle executable runnable");
        let manifest = format!(
            r#"{{
                "schema": "phoxal/bundle/v0",
                "robot_id": "fixture",
                "root_package": {{
                    "id": "path+file:///fixture#fixture@0.1.0",
                    "name": "fixture",
                    "source": "local"
                }},
                "target": "host",
                "profile": "dev",
                "features": [],
                "executables": [{{
                    "role": "brain",
                    "instance": "brain",
                    "package_id": "path+file:///fixture#fixture@0.1.0",
                    "package": "fixture",
                    "target": "fixture",
                    "path": "bin/brain",
                    "artifact": null
                }}],
                "components": []
            }}"#
        );
        fs::write(directory.path().join("manifest.json"), manifest)
            .expect("source bundle manifest");
        fs::write(
            directory.path().join("robot.yaml"),
            "schema: phoxal/robot/v0\nrobot:\n  id: fixture\n  components: {}\nservices: {}\nconnections: {}\n",
        )
        .expect("compiled robot document");

        let bundle = open(directory.path()).expect("source bundle admission");
        let Bundle::Source(bundle) = bundle;
        let SourceManifest::V0 { robot_id, .. } = &bundle.manifest;
        assert_eq!(robot_id, "fixture");
        assert_eq!(
            bundle
                .executables()
                .next()
                .expect("brain executable")
                .path(),
            Path::new("bin/brain")
        );
    }

    #[test]
    fn obsolete_bundle_scenario_artifacts_are_rejected_during_decode() {
        let value = serde_json::json!({
            "schema": "phoxal/bundle/v0",
            "robot_id": "fixture",
            "root_package": {"id": "fixture", "name": "fixture", "source": "local"},
            "target": "host",
            "profile": "dev",
            "features": [],
            "executables": [],
            "components": [],
            "scenario": {"program": "obsolete"}
        });
        let error = serde_json::from_value::<SourceManifest>(value)
            .expect_err("the immutable bundle schema has no scenario section");
        assert!(error.to_string().contains("unknown field `scenario`"));
    }

    #[test]
    fn simulation_bindings_reject_conflicting_run_owned_producers() {
        let manifest = SourceManifest::for_test("fixture", Vec::new());
        let mut bundle = SourceBundle::for_test(Path::new("."), manifest);
        let binding = phoxal::artifact::simulation_run::SimulationBinding {
            target_instance: "controller".to_owned(),
            source_instance: "supervisor".to_owned(),
            signature: phoxal::artifact::MethodSignature {
                endpoint: "manual".to_owned(),
                service: "phoxal.motion.v1.Motion".to_owned(),
                method: "Manual".to_owned(),
                shape: MethodShape::Call,
                request: "phoxal.motion.v1.MotionIntent".to_owned(),
                response: "google.protobuf.Empty".to_owned(),
                retained_latest: false,
                lease_valid_for_ms: Some(100),
            },
            max_message_bytes: 32,
            replaces_authored_source: false,
        };
        let error = bundle
            .apply_simulation_bindings(&[binding.clone(), binding])
            .expect_err("one target cannot have two run-owned producers");
        assert!(error.to_string().contains("conflicting producers"));
    }

    fn simulation_fixture() -> SourceSimulation {
        SourceSimulation {
            protocol: "phoxal.simulation.v1".to_owned(),
            mode: "controlled".to_owned(),
            model_identity: "model-digest".to_owned(),
            quantum_ns: 10_000_000,
            providers: vec![SourceSimulationProvider {
                rate_microhertz: 100_000_000,
                service_fqn: "fixture.Sensor".into(),
                method: "Sample".into(),
                service_instance: "imu".to_owned(),
                port: "sample".to_owned(),
                shape: MethodShape::Observation,
                retained_latest: false,
                lease_valid_for_ms: None,
                input_fqn: "google.protobuf.Empty".to_owned(),
                payload_fqn: "example.Imu".to_owned(),
                max_message_bytes: 1024,
                max_buffered_items: 16,
            }],
            actuation_bindings: vec![SourceActuationBinding {
                service_instance: "motion".to_owned(),
                port: "actuators".to_owned(),
                payload_fqn: "example.Actuators".to_owned(),
                actuator_ids: vec!["motor".to_owned()],
            }],
        }
    }

    fn driver_components() -> BTreeMap<String, SourceComponent> {
        BTreeMap::from([(
            "imu".to_owned(),
            SourceComponent {
                package: "phoxal-component-bno085".to_owned(),
                version: "0.0.0-dev.2".to_owned(),
                mount_site: "base".to_owned(),
                binary: None,
                source: None,
                driver: Some(serde_json::json!({})),
                config: None,
            },
        )])
    }

    #[test]
    fn simulation_admission_requires_provider_identity_to_be_a_driver_instance() {
        let mut simulation = simulation_fixture();
        simulation.providers[0].service_instance = "navigation".to_owned();
        let error = validate_source_simulation(&simulation, &driver_components())
            .expect_err("robot products cannot become native provider identities");
        assert!(format!("{error:#}").contains("selected physical driver"));
    }

    #[test]
    fn simulation_admission_rejects_non_positive_provider_bounds() {
        let mut simulation = simulation_fixture();
        simulation.providers[0].max_message_bytes = 0;
        let error = validate_source_simulation(&simulation, &driver_components())
            .expect_err("provider metadata must retain finite public bounds");
        assert!(format!("{error:#}").contains("non-positive public bound"));
    }

    #[test]
    fn simulation_admission_excludes_physical_driver_executables() {
        let simulation = simulation_fixture();
        let mut executable = SourceExecutable::for_test("imu", "bin/imu", 1, "0".repeat(64));
        executable.role = "driver".to_owned();
        let error = validate_simulation_executables(Some(&simulation), &[executable])
            .expect_err("controlled bundles must not roster physical drivers");
        assert!(format!("{error:#}").contains("exclude physical driver executables"));
    }

    #[test]
    fn native_driver_delivery_is_removed_only_from_the_execution_graph() {
        let mut manifest = SourceManifest::V0 {
            robot_id: "fixture".to_owned(),
            document: SourceDocument::V0 {
                robot: SourceRobot {
                    id: "fixture".to_owned(),
                    model: None,
                    components: BTreeMap::new(),
                },
                brain: None,
                services: BTreeMap::new(),
                connections: BTreeMap::from([
                    ("imu.actuator".into(), serde_json::json!("motion.actuators")),
                    (
                        "motion.measurements".into(),
                        serde_json::json!("imu.sample"),
                    ),
                ]),
            },
            root_package: SourcePackage {
                id: "fixture".to_owned(),
                name: "fixture".to_owned(),
                source: "local".to_owned(),
            },
            target: "host".to_owned(),
            profile: "dev".to_owned(),
            features: Vec::new(),
            executables: vec![SourceExecutable::for_test(
                "motion",
                "bin/motion",
                1,
                "0".repeat(64),
            )],
            components: Vec::new(),
            component_sources: BTreeMap::new(),
            model: None,
            simulation: Some(simulation_fixture()),
        };
        {
            let SourceManifest::V0 {
                document: manifest_document,
                ..
            } = &manifest;
            let SourceDocument::V0 {
                connections: manifest_connections,
                ..
            } = manifest_document;
            let authored = manifest_connections.clone();
            let graph = execution_connections(&manifest).unwrap();
            assert_eq!(graph.len(), 1);
            assert_eq!(graph["motion.measurements"], "imu.sample");
            assert_eq!(*manifest_connections, authored);
        }
        for source in [
            serde_json::json!("motion.unbound"),
            serde_json::json!([]),
            serde_json::json!([null]),
        ] {
            let SourceManifest::V0 {
                document: mut_document,
                ..
            } = &mut manifest;
            let SourceDocument::V0 {
                connections: mut_connections,
                ..
            } = mut_document;
            mut_connections.insert("imu.actuator".into(), source);
        }
        assert!(execution_connections(&manifest).is_err());
        {
            let SourceManifest::V0 {
                document: mut_document,
                ..
            } = &mut manifest;
            let SourceDocument::V0 {
                connections: mut_connections,
                ..
            } = mut_document;
            *mut_connections = BTreeMap::from([(
                "missing.actuator".into(),
                serde_json::json!("motion.actuators"),
            )]);
        }
        assert!(execution_connections(&manifest).is_err());
        {
            let SourceManifest::V0 {
                document: mut_document,
                simulation: mut_simulation,
                ..
            } = &mut manifest;
            let SourceDocument::V0 {
                connections: mut_connections,
                ..
            } = mut_document;
            *mut_connections = BTreeMap::from([
                ("imu.actuator".into(), serde_json::json!("motion.actuators")),
                (
                    "motion.measurements".into(),
                    serde_json::json!("imu.sample"),
                ),
            ]);
            *mut_simulation = None;
        }
        assert!(execution_connections(&manifest).is_err());
    }
}
