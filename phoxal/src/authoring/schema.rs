//! Portable JSON Schema documents for authored manifest editors.
//!
//! These schemas describe the serde wire shape of the current authored v0
//! documents. They are an editor aid only; [`crate::authoring::source`] parsing and
//! validation remain authoritative for semantic and cross-file constraints.
//!
//! The generation entry point is a method on [`DocumentKind`], the same value
//! that names an authored document everywhere else in this crate.

use schemars::{SchemaGenerator, generate::SchemaSettings};

use crate::authoring::source::DocumentKind;

impl DocumentKind {
    /// The stable file name for this document kind's generated editor schema.
    #[must_use]
    pub const fn schema_file_name(self) -> &'static str {
        match self {
            Self::Robot => "robot.schema.json",
            Self::Component => "component.schema.json",
            Self::Simulation => "simulation.schema.json",
            Self::World => "world.schema.json",
        }
    }

    /// Generate a portable Draft 2020-12 JSON Schema for this document kind.
    ///
    /// The returned ordinary JSON value can be serialized directly into an
    /// editor's local schema cache without exposing `schemars` types in this
    /// crate's public API.
    #[must_use]
    pub fn generate(self) -> serde_json::Value {
        let mut schema = match self {
            Self::Robot => SchemaGenerator::new(SchemaSettings::draft2020_12())
                .into_root_schema_for::<crate::authoring::source::robot::Manifest>(),
            Self::Component => SchemaGenerator::new(SchemaSettings::draft2020_12())
                .into_root_schema_for::<crate::authoring::source::component::Manifest>(
            ),
            Self::Simulation => SchemaGenerator::new(SchemaSettings::draft2020_12())
                .into_root_schema_for::<crate::authoring::source::simulation::Manifest>(
            ),
            Self::World => SchemaGenerator::new(SchemaSettings::draft2020_12())
                .into_root_schema_for::<crate::authoring::source::world::Manifest>(),
        };
        let (title, description) = self.schema_metadata();
        schema.insert("title".into(), title.into());
        schema.insert("description".into(), description.into());
        schema.to_value()
    }

    const fn schema_metadata(self) -> (&'static str, &'static str) {
        match self {
            Self::Robot => (
                "Phoxal robot manifest (phoxal/robot/v0)",
                "Editor schema for an authored Phoxal robot.yaml document.",
            ),
            Self::Component => (
                "Phoxal component manifest (phoxal/component/v0)",
                "Editor schema for an authored Phoxal component.yaml document.",
            ),
            Self::Simulation => (
                "Phoxal simulation manifest (phoxal/simulation/v0)",
                "Editor schema for an authored Phoxal simulation.yaml document.",
            ),
            Self::World => (
                "Phoxal world manifest (phoxal/world/v0)",
                "Editor schema for an authored Phoxal world.yaml document.",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::authoring::source::DocumentKind;

    fn validator(kind: DocumentKind) -> jsonschema::Validator {
        let document_schema = kind.generate();
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

    fn fixture_document(kind: DocumentKind) -> serde_json::Value {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixture");
        let path = match kind {
            DocumentKind::Robot => root.join("robot/rgbd-imu-diff-drive/robot.yaml"),
            DocumentKind::Component => root.join("components/drive_motor/component.yaml"),
            DocumentKind::Simulation => root.join("components/drive_motor/simulation.yaml"),
            DocumentKind::World => root.join("worlds/warehouse/world.yaml"),
        };
        let document = std::fs::read_to_string(path).expect("fixture document should be readable");
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
            4,
            "update the stable schema-generation inventory for every document kind"
        );
        for kind in DocumentKind::ALL {
            let (title, file_name) = match kind {
                DocumentKind::Robot => (
                    "Phoxal robot manifest (phoxal/robot/v0)",
                    "robot.schema.json",
                ),
                DocumentKind::Component => (
                    "Phoxal component manifest (phoxal/component/v0)",
                    "component.schema.json",
                ),
                DocumentKind::Simulation => (
                    "Phoxal simulation manifest (phoxal/simulation/v0)",
                    "simulation.schema.json",
                ),
                DocumentKind::World => (
                    "Phoxal world manifest (phoxal/world/v0)",
                    "world.schema.json",
                ),
            };
            let generated = kind.generate();
            assert_eq!(kind.schema_file_name(), file_name);
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
    fn schemas_validate_the_fixture_robot_authored_documents() {
        for kind in DocumentKind::ALL {
            assert_valid(&validator(kind), &fixture_document(kind));
        }
    }

    #[test]
    fn schemas_reject_unknown_root_properties() {
        for kind in DocumentKind::ALL {
            let mut value = fixture_document(kind);
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
        // Project policy belongs to the one mandatory root brain, so there is
        // no `behavior:` subsystem for a document to configure. The schema is
        // derived straight from the source DTO, so this is what keeps the
        // editor's view of the grammar honest about that.
        let schema = DocumentKind::Robot.generate();
        let serialized = serde_json::to_string(&schema).expect("schema serializes");
        assert!(
            !serialized.to_lowercase().contains("behavior"),
            "the robot schema describes a behavior subsystem that does not exist: {serialized}"
        );

        // A `behavior:` root property must also fail structurally, not merely
        // be undescribed.
        let mut document = fixture_document(DocumentKind::Robot);
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
            let first = serde_json::to_string_pretty(&kind.generate())
                .expect("generated schema should serialize deterministically");
            let second = serde_json::to_string_pretty(&kind.generate())
                .expect("generated schema should serialize deterministically");
            assert_eq!(first, second);
        }
    }

    #[test]
    fn schemas_cover_tagged_shapes_and_custom_string_wire_types() {
        let robot = yaml_value(
            r#"
schema: phoxal/robot/v0
robot:
  id: schema-bot
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
        connection:
          type: gpio
          chip: gpiochip0
          pins: [{ line: 1, direction: output }]
        config: { poll_hz: 10 }
      parameters:
        motor: { kind: motor, direction_sign: -1 }
"#,
        );
        assert_valid(&validator(DocumentKind::Robot), &robot);

        let component = yaml_value(
            r#"
schema: phoxal/component/v0
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
schema: phoxal/simulation/v0
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
schema: phoxal/robot/v0
robot:
  id: ""
  kinematic: { kind: omnidirectional, actuators: [], encoders: [] }
  motion_limits: { max_linear_speed_mps: 1.0, max_angular_speed_radps: 1.0 }
"#;
        let value = yaml_value(document);
        assert_valid(&validator(DocumentKind::Robot), &value);

        let manifest: crate::authoring::source::robot::Manifest =
            serde_yaml::from_str(document).expect("document should match the serde DTO");
        let crate::authoring::source::robot::Manifest::V0(manifest) = manifest;
        assert!(
            manifest.validate().is_err(),
            "semantic validation remains authoritative"
        );
    }
}
