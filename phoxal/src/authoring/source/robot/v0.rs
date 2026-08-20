//! Exact `robot.yaml` v0 document.
//!
//! Everything here is one struct's grammar: [`Manifest`] and the field types
//! that only exist because it has those fields. The rules the document must
//! satisfy live in the private `validation` module, which is a separate concern
//! with its own contract - it reads a parsed document and reports every rule it
//! broke.

mod validation;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// Kinematics and motion limits are canonical domain facts with exactly one
// definition. This document describes the shape they take in authored YAML; it
// does not re-export them, so `crate::model::robot` stays their only path.
use crate::model::CapabilityRole;
use crate::model::robot::{KinematicConfig, MotionLimits};

// The driver block is shared across robot document generations rather than
// owned by this one, so it is named here at its established authored path and
// defined once beside the document family. Its connection vocabulary is the
// canonical model's, not a document type at all.
pub use super::driver::DriverConfig;

/// Exact top-level `robot.yaml` v0 document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Ordered parent robot documents composed before this leaf manifest.
    ///
    /// Parent maps are deep-merged in declaration order, then the leaf wins.
    /// Sequence and scalar values replace earlier values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extends: Vec<PathBuf>,
    pub robot: RobotSection,
    /// The user services this robot runs, keyed by service identity, each with
    /// its user-owned configuration. The Cargo workspace is the candidate set
    /// (a declared service must have a matching workspace crate); this map
    /// selects which discovered services belong to the robot and are built,
    /// staged, and launched. An undeclared workspace service crate is legal
    /// and simply not part of the robot.
    ///
    /// [`RESERVED_BRAIN_ID`] is not an available key: the mandatory root brain
    /// is the root Cargo package's binary, never an authored service.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub services: BTreeMap<String, UserService>,
}

/// The participant identity of the one mandatory root brain.
///
/// The brain is the root Cargo package's binary, discovered and staged by the
/// CLI as `bin/brain`. It is never an authored participant, so this identity
/// is reserved against every authored identity map in this document.
pub const RESERVED_BRAIN_ID: &str = "brain";

/// `robot:` - the robot model: identity, structure, kinematic, and
/// components.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RobotSection {
    /// The robot identifier.
    pub id: String,
    /// Path to the URDF structure file, relative to the robot root.
    #[serde(default = "default_structure_path")]
    pub structure: PathBuf,
    /// The kinematic model.
    pub kinematic: KinematicConfig,
    /// Manifest-wide planar speed limits. Both `motion` and `drive` enforce these
    /// independently so an arbitration defect cannot bypass the actuator
    /// backstop.
    pub motion_limits: MotionLimits,
    /// Component instance map: instance-id -> instance.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub components: BTreeMap<String, Component>,
}

/// One declared user service: presence in `services` is the declaration;
/// `config` is its user-owned configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UserService {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
}

/// One mounted component instance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Component {
    pub component: String,
    pub mount_link: String,
    /// The driver block. Its presence is what declares a component driver for
    /// this instance; it states how the component is wired to the machine and
    /// carries that driver's own configuration. The driver's participant id is
    /// the instance id this block sits under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver: Option<DriverConfig>,
    /// What each capability on this instance is declared to be for.
    ///
    /// This is the input to service activation: which services a robot runs is
    /// decided from the roles its capabilities declare, so that a robot with
    /// no capability serving a role does not start the service that consumes
    /// it. The compiler resolves these keys against the selected component
    /// document and persists the typed assignment in `manifest.json`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub roles: BTreeMap<String, Vec<CapabilityRole>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parameters: BTreeMap<String, Parameters>,
}

/// Per-instance overrides for one capability the component type declares.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Parameters {
    Motor(MotorParameters),
    Encoder(EncoderParameters),
    Accelerometer(NoParameters),
    Gyroscope(NoParameters),
    Magnetometer(NoParameters),
    Imu(NoParameters),
    Gnss(NoParameters),
    Camera(NoParameters),
    Depth(NoParameters),
    Range(NoParameters),
    Lidar(NoParameters),
    Mmwave(NoParameters),
    Microphone(NoParameters),
    Speaker(NoParameters),
    Battery(NoParameters),
    Led(NoParameters),
    EmergencyStop(NoParameters),
}

