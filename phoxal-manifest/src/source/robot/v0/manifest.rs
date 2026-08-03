use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;

use super::motion::{KinematicConfig, MotionLimits};
use super::{Component, Role};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum Schema {
    #[serde(rename = "robot/v0")]
    V0,
}

/// Exact top-level `robot.yaml` v0 document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: Schema,
    /// Ordered parent robot documents composed before this leaf manifest.
    ///
    /// Parent maps are deep-merged in declaration order, then the leaf wins.
    /// Sequence and scalar values replace earlier values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extends: Vec<PathBuf>,
    pub robot: RobotSection,
    /// Authoritative behavior root selected for this robot. Behavior
    /// definitions themselves live under `behaviors/` in the robot root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior: Option<BehaviorConfig>,
    /// The user services this robot runs, keyed by service identity, each with
    /// its user-owned configuration. The Cargo workspace is the candidate set
    /// (a declared service must have a matching workspace crate); this map
    /// selects which discovered services belong to the robot and are built,
    /// staged, and launched. An undeclared workspace service crate is legal
    /// and simply not part of the robot.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub services: BTreeMap<String, UserService>,
    /// Optional project-relative Zenoh router configuration.
    #[serde(default, skip_serializing_if = "Router::is_empty")]
    pub router: Router,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BehaviorConfig {
    pub root: String,
    #[serde(default)]
    pub autostart: bool,
}

/// `robot:` - the robot model: identity, structure, kinematic, and
/// components.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RobotSection {
    /// The robot identifier.
    pub id: String,
    /// The bus namespace.
    pub namespace: String,
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Router {
    /// Zenoh JSON5 configuration path relative to the resolved robot root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<PathBuf>,
}

impl Router {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.config.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    EmptyRobotId,
    EmptyRobotNamespace,
    EmptyRouterConfigPath,
    AbsoluteRouterConfigPath {
        path: PathBuf,
    },
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
        role: Role,
    },
    InvalidRuntimeClock {
        instance: String,
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
}

impl Manifest {
    pub fn validate(&self) -> std::result::Result<(), Vec<ValidationError>> {
        let mut errors = Vec::new();
        self.validate_basics(&mut errors);
        self.validate_router(&mut errors);
        self.validate_component_structure(&mut errors);
        self.validate_driver_structure(&mut errors);
        self.validate_role_hints(&mut errors);
        self.validate_kinematics(&mut errors);
        self.validate_numerics(&mut errors);
        validation_result(errors)
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
        if self.robot.namespace.trim().is_empty() {
            errors.push(ValidationError::EmptyRobotNamespace);
        }
    }

