//! The one resolved bundle manifest shared by the compiler, supervisor,
//! simulator, and SDK participant runner.
//!
//! The manifest is the immutable authority for one compiled robot: an
//! artifact inventory storing each distinct executable once, an instance
//! inventory of independent launch identities, the resolved typed
//! connection graph, native composition facts, and the optional immutable
//! simulation contract. Authored `robot.yaml` stays with the robot author;
//! the deployed bundle never carries a copy of it.
//!
//! Bundle assembly stays in `cargo-phoxal`; this module owns only the inert
//! records and the pure structural validation every untrusted boundary
//! reuses before admitting a bundle.

#![deny(unsafe_code)]

use serde::{Deserialize, Serialize};

use super::document::ComponentDocument;
use super::{
    DescriptorSummary, InputDelivery, InputRecord, MethodShape, MethodSignature, OutputRecord,
    RuntimeRecord,
};

/// The resolved graph and artifact inventory for one compiled robot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "schema", deny_unknown_fields)]
pub enum BundleManifest {
    /// The resolved bundle: one artifact inventory, one instance inventory,
    /// one typed connection graph, and no authored document.
    #[serde(rename = "phoxal/bundle/v0")]
    V0 {
        /// Authored robot identity.
        robot_id: String,
        /// Concrete Cargo target triple. A supervisor
        /// refuses a bundle built for another platform.
        target: String,
        /// The explicit supervisor executable selection. The supervisor is
        /// part of the immutable bundle but never a participant child.
        supervisor: BundleSupervisor,
        /// Every distinct executable artifact, stored once.
        artifacts: Vec<BundleArtifactRecord>,
        /// Every independent launch instance, each referencing one artifact.
        instances: Vec<BundleInstance>,
        /// The resolved connection graph with typed endpoint references.
        connections: Vec<BundleConnection>,
        /// Every mounted component, including passive components.
        components: Vec<BundleComponent>,
        /// Bundle-relative component model source directories by instance.
        #[serde(default)]
        component_sources: std::collections::BTreeMap<String, String>,
        /// Portable robot model resources retained for native simulation.
        #[serde(default)]
        model: Option<BundleModelAssets>,
    },
}

/// The explicit supervisor executable selection of one bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleSupervisor {
    /// Bundle-relative supervisor executable path.
    pub path: String,
}

/// One selected build output stored once in the bundle.
///
/// The artifact identity is a bundle-local deterministic ID derived from
/// the complete selection (source or revision, version, target, profile,
/// features, and binary), so repeated instances of one selection share the
/// stored executable while unrelated selections never merge. Executable
/// contents are trusted and never digested.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleArtifactRecord {
    /// Bundle-local deterministic artifact identity.
    pub id: String,
    /// Bundle-relative executable path; `bin/` plus the artifact identity.
    pub path: String,
    /// Package provenance retained for diagnostics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<BundleArtifactProvenance>,
    /// The primary runtime contract and its descriptor inventory.
    pub runtime: RuntimeRecord,
    /// Original descriptor closure digests and file names.
    pub descriptors: Vec<DescriptorSummary>,
}

/// One artifact's package provenance, retained for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleArtifactProvenance {
    /// Cargo package name that produced the artifact.
    pub package: String,
    /// Cargo package version.
    pub version: String,
    /// Stable Cargo source identity.
    pub source: String,
}

/// The concrete target triple this process was built for.
///
/// A resolved bundle names its deployment target explicitly; `host` is not a
/// target, so assembly resolves the assembling host's triple and every
/// consumer refuses a bundle built for a different triple before launching
/// any participant.
pub fn host_execution_target() -> String {
    crate::artifact::application::HOST_EXECUTION_TARGET.to_owned()
}

/// One independent launch instance of one artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleInstance {
    /// Runtime instance identity; `brain` is reserved for the brain role.
    pub id: String,
    /// The launch role of this instance.
    pub role: InstanceRole,
    /// Referenced artifact identity from the artifact inventory.
    pub artifact: String,
    /// Resolved configuration for this instance. An absent field is an
    /// absent configuration; an explicit JSON `null` is refused while the
    /// manifest decodes, so a unit configuration stays distinguishable from
    /// malformed input.
    #[serde(default, skip_serializing_if = "InstanceConfig::is_absent")]
    pub config: InstanceConfig,
}

/// One instance's resolved configuration.
///
/// The canonical absent form is the omitted field. A present configuration
/// must be a JSON value other than `null`: a runtime declaring `Config = ()`
/// carries a `{"type": "null"}` schema and is launched with an absent
/// configuration, never an explicit null.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstanceConfig(Option<serde_json::Value>);

impl InstanceConfig {
    /// The absent configuration.
    pub fn absent() -> Self {
        Self(None)
    }

    /// One present configuration value. An explicit `null` is refused.
    pub fn present(value: serde_json::Value) -> Result<Self, String> {
        if value.is_null() {
            return Err(
                "an instance configuration must be a value other than null; omit the field                  for a unit configuration"
                    .to_owned(),
            );
        }
        Ok(Self(Some(value)))
    }

    /// Whether this is the absent configuration.
    pub fn is_absent(&self) -> bool {
        self.0.is_none()
    }

    /// The present configuration value, if any.
    pub fn value(&self) -> Option<&serde_json::Value> {
        self.0.as_ref()
    }
}

impl Serialize for InstanceConfig {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match &self.0 {
            Some(value) => serializer.serialize_some(value),
            None => serializer.serialize_none(),
        }
    }
}

impl<'de> Deserialize<'de> for InstanceConfig {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Option::<serde_json::Value>::deserialize(deserializer)?;
        match value {
            None => Err(serde::de::Error::custom(
                "an instance configuration must be a value other than null; omit the field                  for a unit configuration",
            )),
            Some(value) if value.is_null() => Err(serde::de::Error::custom(
                "an instance configuration must be a value other than null; omit the field                  for a unit configuration",
            )),
            Some(value) => Ok(Self(Some(value))),
        }
    }
}

/// The launch role of one runtime instance.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstanceRole {
    /// The robot's authored brain runtime.
    Brain,
    /// A composed service.
    Service,
    /// A physical component driver.
    Driver,
}

/// One typed endpoint reference in the resolved graph.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointReference {
    /// Owning instance identity.
    pub instance: String,
    /// Endpoint name on that instance.
    pub endpoint: String,
}

impl EndpointReference {
    /// Parses one `instance.endpoint` reference.
    pub fn parse(value: &str) -> Result<Self, String> {
        let (instance, endpoint) = value
            .split_once('.')
            .ok_or_else(|| format!("endpoint reference `{value}` has no instance separator"))?;
        validate_segment(instance, "endpoint instance")?;
        validate_segment(endpoint, "endpoint name")?;
        Ok(Self {
            instance: instance.to_owned(),
            endpoint: endpoint.to_owned(),
        })
    }
}

impl std::fmt::Display for EndpointReference {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.{}", self.instance, self.endpoint)
    }
}

/// One resolved connection: one consumer endpoint and its uniform source
/// list. Authored single-source and multi-source spellings lower to this one
/// record; consumers never rediscover string-versus-array shapes.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleConnection {
    /// The consuming endpoint.
    pub consumer: EndpointReference,
    /// The uniform source list; never empty.
    pub sources: Vec<EndpointReference>,
}

/// One mounted component retained in the resolved graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleComponent {
    /// Authored component instance identity.
    pub instance: String,
    /// Whether this component is an active physical driver. Passive
    /// components mount and compose but never launch a process.
    pub driver: bool,
    /// Cargo package name retained for diagnostics.
    pub package: String,
    /// Stable Cargo source identity.
    pub source: String,
    /// Persistent site in the parent robot model receiving this instance.
    pub mount_site: String,
    /// Component-owned semantic capabilities and native model-local bindings.
    pub definition: ComponentDocument,
}

/// Model files carried beside a compiled robot for native simulation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleModelAssets {
    /// Bundle-relative model entry.
    pub entry: String,
    /// Bundle-relative resource paths.
    pub resources: Vec<String>,
}

