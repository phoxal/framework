//! Reading the supervisor's sole persisted input, and locating the run
//! directory that owns it.
//!
//! Opening a source bundle admits `manifest.json`, validates its exact
//! executable records, and stops before launching anything. The supervisor
//! later launches only that admitted graph.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};

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
            Self::Source(bundle) => &bundle.manifest.robot_id,
        }
    }

    /// Returns the scenario nondeployable marker carried by the
    /// underlying source manifest, if any. Returns `None` for an
    /// ordinary source bundle. Scenario bundles that the case host
    /// has built carry the stable marker so the admission policy can
    /// reject hardware launches.
    pub(crate) fn scenario_marker(&self) -> Option<String> {
        match self {
            Self::Source(bundle) => bundle.manifest.scenario_marker.clone(),
        }
    }

    /// Returns the validated scenario program identity if the
    /// manifest carries the nondeployable marker. Carries the exact
    /// bounded program path, byte length, SHA-256 digest, fixture
    /// instance id, and the controlled-execution flag the case host
    /// wrote. The supervisor verifies the bytes before admission.
    pub(crate) fn scenario_program(&self) -> Option<ScenarioProgramRef> {
        match self {
            Self::Source(bundle) => bundle.manifest.scenario_program.clone(),
        }
    }

    /// Returns the controlled-simulation definition carried by the
    /// source manifest, if any. A scenario bundle that does not also
    /// carry a controlled simulation definition is refused because
    /// the fixture has nothing to schedule against.
    pub(crate) fn simulation(&self) -> Option<&SourceSimulation> {
        match self {
            Self::Source(bundle) => bundle.manifest.simulation.as_ref(),
        }
    }

    pub(crate) fn source(&self) -> Option<&SourceBundle> {
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
        self.manifest.executables.iter()
    }

    /// Return the exact artifact summary retained for one executable.
    pub(crate) fn artifact(&self, instance: &str) -> Option<&serde_json::Value> {
        self.manifest
            .executables
            .iter()
            .find(|executable| executable.instance == instance)
            .and_then(|executable| executable.artifact.as_ref())
    }

    pub(crate) fn connections(&self) -> &BTreeMap<String, serde_json::Value> {
        &self.execution_connections
    }
    /// Return the immutable simulation contract carried by this source bundle.
    pub(crate) fn simulation(&self) -> Option<&SourceSimulation> {
        self.manifest.simulation.as_ref()
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
    let manifest = serde_json::from_slice::<SourceManifest>(&bytes)
        .with_context(|| format!("{} is not a supported compiled bundle", root.display()))?;
    if manifest.schema != SOURCE_SCHEMA {
        bail!("unsupported bundle schema `{}`", manifest.schema);
    }
    Ok(Bundle::Source(admit_source(root, manifest)?))
}

const SOURCE_SCHEMA: &str = "phoxal/bundle/v0";
const MANIFEST_FILE: &str = "manifest.json";
const MAX_MANIFEST_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceManifest {
    pub(crate) schema: String,
    pub(crate) robot_id: String,
    pub(crate) document: SourceDocument,
    root_package: SourcePackage,
    target: String,
    profile: String,
    features: Vec<String>,
    pub(crate) executables: Vec<SourceExecutable>,
    components: Vec<SourceComponentRecord>,
    #[serde(default)]
    pub(crate) simulation: Option<SourceSimulation>,
    /// Optional scenario nondeployable marker. Set by the case host
    /// when it builds a scenario bundle; ordinary source bundles
    /// omit the field. The supervisor admission policy rejects
    /// hardware launches of bundles that carry the marker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) scenario_marker: Option<String>,
    /// Validated program identity for the scenario bundle. The case
    /// host serializes this when it writes the manifest; the
    /// supervisor verifies the bounded program bytes against the
    /// recorded length and SHA-256 digest during admission. Bundles
    /// that carry `scenario_marker` but omit `scenario_program` are
    /// refused outright.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) scenario_program: Option<ScenarioProgramRef>,
}

