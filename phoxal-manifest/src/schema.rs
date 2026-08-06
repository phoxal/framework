//! Portable JSON Schema documents for authored manifest editors.
//!
//! These schemas describe the serde wire shape of the current authored v0
//! documents. They are an editor aid only; [`crate::source`] parsing and
//! validation remain authoritative for semantic and cross-file constraints.

use schemars::{SchemaGenerator, generate::SchemaSettings};

/// The authored document kind whose schema an editor needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentKind {
    /// A project-root `robot.yaml` document.
    Robot,
    /// A component-local `component.yaml` document.
    Component,
    /// A component-local `simulation.yaml` document.
    Simulation,
}

impl DocumentKind {
    /// Every authored document kind in stable generation order.
    pub const ALL: [Self; 3] = [Self::Robot, Self::Component, Self::Simulation];

    /// The stable file name for this document kind's generated editor schema.
    #[must_use]
    pub const fn file_name(self) -> &'static str {
        match self {
            Self::Robot => "robot.schema.json",
            Self::Component => "component.schema.json",
            Self::Simulation => "simulation.schema.json",
        }
    }

    const fn metadata(self) -> (&'static str, &'static str) {
        match self {
            Self::Robot => (
                "Phoxal robot/v0 manifest",
                "Editor schema for an authored Phoxal robot.yaml document.",
            ),
            Self::Component => (
                "Phoxal component/v0 manifest",
                "Editor schema for an authored Phoxal component.yaml document.",
            ),
            Self::Simulation => (
                "Phoxal simulation/v0 manifest",
                "Editor schema for an authored Phoxal simulation.yaml document.",
            ),
        }
    }
}

/// Generate a portable Draft 2020-12 JSON Schema for an authored document.
///
/// The returned ordinary JSON value can be serialized directly into an
/// editor's local schema cache without exposing `schemars` types in this
/// crate's public API.
#[must_use]
pub fn generate(kind: DocumentKind) -> serde_json::Value {
    let mut schema = match kind {
        DocumentKind::Robot => SchemaGenerator::new(SchemaSettings::draft2020_12())
            .into_root_schema_for::<crate::source::robot::Manifest>(),
        DocumentKind::Component => SchemaGenerator::new(SchemaSettings::draft2020_12())
            .into_root_schema_for::<crate::source::component::Manifest>(),
        DocumentKind::Simulation => SchemaGenerator::new(SchemaSettings::draft2020_12())
            .into_root_schema_for::<crate::source::simulation::Manifest>(),
    };
    let (title, description) = kind.metadata();
    schema.insert("title".into(), title.into());
    schema.insert("description".into(), description.into());
    schema.to_value()
}

#[cfg(test)]
mod tests {
    use super::{DocumentKind, generate};

    fn validator(kind: DocumentKind) -> jsonschema::Validator {
        let document_schema = generate(kind);
        assert_eq!(
            document_schema
                .get("$schema")
                .and_then(serde_json::Value::as_str),
            Some("https://json-schema.org/draft/2020-12/schema")
        );
        jsonschema::validator_for(&document_schema)
            .expect("generated schema should be a valid Draft 2020-12 schema")
    }

    fn yaml_value(text: &str) -> serde_json::Value {
        serde_yaml::from_str(text).expect("test document should be valid YAML")
    }

    fn hello_rover_fixture(kind: DocumentKind) -> serde_json::Value {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/hello-rover");
        let path = match kind {
            DocumentKind::Robot => root.join("robot.yaml"),
            DocumentKind::Component => root.join("components/wheel_drive/component.yaml"),
            DocumentKind::Simulation => root.join("components/wheel_drive/simulation.yaml"),
        };
        let document =
            std::fs::read_to_string(path).expect("hello-rover document should be readable");
        yaml_value(&document)
    }

    fn assert_valid(validator: &jsonschema::Validator, value: &serde_json::Value) {
        let errors = validator
            .iter_errors(value)
            .map(|error| error.to_string())
            .collect::<Vec<_>>();
        assert!(
            errors.is_empty(),
            "document should be structurally valid: {errors:?}"
        );
    }