/// The simulator-facing facts selected while assembling one robot bundle.
///
/// This type is deliberately a neutral bundle record.  The project compiler
/// does not depend on the Runtime SDK, the supervisor, or a native simulator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleSimulation {
    /// Public simulation protocol implemented by the independent application.
    pub protocol: String,
    /// Scheduling mode selected for this bundle.
    pub mode: String,
    /// Exact closed-scene model identity supplied by the native application.
    pub model_identity: String,
    /// Common controlled quantum in nanoseconds.
    pub quantum_ns: u64,
    /// Complete generated observation provider requirements.
    pub providers: Vec<BundleSimulationProvider>,
    /// Exact setpoint-to-native-actuator bindings selected for the scene.
    pub actuation_bindings: Vec<BundleActuationBinding>,
}

/// One generated public observation provider required by a simulation run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleSimulationProvider {
    /// Phase-aligned publication frequency in millionths of one hertz.
    pub rate_microhertz: u64,
    /// Runtime service or brain instance owning the public port.
    pub service_instance: String,
    /// Generated public output port.
    pub port: String,
    /// Protobuf service declaring the generated output.
    pub service_fqn: String,
    /// Protobuf method declaring the generated output.
    pub method: String,
    /// Public observation semantic kind.
    pub shape: MethodShape,
    /// Whether admission replays the latest accepted observation.
    pub retained_latest: bool,
    /// Optional contract-owned validity interval for each observation.
    pub lease_valid_for_ms: Option<u64>,
    /// Request message identity from the generated port signature.
    pub input_fqn: String,
    /// Observation payload message identity from the generated port signature.
    pub payload_fqn: String,
    /// Maximum encoded provider payload admitted by the runtime.
    pub max_message_bytes: u32,
    /// Maximum provider items retained for one public port.
    pub max_buffered_items: u32,
}

/// One explicit generated observation-provider binding supplied by the native
/// simulator before bundle assembly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimulationProviderBinding {
    /// Phase-aligned publication frequency in millionths of one hertz.
    pub rate_microhertz: u64,
    /// Runtime driver instance owning the public port.
    pub service_instance: String,
    /// Generated public output port.
    pub port: String,
    /// Public observation semantic kind.
    pub shape: MethodShape,
    /// Whether admission replays the latest accepted observation.
    pub retained_latest: bool,
    /// Optional contract-owned validity interval for each observation.
    pub lease_valid_for_ms: Option<u64>,
    /// Request message identity from the generated port signature.
    pub input_fqn: String,
    /// Observation payload message identity from the generated port signature.
    pub payload_fqn: String,
}

/// One exact generated setpoint output and its native actuator membership.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleActuationBinding {
    /// Runtime service instance owning the setpoint output.
    pub service_instance: String,
    /// Generated setpoint output port.
    pub port: String,
    /// Setpoint payload message identity from the generated signature.
    pub payload_fqn: String,
    /// Native actuator names covered by this output.
    pub actuator_ids: Vec<String>,
}

/// Native scene facts and explicit generated bindings returned by the
/// independent simulator before bundle assembly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimulationModelFacts {
    /// Exact closed-scene model identity.
    pub model_identity: String,
    /// Exact native physics quantum in nanoseconds.
    pub quantum_ns: u64,
    /// Complete explicit observation-provider bindings for substituted
    /// physical drivers.
    pub providers: Vec<SimulationProviderBinding>,
    /// Complete explicit setpoint-to-native-actuator bindings.
    pub actuation_bindings: Vec<BundleActuationBinding>,
}

/// Validates one identifier segment shared by instances, endpoints, and
/// robot identities.
pub fn validate_segment(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 64
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        return Err(format!(
            "{label} must be 1-64 lowercase ASCII letters, digits, '-' or '_'"
        ));
    }
    Ok(())
}

/// Validates one bundle-relative path string: normalized and relative.
pub fn validate_relative_path(value: &str, label: &str) -> Result<(), String> {
    let path = std::path::Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || value.contains('\\')
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir
                    | std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(format!(
            "{label} path {value:?} is not normalized and relative"
        ));
    }
    Ok(())
}

/// Validates one SHA-256 digest string.
pub fn validate_digest(value: &str, label: &str) -> Result<(), String> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("{label} is not a SHA-256 digest"));
    }
    Ok(())
}

/// The structurally valid view of one resolved bundle, built once from the
/// manifest and reused by every consumer that must not re-derive graph facts
/// from strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedBundle {
    /// Authored robot identity.
    pub robot_id: String,
    /// Target platform marker.
    pub target: String,
    /// The supervisor executable selection.
    pub supervisor: BundleSupervisor,
    /// Artifact records keyed by artifact identity.
    pub artifacts: std::collections::BTreeMap<String, BundleArtifactRecord>,
    /// Configuration-resolved contracts keyed by instance identity.
    pub resolved_runtimes: std::collections::BTreeMap<String, RuntimeRecord>,
    /// Launch instances keyed by instance identity.
    pub instances: std::collections::BTreeMap<String, BundleInstance>,
    /// The execution graph keyed by consumer endpoint reference. Driver
    /// inputs natively substituted by actuation bindings are absent.
    pub execution_connections:
        std::collections::BTreeMap<EndpointReference, Vec<EndpointReference>>,
    /// Every mounted component, keyed by instance identity.
    pub components: std::collections::BTreeMap<String, BundleComponent>,
    /// Component model source directories by instance.
    pub component_sources: std::collections::BTreeMap<String, String>,
    /// Portable robot model resources.
    pub model: Option<BundleModelAssets>,
    /// The immutable simulation contract, when present.
    pub simulation: Option<BundleSimulation>,
}

impl AdmittedBundle {
    /// Validates one decoded manifest into the shared admitted view.
    ///
    /// This is the one pure structural validation of the resolved model:
    /// every untrusted boundary decodes, then calls this before reading any
    /// record. On-disk facts such as executable presence and confinement stay
    /// with the process that launches or executes them.
    pub fn validate(manifest: BundleManifest) -> Result<Self, String> {
        let BundleManifest::V0 {
            robot_id,
            target,
            supervisor,
            artifacts,
            instances,
            connections,
            components,
            component_sources,
            model,
        } = manifest;
        validate_segment(&robot_id, "robot_id")?;
        if target.is_empty() {
            return Err("bundle target must not be empty".to_owned());
        }
        validate_relative_path(&supervisor.path, "supervisor path")?;
        if !supervisor.path.starts_with("bin/") {
            return Err(format!(
                "supervisor path {} is not under bin/",
                supervisor.path
            ));
        }

        let mut artifact_records = std::collections::BTreeMap::new();
        for artifact in artifacts {
            validate_segment(&artifact.id, "artifact identity")?;
            validate_relative_path(&artifact.path, "artifact path")?;
            let expected_path = format!("bin/{}", artifact.id);
            if artifact.path != expected_path {
                return Err(format!(
                    "artifact path {} does not match its identity",
                    artifact.path
                ));
            }
            validate_runtime_record(&artifact.runtime, "artifact")?;
            // A minimal runtime with no generated contracts retains no
            // descriptor files; when any are retained they must be
            // well-formed, deduplicated, and safely named.
            validate_descriptors(&artifact.id, &artifact.descriptors)?;
            if artifact_records
                .insert(artifact.id.clone(), artifact.clone())
                .is_some()
            {
                return Err(format!("artifact {} is duplicated", artifact.id));
            }
        }
        if artifact_records.is_empty() {
            return Err("bundle contains no artifacts".to_owned());
        }

        let mut instance_records = std::collections::BTreeMap::new();
        let mut brain_count = 0;
        for instance in instances {
            validate_segment(&instance.id, "instance")?;
            if instance.id == "brain" {
                if instance.role != InstanceRole::Brain {
                    return Err("only the brain role may use instance `brain`".to_owned());
                }
                brain_count += 1;
            } else if instance.role == InstanceRole::Brain {
                return Err(format!(
                    "brain role instance `{}` must use the identity `brain`",
                    instance.id
                ));
            }
            if !artifact_records.contains_key(&instance.artifact) {
                return Err(format!(
                    "instance `{}` references unknown artifact {}",
                    instance.id, instance.artifact
                ));
            }
            if instance_records
                .insert(instance.id.clone(), instance.clone())
                .is_some()
            {
                return Err(format!("instance {} is duplicated", instance.id));
            }
        }
        if brain_count != 1 {
            return Err("bundle must contain exactly one brain instance".to_owned());
        }
        let mut resolved_runtimes = std::collections::BTreeMap::new();
        for (id, instance) in &instance_records {
            let artifact = &artifact_records[&instance.artifact];
            let runtime = &artifact.runtime;
            let RuntimeRecord::V0 { config_schema, .. } = runtime;
            validate_config_schema(id, &instance.config, config_schema)?;
            let effective = instance
                .config
                .value()
                .cloned()
                .or_else(|| effective_absent_config(config_schema))
                .ok_or("missing effective configuration")?;
            resolved_runtimes.insert(id.clone(), runtime.resolve_outputs(&effective)?);
        }
        let mut component_records = std::collections::BTreeMap::new();
        for component in components {
            validate_segment(&component.instance, "component instance")?;
            if component.mount_site.is_empty() {
                return Err(format!(
                    "component `{}` has an empty mount site",
                    component.instance
                ));
            }
            if component_records
                .insert(component.instance.clone(), component.clone())
                .is_some()
            {
                return Err(format!("component {} is duplicated", component.instance));
            }
        }
        for (instance, source) in &component_sources {
            validate_segment(instance, "component source instance")?;
            validate_relative_path(source, "component source")?;
            if !component_records.contains_key(instance) {
                return Err(format!(
                    "component source {instance} has no component record"
                ));
            }
        }
        if let Some(model) = model.as_ref() {
            validate_relative_path(&model.entry, "model entry")?;
            if !model
                .resources
                .iter()
                .any(|resource| resource == &model.entry)
            {
                return Err(format!("bundle model entry {} is not staged", model.entry));
            }
            for resource in &model.resources {
                validate_relative_path(resource, "model resource")?;
            }
        }

        let runtime_records = resolved_runtimes
            .iter()
            .map(|(id, runtime)| (id.as_str(), runtime))
            .collect();
        let execution_connections =
            validate_connections(connections, &instance_records, &runtime_records)?;
        Ok(Self {
            robot_id,
            target,
            supervisor,
            artifacts: artifact_records,
            resolved_runtimes,
            instances: instance_records,
            execution_connections,
            components: component_records,
            component_sources,
            model,
            simulation: None,
        })
    }

