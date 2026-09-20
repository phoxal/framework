//! Tool-side authored-file validation.
//!
//! The inert record family (`RobotDocument`, `ComponentDocument`,
//! `BrainSelection`, capability declarations, etc.) lives in
//! `phoxal::artifact::document`. This module re-exports those
//! types and adds the project-side validation logic that the inert
//! framework module intentionally does not own.
//!
//! Authored YAML parsing is owned here; tool callers use [`parse_and_validate`]
//! to combine parsing and validation.

#![deny(unsafe_code)]

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;

use crate::project::error::{ValidationError, ValidationErrors};

// Re-exports from the framework artifact module. That module is the source of
// truth for every inert record; this tool module keeps the
// tool's validation logic and reads YAML.
pub use phoxal::artifact::document::{
    BrainSelection, ComponentDocument, ComponentInstance, ConnectionSources, PortReference,
    RobotDocument, ServiceSelection,
};

/// Re-export of the source-language tag for callers that still import
/// `document::ROBOT_SCHEMA`.
pub use phoxal::artifact::document::{COMPONENT_SCHEMA, ROBOT_SCHEMA};

/// Parses and validates a `robot.yaml` document with its authored path
/// attached to errors. The YAML parse and the validation belong
/// together at this boundary because a malformed file must surface both
/// structural and semantic errors in one round-trip.
pub(crate) fn parse_and_validate(
    text: &str,
    path: &Path,
) -> Result<RobotDocument, crate::project::error::Error> {
    let document: RobotDocument =
        serde_yaml::from_str(text).map_err(|source| crate::project::error::Error::ParseRobot {
            path: path.to_owned(),
            source,
        })?;
    document
        .validate()
        .map_err(|errors| crate::project::error::Error::InvalidRobot {
            path: path.to_owned(),
            errors: ValidationErrors(errors),
        })?;
    Ok(document)
}

/// Validates source-language and composition rules for a `RobotDocument`.
///
/// Validation remains tool-owned and is implemented as a trait over the inert
/// framework record so callers can read `document.validate()?`.
pub trait ValidateDocument {
    /// Run all source-language and composition checks and collect any
    /// validation errors into a single `Vec`.
    fn validate(&self) -> Result<(), Vec<ValidationError>>;
}