impl Parameters {
    /// The capability kind these parameters claim to override.
    ///
    /// The compiler compares this against the kind the component type declares,
    /// so it returns the canonical kind rather than a parallel spelling of it.
    #[must_use]
    pub const fn kind(&self) -> crate::model::component::capability::CapabilityKind {
        use crate::model::component::capability::CapabilityKind;
        match self {
            Self::Motor(_) => CapabilityKind::Motor,
            Self::Encoder(_) => CapabilityKind::Encoder,
            Self::Accelerometer(_) => CapabilityKind::Accelerometer,
            Self::Gyroscope(_) => CapabilityKind::Gyroscope,
            Self::Magnetometer(_) => CapabilityKind::Magnetometer,
            Self::Imu(_) => CapabilityKind::Imu,
            Self::Gnss(_) => CapabilityKind::Gnss,
            Self::Camera(_) => CapabilityKind::Camera,
            Self::Depth(_) => CapabilityKind::Depth,
            Self::Range(_) => CapabilityKind::Range,
            Self::Lidar(_) => CapabilityKind::Lidar,
            Self::Mmwave(_) => CapabilityKind::Mmwave,
            Self::Microphone(_) => CapabilityKind::Microphone,
            Self::Speaker(_) => CapabilityKind::Speaker,
            Self::Battery(_) => CapabilityKind::Battery,
            Self::Led(_) => CapabilityKind::Led,
            Self::EmergencyStop(_) => CapabilityKind::EmergencyStop,
        }
    }