    /// Returns the runtime record one instance launches.
    pub fn instance_runtime(&self, instance: &str) -> Option<&RuntimeRecord> {
        self.resolved_runtimes.get(instance)
    }
}

/// Lowers the resolved connection list into the execution graph.
///
/// Every connection resolves against its authored compiled endpoints.
/// Native implementation selection never removes or replaces graph entries.
fn validate_connections(
    connections: Vec<BundleConnection>,
    instances: &std::collections::BTreeMap<String, BundleInstance>,
    runtimes: &std::collections::BTreeMap<&str, &RuntimeRecord>,
) -> Result<std::collections::BTreeMap<EndpointReference, Vec<EndpointReference>>, String> {
    let mut graph = std::collections::BTreeMap::new();
    let mut consumers = std::collections::BTreeSet::new();
    for connection in connections {
        let consumer = connection.consumer;
        validate_segment(&consumer.instance, "connection consumer instance")?;
        validate_segment(&consumer.endpoint, "connection consumer endpoint")?;
        if !consumers.insert(consumer.clone()) {
            return Err(format!("connection consumer {consumer} is duplicated"));
        }
        for source in &connection.sources {
            validate_segment(&source.instance, "connection source instance")?;
            validate_segment(&source.endpoint, "connection source endpoint")?;
        }
        if connection.sources.is_empty() {
            return Err(format!("connection {consumer} has an empty source list"));
        }
        if !instances.contains_key(&consumer.instance) {
            return Err(format!(
                "connection consumer {consumer} has no authored instance"
            ));
        }
        let runtime = runtimes
            .get(consumer.instance.as_str())
            .ok_or_else(|| format!("connection consumer {consumer} has no runtime record"))?;
        let input = find_input(runtime, &consumer.endpoint).ok_or_else(|| {
            format!("connection consumer {consumer} names no connectable input of its runtime")
        })?;
        if input.delivery == InputDelivery::CallCompletions && connection.sources.len() != 1 {
            return Err(format!(
                "outgoing call {consumer} requires exactly one provider"
            ));
        }
        let mut sources = Vec::with_capacity(connection.sources.len());
        for source in connection.sources {
            if !instances.contains_key(&source.instance) {
                return Err(format!(
                    "connection source {source} has no authored instance"
                ));
            }
            let producer = runtimes
                .get(source.instance.as_str())
                .ok_or_else(|| format!("connection source {source} has no runtime record"))?;
            if input.delivery == InputDelivery::CallCompletions {
                let ingress = find_input(producer, &source.endpoint)
                    .filter(|input| input.delivery == InputDelivery::CallIngress)
                    .ok_or_else(|| {
                        format!("connection source {source} names no public call ingress")
                    })?;
                let expected = input.signature.as_ref().ok_or_else(|| {
                    format!("outgoing call field {consumer} has no declared contract")
                })?;
                let served = ingress
                    .signature
                    .as_ref()
                    .ok_or_else(|| format!("call provider {source} has no declared contract"))?;
                if expected.shape != MethodShape::Call
                    || served.shape != MethodShape::Call
                    || expected.service != served.service
                    || expected.request != served.request
                    || expected.response != served.response
                    || expected.lease_valid_for_ms != served.lease_valid_for_ms
                {
                    return Err(format!(
                        "outgoing call {consumer} disagrees with provider {source}"
                    ));
                }
                sources.push(source);
                continue;
            }
            let output = find_output(producer, &source.endpoint).ok_or_else(|| {
                format!("connection source {source} names no served port of its producer")
            })?;
            check_wire_contract(&consumer, input, &source, output)?;
            sources.push(source);
        }
        graph.insert(consumer, sources);
    }
    Ok(graph)
}

/// The connectable input one consumer endpoint names, if any.
///
/// A public port matches by its served name; a private observation input
/// matches by its field name. Private plumbing roles are not connectable.
fn find_input<'a>(runtime: &'a RuntimeRecord, endpoint: &str) -> Option<&'a InputRecord> {
    let RuntimeRecord::V0 { inputs, .. } = runtime;
    inputs.iter().find(|input| {
        if input.delivery == InputDelivery::CallCompletions {
            input.name == endpoint
        } else {
            input.port.as_deref() == Some(endpoint)
                || input.port.is_none() && input.name == endpoint
        }
    })
}

/// The served method output one source endpoint names, if any.
fn find_output<'a>(runtime: &'a RuntimeRecord, endpoint: &str) -> Option<&'a OutputRecord> {
    let RuntimeRecord::V0 { outputs, .. } = runtime;
    outputs
        .iter()
        .find(|output| output.port.as_deref() == Some(endpoint))
}

