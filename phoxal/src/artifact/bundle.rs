//! Compiled robot bundle records shared by the compiler, supervisor, and simulator.
//!
//! The manifest carries executable paths and optional native simulation assets.
//! The full compiled robot document is written beside it as `robot.yaml`.
//! Bundle assembly stays in `cargo-phoxal`.

#![deny(unsafe_code)]

use serde::{Deserialize, Serialize};

use super::{DescriptorSummary, MethodShape, RuntimeRecord};

/// The inspectable graph and artifact inventory for one compiled robot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "schema")]
pub enum BundleManifest {
    /// The first compiled project-bundle generation.
    #[serde(rename = "phoxal/bundle/v0")]
    V0 {
        /// Authored robot identity.
        robot_id: String,
        /// Root Cargo package selected as the brain owner.
        root_package: BundlePackage,
        /// Cargo target triple or the explicit host marker.
        target: String,
        /// Cargo profile used for artifact construction.
        profile: String,
        /// Root features selected for this build.
        features: Vec<String>,
        /// Every executable selected for this robot, in stable bundle order.
        executables: Vec<BundleExecutable>,
        /// Every mounted component, including passive components without a binary.
        components: Vec<BundleComponent>,
        /// Paths to component model sources retained for native simulation.
        #[serde(default)]
        component_sources: std::collections::BTreeMap<String, String>,
        /// Portable robot model resources retained for native simulation.
        #[serde(default)]
        model: Option<BundleModelAssets>,
        /// The immutable controlled-simulation contract, when this bundle was
        /// assembled for an independent simulator run.
        #[serde(default)]
        simulation: Option<BundleSimulation>,
        /// Explicit receiver-side observation projections compiled against
        /// both declarations' descriptors.  Each record carries the two
        /// descriptor closures the receiving runtime needs to convert the
        /// foreign payload; observation metadata is preserved untouched.
        #[serde(default)]
        projections: Vec<ConnectionProjection>,
    },
}

/// One compiled explicit observation projection between differently named
/// message contracts.
///
/// The project compiler validates the mapping against both declarations and
/// stores this self-contained form; the receiving runtime applies it at the
/// delivery admission boundary without rebuilding the consumer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionProjection {
    /// Consumer instance whose declared input receives the mapped payload.
    pub consumer_instance: String,
    /// Consumer's local input field/endpoint name.
    pub consumer_field: String,
    /// Producing instance named by the connection's `from`.
    pub source_instance: String,
    /// Producer's local output endpoint name.
    pub source_port: String,
    /// Fully-qualified foreign source message.
    pub source_message: String,
    /// Fully-qualified declared destination message.
    pub destination_message: String,
    /// Destination field path to source field path (top-level scalars).
    pub map: std::collections::BTreeMap<String, String>,
    /// Serialized `FileDescriptorSet` closure defining the source message.
    pub source_descriptors: Vec<u8>,
    /// Serialized `FileDescriptorSet` closure defining the destination message.
    pub destination_descriptors: Vec<u8>,
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

/// A package identity retained in the compiled graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundlePackage {
    /// Cargo package identity, including source and version.
    pub id: String,
    /// Human-readable Cargo package name.
    pub name: String,
    /// Cargo source identity, when the package is not a local workspace member.
    pub source: String,
}

/// One executable copied into bin/ in the compiled bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleExecutable {
    /// brain, service, or component driver.
    pub role: String,
    /// Runtime instance identity used as the bundle filename.
    pub instance: String,
    /// Cargo package identity that produced this executable.
    pub package_id: String,
    /// Cargo package name.
    pub package: String,
    /// Cargo target name.
    pub target: String,
    /// Bundle-relative executable path.
    pub path: String,
    /// Runtime contract and retained descriptor inventory when present.
    pub artifact: Option<BundleArtifact>,
}

/// Model files carried beside a compiled robot for native simulation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleModelAssets {
    /// Bundle-relative model entry.
    pub entry: String,
    /// Bundle-relative resource paths.
    pub resources: Vec<String>,
}

/// Manifest-safe native artifact contract inventory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleArtifact {
    /// Runtime timing, configuration, and binding records.
    pub runtime: RuntimeRecord,
    /// Original descriptor closure digests and file names.
    pub descriptors: Vec<DescriptorSummary>,
}

impl From<super::ArtifactSummary> for BundleArtifact {
    fn from(summary: super::ArtifactSummary) -> Self {
        Self {
            runtime: summary.runtime,
            descriptors: summary.descriptors,
        }
    }
}

/// One mounted component retained in the compiled graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleComponent {
    /// Authored component instance identity.
    pub instance: String,
    /// Exact Cargo dependency key selected by robot.yaml.
    pub dependency_key: String,
    /// Cargo package identity.
    pub package_id: String,
    /// Cargo package name.
    pub package: String,
    /// Stable Cargo source identity.
    pub source: String,
    /// Persistent site in the parent robot model receiving this instance.
    pub mount_site: String,
    /// Component-owned semantic capabilities and native model-local bindings.
    pub definition: super::document::ComponentDocument,
}

#[cfg(test)]
mod tests {
    //! Round-trip checks for the bundle record family.

    use super::*;

    fn sample_manifest() -> BundleManifest {
        BundleManifest::V0 {
            robot_id: "rover".to_owned(),
            root_package: BundlePackage {
                id: "brain@0.1.0".to_owned(),
                name: "brain".to_owned(),
                source: "local".to_owned(),
            },
            target: "x86_64-unknown-linux-gnu".to_owned(),
            profile: "release".to_owned(),
            features: Vec::new(),
            executables: Vec::new(),
            components: Vec::new(),
            component_sources: std::collections::BTreeMap::new(),
            model: None,
            simulation: None,
            projections: Vec::new(),
        }
    }

    #[test]
    fn bundle_manifest_round_trips() {
        let manifest = sample_manifest();
        let json = serde_json::to_string(&manifest).expect("serializes");
        let decoded: BundleManifest = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(decoded, manifest);
    }
}
