//! Bundle manifest, package records, and provenance closure.
//!
//! Owns every serialized record a compiled bundle exchanges with the
//! supervisor, the simulator, and the SDK. This includes the manifest,
//! the provenance closure, the controlled-simulation section, and the
//! source-file digest.
//!
//! Pure algorithms (`digest_source_files`, `digest_bytes`) live here
//! because they have no filesystem, network, or process dependencies.
//! Bundle assembly, staging, publication, Cargo execution, file
//! copying, and locks stay with the tool layer in `cargo-phoxal`.

#![deny(unsafe_code)]

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
        /// The complete source document used for this compilation.
        document: super::document::RobotDocument,
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
        /// The immutable controlled-simulation contract, when this bundle was
        /// assembled for an independent simulator run.
        #[serde(default)]
        simulation: Option<BundleSimulation>,
    },
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
    /// Exact executable byte count.
    pub bytes: u64,
    /// Lowercase SHA-256 digest of the executable bytes.
    pub sha256: String,
    /// Runtime contract and retained descriptor inventory when present.
    pub artifact: Option<BundleArtifact>,
}

/// Exact provenance for the supervisor executable carried by a bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleSupervisor {
    /// Fixed supervisor role label.
    pub role: String,
    /// Fixed bundle-local supervisor instance label.
    pub instance: String,
    /// Cargo package identity that produced the supervisor.
    pub package_id: String,
    /// Cargo package name.
    pub package: String,
    /// Stable Cargo source identity for the package.
    pub source: String,
    /// Exact Cargo package version.
    pub version: String,
    /// Cargo binary target name.
    pub target: String,
    /// Bundle-relative executable path.
    pub path: String,
    /// Number of bytes in the copied executable.
    pub bytes: u64,
    /// SHA-256 digest of the copied executable.
    pub sha256: String,
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

/// The source class of one package in the resolved Cargo closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BundleSourceKind {
    /// A package selected from the robot's local workspace or an external path.
    Local,
    /// A package selected from a pinned Git revision.
    Git,
    /// A package selected from a Cargo registry archive.
    Registry,
    /// A Cargo source not recognized by this version of the tooling.
    Other,
}

/// Immutable provenance for a pinned Git package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleGitSource {
    /// Repository URL without Cargo's source prefix or query string.
    pub repository: String,
    /// Full immutable commit selected by Cargo.
    pub revision: String,
    /// Package subdirectory within the checked-out revision.
    pub subdirectory: String,
}

/// One source file captured as a digest in bundle provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleSourceFile {
    /// Path relative to the source package root.
    pub path: String,
    /// Lowercase SHA-256 digest of the source bytes.
    pub sha256: String,
    /// Exact source byte count.
    pub bytes: u64,
}

/// One package and its exact source closure in the resolved Cargo graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleSource {
    /// Stable identity that does not contain a developer-local absolute path.
    pub identity: String,
    /// Public package identity used by the bundle, without local path leakage.
    pub package_id: String,
    /// Cargo package name.
    pub package: String,
    /// Cargo package version.
    pub version: String,
    /// Stable Cargo source representation, or `local` for path packages.
    pub source: String,
    /// Source classification.
    pub kind: BundleSourceKind,
    /// Digest over the sorted source file path and byte closure.
    pub digest: String,
    /// Every regular file in the package source closure.
    pub files: Vec<BundleSourceFile>,
    /// Registry archive checksum when this is a registry package.
    pub registry_checksum: Option<String>,
    /// Git provenance when this is a Git package.
    pub git: Option<BundleGitSource>,
    /// Authored package path relative to the robot source root when this is a
    /// local package.
    #[serde(default)]
    pub authored_path: Option<String>,
    /// Files derived in an isolated carrier rather than authored by the
    /// package, such as the inert Cargo target for a passive component.
    #[serde(default)]
    pub derived_files: Vec<String>,
}

/// Toolchain and invocation inputs used to create one compiled bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleToolchain {
    /// Exact Cargo version output.
    pub cargo: String,
    /// Exact verbose rustc version output.
    pub rustc: String,
    /// Target triple or the explicit host marker.
    pub target: String,
    /// Cargo profile selected for the build.
    pub profile: String,
    /// Root features selected for the build.
    pub features: Vec<String>,
    /// Whether the root requested all features.
    pub all_features: bool,
    /// Whether the root disabled default features.
    pub no_default_features: bool,
    /// Cargo lock policy used for the build.
    pub lock: String,
    /// Whether Cargo was forced offline.
    pub offline: bool,
    /// Cargo's caller-provided arguments, retained in invocation order.
    pub cargo_args: Vec<String>,
    /// Cargo message format requested by the caller.
    pub message_format: Option<String>,
    /// Environment inputs that can affect Cargo, Rust, or native builds.
    pub environment: Vec<BundleEnvironment>,
    /// Native tool identities observed in the build environment.
    pub native_tools: Vec<BundleNativeTool>,
    /// Workspace configuration files carried by the source closure.
    pub config_files: Vec<BundleFile>,
    /// Exact Cargo argument vectors used for selected executable builds.
    pub invocations: Vec<BundleCargoInvocation>,
}

/// One environment value retained as build provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleEnvironment {
    /// Environment variable name.
    pub name: String,
    /// Environment variable value, or an explicit redaction marker.
    pub value: String,
}

/// One native compiler or linker input retained as build provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleNativeTool {
    /// Environment variable selecting the tool.
    pub name: String,
    /// Configured tool path or command.
    pub command: String,
    /// Version output when the configured command could be queried.
    pub version: Option<String>,
}