/// Checks that one consumer input and one producer output agree on the wire
/// contract their roles imply.
fn check_wire_contract(
    consumer: &EndpointReference,
    input: &InputRecord,
    source: &EndpointReference,
    output: &OutputRecord,
) -> Result<(), String> {
    let signature = output
        .signature
        .as_ref()
        .ok_or_else(|| format!("connection source {source} serves no generated signature"))?;
    match input.delivery {
        InputDelivery::LeasedValue => {
            // A leased setpoint consumer receives the observation method's
            // response payload under a finite lease.
            if let Some(payload) = input.request_fqn.as_deref()
                && payload != signature.response
            {
                return Err(format!(
                    "connection {consumer} <- {source} expects setpoint payload `{payload}` but the source publishes `{}`",
                    signature.response
                ));
            }
            if signature.shape != MethodShape::Observation {
                return Err(format!(
                    "connection {consumer} <- {source} leases from a non-observation method"
                ));
            }
            if !signature.lease_valid_for_ms.is_some_and(|lease| lease > 0) {
                return Err(format!(
                    "connection {consumer} <- {source} leases from a method without a positive lease"
                ));
            }
            if signature.request != "google.protobuf.Empty" {
                return Err(format!(
                    "connection {consumer} <- {source} leases from a non-canonical observation"
                ));
            }
        }
        InputDelivery::ObservationLatest | InputDelivery::ObservationHistory => {
            if signature.shape != MethodShape::Observation {
                return Err(format!(
                    "connection {consumer} <- {source} feeds an observation input from a non-observation method"
                ));
            }
            if let Some(payload) = input.response_fqn.as_deref()
                && payload != signature.response
            {
                return Err(format!(
                    "connection {consumer} <- {source} expects payload `{payload}` but the source publishes `{}`",
                    signature.response
                ));
            }
        }
        InputDelivery::CallIngress => {
            if signature.shape != MethodShape::Call {
                return Err(format!(
                    "connection {consumer} <- {source} feeds a call ingress from a non-call method"
                ));
            }
            if let Some(payload) = input.request_fqn.as_deref()
                && payload != signature.request
            {
                return Err(format!(
                    "connection {consumer} <- {source} expects request payload `{payload}` but the source sends `{}`",
                    signature.request
                ));
            }
            if let Some(payload) = input.response_fqn.as_deref()
                && payload != signature.response
            {
                return Err(format!(
                    "connection {consumer} <- {source} expects response payload `{payload}` but the source returns `{}`",
                    signature.response
                ));
            }
        }
        InputDelivery::CallResult | InputDelivery::CallTarget | InputDelivery::CallCompletions => {
            return Err(format!(
                "connection consumer {consumer} targets the private input role `{:?}`",
                input.delivery
            ));
        }
    }
    Ok(())
}

/// Checks one consumer input against a native simulation provider.
/// Validates one instance's effective configuration against its runtime's
/// compiled JSON schema.
///
/// An absent configuration resolves to the runtime's effective value: the
/// unit `null` when the schema declares one, an empty object for object
/// schemas, and a refusal otherwise, so a schema with required fields
/// cannot be satisfied by omission. Validation uses the complete JSON
/// Schema semantics the compiled schemas carry, never a weaker subset.
fn validate_config_schema(
    instance: &str,
    config: &InstanceConfig,
    schema: &serde_json::Value,
) -> Result<(), String> {
    let effective = match config.value() {
        Some(value) => value.clone(),
        None => effective_absent_config(schema).ok_or_else(|| {
            format!(
                "instance {instance} omits its configuration but the compiled schema \
                 requires a value"
            )
        })?,
    };
    let validator = jsonschema::validator_for(schema).map_err(|error| {
        format!("instance {instance} carries an invalid compiled config schema: {error}")
    })?;
    if let Some(error) = validator.iter_errors(&effective).next() {
        return Err(format!(
            "instance {instance} configuration is invalid: {} at {}",
            error,
            error.instance_path()
        ));
    }
    Ok(())
}

/// The effective configuration value for an absent authored entry.
fn effective_absent_config(schema: &serde_json::Value) -> Option<serde_json::Value> {
    if let Some(accepts) = schema.as_bool() {
        return accepts.then_some(serde_json::Value::Null);
    }
    let types = schema.get("type")?;
    let names: Vec<&str> = match types {
        serde_json::Value::String(name) => vec![name.as_str()],
        serde_json::Value::Array(names) => names.iter().filter_map(|name| name.as_str()).collect(),
        _ => return None,
    };
    if names.contains(&"null") {
        Some(serde_json::Value::Null)
    } else if names.contains(&"object") {
        Some(serde_json::Value::Object(serde_json::Map::new()))
    } else {
        None
    }
}

/// Lowers the resolved connection list into the execution graph.
///
/// Every endpoint is resolved against the applicable admitted runtime,
/// or native contract before it enters the graph: a consumer must
/// name one connectable input of its instance, a source must name one served
/// method port of its producer, and the two must agree on the wire contract
/// their roles imply. Consumers whose instance is present keep their sources.
/// A driver input is delivered natively by actuation substitution, so its
/// consumer entry is admitted only when the actuation bindings cover it and
/// is then removed from the execution graph: native actuation replaces
/// driver input delivery, so no nonexistent driver may appear in the
/// receiver roster.
/// Validates one runtime record's structural invariants.
pub fn validate_runtime_record(record: &RuntimeRecord, label: &str) -> Result<(), String> {
    let RuntimeRecord::V0 {
        period_ms,
        timeout_ms,
        init_timeout_ms,
        inputs,
        outputs,
        conversions,
        ..
    } = record;
    if *period_ms == 0 || *timeout_ms == 0 || *init_timeout_ms == 0 {
        return Err(format!("{label} runtime timing must be positive"));
    }
    let mut input_names = std::collections::BTreeSet::new();
    for input in inputs {
        if !input_names.insert(input.name.as_str()) {
            return Err(format!(
                "{label} runtime input `{}` is duplicated",
                input.name
            ));
        }
        if let Some(signature) = input.signature.as_ref() {
            validate_signature_identity(signature)?;
            if input.port.as_deref() != Some(signature.endpoint.as_str()) {
                return Err(format!(
                    "{label} runtime input `{}` port and signature endpoint disagree",
                    input.name
                ));
            }
        }
        if input.port.is_some() && input.signature.is_none() {
            return Err(format!(
                "{label} runtime input `{}` exposes a port without its generated signature",
                input.name
            ));
        }
    }
    let mut output_names = std::collections::BTreeSet::new();
    for output in outputs.iter() {
        if let Some(family) = &output.family {
            family.validate(output)?;
        }
        if !output_names.insert(output.name.as_str()) {
            return Err(format!(
                "{label} runtime output `{}` is duplicated",
                output.name
            ));
        }
        if let Some(signature) = output.signature.as_ref() {
            validate_signature_identity(signature)?;
            if output.port.as_deref() != Some(signature.endpoint.as_str()) {
                return Err(format!(
                    "{label} runtime output `{}` port and signature endpoint disagree",
                    output.name
                ));
            }
            if let Some(lease) = signature.lease_valid_for_ms
                && lease == 0
            {
                return Err(format!(
                    "{label} runtime output `{}` declares a non-positive lease",
                    output.name
                ));
            }
        }
        if output.port.is_none() || output.signature.is_none() {
            return Err(format!(
                "{label} output `{}` serves no generated endpoint",
                output.name
            ));
        }
    }
    let mut consumers = std::collections::BTreeSet::new();
    for route in conversions {
        EndpointReference::parse(&route.producer)?;
        EndpointReference::parse(&route.consumer)?;
        if !consumers.insert(&route.consumer) {
            return Err(format!(
                "{label} compiled conversion consumer is duplicated: {}",
                route.consumer
            ));
        }
        if find_input(record, &route.input_endpoint).is_none()
            || find_output(record, &route.output_endpoint).is_none()
        {
            return Err(format!(
                "{label} compiled conversion refers to an absent runtime endpoint"
            ));
        }
    }
    Ok(())
}

/// Validates one artifact's descriptor inventory invariants.
fn validate_descriptors(artifact: &str, descriptors: &[DescriptorSummary]) -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    for descriptor in descriptors {
        validate_digest(&descriptor.sha256, "descriptor digest")?;
        if descriptor.bytes == 0 {
            return Err(format!(
                "artifact {artifact} descriptor {} has no bytes",
                descriptor.sha256
            ));
        }
        if descriptor.files.is_empty() {
            return Err(format!(
                "artifact {artifact} descriptor {} retains no files",
                descriptor.sha256
            ));
        }
        let mut files = std::collections::BTreeSet::new();
        for file in &descriptor.files {
            validate_relative_path(file, "descriptor file")?;
            if !files.insert(file.as_str()) {
                return Err(format!(
                    "artifact {artifact} descriptor {} lists file {file} twice",
                    descriptor.sha256
                ));
            }
        }
        if !seen.insert(descriptor.sha256.as_str()) {
            return Err(format!(
                "artifact {artifact} lists descriptor {} twice",
                descriptor.sha256
            ));
        }
    }
    Ok(())
}

fn validate_signature_identity(signature: &MethodSignature) -> Result<(), String> {
    if signature.endpoint.is_empty()
        || signature.service.is_empty()
        || signature.method.is_empty()
        || signature.request.is_empty()
        || signature.response.is_empty()
    {
        return Err(format!(
            "generated signature for `{}` has an empty identity",
            signature.endpoint
        ));
    }
    Ok(())
}

