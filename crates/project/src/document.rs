use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{ValidationError, ValidationErrors};

/// The source-language tag accepted by this first project compiler.
pub const ROBOT_SCHEMA: &str = "phoxal/robot/v0";

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
    /// Parses one complete document and returns all structural errors together.
    pub fn parse(text: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(text)
    }

    /// Validates source-language and composition rules.
    ///
    /// The method intentionally validates only what belongs to the project
    /// boundary. Service configuration schemas and port payload types belong
    /// to the selected package owners and are checked after Cargo resolution.
    pub fn validate(&self) -> Result<(), Vec<ValidationError>> {
        let mut errors = Vec::new();
        self.validate_schema(&mut errors);
        self.validate_robot(&mut errors);
        self.validate_brain(&mut errors);
        self.validate_services(&mut errors);
        self.validate_connections(&mut errors);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Returns every instance identity available to a connection source.
    #[must_use]
    pub fn instance_ids(&self) -> BTreeSet<String> {
        let mut ids = self.services.keys().cloned().collect::<BTreeSet<_>>();
        ids.extend(self.robot.components.keys().cloned());
        ids.insert("brain".to_owned());
        ids
    }

    fn validate_schema(&self, errors: &mut Vec<ValidationError>) {
        if let Some(schema) = &self.schema
            && schema != ROBOT_SCHEMA
        {
            errors.push(ValidationError::UnsupportedSchema {
                value: schema.clone(),
            });
        }
    }

    fn validate_robot(&self, errors: &mut Vec<ValidationError>) {
        if self.robot.id.trim().is_empty() {
            errors.push(ValidationError::EmptyRobotId);
        } else if !is_identifier(&self.robot.id) {
            errors.push(ValidationError::InvalidIdentifier {
                field: "robot.id".to_owned(),
                value: self.robot.id.clone(),
            });
        }

        for (instance, component) in &self.robot.components {
            validate_identifier(&format!("robot.components.{instance}"), instance, errors);
            if self.services.contains_key(instance) {
                errors.push(ValidationError::InstanceCollision {
                    instance: instance.clone(),
                });
            }
            if instance == "brain" {
                errors.push(ValidationError::ReservedBrainId {
                    field: "robot.components".to_owned(),
                });
            }
            if component.component.trim().is_empty() {
                errors.push(ValidationError::EmptySourceKey {
                    field: format!("robot.components.{instance}.component"),
                });
            } else if !is_identifier(&component.component) {
                errors.push(ValidationError::InvalidIdentifier {
                    field: format!("robot.components.{instance}.component"),
                    value: component.component.clone(),
                });
            }
            if component.mount_link.trim().is_empty() {
                errors.push(ValidationError::EmptySourceKey {
                    field: format!("robot.components.{instance}.mount_link"),
                });
            }
            if let Some(config) = &component.config {
                validate_config_value(
                    config,
                    &format!("robot.components.{instance}.config"),
                    errors,
                );
            }
            if let Some(driver) = &component.driver {
                let Some(driver) = driver.as_object() else {
                    errors.push(ValidationError::InvalidDriver {
                        component: instance.clone(),
                        message: "must be a mapping when present".to_owned(),
                    });
                    continue;
                };
                if let Some(config) = driver.get("config") {
                    validate_config_value(
                        config,
                        &format!("robot.components.{instance}.driver.config"),
                        errors,
                    );
                }
            }
        }
    }

    fn validate_brain(&self, errors: &mut Vec<ValidationError>) {
        if let Some(brain) = &self.brain
            && let Some(binary) = &brain.binary
            && (binary.trim().is_empty() || !is_identifier(binary))
        {
            errors.push(ValidationError::InvalidIdentifier {
                field: "brain.binary".to_owned(),
                value: binary.clone(),
            });
        }
    }

    fn validate_services(&self, errors: &mut Vec<ValidationError>) {
        for (service, selection) in &self.services {
            validate_identifier(&format!("services.{service}"), service, errors);
            if service == "brain" {
                errors.push(ValidationError::ReservedBrainId {
                    field: "services".to_owned(),
                });
            }
            if let Some(implementation) = &selection.implementation {
                if implementation.trim().is_empty() {
                    errors.push(ValidationError::InvalidService {
                        service: service.clone(),
                        message: "implementation must not be empty".to_owned(),
                    });
                } else if !is_identifier(implementation) {
                    errors.push(ValidationError::InvalidService {
                        service: service.clone(),
                        message: format!(
                            "implementation '{implementation}' is not a valid Cargo dependency key"
                        ),
                    });
                }
            }
            if let Some(binary) = &selection.binary
                && (binary.trim().is_empty() || !is_identifier(binary))
            {
                errors.push(ValidationError::InvalidService {
                    service: service.clone(),
                    message: format!("binary '{binary}' is not a valid target name"),
                });
            }
            if let Some(config) = &selection.config {
                validate_config_value(config, &format!("services.{service}.config"), errors);
            }
        }
    }

    fn validate_connections(&self, errors: &mut Vec<ValidationError>) {
        let known = self.instance_ids();
        for (consumer_text, sources) in &self.connections {
            let consumer = match PortReference::parse(consumer_text) {
                Ok(reference) => reference,
                Err(_) => {
                    errors.push(ValidationError::InvalidPortReference {
                        field: "connections".to_owned(),
                        value: consumer_text.clone(),
                    });
                    continue;
                }
            };
            if !known.contains(&consumer.instance) {
                errors.push(ValidationError::UnknownConnectionInstance {
                    field: format!("connections.{consumer_text}"),
                    instance: consumer.instance.clone(),
                });
            } else if consumer.instance != "brain"
                && !self.services.contains_key(&consumer.instance)
            {
                errors.push(ValidationError::InvalidConnectionConsumer {
                    field: format!("connections.{consumer_text}"),
                    instance: consumer.instance.clone(),
                });
            }

            let source_values = sources.as_slice();
            if source_values.is_empty() {
                errors.push(ValidationError::EmptyConnectionSources {
                    field: format!("connections.{consumer_text}"),
                });
                continue;
            }
            let mut seen = BTreeSet::new();
            for source_text in source_values {
                let source = match PortReference::parse(source_text) {
                    Ok(reference) => reference,
                    Err(_) => {
                        errors.push(ValidationError::InvalidPortReference {
                            field: format!("connections.{consumer_text}"),
                            value: source_text.clone(),
                        });
                        continue;
                    }
                };
                if !known.contains(&source.instance) {
                    errors.push(ValidationError::UnknownConnectionInstance {
                        field: format!("connections.{consumer_text}"),
                        instance: source.instance,
                    });
                }
                if !seen.insert(source_text) {
                    errors.push(ValidationError::DuplicateConnectionSource {
                        field: format!("connections.{consumer_text}"),
                        producer: source_text.clone(),
                    });
                }
            }
        }
    }
}

