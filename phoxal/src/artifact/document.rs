//! Resolved robot and component document DTOs.
//!
//! Owns the inert `RobotDocument` and `ComponentDocument` records plus
//! their declared DTO closure (capabilities, native targets, services,
//! connections, port references). Reading authored YAML files,
//! filesystem walks, source-graph validation, and Cargo resolution
//! remain in `cargo-phoxal`'s tool layer.
#![deny(unsafe_code)]
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
/// A strict resolved robot composition with concrete source selections.
///
/// The developer tool merges authored files and resolves named references first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "schema", deny_unknown_fields)]
pub enum RobotDocument {
    /// The first authored robot-document generation.
    #[serde(rename = "phoxal/robot/v0")]
    V0 {
        /// Robot identity, physical model, and component instances.
        robot: RobotSection,
        /// The supervisor application selection.
        supervisor: SupervisorSelection,
    },
}
impl RobotDocument {
    /// Groups ordered producer endpoints by consuming endpoint for admission.
    /// Duplicate edges remain present so validators can reject them.
    #[must_use]
    pub fn connection_sources(&self) -> BTreeMap<String, Vec<String>> {
        let Self::V0 { robot, .. } = self;
        let RobotSection {
            services, brain, ..
        } = robot;
        let mut grouped = BTreeMap::new();
        for (instance, bindings) in services
            .iter()
            .map(|(id, service)| (id.as_str(), &service.bindings))
            .chain(robot.components.iter().filter_map(|(id, component)| {
                component
                    .driver
                    .as_ref()
                    .map(|driver| (id.as_str(), &driver.bindings))
            }))
            .chain(brain.iter().map(|brain| ("brain", &brain.bindings)))
        {
            for (endpoint, sources) in bindings {
                grouped.insert(format!("{instance}.{endpoint}"), sources.clone());
            }
        }
        grouped
    }
    /// Returns the authored local path of a consuming endpoint.
    #[must_use]
    pub fn binding_path(&self, consumer: &str) -> String {
        let (instance, endpoint) = consumer.split_once('.').unwrap_or((consumer, ""));
        let Self::V0 { robot, .. } = self;
        let services = &robot.services;
        let prefix = if instance == "brain" {
            "robot.brain".to_owned()
        } else if services.contains_key(instance) {
            format!("robot.services.{instance}")
        } else {
            format!("robot.components.{instance}.driver")
        };
        format!("{prefix}.bindings.{endpoint}")
    }
    /// Replaces one consuming binding while lowering compiled conversion routes.
    pub fn set_binding(&mut self, consumer: &str, sources: Vec<String>) -> bool {
        let Some((instance, endpoint)) = consumer.split_once('.') else {
            return false;
        };
        let Self::V0 { robot, .. } = self;
        let RobotSection {
            services, brain, ..
        } = robot;
        let bindings = if instance == "brain" {
            &mut brain.get_or_insert_with(BrainSelection::default).bindings
        } else if let Some(service) = services.get_mut(instance) {
            &mut service.bindings
        } else if let Some(driver) = robot
            .components
            .get_mut(instance)
            .and_then(|component| component.driver.as_mut())
        {
            &mut driver.bindings
        } else {
            return false;
        };
        bindings.insert(endpoint.to_owned(), sources);
        true
    }
    /// Returns every instance identity available to a connection source.
    #[must_use]
    pub fn instance_ids(&self) -> BTreeSet<String> {
        let Self::V0 { robot, .. } = self;
        let services = &robot.services;
        let mut ids = services.keys().cloned().collect::<BTreeSet<_>>();
        ids.extend(robot.components.keys().cloned());
        ids.insert("brain".to_owned());
        ids
    }
}
/// Whether a value is valid for an authored instance, service, or target id.
///
/// Hyphens are allowed because published Cargo package keys commonly use them.
/// Dots are reserved for endpoint references and are therefore not accepted in
/// identities themselves.
#[must_use]
pub fn is_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
        })
}
/// An optional explicit root-package binary selection.
///
/// The brain's endpoint contract is authored in its Rust Runtime.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrainSelection {
    /// Binary target name when the root package has multiple eligible binaries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
    /// Brain-owned runtime configuration.
    #[serde(
        default,
        deserialize_with = "deserialize_optional_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub config: Option<serde_json::Value>,
    /// Local consuming endpoints and their provider source lists.
    #[serde(default, deserialize_with = "deserialize_bindings")]
    pub bindings: BTreeMap<String, Vec<String>>,
}
/// Robot identity, physical composition and behavioral runtimes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RobotSection {
    /// Stable robot identity.
    pub id: String,
    /// Native robot model path, relative to the robot root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<PathBuf>,
    /// Optional explicit binary selection for a multi-binary root package.
    #[serde(
        default,
        deserialize_with = "deserialize_present",
        skip_serializing_if = "Option::is_none"
    )]
    pub brain: Option<BrainSelection>,
    /// Explicit behavioral service instances.
    #[serde(default)]
    pub services: BTreeMap<String, ServiceSelection>,
    /// Mounted component instances.
    #[serde(default)]
    pub components: BTreeMap<String, ComponentInstance>,
}
/// One mounted component and its physical connection information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentInstance {
    /// Source of this mounted component.
    pub source: Source,
    /// Persistent native site in the parent robot model receiving the component root.
    pub mount_site: String,
    /// Explicit runtime selection; absence means a passive component.
    #[serde(
        default,
        deserialize_with = "deserialize_present",
        skip_serializing_if = "Option::is_none"
    )]
    pub driver: Option<DriverSelection>,
}
/// One component driver's runtime selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriverSelection {
    /// Optional executable target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
    /// Driver-owned runtime configuration.
    #[serde(
        default,
        deserialize_with = "deserialize_optional_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub config: Option<serde_json::Value>,
    /// Local consuming endpoints and provider sources.
    #[serde(default, deserialize_with = "deserialize_bindings")]
    pub bindings: BTreeMap<String, Vec<String>>,
}
fn deserialize_bindings<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, Vec<String>>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Sources {
        One(String),
        Many(Vec<String>),
    }
    let values = BTreeMap::<String, Sources>::deserialize(deserializer)?;
    Ok(values
        .into_iter()
        .map(|(key, sources)| {
            (
                key,
                match sources {
                    Sources::One(value) => vec![value],
                    Sources::Many(values) => values,
                },
            )
        })
        .collect())
}
/// A component-owned native model and semantic capability definition.
///
/// Capabilities derive standard endpoints; a driver owns any additional
/// endpoints in its Rust Runtime contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "schema", deny_unknown_fields)]
pub enum ComponentDocument {
    /// The first component-definition generation consumed by native model preparation.
    #[serde(rename = "phoxal/component/v0")]
    V0 {
        /// Native model entry and attachment root.
        model: ComponentModel,
        /// Public semantic capabilities keyed by component-local identity.
        capabilities: BTreeMap<String, CapabilityDeclaration>,
        /// Explicit additional package assets retained by publication tooling.
        #[serde(default)]
        assets: Vec<PathBuf>,
    },
}
/// The native model entry selected by a component definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentModel {
    /// MJCF entry path relative to the component package root.
    pub file: PathBuf,
    /// Exactly one component-local body attached to the parent mount site.
    pub root_body: String,
}
/// A stable native target category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeTargetKind {
    /// A compiled scalar native actuator.
    Actuator,
    /// A compiled native joint.
    Joint,
    /// A persistent native site.
    Site,
    /// A compiled native camera.
    Camera,
}
/// One component-local native object selected by a semantic capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeTarget {
    /// Required native object category.
    pub kind: NativeTargetKind,
    /// Component-local native object name.
    pub id: String,
}
/// A semantic capability plus the minimum native binding needed by a provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityDeclaration {
    /// Semantic capability kind owned by the component contract.
    pub kind: String,
    /// Persistent native target selected by this capability.
    pub target: NativeTarget,
    /// Joint transmitted by a motor actuator, when this is a motor.
    #[serde(default)]
    pub joint: Option<String>,
    /// Component-local native signal names required to encode the public payload.
    #[serde(default)]
    pub signals: BTreeMap<String, String>,
    /// Capability-specific semantic values retained without creating a second physical model.
    #[serde(flatten)]
    pub semantics: BTreeMap<String, serde_json::Value>,
}
/// One explicit behavioral service instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceSelection {
    /// Source of this service package.
    pub source: Source,
    /// Binary target when the package exposes more than one executable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
    /// Service-owned configuration object.
    #[serde(
        default,
        deserialize_with = "deserialize_optional_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub config: Option<serde_json::Value>,
    /// Local consuming endpoints and provider sources.
    #[serde(default, deserialize_with = "deserialize_bindings")]
    pub bindings: BTreeMap<String, Vec<String>>,
}
/// The robot's supervisor application selection.
///
/// The supervisor shares the participants' source-selection type and
/// acquisition facilities while remaining an application, never a service
/// instance or connection participant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupervisorSelection {
    /// Source of the supervisor application package.
    pub source: Source,
    /// Binary target when the package exposes more than one executable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
}
/// One authored participant source. Each variant carries its own selection identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "SourceWire", into = "SourceWire")]
pub enum Source {
    /// Mutable local development source relative to the robot root.
    Path(String),
    /// Immutable Git revision selecting one package.
    Git(GitSourceSelection),
}
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum SourceWire {
    Path(PathSourceWire),
    Git(GitSourceWire),
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PathSourceWire {
    path: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GitSourceWire {
    git: GitSourceSelection,
}
impl From<SourceWire> for Source {
    fn from(source: SourceWire) -> Self {
        match source {
            SourceWire::Path(source) => Self::Path(source.path),
            SourceWire::Git(source) => Self::Git(source.git),
        }
    }
}
impl From<Source> for SourceWire {
    fn from(source: Source) -> Self {
        match source {
            Source::Path(path) => Self::Path(PathSourceWire { path }),
            Source::Git(git) => Self::Git(GitSourceWire { git }),
        }
    }
}
/// An immutable Git package selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitSourceSelection {
    /// Cargo package name within the selected checkout.
    pub name: String,
    /// Repository URL.
    pub url: String,
    /// Immutable commit revision.
    pub rev: String,
    /// Package path below the repository root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}
/// A parsed `instance.port` endpoint reference.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PortReference {
    /// Instance identity.
    pub instance: String,
    /// Public port or local input name.
    pub port: String,
}
/// Why an authored endpoint reference could not be parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PortReferenceError {
    /// The reference did not contain an instance separator.
    #[error("missing instance separator")]
    MissingSeparator,
    /// One side of the separator was empty.
    #[error("instance and port must not be empty")]
    EmptyPart,
    /// The port contained a second separator.
    #[error("endpoint references may contain only one separator")]
    NestedSeparator,
    /// An instance or port name used unsupported characters.
    #[error("instance and port must be valid project identifiers")]
    InvalidIdentifier,
}
impl PortReference {
    /// Parses an endpoint reference containing exactly one instance separator.
    pub fn parse(value: &str) -> Result<Self, PortReferenceError> {
        let (instance, port) = value
            .split_once('.')
            .ok_or(PortReferenceError::MissingSeparator)?;
        if instance.is_empty() || port.is_empty() {
            return Err(PortReferenceError::EmptyPart);
        }
        if port.contains('.') {
            return Err(PortReferenceError::NestedSeparator);
        }
        if !is_identifier(instance) || !is_identifier(port) {
            return Err(PortReferenceError::InvalidIdentifier);
        }
        Ok(Self {
            instance: instance.to_owned(),
            port: port.to_owned(),
        })
    }
}
fn deserialize_present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}
fn deserialize_optional_value<'de, D>(
    deserializer: D,
) -> Result<Option<serde_json::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(serde_json::Value::deserialize(deserializer)?))
}
#[cfg(test)]
mod tests {
    //! Round-trip and parse tests for the document DTO family.
    use super::*;
    #[test]
    fn directed_connections_preserve_fan_in_and_duplicates_for_admission() {
        let value = serde_json::json!({
            "schema": "phoxal/robot/v0", "robot": {"id": "robot", "brain": {"bindings": {"events": ["first.events", "second.events", "first.events"]}}},
            "supervisor": {"source": {"path": "supervisor"}},


        });
        let document: RobotDocument = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(
            document.connection_sources()["brain.events"],
            ["first.events", "second.events", "first.events"]
        );
        assert_eq!(
            serde_json::to_value(document).unwrap()["robot"]["brain"],
            value["robot"]["brain"]
        );
    }

