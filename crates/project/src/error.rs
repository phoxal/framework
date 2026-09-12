use std::path::PathBuf;

use crate::selection::TargetRole;

/// A failure while discovering a project from an invocation directory.
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    /// The invocation path could not be canonicalized.
    #[error("cannot resolve {path}: {source}")]
    Resolve {
        /// The path supplied by the caller.
        path: PathBuf,
        /// The filesystem failure.
        source: std::io::Error,
    },
    /// No authored robot document was found in the ancestor chain.
    #[error("no robot.yaml found at or above {start}")]
    MissingRobot { start: PathBuf },
    /// The discovered robot root does not carry its required Cargo package.
    #[error(
        "robot root {root} has no Cargo.toml; the root-local brain must be an ordinary Cargo package"
    )]
    MissingManifest {
        /// Directory containing robot.yaml.
        root: PathBuf,
    },
}

/// A malformed authored robot document.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    /// The document selected a source-language generation this tool does not know.
    #[error("schema '{value}' is not supported; expected phoxal/robot/v0")]
    UnsupportedSchema {
        /// Authored schema value.
        value: String,
    },
    /// The robot identity is empty or whitespace.
    #[error("robot.id must not be empty")]
    EmptyRobotId,
    /// An identity used for an instance or service is malformed.
    #[error(
        "{field} '{value}' is not a valid project identifier; use lowercase letters, digits, '-' or '_'"
    )]
    InvalidIdentifier {
        /// Authored field containing the identity.
        field: String,
        /// Invalid value.
        value: String,
    },
    /// An authored map tried to claim the reserved brain identity.
    #[error("{field} uses reserved instance id 'brain'; the root Cargo package owns the brain")]
    ReservedBrainId {
        /// Authored map containing the identity.
        field: String,
    },
    /// A service or component source key is empty.
    #[error("{field} must not be empty")]
    EmptySourceKey {
        /// Authored field containing the key.
        field: String,
    },
    /// A service configuration explicitly used null.
    #[error("{field} must be a mapping when present; omit config for an empty configuration")]
    NullConfiguration {
        /// Authored configuration field.
        field: String,
    },
    /// A service configuration had a non-object root.
    #[error("{field} must be a mapping when present")]
    NonMappingConfiguration {
        /// Authored configuration field.
        field: String,
    },
    /// A connection key or endpoint did not have exactly one instance separator.
    #[error("{field} '{value}' must use instance.port syntax")]
    InvalidPortReference {
        /// Authored connection field.
        field: String,
        /// Invalid endpoint text.
        value: String,
    },
    /// A connection list was present but empty.
    #[error("{field} must contain at least one producer")]
    EmptyConnectionSources {
        /// Authored connection field.
        field: String,
    },
    /// A connection listed the same producer more than once.
    #[error("{field} lists producer '{producer}' more than once")]
    DuplicateConnectionSource {
        /// Authored connection field.
        field: String,
        /// Repeated producer endpoint.
        producer: String,
    },
    /// A connection named an instance that is absent from the composition.
    #[error("{field} references unknown instance '{instance}'")]
    UnknownConnectionInstance {
        /// Authored connection field.
        field: String,
        /// Missing instance.
        instance: String,
    },
    /// A connection key used an instance that cannot consume graph inputs.
    #[error(
        "{field} uses '{instance}' as a consumer, but only services and brain may consume connections"
    )]
    InvalidConnectionConsumer {
        /// Authored connection field.
        field: String,
        /// Consumer instance.
        instance: String,
    },
    /// A service entry used an unsupported field.
    #[error("services.{service}: {message}")]
    InvalidService {
        /// Service instance id.
        service: String,
        /// Specific service error.
        message: String,
    },
}