/// Validates the immutable simulation contract against the component set.
pub(super) fn validate_simulation(
    simulation: &BundleSimulation,
    components: &std::collections::BTreeMap<String, BundleComponent>,
) -> Result<(), String> {
    if simulation.protocol != "phoxal.simulation.v1" {
        return Err(format!(
            "unsupported simulation protocol `{}`; expected phoxal.simulation.v1",
            simulation.protocol
        ));
    }
    if simulation.mode != "controlled" {
        return Err(format!(
            "unsupported simulation mode `{}`; expected controlled",
            simulation.mode
        ));
    }
    if simulation.model_identity.is_empty()
        || simulation.model_identity.len() > 512
        || !simulation.model_identity.is_ascii()
        || simulation.model_identity.chars().any(char::is_whitespace)
    {
        return Err("simulation model identity is invalid".to_owned());
    }
    if simulation.quantum_ns == 0 {
        return Err("simulation quantum must be positive".to_owned());
    }
    if simulation.providers.is_empty() {
        return Err("simulation provider set must not be empty".to_owned());
    }
    let mut providers = std::collections::BTreeSet::new();
    for provider in &simulation.providers {
        validate_segment(
            &provider.service_instance,
            "simulation provider service instance",
        )?;
        validate_segment(&provider.port, "simulation provider port")?;
        let driver = components
            .get(&provider.service_instance)
            .is_some_and(|component| component.driver);
        if !driver {
            return Err(format!(
                "simulation provider `{}.{}` is not owned by a selected physical driver",
                provider.service_instance, provider.port
            ));
        }
        if provider.shape != MethodShape::Observation {
            return Err(format!(
                "simulation provider `{}.{}` must be an observation",
                provider.service_instance, provider.port
            ));
        }
        if provider.input_fqn.is_empty()
            || provider.payload_fqn.is_empty()
            || provider.service_fqn.is_empty()
            || provider.method.is_empty()
        {
            return Err(format!(
                "simulation provider `{}.{}` has an empty message identity",
                provider.service_instance, provider.port
            ));
        }
        if provider.max_message_bytes == 0 || provider.max_buffered_items == 0 {
            return Err(format!(
                "simulation provider `{}.{}` has a non-positive public bound",
                provider.service_instance, provider.port
            ));
        }
        if !providers.insert((&provider.service_instance, &provider.port)) {
            return Err(format!(
                "simulation provider `{}.{}` is duplicated",
                provider.service_instance, provider.port
            ));
        }
    }
    if simulation.actuation_bindings.is_empty() {
        return Err("simulation actuation binding set must not be empty".to_owned());
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
            return Err(format!(
                "simulation actuation `{}.{}` must carry a payload identity and native actuator IDs",
                binding.service_instance, binding.port
            ));
        }
        if !bindings.insert((&binding.service_instance, &binding.port)) {
            return Err(format!(
                "simulation actuation `{}.{}` is duplicated",
                binding.service_instance, binding.port
            ));
        }
        for actuator in &binding.actuator_ids {
            if actuator.is_empty()
                || actuator.len() > 64
                || !actuator.is_ascii()
                || actuator.chars().any(char::is_whitespace)
            {
                return Err(
                    "simulation actuation contains an invalid native actuator ID".to_owned(),
                );
            }
            if !actuators.insert(actuator) {
                return Err(format!(
                    "native actuator `{actuator}` is mapped more than once"
                ));
            }
        }
    }
    Ok(())
}

// The leased-observation semantics of one served setpoint source live with
// the runtime admission that applies them; the manifest carries only the
// shared typed signature records they validate.

#[cfg(test)]
mod tests {

    use super::*;

    fn runtime_record() -> RuntimeRecord {
        RuntimeRecord::V0 {
            record: super::super::RUNTIME_RECORD.to_owned(),
            conversions: Vec::new(),
            period_ms: 20,
            timeout_ms: 100,
            init_timeout_ms: 1_000,
            config_schema: serde_json::json!({"type": "object"}),
            inputs: Vec::new(),
            outputs: Vec::new(),
        }
    }

    fn artifact(id: &str) -> BundleArtifactRecord {
        BundleArtifactRecord {
            id: id.to_owned(),
            path: format!("bin/{id}"),
            provenance: None,
            runtime: runtime_record(),
            descriptors: vec![DescriptorSummary {
                sha256: "ab".repeat(32),
                bytes: 8,
                files: vec!["fixture.proto".to_owned()],
            }],
        }
    }

    /// A motion service fixture: an odometry consumer input and a leased
    /// actuator setpoint source.
    fn motion_runtime_record() -> RuntimeRecord {
        RuntimeRecord::V0 {
            record: super::super::RUNTIME_RECORD.to_owned(),
            conversions: Vec::new(),
            period_ms: 20,
            timeout_ms: 100,
            init_timeout_ms: 1_000,
            config_schema: serde_json::json!({"type": "object"}),
            inputs: vec![
                InputRecord {
                    name: "measurements".to_owned(),
                    delivery: InputDelivery::ObservationLatest,
                    max_age_ms: None,
                    max_items: None,
                    max_bytes: None,
                    port: None,
                    signature: None,
                    request_fqn: None,
                    response_fqn: Some("fixture.Imu".to_owned()),
                    response_max_bytes: None,
                    response_max_items: None,
                },
                InputRecord {
                    name: "actuator".to_owned(),
                    delivery: InputDelivery::LeasedValue,
                    max_age_ms: None,
                    max_items: None,
                    max_bytes: None,
                    port: None,
                    signature: None,
                    request_fqn: Some("fixture.Actuators".to_owned()),
                    response_fqn: None,
                    response_max_bytes: None,
                    response_max_items: None,
                },
            ],
            outputs: vec![OutputRecord {
                family: None,
                family_template: None,
                name: "actuators".to_owned(),
                port: Some("actuators".to_owned()),
                signature: Some(MethodSignature {
                    endpoint: "actuators".to_owned(),
                    service: "fixture.Motion".to_owned(),
                    method: "Actuators".to_owned(),
                    shape: MethodShape::Observation,
                    request: "google.protobuf.Empty".to_owned(),
                    response: "fixture.Actuators".to_owned(),
                    retained_latest: true,
                    lease_valid_for_ms: Some(100),
                }),
                max_items: None,
                max_bytes: None,
                max_request_bytes: None,
                every_steps: None,
                bootstrap: false,
                timeout_ms: None,
            }],
        }
    }

    /// A brain fixture serving one leased actuator setpoint source.
    fn brain_leased_source_runtime() -> RuntimeRecord {
        RuntimeRecord::V0 {
            record: super::super::RUNTIME_RECORD.to_owned(),
            conversions: Vec::new(),
            period_ms: 20,
            timeout_ms: 100,
            init_timeout_ms: 1_000,
            config_schema: serde_json::json!({"type": "object"}),
            inputs: Vec::new(),
            outputs: vec![OutputRecord {
                family: None,
                family_template: None,
                name: "actuators".to_owned(),
                port: Some("actuators".to_owned()),
                signature: Some(MethodSignature {
                    endpoint: "actuators".to_owned(),
                    service: "fixture.Motion".to_owned(),
                    method: "Actuators".to_owned(),
                    shape: MethodShape::Observation,
                    request: "google.protobuf.Empty".to_owned(),
                    response: "fixture.Actuators".to_owned(),
                    retained_latest: true,
                    lease_valid_for_ms: Some(100),
                }),
                max_items: None,
                max_bytes: None,
                max_request_bytes: None,
                every_steps: None,
                bootstrap: false,
                timeout_ms: None,
            }],
        }
    }

    fn manifest() -> BundleManifest {
        BundleManifest::V0 {
            robot_id: "fixture".to_owned(),
            target: "host".to_owned(),
            supervisor: BundleSupervisor {
                path: "bin/supervisor".to_owned(),
            },
            artifacts: vec![artifact(&"aa".repeat(32))],
            instances: vec![BundleInstance {
                id: "brain".to_owned(),
                role: InstanceRole::Brain,
                artifact: "aa".repeat(32),
                config: InstanceConfig::absent(),
            }],
            connections: Vec::new(),
            components: Vec::new(),
            component_sources: std::collections::BTreeMap::new(),
            model: None,
        }
    }

