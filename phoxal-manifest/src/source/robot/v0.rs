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
// does not re-export them, so `phoxal_model::robot` stays their only path.
use phoxal_model::CapabilityRole;
use phoxal_model::robot::{KinematicConfig, MotionLimits};

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
    /// The time domain this robot runs on.
    ///
    /// Authored documents omit it and get [`Clock::Real`]; finalization writes
    /// the resolved value explicitly into the bundle's `robot.yaml`.
    #[serde(default)]
    pub clock: Clock,
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
    /// Optional project-relative Zenoh router configuration.
    #[serde(default, skip_serializing_if = "RouterConfig::is_empty")]
    pub router: RouterConfig,
}

/// Authored `clock:` - the time domain the robot runs on.
///
/// This mirrors [`phoxal_model::Clock`] rather than reusing it: the canonical
/// value deliberately carries no wire form, and the exhaustive [`From`] impl
/// below turns any future divergence between the two into a compile error.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Clock {
    /// Host wall/monotonic time driven by real hardware.
    #[default]
    Real,
    /// Time published by a simulation world authority.
    Simulated,
}

impl From<Clock> for phoxal_model::Clock {
    fn from(clock: Clock) -> Self {
        match clock {
            Clock::Real => Self::Real,
            Clock::Simulated => Self::Simulated,
        }
    }
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

/// Authored `router:` - how this robot configures the Zenoh router it runs on.
///
/// Named for what it is rather than what it configures: the running router
/// itself is `phoxal-bus`'s `Router`, and one value configuring another is not
/// a reason for the two to share a name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "Router")]
pub struct RouterConfig {
    /// Zenoh JSON5 configuration path relative to the resolved robot root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<PathBuf>,
}

impl RouterConfig {
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.config.is_none()
    }
}

/// One mounted component instance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Component {
    pub component: String,
    pub mount_link: String,
    /// Hardware connection metadata. Its presence is what declares a component
    /// driver participant for this instance, and its body is that participant's
    /// configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver: Option<DriverConfig>,
    /// What each capability on this instance is declared to be for.
    ///
    /// This is the input to service activation: which services a robot runs is
    /// decided from the roles its capabilities declare, so that a robot with
    /// no capability serving a role does not start the service that consumes
    /// it. The compiler resolves these keys against the selected component
    /// document and persists the typed assignment in `runtime.json`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub roles: BTreeMap<String, Vec<CapabilityRole>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parameters: BTreeMap<String, Parameters>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DriverConfig {
    pub connection: ConnectionConfig,
    #[serde(default = "default_runtime_clock_ms")]
    pub runtime_clock_ms: u64,
}