/// A source-selection failure after Cargo has resolved the project graph.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    /// A composition key does not name a direct normal Cargo dependency.
    #[error(
        "{role} '{instance}' selects dependency key '{key}', but that key is not a normal direct dependency in Cargo.toml"
    )]
    DependencyNotDeclared {
        /// Graph role being resolved.
        role: TargetRole,
        /// Authored instance id.
        instance: String,
        /// Authored dependency key.
        key: String,
    },
    /// A direct dependency exists but is only a dev or build dependency.
    #[error(
        "{role} '{instance}' selects dependency key '{key}', but it is a {kind} dependency; execution sources must be normal dependencies"
    )]
    DependencyWrongKind {
        /// Graph role being resolved.
        role: TargetRole,
        /// Authored instance id.
        instance: String,
        /// Authored dependency key.
        key: String,
        /// Cargo dependency kind.
        kind: String,
    },
    /// Cargo's resolve graph did not contain the selected direct edge.
    #[error(
        "{role} '{instance}' dependency key '{key}' is declared but is not present in Cargo's resolved graph"
    )]
    DependencyUnresolved {
        /// Graph role being resolved.
        role: TargetRole,
        /// Authored instance id.
        instance: String,
        /// Authored dependency key.
        key: String,
    },
    /// The resolved package did not have the target role required by the document.
    #[error(
        "{role} '{instance}' dependency key '{key}' resolves to package '{package}', which has no {target_kind} target"
    )]
    MissingTarget {
        /// Graph role being resolved.
        role: TargetRole,
        /// Authored instance id.
        instance: String,
        /// Authored dependency key.
        key: String,
        /// Resolved package name.
        package: String,
        /// Target kind expected by the role.
        target_kind: String,
    },
    /// More than one executable target was available without an explicit binary selection.
    #[error(
        "{role} '{instance}' dependency key '{key}' has multiple binaries ({candidates}); set binary explicitly"
    )]
    AmbiguousBinary {
        /// Graph role being resolved.
        role: TargetRole,
        /// Authored instance id.
        instance: String,
        /// Authored dependency key.
        key: String,
        /// Candidate target names.
        candidates: String,
    },
    /// An explicitly selected binary target was not found.
    #[error("{role} '{instance}' dependency key '{key}' has no binary target named '{binary}'")]
    MissingBinary {
        /// Graph role being resolved.
        role: TargetRole,
        /// Authored instance id.
        instance: String,
        /// Authored dependency key.
        key: String,
        /// Requested target name.
        binary: String,
    },
    /// A selected target is gated by features that the package does not define.
    #[error("{role} '{instance}' binary '{binary}' requires undefined Cargo features: {features}")]
    UndefinedRequiredFeatures {
        /// Graph role being resolved.
        role: TargetRole,
        /// Authored instance id.
        instance: String,
        /// Selected target name.
        binary: String,
        /// Undefined feature names.
        features: String,
    },
    /// A root-local brain could not be identified from the root package.
    #[error(
        "the robot root package has no eligible brain binary; add one binary target or set brain.binary"
    )]
    MissingBrain,
    /// A requested brain binary is not an ordinary root-package binary.
    #[error("brain binary '{binary}' is not an eligible binary target of the root Cargo package")]
    InvalidBrainBinary {
        /// Requested target name.
        binary: String,
    },
    /// More than one root-package binary could serve as the brain.
    #[error(
        "the robot root package has multiple eligible brain binaries ({candidates}); set brain.binary"
    )]
    AmbiguousBrain {
        /// Candidate target names.
        candidates: String,
    },
    /// The selected brain binary requires features that are not enabled by the root graph.
    #[error("brain binary '{binary}' requires Cargo features that are not enabled: {features}")]
    BrainFeatures {
        /// Selected target name.
        binary: String,
        /// Required feature names.
        features: String,
    },
}

/// The top-level error returned by project discovery, preparation, and Cargo commands.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Project discovery failed.
    #[error("project discovery failed: {0}")]
    Discovery(#[from] DiscoveryError),
    /// Reading robot.yaml failed.
    #[error("cannot read robot.yaml at {path}: {source}")]
    ReadRobot {
        /// Authored manifest path.
        path: PathBuf,
        /// Filesystem failure.
        source: std::io::Error,
    },
    /// Parsing robot.yaml failed.
    #[error("cannot parse robot.yaml at {path}: {source}")]
    ParseRobot {
        /// Authored manifest path.
        path: PathBuf,
        /// YAML parser failure.
        source: serde_yaml::Error,
    },
    /// The parsed robot document violated one or more authoring rules.
    #[error("robot.yaml at {path} is invalid: {errors}")]
    InvalidRobot {
        /// Authored manifest path.
        path: PathBuf,
        /// All discovered validation failures.
        errors: ValidationErrors,
    },
    /// Reading Cargo.toml failed.
    #[error("cannot read Cargo.toml at {path}: {source}")]
    ReadManifest {
        /// Root package manifest path.
        path: PathBuf,
        /// Filesystem failure.
        source: std::io::Error,
    },
    /// Parsing Cargo.toml failed.
    #[error("cannot parse Cargo.toml at {path}: {source}")]
    ParseManifest {
        /// Root package manifest path.
        path: PathBuf,
        /// TOML parser failure.
        source: toml::de::Error,
    },
    /// The root Cargo.toml is a virtual manifest rather than the robot package.
    #[error(
        "Cargo.toml at {path} is a virtual manifest; the robot root must own an ordinary package for its brain"
    )]
    VirtualManifest {
        /// Root package manifest path.
        path: PathBuf,
    },
    /// Cargo metadata failed during preparation.
    #[error("cargo metadata failed for {manifest}: {source}")]
    CargoMetadata {
        /// Manifest passed to Cargo.
        manifest: PathBuf,
        /// Cargo metadata failure.
        source: cargo_metadata::Error,
    },
    /// A selected source could not be resolved.
    #[error("source selection failed: {0}")]
    Source(#[from] SourceError),
    /// Cargo command execution failed.
    #[error("cargo {operation} failed ({status}): {stderr}")]
    CargoCommand {
        /// Cargo subcommand.
        operation: String,
        /// Exit status rendered for diagnostics.
        status: String,
        /// Captured standard error.
        stderr: String,
    },
    /// A command could not be started.
    #[error("cannot start cargo {operation}: {source}")]
    CargoSpawn {
        /// Cargo subcommand.
        operation: String,
        /// Process-spawn failure.
        source: std::io::Error,
    },
    /// Command options are contradictory.
    #[error("invalid Cargo command options: {message}")]
    InvalidOptions {
        /// Option diagnostic.
        message: String,
    },
}

/// A stable display wrapper for all validation failures in one document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationErrors(pub Vec<ValidationError>);

impl std::fmt::Display for ValidationErrors {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, error) in self.0.iter().enumerate() {
            if index > 0 {
                formatter.write_str("; ")?;
            }
            error.fmt(formatter)?;
        }
        Ok(())
    }
}