    #[test]
    fn outgoing_calls_bind_the_declared_field_to_a_matching_public_ingress() {
        let mut value = manifest();
        let signature = MethodSignature {
            endpoint: "start".into(),
            service: "fixture.Start".into(),
            method: "start".into(),
            shape: MethodShape::Call,
            request: "fixture.Request".into(),
            response: "fixture.Response".into(),
            retained_latest: false,
            lease_valid_for_ms: None,
        };
        let mut required = signature.clone();
        required.method = "client_start".into();
        let field = InputRecord {
            name: "start_countdown".into(),
            delivery: InputDelivery::CallCompletions,
            max_age_ms: None,
            max_items: Some(8),
            max_bytes: Some(1024),
            port: Some("start".into()),
            signature: Some(required),
            request_fqn: None,
            response_fqn: None,
            response_max_bytes: None,
            response_max_items: None,
        };
        let BundleManifest::V0 {
            artifacts,
            instances,
            connections,
            ..
        } = &mut value;
        let RuntimeRecord::V0 { inputs, .. } = &mut artifacts[0].runtime;
        inputs.push(field.clone());
        let mut provider = artifact("provider");
        let RuntimeRecord::V0 { inputs, .. } = &mut provider.runtime;
        let mut ingress = field;
        ingress.name = "start".into();
        ingress.delivery = InputDelivery::CallIngress;
        ingress.signature = Some(signature.clone());
        inputs.push(ingress);
        artifacts.push(provider);
        instances.push(BundleInstance {
            id: "provider".into(),
            role: InstanceRole::Service,
            artifact: "provider".into(),
            config: InstanceConfig::absent(),
        });
        connections.push(BundleConnection {
            consumer: EndpointReference::parse("brain.start_countdown").expect("consumer"),
            sources: vec![EndpointReference::parse("provider.start").expect("provider")],
        });
        AdmittedBundle::validate(value.clone()).expect("declared call field admits");
        let BundleManifest::V0 { artifacts, .. } = &mut value;
        let RuntimeRecord::V0 { inputs, .. } = &mut artifacts[1].runtime;
        inputs[0]
            .signature
            .as_mut()
            .expect("ingress signature")
            .response = "fixture.WrongResponse".into();
        let error =
            AdmittedBundle::validate(value.clone()).expect_err("incompatible response refused");
        assert!(error.contains("disagrees with provider"), "{error}");
        let BundleManifest::V0 {
            artifacts,
            connections,
            ..
        } = &mut value;
        let RuntimeRecord::V0 { inputs, .. } = &mut artifacts[1].runtime;
        inputs[0].signature = Some(signature);
        connections[0].consumer.endpoint = "start".into();
        AdmittedBundle::validate(value)
            .expect_err("method identity cannot substitute for the outgoing field");
    }

    #[test]
    fn unknown_connection_endpoints_are_refused_before_launch() {
        for endpoint in ["bad.name", "no_such_input"] {
            let mut value = manifest();
            let BundleManifest::V0 { connections, .. } = &mut value;
            connections.push(BundleConnection {
                consumer: EndpointReference {
                    instance: "brain".to_owned(),
                    endpoint: endpoint.to_owned(),
                },
                sources: vec![EndpointReference {
                    instance: "brain".to_owned(),
                    endpoint: "nonexistent_output".to_owned(),
                }],
            });
            let error = AdmittedBundle::validate(value)
                .expect_err("an unknown endpoint pair must be refused");
            assert!(
                error.contains("names no connectable input")
                    || error.contains("must be 1-64 lowercase ASCII"),
                "unexpected refusal for {endpoint}: {error}"
            );
        }
        // A launchable consumer with a known input is still refused when the
        // source names no served port of its producer.
        let mut value = manifest();
        let BundleManifest::V0 {
            artifacts,
            instances,
            connections,
            ..
        } = &mut value;
        artifacts.push({
            let mut record = artifact(&"bb".repeat(32));
            record.runtime = motion_runtime_record();
            record
        });
        instances.push(BundleInstance {
            id: "motion".to_owned(),
            role: InstanceRole::Service,
            artifact: "bb".repeat(32),
            config: InstanceConfig::absent(),
        });
        connections.push(BundleConnection {
            consumer: EndpointReference::parse("motion.measurements").expect("consumer"),
            sources: vec![EndpointReference::parse("brain.nonexistent").expect("source")],
        });
        let error = AdmittedBundle::validate(value).expect_err("an unknown source port is refused");
        assert!(
            error.contains("names no served port"),
            "unexpected refusal: {error}"
        );
    }

    #[test]
    fn duplicate_consumers_are_refused_in_the_common_build() {
        let (mut value, _) = native_common_fixture();
        let BundleManifest::V0 { connections, .. } = &mut value;
        connections.push(connections[0].clone());
        let error = AdmittedBundle::validate(value).expect_err("duplicate authored input");
        assert!(error.contains("imu.actuator is duplicated"), "{error}");
    }

    #[test]
    fn explicit_null_configuration_is_refused_while_decoding() {
        let json = serde_json::to_string(&manifest()).expect("serializes");
        let nulled = json.replace(
            "{\"id\":\"brain\",\"role\":\"brain\",\"artifact\":\"aa",
            "{\"id\":\"brain\",\"role\":\"brain\",\"config\":null,\"artifact\":\"aa",
        );
        let error = serde_json::from_str::<BundleManifest>(&nulled)
            .expect_err("an explicit null configuration is refused");
        assert!(
            error.to_string().contains("other than null"),
            "unexpected refusal: {error}"
        );
        // The canonical absent configuration is the omitted field and still
        // admits.
        AdmittedBundle::validate(manifest()).expect("absent configuration admits");
    }

    #[test]
    fn configurations_validate_against_the_complete_compiled_schema() {
        for schema in [
            serde_json::json!({
                "type": "object",
                "properties": {"value": {"type": "number", "minimum": 10}},
                "required": ["value"],
                "additionalProperties": false,
            }),
            serde_json::json!({
                "type": "object",
                "properties": {"mode": {"enum": ["manual", "autonomous"]}},
                "required": ["mode"],
            }),
            serde_json::json!({
                "$defs": {"item": {"type": "string"}},
                "type": "object",
                "properties": {"names": {"type": "array", "items": {"$ref": "#/$defs/item"}}},
                "required": ["names"],
            }),
        ] {
            let mut value = manifest();
            let BundleManifest::V0 {
                artifacts,
                instances,
                ..
            } = &mut value;
            let RuntimeRecord::V0 { config_schema, .. } = &mut artifacts[0].runtime;
            *config_schema = schema.clone();

            // An absent configuration resolves to the effective value and is
            // validated: required fields cannot be satisfied by omission.
            instances[0].config = InstanceConfig::absent();
            let error = AdmittedBundle::validate(value.clone())
                .expect_err("an absent configuration still validates");
            assert!(
                error.contains("brain configuration is invalid")
                    || error.contains("requires a value"),
                "unexpected absent refusal: {error}"
            );
        }

        // Concrete invalid and valid values against the bounded schema.
        let mut value = manifest();
        let BundleManifest::V0 {
            artifacts,
            instances,
            ..
        } = &mut value;
        let RuntimeRecord::V0 { config_schema, .. } = &mut artifacts[0].runtime;
        *config_schema = serde_json::json!({
            "type": "object",
            "properties": {"value": {"type": "number", "minimum": 10}},
            "required": ["value"],
            "additionalProperties": false,
        });
        // Below the minimum bound.
        instances[0].config =
            InstanceConfig::present(serde_json::json!({"value": 1})).expect("present");
        let error = AdmittedBundle::validate(value.clone())
            .expect_err("a value below the minimum is refused");
        assert!(
            error.contains("brain configuration is invalid"),
            "unexpected refusal: {error}"
        );
        // An undeclared field.
        let BundleManifest::V0 { instances, .. } = &mut value;
        instances[0].config =
            InstanceConfig::present(serde_json::json!({"value": 11, "extra": 1})).expect("present");
        let error = AdmittedBundle::validate(value.clone())
            .expect_err("an undeclared configuration field is refused");
        assert!(
            error.contains("Additional properties"),
            "unexpected refusal: {error}"
        );
        // The declared configuration admits.
        let BundleManifest::V0 { instances, .. } = &mut value;
        instances[0].config =
            InstanceConfig::present(serde_json::json!({"value": 11})).expect("present");
        AdmittedBundle::validate(value).expect("the declared configuration admits");
    }