    #[test]
    fn robot_document_round_trips() {
        let yaml = r#"
schema: phoxal/robot/v0
robot:
  id: rover
  model: model.xml
  components:
    imu:
      source:
        path: ../imu
      mount_site: imu_mount
      driver:
        config:
          rate_hz: 100
  services:
    navigation:
      source:
        path: ../navigation
      config:
        gain: 1.5
      bindings:
        imu:
        - imu.sample
supervisor:
  source:
    path: ../supervisor
"#;
        let document: RobotDocument = serde_yaml::from_str(yaml).expect("parses");
        let json = serde_json::to_string(&document).expect("serializes");
        let decoded: RobotDocument = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(decoded, document);
    }
    #[test]
    fn instance_ids_collect_components_services_and_brain() {
        let yaml = r#"
schema: phoxal/robot/v0
robot:
  id: rover
  components:
    imu:
      source: { path: ../imu }
      mount_site: imu_mount
  services:
    navigation:
      source: { path: ../navigation }
supervisor:
  source: { path: ../supervisor }
"#;
        let document: RobotDocument = serde_yaml::from_str(yaml).expect("parses");
        let ids = document.instance_ids();
        assert!(ids.contains("brain"));
        assert!(ids.contains("navigation"));
        assert!(ids.contains("imu"));
    }
    #[test]
    fn participant_source_is_the_only_package_selection() {
        let git = "source:\n  git:\n    name: motion\n    url: https://example.test/motion.git\n    rev: 0123456789abcdef0123456789abcdef01234567\n";
        let selected: ServiceSelection = serde_yaml::from_str(git).expect("Git source parses");
        assert!(matches!(selected.source, Source::Git(_)));
        let path = "source: { path: ../motion }\n";
        let selected: ServiceSelection = serde_yaml::from_str(path).expect("path source parses");
        assert_eq!(selected.source, Source::Path("../motion".to_owned()));
    }
    #[test]
    fn supervisor_selection_is_required_and_shares_the_source_type() {
        let git = "source:\n  git:\n    name: phoxal-supervisor\n    url: https://example.test/supervisor.git\n    rev: 0123456789abcdef0123456789abcdef01234567\n";
        let selected: SupervisorSelection = serde_yaml::from_str(git).expect("git source parses");
        assert!(matches!(selected.source, Source::Git(_)));
        let path = "source: { path: ../supervisor }\n";
        let selected: SupervisorSelection = serde_yaml::from_str(path).expect("path source parses");
        assert_eq!(selected.source, Source::Path("../supervisor".to_owned()));
    }
    #[test]
    fn port_reference_round_trips() {
        let reference = PortReference {
            instance: "imu".to_owned(),
            port: "sample".to_owned(),
        };
        assert_eq!(
            PortReference::parse("imu.sample").expect("parses"),
            reference
        );
        assert!(matches!(
            PortReference::parse("imu").expect_err("no separator"),
            PortReferenceError::MissingSeparator
        ));
        assert!(matches!(
            PortReference::parse("imu.sample.extra").expect_err("nested"),
            PortReferenceError::NestedSeparator
        ));
    }
    #[test]
    fn is_identifier_rejects_unsupported_characters() {
        assert!(is_identifier("rover"));
        assert!(is_identifier("ddsm115"));
        assert!(is_identifier("zed_f9p"));
        assert!(!is_identifier(""));
        assert!(!is_identifier("Rover"));
        assert!(!is_identifier("rover.brain"));
        assert!(!is_identifier("rover brain"));
    }
    #[test]
    fn native_target_kind_serializes_as_snake_case() {
        for (kind, expected) in [
            (NativeTargetKind::Actuator, "\"actuator\""),
            (NativeTargetKind::Joint, "\"joint\""),
            (NativeTargetKind::Site, "\"site\""),
            (NativeTargetKind::Camera, "\"camera\""),
        ] {
            let json = serde_json::to_string(&kind).expect("serializes");
            assert_eq!(json, expected);
        }
    }
}