    #[test]
    fn schemas_are_self_validating_and_name_their_documents() {
        assert_eq!(
            DocumentKind::ALL.len(),
            3,
            "update the stable schema-generation inventory for every document kind"
        );
        for kind in DocumentKind::ALL {
            let (title, file_name) = match kind {
                DocumentKind::Robot => ("Phoxal robot/v0 manifest", "robot.schema.json"),
                DocumentKind::Component => {
                    ("Phoxal component/v0 manifest", "component.schema.json")
                }
                DocumentKind::Simulation => {
                    ("Phoxal simulation/v0 manifest", "simulation.schema.json")
                }
            };
            let generated = generate(kind);
            assert_eq!(kind.file_name(), file_name);
            assert_eq!(
                generated.get("title").and_then(serde_json::Value::as_str),
                Some(title)
            );
            assert!(
                generated
                    .get("description")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|description| description.contains("Editor schema"))
            );
            let _ = validator(kind);
        }
    }

    #[test]
    fn schemas_validate_hello_rover_authored_documents() {
        for kind in DocumentKind::ALL {
            assert_valid(&validator(kind), &hello_rover_fixture(kind));
        }
    }

    #[test]
    fn schemas_reject_unknown_root_properties() {
        for kind in DocumentKind::ALL {
            let mut value = hello_rover_fixture(kind);
            value
                .as_object_mut()
                .expect("authored document is an object")
                .insert("not_a_real_key".into(), serde_json::json!({}));
            assert!(
                !validator(kind).is_valid(&value),
                "{kind:?} schema should reject an unknown root property"
            );
        }
    }

    #[test]
    fn the_generated_robot_schema_carries_no_behavior_definition_or_field() {
        // The generated schema is derived straight from the source DTO, so
        // this is the standing proof that deleting `BehaviorConfig` and
        // `Manifest::behavior` also removed them from every editor schema -
        // there is no committed schema file to rebaseline.
        let schema = generate(DocumentKind::Robot);
        let serialized = serde_json::to_string(&schema).expect("schema serializes");
        assert!(
            !serialized.to_lowercase().contains("behavior"),
            "the robot schema still mentions the retired behavior subsystem: {serialized}"
        );

        // A `behavior:` root property must also fail structurally, not merely
        // be undescribed.
        let mut document = hello_rover_fixture(DocumentKind::Robot);
        document
            .as_object_mut()
            .expect("authored document is an object")
            .insert(
                "behavior".into(),
                serde_json::json!({ "root": "system.root" }),
            );
        assert!(!validator(DocumentKind::Robot).is_valid(&document));
    }

    #[test]
    fn schema_generation_is_deterministic() {
        for kind in DocumentKind::ALL {
            let first = serde_json::to_string_pretty(&generate(kind))
                .expect("generated schema should serialize deterministically");
            let second = serde_json::to_string_pretty(&generate(kind))
                .expect("generated schema should serialize deterministically");
            assert_eq!(first, second);
        }
    }

    #[test]
    fn schemas_cover_tagged_shapes_and_custom_string_wire_types() {
        let robot = yaml_value(
            r#"
schema: robot/v0
robot:
  id: schema-bot
  namespace: dev
  kinematic:
    kind: ackermann
    steering_actuator: steering.motor
    drive_actuator: drive.motor
    steering_encoder: steering.encoder
    drive_encoder: drive.encoder
    wheel_base_m: 0.3
    track_m: 0.2
    max_steering_angle_rad: 0.5
  motion_limits: { max_linear_speed_mps: 1.0, max_angular_speed_radps: 1.0 }
  components:
    drive:
      component: wheel_drive
      mount_link: wheel
      driver:
        runtime_clock_ms: 10
        connection:
          type: gpio
          chip: gpiochip0
          pins: [{ line: 1, direction: output }]
      parameters:
        motor: { kind: motor, direction_sign: -1 }
"#,
        );
        assert_valid(&validator(DocumentKind::Robot), &robot);

        let component = yaml_value(
            r#"
schema: component/v0
gtin: "1234567890123"
capabilities:
  motor:
    kind: motor
    target: { kind: joint, id: motor_joint }
    command: velocity
"#,
        );
        assert_valid(&validator(DocumentKind::Component), &component);

        let simulation = yaml_value(
            r#"
schema: simulation/v0
capabilities:
  motor:
    kind: motor
    actuator_type: velocity
"#,
        );
        assert_valid(&validator(DocumentKind::Simulation), &simulation);

        let mut malformed_robot = robot;
        malformed_robot["robot"]["kinematic"]["steering_actuator"] =
            serde_json::json!({ "component_id": "steering", "capability_id": "motor" });
        assert!(!validator(DocumentKind::Robot).is_valid(&malformed_robot));

        let mut malformed_component = component;
        malformed_component["gtin"] = serde_json::json!({ "value": "1234567890123" });
        assert!(!validator(DocumentKind::Component).is_valid(&malformed_component));
    }

    #[test]
    fn schema_is_structural_while_manifest_validation_is_semantic() {
        let document = r#"
schema: robot/v0
robot:
  id: ""
  namespace: dev
  kinematic: { kind: omnidirectional, actuators: [], encoders: [] }
  motion_limits: { max_linear_speed_mps: 1.0, max_angular_speed_radps: 1.0 }
"#;
        let value = yaml_value(document);
        assert_valid(&validator(DocumentKind::Robot), &value);

        let manifest: crate::source::robot::Manifest =
            serde_yaml::from_str(document).expect("document should match the serde DTO");
        let crate::source::robot::Manifest::V0(manifest) = manifest;
        assert!(
            manifest.validate().is_err(),
            "semantic validation remains authoritative"
        );
    }
}