impl ValidateDocument for RobotDocument {
    fn validate(&self) -> Result<(), Vec<ValidationError>> {
        let mut errors = Vec::new();
        validate_schema(self, &mut errors);
        validate_robot(self, &mut errors);
        validate_brain(self, &mut errors);
        validate_services(self, &mut errors);
        validate_connections(self, &mut errors);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// Validates a single `ComponentDocument` against the project-side
/// rules the framework record module does not own.
pub trait ValidateComponentDocument {
    /// Run all component-side checks and return any errors as a single
    /// joined string for the supervisor and publication tooling.
    fn validate(&self) -> Result<(), String>;
}

impl ValidateComponentDocument for ComponentDocument {
    fn validate(&self) -> Result<(), String> {
        let mut errors: Vec<String> = Vec::new();
        validate_component_model(self, &mut errors);
        validate_component_capabilities(self, &mut errors);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}

fn validate_schema(document: &RobotDocument, errors: &mut Vec<ValidationError>) {
    if let Some(schema) = &document.schema
        && schema != ROBOT_SCHEMA
    {
        errors.push(ValidationError::UnsupportedSchema {
            value: schema.clone(),
        });
    }
}

fn validate_robot(document: &RobotDocument, errors: &mut Vec<ValidationError>) {
    if document.robot.id.trim().is_empty() {
        errors.push(ValidationError::EmptyRobotId);
    } else if !is_identifier(&document.robot.id) {
        errors.push(ValidationError::InvalidIdentifier {
            field: "robot.id".to_owned(),
            value: document.robot.id.clone(),
        });
    }

    for (instance, component) in &document.robot.components {
        push_identifier_error(&format!("robot.components.{instance}"), instance, errors);
        if instance.contains("__") {
            errors.push(ValidationError::ReservedNamespaceSeparator {
                field: format!("robot.components.{instance}"),
                value: instance.clone(),
            });
        }
        if document.services.contains_key(instance) {
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
        if component.mount_site.trim().is_empty() {
            errors.push(ValidationError::EmptySourceKey {
                field: format!("robot.components.{instance}.mount_site"),
            });
        }
        if let Some(config) = &component.config {
            push_config_errors(
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
                push_config_errors(
                    config,
                    &format!("robot.components.{instance}.driver.config"),
                    errors,
                );
            }
        }
    }
}

fn validate_brain(document: &RobotDocument, errors: &mut Vec<ValidationError>) {
    if let Some(brain) = &document.brain
        && let Some(binary) = &brain.binary
        && (binary.trim().is_empty() || !is_identifier(binary))
    {
        errors.push(ValidationError::InvalidIdentifier {
            field: "brain.binary".to_owned(),
            value: binary.clone(),
        });
    }
}

fn validate_services(document: &RobotDocument, errors: &mut Vec<ValidationError>) {
    for (service, selection) in &document.services {
        push_identifier_error(&format!("services.{service}"), service, errors);
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
            push_config_errors(config, &format!("services.{service}.config"), errors);
        }
    }
}

fn validate_connections(document: &RobotDocument, errors: &mut Vec<ValidationError>) {
    let known = document.instance_ids();
    for (consumer_text, sources) in &document.connections {
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
            && !document.services.contains_key(&consumer.instance)
            && document
                .robot
                .components
                .get(&consumer.instance)
                .is_none_or(|component| component.driver.is_none())
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

fn validate_component_model(document: &ComponentDocument, errors: &mut Vec<String>) {
    if document.schema != COMPONENT_SCHEMA {
        errors.push(format!(
            "unsupported component schema '{}'; expected {COMPONENT_SCHEMA}",
            document.schema
        ));
    }
    if document.model.file.as_os_str().is_empty()
        || document.model.file.to_string_lossy().contains("..")
    {
        errors.push(format!(
            "component model path must be a relative, parent-free POSIX path: got '{}'",
            document.model.file.display()
        ));
    }
    if !is_identifier(&document.model.root_body) {
        errors.push(format!(
            "component root_body '{}' must be a valid native body name",
            document.model.root_body
        ));
    }
}

fn validate_component_capabilities(document: &ComponentDocument, errors: &mut Vec<String>) {
    let mut seen_names = BTreeSet::new();
    for (name, capability) in &document.capabilities {
        if !is_identifier(name) {
            errors.push(format!(
                "capability name '{name}' must be a valid identifier"
            ));
        }
        if !seen_names.insert(name) {
            errors.push(format!("capability name '{name}' is duplicated"));
        }
        if !is_identifier(&capability.target.id) {
            errors.push(format!(
                "capability '{name}' binds native id '{}' which is not a valid native identifier",
                capability.target.id
            ));
        }
    }
}

fn push_identifier_error(field: &str, value: &str, errors: &mut Vec<ValidationError>) {
    if !is_identifier(value) {
        errors.push(ValidationError::InvalidIdentifier {
            field: field.to_owned(),
            value: value.to_owned(),
        });
    }
}

fn push_config_errors(value: &Value, field: &str, errors: &mut Vec<ValidationError>) {
    match value {
        Value::Null => errors.push(ValidationError::NullConfiguration {
            field: field.to_owned(),
        }),
        Value::Object(_) => {}
        _ => errors.push(ValidationError::NonMappingConfiguration {
            field: field.to_owned(),
        }),
    }
}

pub(crate) fn is_identifier(value: &str) -> bool {
    phoxal::artifact::document::is_identifier(value)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn maintained_example(relative: &str) -> (std::path::PathBuf, String) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("examples/runtime-rewrite")
            .join(relative);
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
        (path, text)
    }

    #[test]
    fn parse_and_validate_accepts_a_minimal_document() {
        let yaml = r#"
schema: phoxal/robot/v0
robot:
  id: rover
"#;
        let doc = parse_and_validate(yaml, Path::new("robot.yaml")).expect("valid");
        assert_eq!(doc.robot.id, "rover");
    }

    #[test]
    fn parse_and_validate_collects_identifier_errors() {
        let yaml = r#"
schema: phoxal/robot/v0
robot:
  id: "Rover"
"#;
        let err =
            parse_and_validate(yaml, Path::new("robot.yaml")).expect_err("invalid identifier");
        assert!(matches!(
            err,
            crate::project::error::Error::InvalidRobot { .. }
        ));
    }

    #[test]
    fn maintained_example_documents_pass_tool_owned_validation() {
        for relative in [
            "components/bench-motor/component.yaml",
            "components/bench-imu/component.yaml",
        ] {
            let (path, text) = maintained_example(relative);
            let document: ComponentDocument = serde_yaml::from_str(&text)
                .unwrap_or_else(|error| panic!("cannot parse {}: {error}", path.display()));
            document
                .validate()
                .unwrap_or_else(|error| panic!("{} is invalid: {error}", path.display()));
        }

        let (path, text) = maintained_example("robots/workspace-robot/robot.yaml");
        parse_and_validate(&text, &path).expect("maintained robot document is valid");
    }
}