    #[test]
    fn boolean_false_schemas_admit_no_configuration() {
        let mut value = manifest();
        let BundleManifest::V0 {
            artifacts,
            instances,
            ..
        } = &mut value;
        let RuntimeRecord::V0 { config_schema, .. } = &mut artifacts[0].runtime;
        *config_schema = serde_json::Value::Bool(false);
        instances[0].config =
            InstanceConfig::present(serde_json::json!({"any": true})).expect("present");
        let error = AdmittedBundle::validate(value.clone())
            .expect_err("a false schema refuses every configuration");
        assert!(
            error.contains("brain configuration is invalid"),
            "unexpected refusal: {error}"
        );
        // Absence is refused as well: no effective value can satisfy it.
        let BundleManifest::V0 { instances, .. } = &mut value;
        instances[0].config = InstanceConfig::absent();
        let error =
            AdmittedBundle::validate(value).expect_err("a false schema refuses absence too");
        assert!(
            error.contains("requires a value"),
            "unexpected refusal: {error}"
        );
    }

    #[test]
    fn absent_unit_configurations_admit_against_null_schemas() {
        // The base fixture's unit-configured brain stays legitimate.
        AdmittedBundle::validate(manifest()).expect("a null schema with absence admits");
        // An object schema without required fields also accepts absence.
        let mut value = manifest();
        let BundleManifest::V0 { artifacts, .. } = &mut value;
        let RuntimeRecord::V0 { config_schema, .. } = &mut artifacts[0].runtime;
        *config_schema = serde_json::json!({"type": "object"});
        AdmittedBundle::validate(value).expect("an optional object schema accepts absence");
    }

    #[test]
    fn mismatched_method_shapes_are_refused_before_child_spawn() {
        // An observation consumer fed by a call-shaped source is refused.
        let mut value = manifest();
        let BundleManifest::V0 {
            artifacts,
            instances,
            connections,
            ..
        } = &mut value;
        artifacts.push({
            let mut record = artifact(&"bb".repeat(32));
            record.runtime = motion_runtime_record();
            record
        });
        instances.push(BundleInstance {
            id: "motion".to_owned(),
            role: InstanceRole::Service,
            artifact: "bb".repeat(32),
            config: InstanceConfig::absent(),
        });
        let brain = artifacts.first_mut().expect("brain artifact");
        brain.runtime = brain_leased_source_runtime();
        let RuntimeRecord::V0 { outputs, .. } = &mut brain.runtime;
        if let Some(signature) = outputs[0].signature.as_mut() {
            signature.shape = MethodShape::Call;
            signature.request = "fixture.Actuators".to_owned();
            signature.response = "google.protobuf.Empty".to_owned();
            signature.lease_valid_for_ms = None;
        }
        connections.push(BundleConnection {
            consumer: EndpointReference::parse("motion.measurements").expect("consumer"),
            sources: vec![EndpointReference::parse("brain.actuators").expect("source")],
        });
        let error = AdmittedBundle::validate(value)
            .expect_err("an observation input fed by a call-shaped source is refused");
        assert!(
            error.contains("non-observation method"),
            "unexpected refusal: {error}"
        );
    }

    #[test]
    fn leased_setpoint_routes_validate_lease_and_payload() {
        // A brain-owned leased source with a positive lease and matching
        // payload admits a leased consumer; removing the lease from the
        // same source refuses the route.
        for lease in [Some(100u64), None] {
            let mut value = manifest();
            let BundleManifest::V0 {
                artifacts,
                instances,
                connections,
                ..
            } = &mut value;
            artifacts.push({
                let mut record = artifact(&"bb".repeat(32));
                record.runtime = motion_runtime_record();
                record
            });
            instances.push(BundleInstance {
                id: "motion".to_owned(),
                role: InstanceRole::Service,
                artifact: "bb".repeat(32),
                config: InstanceConfig::absent(),
            });
            let brain = artifacts.first_mut().expect("brain artifact");
            brain.runtime = brain_leased_source_runtime();
            let RuntimeRecord::V0 { outputs, .. } = &mut brain.runtime;
            if let Some(signature) = outputs[0].signature.as_mut() {
                signature.lease_valid_for_ms = lease;
            }
            connections.push(BundleConnection {
                consumer: EndpointReference::parse("motion.actuator").expect("consumer"),
                sources: vec![EndpointReference::parse("brain.actuators").expect("source")],
            });
            match lease {
                Some(_) => {
                    AdmittedBundle::validate(value).expect("a valid leased route admits");
                }
                None => {
                    let error = AdmittedBundle::validate(value)
                        .expect_err("an unleased source cannot feed a leased consumer");
                    assert!(
                        error.contains("without a positive lease"),
                        "unexpected refusal: {error}"
                    );
                }
            }
        }
    }

    #[test]
    fn bundle_manifest_round_trips() {
        let value = manifest();
        let json = serde_json::to_string(&value).expect("serializes");
        let decoded: BundleManifest = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(decoded, value);
    }

    #[test]
    fn unsupported_format_is_refused() {
        let json = serde_json::to_string(&manifest()).expect("serializes");
        let stale = json.replace("phoxal/bundle/v0", "phoxal/bundle/vX");
        let error = serde_json::from_str::<BundleManifest>(&stale).expect_err("refused");
        assert!(error.to_string().contains("phoxal/bundle/v0"));
    }

    #[test]
    fn admitted_view_requires_one_brain_and_known_artifacts() {
        AdmittedBundle::validate(manifest()).expect("minimal bundle admits");
        let mut unknown = manifest();
        let BundleManifest::V0 { instances, .. } = &mut unknown;
        instances[0].artifact = "bb".repeat(32);
        let error = AdmittedBundle::validate(unknown).expect_err("unknown artifact refuses");
        assert!(error.contains("unknown artifact"));
        let mut duplicate = manifest();
        let BundleManifest::V0 { instances, .. } = &mut duplicate;
        instances.push(BundleInstance {
            id: "second".to_owned(),
            role: InstanceRole::Brain,
            artifact: "aa".repeat(32),
            config: InstanceConfig::absent(),
        });
        let error = AdmittedBundle::validate(duplicate).expect_err("second brain refuses");
        assert!(
            error.contains("must use the identity `brain`"),
            "unexpected refusal: {error}"
        );
        let mut brainless = manifest();
        let BundleManifest::V0 { instances, .. } = &mut brainless;
        instances.clear();
        let error = AdmittedBundle::validate(brainless).expect_err("missing brain refuses");
        assert!(error.contains("exactly one brain"));
    }

    #[test]
    fn endpoint_references_parse_and_display() {
        let reference = EndpointReference::parse("brain.manual").expect("parses");
        assert_eq!(reference.instance, "brain");
        assert_eq!(reference.endpoint, "manual");
        assert_eq!(reference.to_string(), "brain.manual");
        assert!(EndpointReference::parse("brain").is_err());
        assert!(EndpointReference::parse("bra in.manual").is_err());
        assert!(EndpointReference::parse("brain.Manual").is_err());
    }