    /// The authored direction sign, where the capability kind has one.
    #[must_use]
    pub const fn direction_sign(&self) -> i8 {
        match self {
            Self::Motor(value) => value.direction_sign,
            Self::Encoder(value) => value.direction_sign,
            _ => 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MotorParameters {
    #[serde(default = "default_direction_sign")]
    pub direction_sign: i8,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct EncoderParameters {
    #[serde(default = "default_direction_sign")]
    pub direction_sign: i8,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct NoParameters {}

/// One authored rule a `robot.yaml` v0 document broke.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    EmptyRobotId,
    InvalidToken {
        field: String,
        value: String,
    },
    EmptyComponentType {
        instance: String,
    },
    EmptyMountLink {
        instance: String,
    },
    EmptyRoleList {
        instance: String,
        capability: String,
    },
    RepeatedRole {
        instance: String,
        capability: String,
        role: CapabilityRole,
    },
    InvalidKinematicField {
        field: String,
        message: String,
    },
    InvalidMotionLimit {
        field: String,
        message: String,
    },
    InvalidDirectionSign {
        instance: String,
        capability: String,
    },
    /// An authored identity map claimed [`RESERVED_BRAIN_ID`].
    ReservedBrainId {
        /// The authored top-level map, e.g. `services`.
        map: String,
    },
}

impl Manifest {
    /// Every rule this document breaks, or `Ok(())` when it breaks none.
    ///
    /// # Errors
    ///
    /// Returns every [`ValidationError`] at once: an author fixing a document
    /// should see the whole list, not one rule per attempt.
    pub fn validate(&self) -> Result<(), Vec<ValidationError>> {
        let mut errors = Vec::new();
        self.validate_basics(&mut errors);
        self.validate_reserved_identities(&mut errors);
        self.validate_component_structure(&mut errors);
        self.validate_role_hints(&mut errors);
        self.validate_kinematics(&mut errors);
        self.validate_numerics(&mut errors);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Resolve this generation's grammar into the version-independent robot.
    ///
    /// Everything that is v0 syntax ends here: `extends` has already been
    /// composed away by the time a document reaches this point, the authored
    /// per-capability parameter blocks collapse into the kind and the direction
    /// sign the compiler acts on, and an authored role list becomes the role
    /// set it always meant (the grammar is what rejects a repeat).
    ///
    /// # Errors
    ///
    /// Returns no error today: every v0 value already has a normalized
    /// counterpart. The boundary stays fallible because normalization is a
    /// generation's own obligation, and a later generation's may not be total.
    pub(crate) fn normalize(
        self,
    ) -> Result<crate::authoring::normalized::Robot, crate::authoring::CompileError> {
        let instances = self
            .robot
            .components
            .into_iter()
            .map(|(id, component)| {
                let parameters = component
                    .parameters
                    .into_iter()
                    .map(|(capability, parameters)| {
                        (
                            capability,
                            crate::authoring::normalized::CapabilityParameters {
                                kind: parameters.kind(),
                                direction_sign: parameters.direction_sign(),
                            },
                        )
                    })
                    .collect();
                let roles = component
                    .roles
                    .into_iter()
                    .map(|(capability, roles)| (capability, roles.into_iter().collect()))
                    .collect();
                (
                    id,
                    crate::authoring::normalized::ComponentInstance {
                        component_type: component.component,
                        mount_link: component.mount_link,
                        driver: component.driver,
                        roles,
                        parameters,
                    },
                )
            })
            .collect();

        Ok(crate::authoring::normalized::Robot {
            id: self.robot.id,
            structure: self.robot.structure,
            kinematic: self.robot.kinematic,
            motion_limits: self.robot.motion_limits,
            instances,
            services: self
                .services
                .into_iter()
                .map(|(id, service)| (id, service.config))
                .collect(),
        })
    }

    #[must_use]
    pub fn used_component_types(&self) -> BTreeSet<&str> {
        self.robot
            .components
            .values()
            .map(|component| component.component.as_str())
            .collect()
    }

    fn validate_basics(&self, errors: &mut Vec<ValidationError>) {
        if self.robot.id.trim().is_empty() {
            errors.push(ValidationError::EmptyRobotId);
        }
        for id in self.services.keys() {
            if !crate::model::identity::is_valid_token(id) {
                errors.push(ValidationError::InvalidToken {
                    field: format!("services.{id}"),
                    value: id.clone(),
                });
            }
        }
    }

    fn validate_reserved_identities(&self, errors: &mut Vec<ValidationError>) {
        if self.services.contains_key(RESERVED_BRAIN_ID) {
            errors.push(ValidationError::ReservedBrainId {
                map: "services".to_string(),
            });
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRobotId => formatter.write_str("robot.id must not be empty"),
            Self::InvalidToken { field, value } => write!(
                formatter,
                "{field} value '{value}' must contain only lowercase ASCII letters, digits, '_' or '-'"
            ),
            Self::EmptyComponentType { instance } => write!(
                formatter,
                "robot.components.{instance}.component must not be empty"
            ),
            Self::EmptyMountLink { instance } => {
                write!(
                    formatter,
                    "robot.components.{instance}.mount_link must not be empty"
                )
            }
            Self::EmptyRoleList {
                instance,
                capability,
            } => write!(
                formatter,
                "robot.components.{instance}.roles.{capability} must list at least one role"
            ),
            Self::RepeatedRole {
                instance,
                capability,
                role,
            } => write!(
                formatter,
                "robot.components.{instance}.roles.{capability} repeats role '{role}'"
            ),
            Self::InvalidKinematicField { field, message } => {
                write!(formatter, "robot.kinematic.{field} {message}")
            }
            Self::InvalidMotionLimit { field, message } => {
                write!(formatter, "robot.motion_limits.{field} {message}")
            }
            Self::InvalidDirectionSign {
                instance,
                capability,
            } => write!(
                formatter,
                "robot.components.{instance}.parameters.{capability}.direction_sign must be either -1 or 1"
            ),
            Self::ReservedBrainId { map } => write!(
                formatter,
                "{map}.{RESERVED_BRAIN_ID} is reserved for the mandatory root brain: the brain is \
                 the root Cargo package's binary (src/main.rs with #[phoxal::brain]) and is never \
                 declared under `{map}:`; rename this entry"
            ),
        }
    }
}

fn default_structure_path() -> PathBuf {
    PathBuf::from("structure.urdf")
}

const fn default_direction_sign() -> i8 {
    1
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::authoring::source::SourceError;
    use crate::authoring::source::robot::Manifest;

    /// The rules a rejected document broke, or an empty list when it failed
    /// before validation ran at all.
    fn robot_violations(error: &SourceError) -> &[super::ValidationError] {
        error
            .violations()
            .and_then(crate::authoring::source::Violations::robot)
            .unwrap_or_default()
    }

    /// Minimal canonical five-root-key manifest that also passes
    /// [`super::Manifest::validate`] (the kinematic model has one actuator so the
    /// "must not be empty" check does not fire). `extra_top_level` is
    /// appended as additional top-level sections after `robot:`.
    fn minimal_manifest(extra_top_level: &str) -> String {
        format!(
            r#"
schema: phoxal/robot/v0
robot:
  id: test-bot
  motion_limits:
    max_linear_speed_mps: 0.6
    max_angular_speed_radps: 2.0
  kinematic:
    kind: omnidirectional
    actuators: [drive.motor]
    encoders: []
  components: {{}}
{extra_top_level}"#
        )
    }

    #[test]
    fn canonical_five_root_key_manifest_parses_and_round_trips() -> anyhow::Result<()> {
        let yaml = r#"
schema: phoxal/robot/v0
robot:
  id: rover
  structure: structure.urdf
  motion_limits:
    max_linear_speed_mps: 0.6
    max_angular_speed_radps: 2.0
  kinematic:
    kind: differential
    left_actuators: [left_drive.motor]
    right_actuators: [right_drive.motor]
    left_encoders: [left_drive.encoder]
    right_encoders: [right_drive.encoder]
    wheel_radius_m: 0.12
    wheel_base_m: 0.6
  components:
    left_drive:
      component: ddsm115
      mount_link: left_wheel_mount
      driver:
        connection: { type: can, bus: 0, node_id: 1 }
      parameters:
        motor:   { kind: motor,   direction_sign: 1 }
        encoder: { kind: encoder, direction_sign: 1 }
services:
  avoid-obstacles:
    config: { max_linear_speed_mps: 0.6 }
"#;
        let manifest = Manifest::parse(yaml)?;
        let Manifest::V0(robot) = manifest.clone();

        assert_eq!(robot.robot.id, "rover");
        assert_eq!(robot.robot.structure, PathBuf::from("structure.urdf"));
        let service = robot
            .services
            .get("avoid-obstacles")
            .expect("service should parse");
        assert_eq!(
            service
                .config
                .as_ref()
                .and_then(|config| config.get("max_linear_speed_mps"))
                .and_then(serde_json::Value::as_f64),
            Some(0.6)
        );

        let serialized = serde_yaml::to_string(&manifest)?;
        let reparsed = Manifest::parse(&serialized)?;
        assert_eq!(reparsed, manifest);

        Ok(())
    }

    /// A driver block is a bundle fact, not a launch decision: whether the
    /// robot is driven by hardware or by a simulator is decided when the CLI
    /// chooses what to start, so the authored document always carries it.
    #[test]
    fn a_component_declares_its_driver_unconditionally() -> anyhow::Result<()> {
        let manifest = Manifest::parse(
            r#"
schema: phoxal/robot/v0
robot:
  id: test-bot
  motion_limits:
    max_linear_speed_mps: 0.6
    max_angular_speed_radps: 2.0
  kinematic:
    kind: omnidirectional
    actuators: [drive.motor]
    encoders: []
  components:
    drive:
      component: drive_motor
      mount_link: base_link
      driver:
        connection: { type: can, bus: 0, node_id: 1 }
"#,
        )?;
        let Manifest::V0(manifest) = manifest;
        assert!(manifest.robot.components["drive"].driver.is_some());
        Ok(())
    }

    #[test]
    fn instance_parameters_parse_emergency_stop_capability() -> anyhow::Result<()> {
        let manifest = Manifest::parse(
            r#"
schema: phoxal/robot/v0
robot:
  id: test-bot
  motion_limits:
    max_linear_speed_mps: 0.6
    max_angular_speed_radps: 2.0
  kinematic:
    kind: omnidirectional
    actuators: [drive.motor]
    encoders: []
  components:
    estop:
      component: estop
      mount_link: base_link
      parameters:
        e_stop:
          kind: emergency_stop
"#,
        )?;
        let Manifest::V0(robot) = manifest;

        let instance = robot
            .robot
            .components
            .get("estop")
            .expect("estop instance should parse");
        let parameters = instance
            .parameters
            .get("e_stop")
            .expect("e_stop capability parameters should parse");
        assert_eq!(parameters.kind().as_str(), "emergency_stop");

        Ok(())
    }

    #[test]
    fn canonical_safety_role_parses_from_authored_documents() -> anyhow::Result<()> {
        let manifest = Manifest::parse(
            r#"
schema: phoxal/robot/v0
robot:
  id: test-bot
  motion_limits:
    max_linear_speed_mps: 0.6
    max_angular_speed_radps: 2.0
  kinematic:
    kind: omnidirectional
    actuators: [drive.motor]
    encoders: []
  components:
    sensor:
      component: range
      mount_link: base_link
      roles:
        range: [safety]
"#,
        )?;
        let Manifest::V0(manifest) = manifest;
        assert_eq!(
            manifest.robot.components["sensor"].roles["range"],
            vec![crate::model::CapabilityRole::Safety]
        );
        Ok(())
    }

    #[test]
    fn user_service_config_parses() -> anyhow::Result<()> {
        let manifest = Manifest::parse(&minimal_manifest(
            r#"services:
  autonomy:
    config:
      max_linear_speed_mps: 0.6
      enabled: true
"#,
        ))?;
        let Manifest::V0(robot) = manifest;

        let service = robot
            .services
            .get("autonomy")
            .expect("user service should parse");
        assert_eq!(
            service
                .config
                .as_ref()
                .and_then(|config| config.get("enabled"))
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );

        Ok(())
    }

    #[test]
    fn user_service_without_config_round_trips_and_omits_config() -> anyhow::Result<()> {
        let manifest = Manifest::parse(&minimal_manifest(
            r#"services:
  autonomy: {}
"#,
        ))?;
        let yaml = serde_yaml::to_string(&manifest)?;

        assert!(
            !yaml.contains("config:"),
            "absent config should be omitted: {yaml}"
        );

        let Manifest::V0(reparsed) = Manifest::parse(&yaml)?;
        let Manifest::V0(robot) = manifest;
        assert_eq!(reparsed.services, robot.services);

        Ok(())
    }

    /// The document grammar is closed: a root key the DTO does not declare is
    /// rejected rather than ignored.
    #[test]
    fn an_undeclared_root_key_is_rejected() {
        let error = Manifest::parse(&minimal_manifest("unknown_section: {}\n"))
            .expect_err("an undeclared root key must not parse");
        assert!(
            error
                .to_string()
                .contains("unknown field `unknown_section`"),
            "got: {error}"
        );
    }

    /// The robot grammar is closed independently of the document root, so an
    /// undeclared robot key cannot be silently dropped during deserialization.
    #[test]
    fn an_undeclared_robot_key_is_rejected() {
        let error = Manifest::parse(
            r#"
schema: phoxal/robot/v0
robot:
  id: test-bot
  unknown_key: {}
  motion_limits:
    max_linear_speed_mps: 0.6
    max_angular_speed_radps: 2.0
  kinematic:
    kind: omnidirectional
    actuators: []
    encoders: []
  components: {}
"#,
        )
        .expect_err("an undeclared robot key must not parse");

        assert!(
            format!("{error:#}").contains("unknown field `unknown_key`"),
            "got: {error:#}"
        );
    }

    /// Every authored section closes its own grammar, not only the document
    /// root, so a typo at any depth fails instead of being silently dropped.
    #[test]
    fn an_undeclared_key_in_each_nested_section_is_rejected() {
        let documents = [
            (
                "services",
                minimal_manifest("services:\n  autonomy:\n    unknown_key: {}\n"),
            ),
            (
                "components.drive",
                r#"
schema: phoxal/robot/v0
robot:
  id: test-bot
  motion_limits:
    max_linear_speed_mps: 0.6
    max_angular_speed_radps: 2.0
  kinematic:
    kind: omnidirectional
    actuators: []
    encoders: []
  components:
    drive:
      component: ddsm115
      mount_link: drive_mount
      unknown_key: {}
"#
                .to_owned(),
            ),
            (
                "components.drive.driver",
                r#"
schema: phoxal/robot/v0
robot:
  id: test-bot
  motion_limits:
    max_linear_speed_mps: 0.6
    max_angular_speed_radps: 2.0
  kinematic:
    kind: omnidirectional
    actuators: []
    encoders: []
  components:
    drive:
      component: ddsm115
      mount_link: drive_mount
      driver:
        connection: { type: can, bus: 0, node_id: 1 }
        unknown_key: {}
"#
                .to_owned(),
            ),
        ];
        for (section, document) in documents {
            let error =
                Manifest::parse(&document).expect_err("an undeclared nested key must not parse");
            assert!(
                format!("{error:#}").contains("unknown field `unknown_key`"),
                "{section}: got: {error:#}"
            );
        }
    }

    #[test]
    fn a_service_named_brain_is_rejected_with_an_actionable_diagnostic() {
        let error = Manifest::parse(&minimal_manifest("services:\n  brain: {}\n"))
            .expect_err("services.brain must be reserved for the mandatory root brain");

        assert!(
            robot_violations(&error).contains(&super::ValidationError::ReservedBrainId {
                map: "services".to_string(),
            }),
            "{error}"
        );
        let message = error.to_string();
        assert!(message.contains("services.brain is reserved"), "{message}");
        assert!(message.contains("#[phoxal::brain]"), "{message}");
    }

    #[test]
    fn robot_manifest_requires_schema_v0() -> anyhow::Result<()> {
        let robot = Manifest::parse(&minimal_manifest(""))?;

        let yaml = serde_yaml::to_string(&robot)?;
        assert!(
            yaml.starts_with("schema: phoxal/robot/v0\nrobot:\n"),
            "the schema tag leads: {yaml}"
        );

        Ok(())
    }

    /// The driver block belongs to the document family rather than to this
    /// generation, and is re-exported here. An authored path that named it
    /// before must keep naming the same type, not a second copy of it - and its
    /// connection is the canonical model's vocabulary, which is what lets a
    /// driver read it as a typed value at runtime.
    #[test]
    fn the_driver_block_keeps_its_established_authored_path() {
        assert_eq!(
            std::any::TypeId::of::<super::DriverConfig>(),
            std::any::TypeId::of::<crate::authoring::source::robot::driver::DriverConfig>()
        );
        let block = super::DriverConfig {
            connection: crate::model::connection::Connection::Can(crate::model::connection::Can {
                bus: 0,
                node_id: 1,
            }),
            config: None,
        };
        assert_eq!(
            block.connection.kind(),
            crate::model::connection::ConnectionKind::Can
        );
    }

    #[test]
    fn empty_robot_id_is_validation_error() {
        let error = Manifest::parse(
            r#"
schema: phoxal/robot/v0
robot:
  id: ""
  motion_limits:
    max_linear_speed_mps: 0.6
    max_angular_speed_radps: 2.0
  kinematic:
    kind: omnidirectional
    actuators: []
    encoders: []
  components: {}
"#,
        )
        .expect_err("blank robot id should fail validation");

        assert!(robot_violations(&error).contains(&super::ValidationError::EmptyRobotId));
        assert_eq!(
            super::ValidationError::EmptyRobotId.to_string(),
            "robot.id must not be empty"
        );
    }
}