/// One Cargo invocation used to produce a selected executable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleCargoInvocation {
    /// Cargo arguments in process order, with local roots made relocatable.
    pub arguments: Vec<String>,
}

/// The relocatable local source closure carried by a compiled bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleSourceClosure {
    /// Bundle-relative source directory.
    pub path: String,
    /// Digest over every retained source file and its relative path.
    pub digest: String,
    /// Exact file inventory relative to the closure directory.
    pub files: Vec<BundleSourceFile>,
}

/// Source and tool inputs used to construct a bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "schema")]
pub enum BundleProvenance {
    /// The first bundle-provenance generation.
    #[serde(rename = "phoxal/provenance/v0")]
    V0 {
        /// SHA-256 of the authored robot document.
        robot_manifest_sha256: String,
        /// SHA-256 of the root Cargo manifest.
        cargo_manifest_sha256: String,
        /// SHA-256 of the owning workspace-root Cargo manifest.
        cargo_workspace_manifest_sha256: String,
        /// SHA-256 of the workspace-owned Cargo lock, when present.
        cargo_lock_sha256: Option<String>,
        /// Exact workspace-owned Cargo.lock input, when present.
        cargo_lock: Option<BundleFile>,
        /// Deduplicated package source closure used by the selected Cargo graph.
        sources: Vec<BundleSource>,
        /// Digest over the ordered source records.
        source_closure_sha256: String,
        /// Relocatable local source and lock closure carried by the bundle.
        source_tree: BundleSourceClosure,
        /// Compiler and invocation inputs used for the bundle.
        toolchain: BundleToolchain,
        /// The exact supervisor executable copied into the bundle and launched by
        /// local execution.
        supervisor: BundleSupervisor,
        /// Model path and digest when the authored model exists.
        model: Option<BundleFile>,
        /// The validated model/resource closure copied into the bundle's assets.
        model_closure: Option<BundleModelClosure>,
    },
}

/// One authored input file and its digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleFile {
    /// Path relative to the robot root.
    pub path: String,
    /// Lowercase SHA-256 digest of the file bytes.
    pub sha256: String,
    /// Exact byte count.
    pub bytes: u64,
}

/// The portable closed model closure carried by a compiled bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleModelClosure {
    /// Bundle-relative entry passed to the native parser.
    pub entry: String,
    /// Digest of the normalized entry/resource closure.
    pub digest: String,
    /// Every resource copied below the bundle's assets directory.
    pub resources: Vec<BundleResource>,
}

/// One resource copied into a compiled bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleResource {
    /// Bundle-relative resource path.
    pub path: String,
    /// Lowercase SHA-256 digest of the resource bytes.
    pub sha256: String,
    /// Exact resource byte count.
    pub bytes: u64,
}

/// Compute the canonical digest of a source closure, independent of file order.
///
/// Each path, content digest, and little-endian byte count is length-prefixed.
/// Consumers must separately verify each file and reject duplicate paths.
#[must_use]
pub fn digest_source_files(files: &[BundleSourceFile]) -> String {
    let mut sorted = files.iter().collect::<Vec<_>>();
    sorted.sort_by(|left, right| left.path.cmp(&right.path));
    let mut hasher = Sha256::new();
    for file in sorted {
        update_digest_bytes(&mut hasher, file.path.as_bytes());
        update_digest_bytes(&mut hasher, file.sha256.as_bytes());
        update_digest_bytes(&mut hasher, &file.bytes.to_le_bytes());
    }
    format!("{:x}", hasher.finalize())
}

/// Lowercase hex SHA-256 of an arbitrary byte slice.
#[must_use]
pub fn digest_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn update_digest_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

#[cfg(test)]
mod tests {
    //! Round-trip and digest-vector tests for the bundle record family.

    use super::*;

    fn sample_manifest() -> BundleManifest {
        let yaml = r#"
schema: phoxal/robot/v0
robot:
  id: rover
  components: {}
"#;
        let document: super::super::document::RobotDocument =
            serde_yaml::from_str(yaml).expect("document parses");
        BundleManifest::V0 {
            robot_id: "rover".to_owned(),
            document,
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
            simulation: None,
        }
    }

    #[test]
    fn bundle_manifest_round_trips() {
        let manifest = sample_manifest();
        let json = serde_json::to_string(&manifest).expect("serializes");
        let decoded: BundleManifest = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(decoded, manifest);
    }

    #[test]
    fn bundle_source_kind_serializes_as_lowercase() {
        for (kind, expected) in [
            (BundleSourceKind::Local, "\"local\""),
            (BundleSourceKind::Git, "\"git\""),
            (BundleSourceKind::Registry, "\"registry\""),
            (BundleSourceKind::Other, "\"other\""),
        ] {
            let json = serde_json::to_string(&kind).expect("serializes");
            assert_eq!(json, expected);
        }
    }

    #[test]
    fn digest_source_files_is_order_independent_and_fixed() {
        let mut files = vec![
            BundleSourceFile {
                path: "b".to_owned(),
                sha256: "22".to_owned(),
                bytes: 9,
            },
            BundleSourceFile {
                path: "a".to_owned(),
                sha256: "11".to_owned(),
                bytes: 3,
            },
        ];
        let digest = digest_source_files(&files);
        files.reverse();
        assert_eq!(digest, digest_source_files(&files));
        // Pinned digest vector guards against accidental algorithm
        // changes that would invalidate every recorded bundle.
        assert_eq!(
            digest,
            "6bba922dab7ab1946ffb4f591cd17c8ddd44a0195de23988aca7def7578b5156"
        );
        files[0].bytes += 1;
        assert_ne!(digest, digest_source_files(&files));
    }
}