    fn native_common_fixture() -> (BundleManifest, BundleSimulation) {
        let simulation = BundleSimulation {
            protocol: "phoxal.simulation.v1".to_owned(),
            mode: "controlled".to_owned(),
            model_identity: "model-digest".to_owned(),
            quantum_ns: 10_000_000,
            providers: vec![BundleSimulationProvider {
                rate_microhertz: 100_000_000,
                service_fqn: "fixture.Sensor".to_owned(),
                method: "Sample".to_owned(),
                service_instance: "imu".to_owned(),
                port: "sample".to_owned(),
                shape: MethodShape::Observation,
                retained_latest: false,
                lease_valid_for_ms: None,
                input_fqn: "google.protobuf.Empty".to_owned(),
                payload_fqn: "fixture.Imu".to_owned(),
                max_message_bytes: 1_024,
                max_buffered_items: 16,
            }],
            actuation_bindings: vec![BundleActuationBinding {
                service_instance: "motion".to_owned(),
                port: "actuators".to_owned(),
                payload_fqn: "phoxal.component.actuator.v1.ActuatorCommand".to_owned(),
                actuator_ids: vec!["imu.motor".to_owned()],
            }],
        };
        let mut value = manifest();
        let BundleManifest::V0 {
            artifacts,
            instances,
            connections,
            components,
            ..
        } = &mut value;
        artifacts.push({
            let mut record = artifact(&"bb".repeat(32));
            record.runtime = motion_runtime_record();
            let RuntimeRecord::V0 {
                inputs, outputs, ..
            } = &mut record.runtime;
            inputs[1].request_fqn = Some("phoxal.component.actuator.v1.ActuatorCommand".into());
            outputs[0]
                .signature
                .as_mut()
                .expect("actuator signature")
                .response = "phoxal.component.actuator.v1.ActuatorCommand".into();
            record
        });
        instances.push(BundleInstance {
            id: "motion".to_owned(),
            role: InstanceRole::Service,
            artifact: "bb".repeat(32),
            config: InstanceConfig::absent(),
        });
        components.push(BundleComponent {
            instance: "imu".to_owned(),
            driver: true,
            package: "fixture-imu".to_owned(),
            source: "local".to_owned(),
            mount_site: "imu_mount".to_owned(),
            definition: ComponentDocument::V0 {
                model: super::super::document::ComponentModel {
                    file: std::path::PathBuf::from("model.xml"),
                    root_body: "root".to_owned(),
                },
                capabilities: std::collections::BTreeMap::from([("motor".into(), serde_json::from_value(serde_json::json!({"kind": "motor", "target": {"kind": "actuator", "id": "motor"}})).expect("motor capability"))]),
                assets: Vec::new(),
            },
        });
        let mut driver = artifact(&"cc".repeat(32));
        let mut driver_runtime = motion_runtime_record();
        let RuntimeRecord::V0 {
            inputs, outputs, ..
        } = &mut driver_runtime;
        inputs.retain(|input| input.name == "actuator");
        inputs[0].request_fqn = Some("phoxal.component.actuator.v1.ActuatorCommand".into());
        let mut sample = outputs[0].clone();
        sample.name = "sample".into();
        sample.port = Some("sample".into());
        sample.max_bytes = Some(1024);
        sample.max_items = Some(16);
        sample.signature = Some(MethodSignature {
            endpoint: "sample".into(),
            service: "fixture.Sensor".into(),
            method: "Sample".into(),
            shape: MethodShape::Observation,
            request: "google.protobuf.Empty".into(),
            response: "fixture.Imu".into(),
            retained_latest: false,
            lease_valid_for_ms: None,
        });
        *outputs = vec![sample];
        driver.runtime = driver_runtime;
        artifacts.push(driver);
        instances.push(BundleInstance {
            id: "imu".into(),
            role: InstanceRole::Driver,
            artifact: "cc".repeat(32),
            config: InstanceConfig::absent(),
        });
        connections.push(BundleConnection {
            consumer: EndpointReference::parse("imu.actuator").expect("consumer"),
            sources: vec![EndpointReference::parse("motion.actuators").expect("source")],
        });
        connections.push(BundleConnection {
            consumer: EndpointReference::parse("motion.measurements").expect("consumer"),
            sources: vec![EndpointReference::parse("imu.sample").expect("source")],
        });
        (value, simulation)
    }

    #[test]
    fn native_context_preserves_the_complete_authored_graph_and_validates_coverage() {
        use crate::artifact::simulation_context::SimulationContext;
        let (manifest, simulation) = native_common_fixture();
        let bytes = serde_json::to_vec(&manifest).expect("manifest");
        let original = AdmittedBundle::validate(manifest).expect("common build admits");
        assert_eq!(original.execution_connections.len(), 2);
        assert_eq!(original.instances["imu"].role, InstanceRole::Driver);
        let context = SimulationContext::new(&bytes, simulation.clone());
        let mut admitted = original.clone();
        context
            .clone()
            .admit(&bytes, &mut admitted)
            .expect("complete native implementation");
        assert_eq!(
            admitted.execution_connections,
            original.execution_connections
        );
        assert_eq!(admitted.instances, original.instances);
        assert!(
            context
                .admit(b"different manifest", &mut original.clone())
                .is_err()
        );
        let mut incomplete = simulation.clone();
        incomplete.providers.clear();
        assert!(
            SimulationContext::new(&bytes, incomplete)
                .admit(&bytes, &mut original.clone())
                .is_err()
        );
        let mut wrong_contract = simulation;
        wrong_contract.providers[0].max_message_bytes += 1;
        let mut refused = original.clone();
        assert!(
            SimulationContext::new(&bytes, wrong_contract)
                .admit(&bytes, &mut refused)
                .is_err()
        );
        assert!(refused.simulation.is_none());
        assert_eq!(
            refused.execution_connections,
            original.execution_connections
        );
    }
    #[test]
    fn native_actuator_membership_follows_disconnected_and_disjoint_authored_edges() {
        use crate::artifact::simulation_context::{SimulationContext, actuator_routes};
        let (mut disconnected, simulation) = native_common_fixture();
        let BundleManifest::V0 { connections, .. } = &mut disconnected;
        connections.retain(|connection| connection.consumer.instance != "imu");
        let bytes = serde_json::to_vec(&disconnected).expect("manifest");
        let mut admitted =
            AdmittedBundle::validate(disconnected).expect("unwired motor is a valid graph");
        assert!(actuator_routes(&admitted).expect("routes").is_empty());
        let error = SimulationContext::new(&bytes, simulation.clone())
            .admit(&bytes, &mut admitted)
            .expect_err("cannot reconnect a disconnected motor through native membership");
        assert!(error.contains("exactly follow authored"), "{error}");
        assert!(admitted.simulation.is_none());

        let (mut disjoint, mut native) = native_common_fixture();
        let BundleManifest::V0 {
            instances,
            components,
            connections,
            ..
        } = &mut disjoint;
        let mut second_motion = instances
            .iter()
            .find(|instance| instance.id == "motion")
            .expect("motion")
            .clone();
        second_motion.id = "motion2".into();
        instances.push(second_motion);
        let mut second_driver = instances
            .iter()
            .find(|instance| instance.id == "imu")
            .expect("driver")
            .clone();
        second_driver.id = "imu2".into();
        instances.push(second_driver);
        let mut second_component = components[0].clone();
        second_component.instance = "imu2".into();
        components.push(second_component);
        connections.push(BundleConnection {
            consumer: EndpointReference::parse("imu2.actuator").expect("consumer"),
            sources: vec![EndpointReference::parse("motion2.actuators").expect("source")],
        });
        let mut provider = native.providers[0].clone();
        provider.service_instance = "imu2".into();
        native.providers.push(provider);
        let mut binding = native.actuation_bindings[0].clone();
        binding.service_instance = "motion2".into();
        binding.actuator_ids = vec!["imu2.motor".into()];
        native.actuation_bindings.push(binding);
        let bytes = serde_json::to_vec(&disjoint).expect("manifest");
        let mut admitted = AdmittedBundle::validate(disjoint).expect("disjoint authored graph");
        let routes = actuator_routes(&admitted).expect("routes");
        assert_eq!(
            routes[&EndpointReference::parse("motion.actuators").expect("source")],
            std::collections::BTreeSet::from(["imu.motor".to_owned()])
        );
        assert_eq!(
            routes[&EndpointReference::parse("motion2.actuators").expect("source")],
            std::collections::BTreeSet::from(["imu2.motor".to_owned()])
        );
        SimulationContext::new(&bytes, native.clone())
            .admit(&bytes, &mut admitted)
            .expect("two distinct producers with disjoint motors are supported");
        native.actuation_bindings[0]
            .actuator_ids
            .push("imu2.motor".into());
        assert!(
            SimulationContext::new(&bytes, native)
                .admit(&bytes, &mut admitted)
                .is_err(),
            "a competing invented route cannot be added by a context"
        );
    }
}
