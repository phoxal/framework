//! Authored-file document DTOs.
//!
//! Owns the inert `RobotDocument` and `ComponentDocument` records plus
//! their declared DTO closure (capabilities, native targets, services,
//! connections, port references). Reading authored YAML files,
//! filesystem walks, source-graph validation, and Cargo resolution
//! remain in `phoxal-project`'s tool layer.

#![deny(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The source-language tag accepted by this first project compiler.
pub const ROBOT_SCHEMA: &str = "phoxal/robot/v0";

/// The component definition generation consumed by native model preparation.
pub const COMPONENT_SCHEMA: &str = "phoxal/component/v0";

/// A parsed and validated `robot.yaml` document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RobotDocument {
    /// The authored document generation.
    #[serde(default)]
    pub schema: Option<String>,
    /// Robot identity, physical model, and component instances.
    pub robot: RobotSection,
    /// Optional explicit binary selection for a multi-binary root package.
    #[serde(default)]
    pub brain: Option<BrainSelection>,
    /// Explicit behavioral service instances.
    #[serde(default)]
    pub services: BTreeMap<String, ServiceSelection>,
    /// Explicit local input to served-port connections.
    #[serde(default)]
    pub connections: BTreeMap<String, ConnectionSources>,
}

impl RobotDocument {
    /// Parses one complete document and returns any YAML structural error.
    ///
    /// Validation belongs to the project tool layer; this parser is the
    /// inert decode step.
    pub fn parse(text: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(text)
    }

    /// Returns every instance identity available to a connection source.
    #[must_use]
    pub fn instance_ids(&self) -> BTreeSet<String> {
        let mut ids = self.services.keys().cloned().collect::<BTreeSet<_>>();
        ids.extend(self.robot.components.keys().cloned());
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrainSelection {
    /// Binary target name when the root package has multiple eligible binaries.
    #[serde(default)]
    pub binary: Option<String>,
}

/// Robot-level model and component composition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RobotSection {
    /// Stable robot identity.
    pub id: String,
    /// Native robot model path, relative to the robot root.
    #[serde(default)]
    pub model: Option<PathBuf>,
    /// Mounted component instances.
    #[serde(default)]
    pub components: BTreeMap<String, ComponentInstance>,
}

/// One mounted component and its physical connection information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentInstance {
    /// Cargo dependency key selecting the component package.
    pub component: String,
    /// Persistent native site in the parent robot model receiving the component root.
    pub mount_site: String,
    /// Component-owned driver connection and configuration.
    #[serde(default)]
    pub driver: Option<serde_json::Value>,
    /// Component-owned configuration, if the component declares one.
    #[serde(default, deserialize_with = "deserialize_optional_value")]
    pub config: Option<serde_json::Value>,
}

/// A component-owned native model and semantic capability definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentDocument {
    /// Authored component document generation.
    pub schema: String,
    /// Native model entry and attachment root.
    pub model: ComponentModel,
    /// Public semantic capabilities keyed by component-local identity.
    pub capabilities: BTreeMap<String, CapabilityDeclaration>,
    /// Explicit additional package assets retained by publication tooling.
    #[serde(default)]
    pub assets: Vec<PathBuf>,
}

impl ComponentDocument {
    /// Parses one complete component definition.
    pub fn parse(text: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(text)
    }
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceSelection {
    /// Cargo dependency key selecting the implementation.
    #[serde(default)]
    pub implementation: Option<String>,
    /// Binary target when the package exposes more than one executable.
    #[serde(default)]
    pub binary: Option<String>,
    /// Service-owned configuration object.
    #[serde(default, deserialize_with = "deserialize_optional_value")]
    pub config: Option<serde_json::Value>,
}

/// One or more ordered producer endpoints for a local consuming input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConnectionSources {
    /// A single producer endpoint.
    One(String),
    /// An ordered set of producer endpoints.
    Many(Vec<String>),
}

impl ConnectionSources {
    /// Returns the authored producer list without changing its order.
    #[must_use]
    pub fn as_slice(&self) -> &[String] {
        match self {
            Self::One(value) => std::slice::from_ref(value),
            Self::Many(values) => values,
        }
    }
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
    fn robot_document_round_trips() {
        let yaml = r#"
schema: phoxal/robot/v0
robot:
  id: rover
  model: model.xml
  components:
    imu:
      component: imu-package
      mount_site: imu_mount
      config:
        rate_hz: 100
services:
  navigation:
    config:
      gain: 1.5
connections:
  navigation.imu: imu.sample
"#;
        let document = RobotDocument::parse(yaml).expect("parses");
        let json = serde_json::to_string(&document).expect("serializes");
        let decoded: RobotDocument = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(decoded, document);
    }

    #[test]
    fn instance_ids_collect_components_services_and_brain() {
        let yaml = r#"
robot:
  id: rover
  components:
    imu:
      component: imu-package
      mount_site: imu_mount
services:
  navigation: {}
"#;
        let document = RobotDocument::parse(yaml).expect("parses");
        let ids = document.instance_ids();
        assert!(ids.contains("brain"));
        assert!(ids.contains("navigation"));
        assert!(ids.contains("imu"));
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