/// Connection configuration for executable drivers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConnectionConfig {
    /// CAN bus connection.
    Can { bus: u8, node_id: u8 },
    /// I2C connection.
    I2c { bus: u8, address: u16 },
    /// SPI connection.
    Spi { bus: u8, chip_select: u8 },
    /// Serial port connection (RS-232/RS-485).
    Serial { port: String, baud: u32 },
    /// UART connection (distinct from Serial for hardware-specific drivers).
    Uart { port: String, baud_rate: u32 },
    /// USB connection.
    Usb {
        vendor_id: Option<u16>,
        product_id: Option<u16>,
    },
    /// GPIO pins.
    Gpio {
        chip: String,
        pins: Vec<GpioPinConfig>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GpioPinConfig {
    pub line: u16,
    pub direction: GpioDirection,
    #[serde(default)]
    pub active_low: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GpioDirection {
    Input,
    Output,
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
    pub const fn kind(&self) -> phoxal_model::component::capability::CapabilityKind {
        use phoxal_model::component::capability::CapabilityKind;
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
        role: CapabilityRole,
    },
    InvalidRuntimeClock {
        instance: String,
    },
    /// `clock: simulated` cannot coexist with a physical component driver.
    DriverUnderSimulatedClock {
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
        self.validate_router(&mut errors);
        self.validate_component_structure(&mut errors);
        self.validate_driver_structure(&mut errors);
        self.validate_role_hints(&mut errors);
        self.validate_kinematics(&mut errors);
        self.validate_numerics(&mut errors);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
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
            if !phoxal_model::identity::is_valid_token(id) {
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
            Self::DriverUnderSimulatedClock { instance } => write!(
                formatter,
                "robot.components.{instance}.driver cannot run under `clock: simulated`: a \
                 component driver owns physical hardware IO; remove the driver block or run \
                 this robot on the real clock"
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

const fn default_runtime_clock_ms() -> u64 {
    100
}

const fn default_direction_sign() -> i8 {
    1
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use crate::source::SourceError;
    use crate::source::robot::Manifest;

    /// The rules a rejected document broke, or an empty list when it failed
    /// before validation ran at all.
    fn robot_violations(error: &SourceError) -> &[super::ValidationError] {
        error
            .violations()
            .and_then(crate::source::Violations::robot)
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
router:
  config: config/router.json5
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
        assert_eq!(
            robot.router.config.as_deref(),
            Some(Path::new("config/router.json5"))
        );

        let serialized = serde_yaml::to_string(&manifest)?;
        let reparsed = Manifest::parse(&serialized)?;
        assert_eq!(reparsed, manifest);

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
            vec![phoxal_model::CapabilityRole::Safety]
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
        assert!(robot.router.is_empty());

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

    #[test]
    fn router_config_parses_and_validates() -> anyhow::Result<()> {
        let manifest = Manifest::parse(&minimal_manifest(
            r#"router:
  config: config/router.json5
"#,
        ))?;
        let Manifest::V0(robot) = manifest;

        assert_eq!(
            robot.router.config.as_deref(),
            Some(Path::new("config/router.json5"))
        );

        Ok(())
    }

    #[test]
    fn router_rejects_absolute_config_path() {
        let error = Manifest::parse(&minimal_manifest(
            r#"router:
  config: /etc/phoxal/router.json5
"#,
        ))
        .expect_err("absolute router config should fail validation");

        assert!(
            robot_violations(&error).contains(&super::ValidationError::AbsoluteRouterConfigPath {
                path: PathBuf::from("/etc/phoxal/router.json5"),
            }),
            "{error}"
        );
    }

    #[test]
    fn router_rejects_empty_config_path() {
        let error = Manifest::parse(&minimal_manifest(
            r#"router:
  config: ""
"#,
        ))
        .expect_err("empty router config should fail validation");
        assert!(
            robot_violations(&error).contains(&super::ValidationError::EmptyRouterConfigPath),
            "{error}"
        );
    }

    /// Bus transport is the router's configuration, reached through
    /// `router.config`, not a second authored transport section.
    #[test]
    fn a_root_bus_section_is_rejected() {
        let error = Manifest::parse(&minimal_manifest(
            r#"bus:
  listen: ["tcp/127.0.0.1:7447"]
"#,
        ))
        .expect_err("a bus: section is not part of the grammar");

        assert!(
            format!("{error:#}").contains("unknown field `bus`"),
            "got: {error:#}"
        );
    }

    #[test]
    fn robot_section_rejects_identity_wrapper() {
        let error = Manifest::parse(
            r#"
schema: phoxal/robot/v0
robot:
  identity:
    id: test-bot
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
        .expect_err("nested identity: wrapper should not parse");

        assert!(
            format!("{error:#}").contains("missing field `id`")
                || format!("{error:#}").contains("unknown field `identity`"),
            "got: {error:#}"
        );
    }

    #[test]
    fn robot_section_rejects_namespace() {
        let error = Manifest::parse(
            r#"
schema: phoxal/robot/v0
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
        .expect_err("robot.namespace is not part of the robot identity grammar");

        assert!(
            format!("{error:#}").contains("unknown field `namespace`"),
            "got: {error:#}"
        );
    }

    #[test]
    fn robot_section_rejects_motion_wrapper() {
        let error = Manifest::parse(
            r#"
schema: phoxal/robot/v0
robot:
  id: test-bot
  motion:
    kinematic:
      kind: omnidirectional
      actuators: []
      encoders: []
  components: {}
"#,
        )
        .expect_err("motion: wrapper should not parse");

        assert!(
            format!("{error:#}").contains("unknown field `motion`")
                || format!("{error:#}").contains("missing field `kinematic`"),
            "got: {error:#}"
        );
    }

    /// The document grammar is closed: a root key the DTO does not declare is
    /// rejected rather than ignored, whatever it is called. `tools:`,
    /// `behavior:` and `artifacts:` are named explicitly because each one was a
    /// plausible declaration an author might still reach for.
    #[test]
    fn an_undeclared_root_key_is_rejected() {
        for (key, document) in [
            ("phoxal_artifacts", "phoxal_artifacts:\n  channel: stable\n"),
            ("phoxal_participants", "phoxal_participants: {}\n"),
            (
                "tools",
                "tools:\n  lidar-viz:\n    config:\n      port: 9000\n",
            ),
            (
                "behavior",
                "behavior:\n  root: system.root\n  autostart: true\n",
            ),
        ] {
            let error = Manifest::parse(&minimal_manifest(document))
                .expect_err("an undeclared root key must not parse");
            assert!(
                error
                    .to_string()
                    .contains(&format!("unknown field `{key}`")),
                "got: {error}"
            );
        }
    }

    #[test]
    fn manifest_rejects_version_discriminator() {
        let error = Manifest::parse(
            r#"
version: legacy
robot:
  id: test-bot
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
        .expect_err("a version discriminator is not the schema tag");

        assert!(format!("{error:#}").contains("schema"), "got: {error:#}");
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

    /// `components:` maps instance ids straight to instances; there is no
    /// intermediate `sources:` / `instances:` split for an author to write.
    #[test]
    fn components_sources_nesting_is_rejected() {
        let error = Manifest::parse(
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
    sources: {}
    instances: {}
"#,
        )
        .expect_err("components.sources/.instances nesting is not the component map");

        assert!(
            format!("{error:#}").contains("component")
                || format!("{error:#}").contains("missing field"),
            "got: {error:#}"
        );
    }

    #[test]
    fn component_driver_rejects_image_field() {
        let error = Manifest::parse(
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
        image: phoxal/component-ddsm115-driver:v0.1.5
        connection: { type: can, bus: 0, node_id: 1 }
"#,
        )
        .expect_err("driver.image is not part of the driver grammar");

        assert!(
            format!("{error:#}").contains("unknown field `image`"),
            "got: {error:#}"
        );
    }

    /// Cargo owns source selection and workspace membership owns service
    /// discovery, so neither has an authored key here.
    #[test]
    fn source_selection_keys_are_rejected() {
        let artifacts = Manifest::parse(&minimal_manifest("artifacts:\n  pins: {}\n"))
            .expect_err("Cargo owns source selection");
        assert!(format!("{artifacts:#}").contains("unknown field `artifacts`"));

        let service_path = Manifest::parse(&minimal_manifest(
            "services:\n  autonomy:\n    path: services/autonomy\n",
        ))
        .expect_err("workspace membership owns service discovery");
        assert!(format!("{service_path:#}").contains("unknown field `path`"));
    }

    #[test]
    fn robot_manifest_requires_schema_v0() -> anyhow::Result<()> {
        let robot = Manifest::parse(&minimal_manifest(""))?;

        let yaml = serde_yaml::to_string(&robot)?;
        assert!(
            yaml.starts_with("schema: phoxal/robot/v0\nclock: real\nrobot:\n"),
            "the schema tag leads, and the resolved clock is always explicit: {yaml}"
        );

        Ok(())
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