fn validate_identifier(field: &str, value: &str, errors: &mut Vec<ValidationError>) {
    if !is_identifier(value) {
        errors.push(ValidationError::InvalidIdentifier {
            field: field.to_owned(),
            value: value.to_owned(),
        });
    }
}

fn validate_config_value(
    value: &serde_json::Value,
    field: &str,
    errors: &mut Vec<ValidationError>,
) {
    match value {
        serde_json::Value::Null => errors.push(ValidationError::NullConfiguration {
            field: field.to_owned(),
        }),
        serde_json::Value::Object(_) => {}
        _ => errors.push(ValidationError::NonMappingConfiguration {
            field: field.to_owned(),
        }),
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
    /// Existing physical/kinematic authoring values remain opaque to this
    /// foundation until their owning domain validators are migrated.
    #[serde(default)]
    pub kinematic: Option<serde_json::Value>,
    /// Existing physical motion limits remain opaque to this foundation.
    #[serde(default)]
    pub motion_limits: Option<serde_json::Value>,
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
    /// Native model mount/site name.
    pub mount_link: String,
    /// Component-owned driver connection and configuration.
    #[serde(default)]
    pub driver: Option<serde_json::Value>,
    /// Component-owned configuration, if the component declares one.
    #[serde(default, deserialize_with = "deserialize_optional_value")]
    pub config: Option<serde_json::Value>,
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

fn deserialize_optional_value<'de, D>(
    deserializer: D,
) -> Result<Option<serde_json::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(serde_json::Value::deserialize(deserializer)?))
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