/// Scenario program identity recorded in the source manifest. The
/// supervisor verifies the exact bounded program bytes against
/// `program_byte_length` and `program_digest` before admission so a
/// tampered bundle cannot drive the fixture.
///
/// `program_path` is a bundle-relative POSIX path with no `..`
/// segments, no absolute prefix, and no symlink escape from the
/// bundle root. The supervisor rejects anything else.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScenarioProgramRef {
    pub(crate) scenario_name: String,
    pub(crate) program_path: String,
    pub(crate) program_byte_length: u32,
    pub(crate) program_digest: String,
    pub(crate) fixture_instance_id: String,
    pub(crate) controlled_execution: bool,
}

/// The maximum size of a single scenario program artifact. P3 keeps
/// the cap low because scenario programs are short, bounded, and
/// fully decoded into typed structures before the fixture runs; an
/// admission that needs more than this is either a misuse or a
/// tampered bundle.
pub(crate) const MAX_SCENARIO_PROGRAM_BYTES: usize = 1024 * 1024;

impl ScenarioProgramRef {
    /// Validate the bundle-relative program path, read the bounded
    /// artifact against `bundle_root`, and verify its length and
    /// digest match the recorded identity. Returns the verified bytes
    /// so the caller can hand the same artifact to the fixture; the
    /// bytes are returned only when every invariant has been
    /// satisfied.
    pub(crate) fn verify_against(&self, bundle_root: &Path) -> Result<Vec<u8>> {
        if self.scenario_name.is_empty() {
            bail!("scenario program carries an empty scenario name");
        }
        if self.fixture_instance_id.is_empty() {
            bail!("scenario program carries an empty fixture instance id");
        }
        if self.program_byte_length == 0 {
            bail!("scenario program declares a zero byte length");
        }
        if self.program_byte_length as usize > MAX_SCENARIO_PROGRAM_BYTES {
            bail!(
                "scenario program declares {} bytes; cap is {}",
                self.program_byte_length,
                MAX_SCENARIO_PROGRAM_BYTES,
            );
        }
        if self.program_digest.len() != 64
            || !self
                .program_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            bail!(
                "scenario program `{}` has an invalid lowercase SHA-256 digest",
                self.scenario_name,
            );
        }
        let relative = safe_relative_path(&self.program_path)?;
        let absolute = bundle_root.join(relative);
        // Canonicalize the bundle root as well so the prefix check
        // works on platforms whose canonical path differs from the
        // input path (notably macOS, where `/var/...` resolves to
        // `/private/var/...`).
        let canonical_root = fs::canonicalize(bundle_root)
            .with_context(|| format!("cannot resolve bundle root {}", bundle_root.display(),))?;
        let metadata = fs::symlink_metadata(&absolute).with_context(|| {
            format!(
                "scenario program `{}` is missing at {}",
                self.scenario_name,
                absolute.display(),
            )
        })?;
        if metadata.file_type().is_symlink() {
            bail!(
                "scenario program `{}` must not be a symbolic link: {}",
                self.scenario_name,
                absolute.display(),
            );
        }
        if !metadata.is_file() {
            bail!(
                "scenario program `{}` is not a regular file: {}",
                self.scenario_name,
                absolute.display(),
            );
        }
        let canonical = absolute.canonicalize().with_context(|| {
            format!(
                "cannot resolve scenario program `{}` at {}",
                self.scenario_name,
                absolute.display(),
            )
        })?;
        if !canonical.starts_with(&canonical_root) {
            bail!(
                "scenario program `{}` escapes its bundle root: {}",
                self.scenario_name,
                canonical.display(),
            );
        }
        // Bounded read with a +1 trailing byte so an oversize file
        // is detected even if the declared `program_byte_length` was
        // also tampered upward.
        let file = fs::File::open(&canonical).with_context(|| {
            format!(
                "cannot open scenario program `{}` at {}",
                self.scenario_name,
                canonical.display(),
            )
        })?;
        let mut bytes = Vec::with_capacity(self.program_byte_length as usize);
        file.take(MAX_SCENARIO_PROGRAM_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .with_context(|| {
                format!(
                    "failed to read scenario program `{}` at {}",
                    self.scenario_name,
                    canonical.display(),
                )
            })?;
        if bytes.len() > MAX_SCENARIO_PROGRAM_BYTES {
            bail!(
                "scenario program `{}` exceeds the {}-byte cap",
                self.scenario_name,
                MAX_SCENARIO_PROGRAM_BYTES,
            );
        }
        if bytes.len() != self.program_byte_length as usize {
            bail!(
                "scenario program `{}` is {} bytes; manifest declares {}",
                self.scenario_name,
                bytes.len(),
                self.program_byte_length,
            );
        }
        let mut hasher = Sha256::new();
        sha2::Digest::update(&mut hasher, &bytes);
        let digest = hasher.finalize();
        let mut hex = String::with_capacity(64);
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(&mut hex, "{byte:02x}");
        }
        if hex != self.program_digest {
            bail!(
                "scenario program `{}` digest {hex} does not match recorded {}",
                self.scenario_name,
                self.program_digest,
            );
        }
        Ok(bytes)
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
    pub(crate) kind: String,
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
#[serde(deny_unknown_fields)]
pub(crate) struct SourceDocument {
    #[serde(default)]
    schema: Option<String>,
    robot: SourceRobot,
    #[serde(default)]
    brain: Option<serde_json::Value>,
    #[serde(default)]
    services: BTreeMap<String, SourceService>,
    #[serde(default)]
    connections: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct SourceService {
    #[serde(default)]
    implementation: Option<String>,
    #[serde(default)]
    binary: Option<String>,
    #[serde(default)]
    config: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceRobot {
    id: String,
    #[serde(default)]
    model: Option<std::path::PathBuf>,
    #[serde(default)]
    components: BTreeMap<String, SourceComponent>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceComponent {
    component: String,
    mount_site: String,
    #[serde(default)]
    driver: Option<serde_json::Value>,
    #[serde(default)]
    config: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourcePackage {
    id: String,
    name: String,
    source: String,
}

/// Supervisor projection of component package provenance. Physical models,
/// resources, and native binding semantics belong to the simulator and are
/// deliberately not interpreted by the hardware-capable supervisor.
#[derive(Clone, Debug, Deserialize)]
struct SourceComponentRecord {
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
    pub(crate) bytes: u64,
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
        manifest.document.connections = connections;
        Self::for_test(root, manifest)
    }
}

#[cfg(test)]
impl SourceManifest {
    pub(crate) fn for_test(
        robot_id: impl Into<String>,
        executables: Vec<SourceExecutable>,
    ) -> Self {
        Self {
            schema: SOURCE_SCHEMA.to_owned(),
            robot_id: robot_id.into(),
            document: SourceDocument {
                schema: Some("phoxal/robot/v0".to_owned()),
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
            scenario_marker: None,
            scenario_program: None,
            target: "host".to_owned(),
            profile: "dev".to_owned(),
            features: Vec::new(),
            executables,
            components: Vec::new(),
            simulation: None,
        }
    }
}

fn admit_source(root: PathBuf, manifest: SourceManifest) -> Result<SourceBundle> {
    if manifest.schema != SOURCE_SCHEMA {
        bail!("unsupported bundle schema `{}`", manifest.schema);
    }
    validate_segment(&manifest.robot_id, "robot_id")?;
    validate_source_document(&manifest)?;
    validate_source_metadata(&manifest)?;
    if let Some(simulation) = manifest.simulation.as_ref() {
        validate_source_simulation(simulation, &manifest.document.robot.components)?;
    }
    validate_simulation_executables(manifest.simulation.as_ref(), &manifest.executables)?;
    if manifest.executables.is_empty() {
        bail!("source bundle contains no executable records");
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut has_brain = false;
    for executable in &manifest.executables {
        if !matches!(executable.role.as_str(), "brain" | "service" | "driver") {
            bail!("unsupported executable role `{}`", executable.role);
        }
        validate_segment(&executable.instance, "executable instance")?;
        if !seen.insert(executable.instance.as_str()) {
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
        if executable.sha256.len() != 64
            || !executable
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            bail!(
                "executable `{}` has an invalid lowercase SHA-256 digest",
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
        verify_digest(&canonical, executable)?;
    }
    if !has_brain {
        bail!("source bundle is missing executable instance `brain`");
    }
    for (service, definition) in &manifest.document.services {
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
        if !matches!(
            provider.kind.as_str(),
            "state" | "sample" | "event" | "stream"
        ) {
            bail!(
                "simulation provider `{}.{}` has unsupported kind `{}`",
                provider.service_instance,
                provider.port,
                provider.kind
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
    let mut connections = BTreeMap::new();
    for (consumer, sources) in &manifest.document.connections {
        let (instance, _) = consumer
            .split_once('.')
            .with_context(|| format!("invalid connection consumer `{consumer}`"))?;
        if manifest
            .executables
            .iter()
            .any(|executable| executable.instance == instance)
        {
            connections.insert(consumer.clone(), sources.clone());
            continue;
        }
        let simulation = manifest
            .simulation
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
    if let Some(schema) = &manifest.document.schema
        && schema != "phoxal/robot/v0"
    {
        bail!("unsupported authored document schema `{schema}`");
    }
    if manifest.document.robot.id != manifest.robot_id {
        bail!(
            "bundle robot_id `{}` does not match document robot.id `{}`",
            manifest.robot_id,
            manifest.document.robot.id
        );
    }
    validate_segment(&manifest.document.robot.id, "document robot.id")?;
    // The source schema rejects unknown fields. Native model interpretation
    // remains owned by the simulator; the supervisor validates execution
    // artifacts and graph routes below.
    let _ = (
        &manifest.document.robot.model,
        &manifest.document.brain,
        &manifest.document.connections,
    );
    for (instance, component) in &manifest.document.robot.components {
        validate_segment(instance, "component instance")?;
        if component.component.is_empty() {
            bail!("component `{instance}` has an empty dependency key");
        }
        if component.mount_site.is_empty() {
            bail!("component `{instance}` has an empty mount link");
        }
        let _ = (&component.driver, &component.config);
    }
    for (service, definition) in &manifest.document.services {
        validate_segment(service, "service instance")?;
        if definition
            .implementation
            .as_deref()
            .is_some_and(str::is_empty)
        {
            bail!("service `{service}` has an empty implementation key");
        }
        if definition.binary.as_deref().is_some_and(str::is_empty) {
            bail!("service `{service}` has an empty binary target");
        }
    }
    Ok(())
}

fn validate_source_metadata(manifest: &SourceManifest) -> Result<()> {
    if manifest.root_package.id.is_empty()
        || manifest.root_package.name.is_empty()
        || manifest.root_package.source.is_empty()
    {
        bail!("source bundle root package metadata is incomplete");
    }
    if manifest.target.is_empty() || manifest.profile.is_empty() {
        bail!("source bundle target and profile metadata must not be empty");
    }
    if manifest.features.iter().any(String::is_empty) {
        bail!("source bundle features must not contain empty names");
    }
    let mut seen = std::collections::BTreeSet::new();
    for component in &manifest.components {
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

fn verify_digest(path: &Path, expected: &SourceExecutable) -> Result<()> {
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
    let digest = format!("{:x}", hasher.finalize());
    if bytes != expected.bytes || digest != expected.sha256 {
        bail!(
            "executable `{}` does not match its recorded size or SHA-256",
            expected.instance
        );
    }
    Ok(())
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
    fn a_source_bundle_admits_the_exact_manifest_and_executable_digest() {
        let directory = tempfile::tempdir().expect("temporary source bundle");
        let bin = directory.path().join("bin");
        fs::create_dir(&bin).expect("bundle bin directory");
        let executable = bin.join("brain");
        let executable_bytes = b"#!/bin/sh\nexit 0\n";
        fs::write(&executable, executable_bytes).expect("bundle executable");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
            .expect("make bundle executable runnable");
        let digest = format!("{:x}", Sha256::digest(executable_bytes));
        let manifest = format!(
            r#"{{
                "schema": "phoxal/bundle/v0",
                "robot_id": "fixture",
                "document": {{
                    "schema": "phoxal/robot/v0",
                    "robot": {{
                        "id": "fixture",
                        "model": null,
                        "components": {{}}
                    }},
                    "brain": null,
                    "services": {{}},
                    "connections": {{}}
                }},
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
                    "bytes": {},
                    "sha256": "{}",
                    "artifact": null
                }}],
                "components": []
            }}"#,
            executable_bytes.len(),
            digest
        );
        fs::write(directory.path().join("manifest.json"), manifest)
            .expect("source bundle manifest");

        let bundle = open(directory.path()).expect("source bundle admission");
        let Bundle::Source(bundle) = bundle;
        assert_eq!(bundle.manifest.robot_id, "fixture");
        assert_eq!(
            bundle
                .executables()
                .next()
                .expect("brain executable")
                .path(),
            Path::new("bin/brain")
        );
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
                kind: "sample".to_owned(),
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
                component: "bno085".to_owned(),
                mount_site: "base".to_owned(),
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
        let mut manifest = SourceManifest::for_test(
            "fixture",
            vec![SourceExecutable::for_test(
                "motion",
                "bin/motion",
                1,
                "0".repeat(64),
            )],
        );
        manifest.simulation = Some(simulation_fixture());
        manifest.document.connections = BTreeMap::from([
            ("imu.actuator".into(), serde_json::json!("motion.actuators")),
            (
                "motion.measurements".into(),
                serde_json::json!("imu.sample"),
            ),
        ]);
        let authored = manifest.document.connections.clone();
        let graph = execution_connections(&manifest).unwrap();
        assert_eq!(graph.len(), 1);
        assert_eq!(graph["motion.measurements"], "imu.sample");
        assert_eq!(manifest.document.connections, authored);

        for source in [
            serde_json::json!("motion.unbound"),
            serde_json::json!([]),
            serde_json::json!([null]),
        ] {
            manifest
                .document
                .connections
                .insert("imu.actuator".into(), source);
            assert!(execution_connections(&manifest).is_err());
        }
        manifest.document.connections = BTreeMap::from([(
            "missing.actuator".into(),
            serde_json::json!("motion.actuators"),
        )]);
        assert!(execution_connections(&manifest).is_err());
        manifest.document.connections = authored;
        manifest.simulation = None;
        assert!(execution_connections(&manifest).is_err());
    }

    fn write_program(directory: &tempfile::TempDir, relative: &str, bytes: &[u8]) -> String {
        let safe = Path::new(relative);
        let absolute = directory.path().join(safe);
        if let Some(parent) = absolute.parent() {
            fs::create_dir_all(parent).expect("program parent");
        }
        fs::write(&absolute, bytes).expect("write program");
        let mut hasher = Sha256::new();
        sha2::Digest::update(&mut hasher, bytes);
        let digest = hasher.finalize();
        let mut hex = String::with_capacity(64);
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(&mut hex, "{byte:02x}");
        }
        hex
    }

    fn program_ref(path: &str, bytes_len: u32, digest: String) -> ScenarioProgramRef {
        ScenarioProgramRef {
            scenario_name: "scenarios/Demo".to_owned(),
            program_path: path.to_owned(),
            program_byte_length: bytes_len,
            program_digest: digest,
            fixture_instance_id: "fixture".to_owned(),
            controlled_execution: true,
        }
    }

    #[test]
    fn scenario_program_verify_accepts_bundled_artifact() {
        let directory = tempfile::tempdir().expect("bundle root");
        let bytes = b"scenarios/Demo program bytes";
        let digest = write_program(&directory, "program.bin", bytes);
        let program = program_ref("program.bin", bytes.len() as u32, digest);
        let verified = program
            .verify_against(directory.path())
            .expect("verify succeeds");
        assert_eq!(verified, bytes);
    }

    #[test]
    fn scenario_program_verify_rejects_absolute_path() {
        let directory = tempfile::tempdir().expect("bundle root");
        let program = program_ref("/etc/passwd", 1, "0".repeat(64));
        let error = program
            .verify_against(directory.path())
            .expect_err("absolute path must be refused");
        assert!(format!("{error:#}").contains("not bundle-relative"));
    }

    #[test]
    fn scenario_program_verify_rejects_parent_traversal() {
        let directory = tempfile::tempdir().expect("bundle root");
        let program = program_ref("../outside.bin", 1, "0".repeat(64));
        let error = program
            .verify_against(directory.path())
            .expect_err("parent traversal must be refused");
        assert!(format!("{error:#}").contains("not bundle-relative"));
    }

    #[test]
    fn scenario_program_verify_rejects_length_mismatch() {
        let directory = tempfile::tempdir().expect("bundle root");
        let bytes = b"short";
        let digest = write_program(&directory, "program.bin", bytes);
        let program = program_ref("program.bin", bytes.len() as u32 + 16, digest);
        let error = program
            .verify_against(directory.path())
            .expect_err("length mismatch must be refused");
        assert!(format!("{error:#}").contains("manifest declares"));
    }

    #[test]
    fn scenario_program_verify_rejects_tampered_digest() {
        let directory = tempfile::tempdir().expect("bundle root");
        let bytes = b"intended";
        let digest = write_program(&directory, "program.bin", bytes);
        let mut tampered = digest;
        // Flip the first hex digit.
        let replacement = if tampered.starts_with('0') { '1' } else { '0' };
        unsafe {
            tampered.as_bytes_mut()[0] = replacement as u8;
        }
        let program = program_ref("program.bin", bytes.len() as u32, tampered);
        let error = program
            .verify_against(directory.path())
            .expect_err("digest mismatch must be refused");
        assert!(format!("{error:#}").contains("does not match recorded"));
    }

    #[test]
    fn scenario_program_verify_rejects_oversize_declaration() {
        let program = program_ref(
            "program.bin",
            u32::try_from(MAX_SCENARIO_PROGRAM_BYTES).unwrap() + 1,
            "0".repeat(64),
        );
        let directory = tempfile::tempdir().expect("bundle root");
        let error = program
            .verify_against(directory.path())
            .expect_err("oversize declaration must be refused");
        assert!(format!("{error:#}").contains("cap is"));
    }

    #[test]
    fn scenario_program_verify_rejects_invalid_digest() {
        let program = program_ref("program.bin", 1, "NOT_HEX".to_owned());
        let directory = tempfile::tempdir().expect("bundle root");
        let error = program
            .verify_against(directory.path())
            .expect_err("invalid digest must be refused");
        assert!(format!("{error:#}").contains("invalid lowercase SHA-256"));
    }

    #[test]
    fn scenario_program_verify_rejects_symlink_escape() {
        let directory = tempfile::tempdir().expect("bundle root");
        let outside = tempfile::tempdir().expect("outside");
        let outside_path = outside.path().join("secret.bin");
        fs::write(&outside_path, b"outside bytes").expect("write outside");
        let link_path = directory.path().join("escape.bin");
        std::os::unix::fs::symlink(&outside_path, &link_path).expect("symlink");
        let mut hasher = Sha256::new();
        sha2::Digest::update(&mut hasher, b"outside bytes");
        let digest = hasher.finalize();
        let mut hex = String::with_capacity(64);
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(&mut hex, "{byte:02x}");
        }
        let program = program_ref("escape.bin", 13, hex);
        let error = program
            .verify_against(directory.path())
            .expect_err("symlink must be refused");
        assert!(format!("{error:#}").contains("symbolic link"));
    }
}