    fn validate_router(&self, errors: &mut Vec<ValidationError>) {
        if let Some(path) = &self.router.config {
            if path.as_os_str().is_empty() {
                errors.push(ValidationError::EmptyRouterConfigPath);
            } else if path.is_absolute() {
                errors.push(ValidationError::AbsoluteRouterConfigPath { path: path.clone() });
            }
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRobotId => formatter.write_str("robot.id must not be empty"),
            Self::EmptyRobotNamespace => formatter.write_str("robot.namespace must not be empty"),
            Self::EmptyRouterConfigPath => formatter.write_str("router.config must not be empty"),
            Self::AbsoluteRouterConfigPath { path } => write!(
                formatter,
                "router.config path '{}' must be project-relative",
                path.display()
            ),
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
            Self::InvalidRuntimeClock { instance } => write!(
                formatter,
                "robot.components.{instance}.driver.runtime_clock_ms must be > 0"
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
        }
    }
}

fn validation_result(
    errors: Vec<ValidationError>,
) -> std::result::Result<(), Vec<ValidationError>> {
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn default_structure_path() -> PathBuf {
    PathBuf::from("structure.urdf")
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    /// Minimal canonical five-root-key manifest that also passes
    /// [`super::Manifest::validate`] (the kinematic model has one actuator so the
    /// "must not be empty" check does not fire). `extra_top_level` is
    /// appended as additional top-level sections after `robot:`.
    fn minimal_manifest(extra_top_level: &str) -> String {
        format!(
            r#"
schema: robot/v0
robot:
  id: test-bot
  namespace: dev
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
schema: robot/v0
robot:
  id: rover
  namespace: dev
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
router:
  config: config/router.json5
"#;
        let robot = crate::source::robot::parse_from_string(yaml)?;

        assert_eq!(robot.robot.id, "rover");
        assert_eq!(robot.robot.namespace, "dev");
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
        assert_eq!(
            robot.router.config.as_deref(),
            Some(Path::new("config/router.json5"))
        );
        robot
            .validate()
            .expect("canonical manifest should validate");

        let serialized = serde_yaml::to_string(&robot)?;
        let reparsed = crate::source::robot::parse_from_string(&serialized)?;
        assert_eq!(reparsed, robot);

        Ok(())
    }

    #[test]
    fn instance_parameters_parse_emergency_stop_capability() -> anyhow::Result<()> {
        let robot = crate::source::robot::parse_from_string(
            r#"
schema: robot/v0
robot:
  id: test-bot
  namespace: dev
  motion_limits:
    max_linear_speed_mps: 0.6
    max_angular_speed_radps: 2.0
  kinematic:
    kind: omnidirectional
    actuators: []
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

        let instance = robot
            .robot
            .components
            .get("estop")
            .expect("estop instance should parse");
        let parameters = instance
            .parameters
            .get("e_stop")
            .expect("e_stop capability parameters should parse");
        assert_eq!(parameters.kind_name(), "emergency_stop");

        Ok(())
    }

    #[test]
    fn user_service_config_parses() -> anyhow::Result<()> {
        let robot = crate::source::robot::parse_from_string(&minimal_manifest(
            r#"services:
  autonomy:
    config:
      max_linear_speed_mps: 0.6
      enabled: true
"#,
        ))?;

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
        assert!(robot.router.is_empty());

        Ok(())
    }

    #[test]
    fn user_service_without_config_round_trips_and_omits_config() -> anyhow::Result<()> {
        let robot = crate::source::robot::parse_from_string(&minimal_manifest(
            r#"services:
  autonomy: {}
"#,
        ))?;
        let yaml = serde_yaml::to_string(&robot)?;

        assert!(
            !yaml.contains("config:"),
            "absent config should be omitted: {yaml}"
        );

        let reparsed = crate::source::robot::parse_from_string(&yaml)?;
        assert_eq!(reparsed.services, robot.services);

        Ok(())
    }

    #[test]
    fn router_config_parses_and_validates() -> anyhow::Result<()> {
        let robot = crate::source::robot::parse_from_string(&minimal_manifest(
            r#"router:
  config: config/router.json5
"#,
        ))?;

        assert_eq!(
            robot.router.config.as_deref(),
            Some(Path::new("config/router.json5"))
        );
        robot
            .validate()
            .expect("project-relative router config should validate");

        Ok(())
    }

    #[test]
    fn router_rejects_absolute_config_path() -> anyhow::Result<()> {
        let robot = crate::source::robot::parse_from_string(&minimal_manifest(
            r#"router:
  config: /etc/phoxal/router.json5
"#,
        ))?;

        let errors = robot
            .validate()
            .expect_err("absolute router config should fail validation");
        assert!(
            errors.contains(&super::ValidationError::AbsoluteRouterConfigPath {
                path: PathBuf::from("/etc/phoxal/router.json5"),
            })
        );

        Ok(())
    }

    #[test]
    fn router_rejects_empty_config_path() -> anyhow::Result<()> {
        let robot = crate::source::robot::parse_from_string(&minimal_manifest(
            r#"router:
  config: ""
"#,
        ))?;

        let errors = robot
            .validate()
            .expect_err("empty router config should fail validation");
        assert!(errors.contains(&super::ValidationError::EmptyRouterConfigPath));
        Ok(())
    }

    #[test]
    fn legacy_bus_section_is_rejected() {
        let error = crate::source::robot::parse_from_string(&minimal_manifest(
            r#"bus:
  listen: ["tcp/127.0.0.1:7447"]
"#,
        ))
        .expect_err("legacy bus section should no longer parse");

        assert!(
            format!("{error:#}").contains("unknown field `bus`"),
            "got: {error:#}"
        );
    }

    #[test]
    fn robot_section_rejects_identity_wrapper() {
        let error = crate::source::robot::parse_from_string(
            r#"
schema: robot/v0
robot:
  identity:
    id: test-bot
    namespace: dev
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
        .expect_err("nested identity: wrapper should no longer parse");

        assert!(
            format!("{error:#}").contains("missing field `id`")
                || format!("{error:#}").contains("unknown field `identity`"),
            "got: {error:#}"
        );
    }

    #[test]
    fn robot_section_rejects_motion_wrapper() {
        let error = crate::source::robot::parse_from_string(
            r#"
schema: robot/v0
robot:
  id: test-bot
  namespace: dev
  motion:
    kinematic:
      kind: omnidirectional
      actuators: []
      encoders: []
  components: {}
"#,
        )
        .expect_err("motion: wrapper should no longer parse");

        assert!(
            format!("{error:#}").contains("unknown field `motion`")
                || format!("{error:#}").contains("missing field `kinematic`"),
            "got: {error:#}"
        );
    }

    #[test]
    fn manifest_rejects_phoxal_artifacts_key() {
        let error = crate::source::robot::parse_from_string(
            r#"
schema: robot/v0
robot:
  id: test-bot
  namespace: dev
  motion_limits:
    max_linear_speed_mps: 0.6
    max_angular_speed_radps: 2.0
  kinematic:
    kind: omnidirectional
    actuators: []
    encoders: []
  components: {}
phoxal_artifacts:
  channel: stable
"#,
        )
        .expect_err("phoxal_artifacts root key should no longer parse");

        assert!(
            format!("{error:#}").contains("unknown field `phoxal_artifacts`"),
            "got: {error:#}"
        );
    }

    #[test]
    fn manifest_rejects_phoxal_participants_key() {
        let error = crate::source::robot::parse_from_string(
            r#"
schema: robot/v0
robot:
  id: test-bot
  namespace: dev
  motion_limits:
    max_linear_speed_mps: 0.6
    max_angular_speed_radps: 2.0
  kinematic:
    kind: omnidirectional
    actuators: []
    encoders: []
  components: {}
phoxal_participants: {}
"#,
        )
        .expect_err("phoxal_participants root key should no longer parse");

        assert!(
            format!("{error:#}").contains("unknown field `phoxal_participants`"),
            "got: {error:#}"
        );
    }

    #[test]
    fn manifest_rejects_version_discriminator() {
        let error = crate::source::robot::parse_from_string(
            r#"
version: legacy
robot:
  id: test-bot
  namespace: dev
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
        .expect_err("version discriminator should no longer parse");

        assert!(format!("{error:#}").contains("schema"), "got: {error:#}");
    }

    #[test]
    fn a_root_tools_map_no_longer_parses() {
        // The tool concept is gone (#978): every former tool is either absorbed
        // by the supervisor or a CLI companion, so `tools:` is now just an
        // unknown field rather than a declaration the manifest still honours.
        let error = crate::source::robot::parse_from_string(&minimal_manifest(
            "tools:\n  lidar-viz:\n    config:\n      port: 9000\n",
        ))
        .expect_err("a tools: map must no longer parse");
        assert!(
            format!("{error:#}").contains("unknown field `tools`"),
            "got: {error:#}"
        );
    }

    #[test]
    fn components_sources_nesting_no_longer_parses() {
        let error = crate::source::robot::parse_from_string(
            r#"
schema: robot/v0
robot:
  id: test-bot
  namespace: dev
  motion_limits:
    max_linear_speed_mps: 0.6
    max_angular_speed_radps: 2.0
  kinematic:
    kind: omnidirectional
    actuators: []
    encoders: []
  components:
    sources: {}
    instances: {}
"#,
        )
        .expect_err("components.sources/.instances nesting should no longer parse");

        assert!(
            format!("{error:#}").contains("component")
                || format!("{error:#}").contains("missing field"),
            "got: {error:#}"
        );
    }

    #[test]
    fn component_driver_rejects_image_field() {
        let error = crate::source::robot::parse_from_string(
            r#"
schema: robot/v0
robot:
  id: test-bot
  namespace: dev
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
        image: phoxal/component-ddsm115-driver:v0.1.5
        connection: { type: can, bus: 0, node_id: 1 }
"#,
        )
        .expect_err("driver.image should no longer parse");

        assert!(
            format!("{error:#}").contains("unknown field `image`"),
            "got: {error:#}"
        );
    }

    #[test]
    fn artifacts_and_service_path_are_rejected() {
        let artifacts =
            crate::source::robot::parse_from_string(&minimal_manifest("artifacts:\n  pins: {}\n"))
                .expect_err("Cargo owns source selection");
        assert!(format!("{artifacts:#}").contains("unknown field `artifacts`"));

        let service_path = crate::source::robot::parse_from_string(&minimal_manifest(
            "services:\n  autonomy:\n    path: services/autonomy\n",
        ))
        .expect_err("workspace membership owns service discovery");
        assert!(format!("{service_path:#}").contains("unknown field `path`"));
    }

    #[test]
    fn robot_manifest_requires_schema_v0() -> anyhow::Result<()> {
        let robot = crate::source::robot::parse_from_string(&minimal_manifest(""))?;

        let yaml = serde_yaml::to_string(&robot)?;
        assert!(
            yaml.starts_with("schema: robot/v0\nrobot:\n"),
            "schema should be the first root key: {yaml}"
        );

        Ok(())
    }

    #[test]
    fn empty_robot_id_is_validation_error() -> anyhow::Result<()> {
        let robot = crate::source::robot::parse_from_string(
            r#"
schema: robot/v0
robot:
  id: ""
  namespace: dev
  motion_limits:
    max_linear_speed_mps: 0.6
    max_angular_speed_radps: 2.0
  kinematic:
    kind: omnidirectional
    actuators: []
    encoders: []
  components: {}
"#,
        )?;

        let errors = robot
            .validate()
            .expect_err("blank robot id should fail validation");

        assert!(errors.contains(&super::ValidationError::EmptyRobotId));
        assert_eq!(
            super::ValidationError::EmptyRobotId.to_string(),
            "robot.id must not be empty"
        );

        Ok(())
    }
}