/// Parses and validates a document with its authored path attached to errors.
pub(crate) fn parse_and_validate(
    text: &str,
    path: &std::path::Path,
) -> Result<RobotDocument, crate::error::Error> {
    let document =
        RobotDocument::parse(text).map_err(|source| crate::error::Error::ParseRobot {
            path: path.to_owned(),
            source,
        })?;
    document
        .validate()
        .map_err(|errors| crate::error::Error::InvalidRobot {
            path: path.to_owned(),
            errors: ValidationErrors(errors),
        })?;
    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_valid(text: &str) -> RobotDocument {
        let document = RobotDocument::parse(text).expect("document parses");
        document.validate().expect("document validates");
        document
    }

    #[test]
    fn explicit_services_and_connections_are_preserved() {
        let document = parse_valid(
            r#"
schema: phoxal/robot/v0
robot:
  id: rover
  model: model.xml
  components:
    imu:
      component: imu-package
      mount_link: imu_mount
brain:
  binary: rover-brain
services:
  navigation: {}
  dashboard:
    implementation: dashboard-package
    binary: dashboard
    config:
      refresh_hz: 5
connections:
  dashboard.status: navigation.state
  navigation.imu: imu.sample
"#,
        );

        assert!(document.services.contains_key("navigation"));
        assert_eq!(
            document.connections["dashboard.status"].as_slice(),
            &["navigation.state"]
        );
        assert_eq!(
            document.connections["navigation.imu"].as_slice(),
            &["imu.sample"]
        );
    }

    #[test]
    fn omitted_service_is_not_invented_and_multiple_sources_keep_order() {
        let document = parse_valid(
            r#"
robot:
  id: rover
  components: {}
services:
  navigation: {}
connections:
  navigation.imu:
    - brain.status
    - navigation.state
"#,
        );
        assert_eq!(document.services.len(), 1);
        assert_eq!(
            document.connections["navigation.imu"].as_slice(),
            &["brain.status", "navigation.state"]
        );
    }

    #[test]
    fn null_and_scalar_configuration_are_distinct_errors() {
        let document = RobotDocument::parse(
            r#"
robot:
  id: rover
  components: {}
services:
  null_config:
    config: null
  scalar_config:
    config: 3
"#,
        )
        .expect("document parses");
        let errors = document.validate().expect_err("invalid config values");
        assert!(errors.iter().any(|error| matches!(
            error,
            ValidationError::NullConfiguration { field } if field == "services.null_config.config"
        )));
        assert!(errors.iter().any(|error| matches!(
            error,
            ValidationError::NonMappingConfiguration { field }
                if field == "services.scalar_config.config"
        )));
    }

    #[test]
    fn roles_and_old_structure_are_rejected_as_unknown_fields() {
        let error = RobotDocument::parse(
            r#"
robot:
  id: rover
  structure: structure.urdf
  components:
    imu:
      component: imu
      mount_link: imu_mount
      roles:
        imu: [navigation]
"#,
        )
        .expect_err("retired inferred composition fields must not parse");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn duplicate_yaml_keys_are_rejected_by_the_boundary_parser() {
        let error =
            RobotDocument::parse("robot:\n  id: rover\n  id: duplicate\n  components: {}\n")
                .expect_err("duplicate keys must not be accepted");
        assert!(error.to_string().contains("duplicate field"));
    }

    #[test]
    fn invalid_connections_name_the_exact_boundary() {
        let document = RobotDocument::parse(
            r#"
robot:
  id: rover
  components: {}
services:
  navigation: {}
connections:
  missing.status: navigation.state
  navigation.empty: []
  navigation.bad: navigation.state.extra
"#,
        )
        .expect("document parses");
        let errors = document.validate().expect_err("invalid graph references");
        assert!(errors.iter().any(|error| matches!(
            error,
            ValidationError::UnknownConnectionInstance { instance, .. }
                if instance == "missing"
        )));
        assert!(errors.iter().any(|error| matches!(
            error,
            ValidationError::EmptyConnectionSources { field }
                if field == "connections.navigation.empty"
        )));
        assert!(errors.iter().any(|error| matches!(
            error,
            ValidationError::InvalidPortReference { value, .. }
                if value == "navigation.state.extra"
        )));
    }
}
