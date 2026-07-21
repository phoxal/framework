use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use super::{Component, KinematicConfig, MotionLimits, Role, capability};

const ROBOT_FILE: &str = "robot.yaml";

/// Top-level `robot:` section - the robot model, and exactly what
/// `ctx.robot()` exposes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Robot {
    pub robot: RobotSection,
    /// Authoritative behavior root selected for this robot. Behavior
    /// definitions themselves live under `behaviors/` in the robot root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior: Option<BehaviorConfig>,
    /// Explicit development-only source overrides for official packages.
    #[serde(default, skip_serializing_if = "Artifacts::is_default")]
    pub artifacts: Artifacts,
    /// User services: project-local source crates built by `phoxal-cli` for
    /// run/sim/deploy. Official services are never declared here.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub services: BTreeMap<String, UserService>,
    /// Optional project-relative Zenoh router configuration. The CLI resolves
    /// this through the normal environment overlay before launching the
    /// Phoxal-owned infrastructure router.
    #[serde(default, skip_serializing_if = "Router::is_empty")]
    pub router: Router,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BehaviorConfig {
    pub root: String,
    #[serde(default)]
    pub autostart: bool,
}

/// `robot:` - the robot model: identity, structure, kinematic, and
/// components.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RobotSection {
    /// The robot id (was `identity.id`).
    pub id: String,
    /// The bus namespace (was `identity.namespace`).
    pub namespace: String,
    /// Path to the URDF structure file, relative to the robot root.
    #[serde(default = "default_structure_path")]
    pub structure: PathBuf,
    /// The kinematic model (was `motion.kinematic`; the `motion:` wrapper is
    /// gone - `kinematic` is a direct field of `robot:`).
    pub kinematic: KinematicConfig,
    /// Robot-wide planar speed limits. Both `motion` and `drive` enforce these
    /// independently so an arbitration defect cannot bypass the actuator
    /// backstop.
    pub motion_limits: MotionLimits,
    /// Component instance map: instance-id -> instance (was
    /// `components.instances`; `components.sources` no longer exists -
    /// official components resolve from the train suite, forks are
    /// `artifacts.pins` entries).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub components: BTreeMap<String, Component>,
}

/// Development-only source overrides. Published artifact inventory comes from
/// the locked framework train's immutable `suite.json`.
///
/// The whole section is optional; a day-0 robot can omit it entirely.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Artifacts {
    /// Fail-closed source override map keyed by provider-qualified package id
    /// (`phoxal/service-drive`, `phoxal/component-ddsm115`, ...).
    ///
    /// Keys must have exactly one `/`, with a non-empty provider segment
    /// before it and a non-empty name segment after it; unqualified keys
    /// (`service-drive`, `driver-ddsm115`) are rejected. The CLI validates
    /// existence in the resolved graph, dev-overlay-only path pins, and
    /// whether each key is actually used; the model only validates key shape.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pins: BTreeMap<String, ArtifactPin>,
}

impl Artifacts {
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.pins.is_empty()
    }
}

/// One development-only `artifacts.pins` source override.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum ArtifactPin {
    /// Resolve this package from a local source checkout (dev-overlay-only).
    Path(ArtifactPathPin),
    /// Resolve this package from a git repository at a pinned revision.
    Git(ArtifactGitPin),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ArtifactPathPin {
    /// Project-relative or overlay-relative source path for this package.
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ArtifactGitPin {
    /// Git repository URL.
    pub git: String,
    /// Pinned revision (commit sha, tag, or branch).
    pub rev: String,
    /// Optional subdirectory within the repository that holds the package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<PathBuf>,
}

/// `services:` (was `user_participants` / `user_services`, and the separate
/// `tools` map) - user services only; official services are framework
/// packages pinned via `artifacts.pins`, never declared here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct UserService {
    pub path: PathBuf,
    /// First-class typed config block, read by the service as `Self::Config`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
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
    EmptyUserServiceConfig {
        service: String,
    },
    EmptyRouterConfigPath,
    AbsoluteRouterConfigPath {
        path: PathBuf,
    },
    InvalidArtifactPinKey {
        key: String,
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

impl Robot {
    pub fn read_from_dir(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        Self::read_from_string(
            &std::fs::read_to_string(path.join(ROBOT_FILE)).with_context(|| {
                format!(
                    "failed to read robot file {}",
                    path.join(ROBOT_FILE).display()
                )
            })?,
        )
    }

    pub fn read_from_string(string: &str) -> Result<Self> {
        crate::model::robot::Robot::read_from_string(string)
            .map(crate::model::robot::Robot::into_v0)
    }

    pub fn parse_from_dir(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        Self::parse_from_string(
            &std::fs::read_to_string(path.join(ROBOT_FILE)).with_context(|| {
                format!(
                    "failed to read robot file {}",
                    path.join(ROBOT_FILE).display()
                )
            })?,
        )
    }

    pub fn parse_from_string(string: &str) -> Result<Self> {
        crate::model::robot::Robot::parse_from_string(string)
            .map(crate::model::robot::Robot::into_v0)
    }

    pub fn write_to_dir(&self, path: impl AsRef<Path>) -> Result<()> {
        crate::model::robot::Robot::V0(self.clone()).write_to_dir(path)
    }

    pub fn validate(&self) -> std::result::Result<(), Vec<ValidationError>> {
        let mut errors = Vec::new();
        self.validate_basics(&mut errors);
        self.validate_router(&mut errors);
        self.validate_artifact_pins(&mut errors);
        self.validate_user_services(&mut errors);
        self.validate_component_structure(&mut errors);
        self.validate_driver_structure(&mut errors);
        self.validate_role_hints(&mut errors);
        self.validate_kinematics(&mut errors);
        self.validate_numerics(&mut errors);
        validation_result(errors)
    }

    /// Historical entry point kept for callers that used to validate against
    /// a platform-participant allowlist. The `phoxal_participants.images`
    /// concept this guarded is gone from the grammar, so this now behaves
    /// identically to [`Robot::validate`].
    pub fn validate_with(
        &self,
        _platform_participant_names: &[&str],
    ) -> std::result::Result<(), Vec<ValidationError>> {
        self.validate()
    }

    #[must_use]
    pub fn robot_id(&self) -> &str {
        &self.robot.id
    }

    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.robot.namespace
    }

    #[must_use]
    pub fn components(&self) -> &BTreeMap<String, Component> {
        &self.robot.components
    }

    #[must_use]
    pub fn component_instance(&self, component_id: &str) -> Option<&Component> {
        self.robot.components.get(component_id)
    }

    #[must_use]
    pub fn parameter(
        &self,
        capability_ref: &crate::model::component::v0::CapabilityRef,
    ) -> Option<&capability::Parameters> {
        self.component_instance(&capability_ref.component_id)
            .and_then(|component| component.parameters.get(&capability_ref.capability_id))
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

    fn validate_artifact_pins(&self, errors: &mut Vec<ValidationError>) {
        for key in self.artifacts.pins.keys() {
            if !is_provider_qualified_pin_key(key) {
                errors.push(ValidationError::InvalidArtifactPinKey { key: key.clone() });
            }
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

    fn validate_user_services(&self, errors: &mut Vec<ValidationError>) {
        for (service, config) in &self.services {
            if config.path.as_os_str().is_empty() {
                errors.push(ValidationError::EmptyUserServiceConfig {
                    service: service.clone(),
                });
            }
        }
    }
}

/// A provider-qualified package id has exactly one `/`, with a non-empty
/// provider segment before it and a non-empty name segment after it.
#[must_use]
pub fn is_provider_qualified_pin_key(key: &str) -> bool {
    let mut parts = key.split('/');
    let (Some(provider), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !provider.is_empty() && !name.is_empty()
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRobotId => formatter.write_str("robot.id must not be empty"),
            Self::EmptyRobotNamespace => formatter.write_str("robot.namespace must not be empty"),
            Self::EmptyUserServiceConfig { service } => {
                write!(formatter, "services.{service}.path must not be empty")
            }
            Self::EmptyRouterConfigPath => formatter.write_str("router.config must not be empty"),
            Self::AbsoluteRouterConfigPath { path } => write!(
                formatter,
                "router.config path '{}' must be project-relative",
                path.display()
            ),
            Self::InvalidArtifactPinKey { key } => write!(
                formatter,
                "artifacts.pins key '{key}' must be a provider-qualified package id ('<provider>/<name>')"
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

    use super::{ArtifactGitPin, ArtifactPathPin, ArtifactPin, Robot};

    /// Minimal canonical five-root-key manifest that also passes
    /// [`Robot::validate`] (the kinematic model has one actuator so the
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

    fn manifest_with_artifacts(artifacts_block: &str) -> String {
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
    actuators: []
    encoders: []
  components: {{}}
artifacts:
{artifacts_block}
"#
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
    path: ./services/avoid-obstacles
    config: { max_linear_speed_mps: 0.6 }
router:
  config: config/router.json5
"#;
        let robot = Robot::parse_from_string(yaml)?;

        assert_eq!(robot.robot.id, "rover");
        assert_eq!(robot.robot.namespace, "dev");
        assert_eq!(robot.robot.structure, PathBuf::from("structure.urdf"));
        assert!(robot.artifacts.pins.is_empty());
        let service = robot
            .services
            .get("avoid-obstacles")
            .expect("service should parse");
        assert_eq!(service.path, PathBuf::from("./services/avoid-obstacles"));
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

        let serialized = serde_yaml::to_string(&crate::model::robot::Robot::V0(robot.clone()))?;
        let reparsed = Robot::parse_from_string(&serialized)?;
        assert_eq!(reparsed, robot);

        Ok(())
    }

    #[test]
    fn instance_parameters_parse_emergency_stop_capability() -> anyhow::Result<()> {
        let robot = Robot::parse_from_string(
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
    fn user_service_with_path_and_config_parses() -> anyhow::Result<()> {
        let robot = Robot::parse_from_string(&minimal_manifest(
            r#"services:
  autonomy:
    path: services/autonomy
    config:
      max_linear_speed_mps: 0.6
      enabled: true
"#,
        ))?;

        let service = robot
            .services
            .get("autonomy")
            .expect("user service should parse");
        assert_eq!(service.path, PathBuf::from("services/autonomy"));
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
        let robot = Robot::parse_from_string(&minimal_manifest(
            r#"services:
  autonomy:
    path: services/autonomy
"#,
        ))?;
        let yaml = serde_yaml::to_string(&crate::model::robot::Robot::V0(robot.clone()))?;

        assert!(
            !yaml.contains("config:"),
            "absent config should be omitted: {yaml}"
        );

        let reparsed = Robot::parse_from_string(&yaml)?;
        assert_eq!(reparsed.services, robot.services);

        Ok(())
    }

    #[test]
    fn router_config_parses_and_validates() -> anyhow::Result<()> {
        let robot = Robot::parse_from_string(&minimal_manifest(
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
        let robot = Robot::parse_from_string(&minimal_manifest(
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
        let robot = Robot::parse_from_string(&minimal_manifest(
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
        let error = Robot::parse_from_string(&minimal_manifest(
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
        let error = Robot::parse_from_string(
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
        let error = Robot::parse_from_string(
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
        let error = Robot::parse_from_string(
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
        let error = Robot::parse_from_string(
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
        let error = Robot::parse_from_string(
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
    fn manifest_rejects_root_tools_key() {
        let error = Robot::parse_from_string(
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
tools:
  router:
    version: "0.4.2"
"#,
        )
        .expect_err("root tools key should no longer parse");

        assert!(
            format!("{error:#}").contains("unknown field `tools`"),
            "got: {error:#}"
        );
    }

    #[test]
    fn components_sources_nesting_no_longer_parses() {
        let error = Robot::parse_from_string(
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
        let error = Robot::parse_from_string(
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
    fn artifacts_pins_version_form_is_rejected() {
        let error = Robot::parse_from_string(&manifest_with_artifacts(
            r#"  pins:
    phoxal/service-drive: v0.8.4"#,
        ))
        .expect_err("published artifact versions come from suite.json");
        assert!(format!("{error:#}").contains("did not match any variant"));
    }

    #[test]
    fn artifacts_pins_git_form_parses() -> anyhow::Result<()> {
        let robot = Robot::parse_from_string(&manifest_with_artifacts(
            r#"  pins:
    phoxal/component-ddsm115:
      git: https://github.com/you/ddsm115-component
      rev: 9f2c1e7
      directory: component/ddsm115"#,
        ))?;

        assert_eq!(
            robot.artifacts.pins.get("phoxal/component-ddsm115"),
            Some(&ArtifactPin::Git(ArtifactGitPin {
                git: "https://github.com/you/ddsm115-component".to_string(),
                rev: "9f2c1e7".to_string(),
                directory: Some(PathBuf::from("component/ddsm115")),
            }))
        );

        Ok(())
    }

    #[test]
    fn artifacts_pins_sha256_form_is_rejected() {
        let error = Robot::parse_from_string(&manifest_with_artifacts(
            r#"  pins:
    phoxal/tool-bus: "sha256:2222222222222222222222222222222222222222222222222222222222222222""#,
        ))
        .expect_err("published checksums come from suite.json");
        assert!(format!("{error:#}").contains("did not match any variant"));
    }

    #[test]
    fn artifacts_pins_path_form_round_trips() -> anyhow::Result<()> {
        let robot = Robot::parse_from_string(&manifest_with_artifacts(
            r#"  pins:
    phoxal/service-drive:
      path: ../framework/service/drive"#,
        ))?;

        assert_eq!(
            robot.artifacts.pins.get("phoxal/service-drive"),
            Some(&ArtifactPin::Path(ArtifactPathPin {
                path: PathBuf::from("../framework/service/drive"),
            }))
        );

        let yaml = serde_yaml::to_string(&crate::model::robot::Robot::V0(robot.clone()))?;
        assert!(
            yaml.contains(
                "pins:\n    phoxal/service-drive:\n      path: ../framework/service/drive"
            ),
            "path pin should serialize in the unified pins map: {yaml}"
        );

        let reparsed = Robot::parse_from_string(&yaml)?;
        assert_eq!(reparsed.artifacts.pins, robot.artifacts.pins);

        Ok(())
    }

    #[test]
    fn artifacts_pins_unqualified_key_is_validation_error() -> anyhow::Result<()> {
        let robot = Robot::parse_from_string(&manifest_with_artifacts(
            r#"  pins:
    service-drive:
      path: services/drive"#,
        ))?;

        let errors = robot
            .validate()
            .expect_err("unqualified pin key should fail validation");

        assert!(
            errors.contains(&super::ValidationError::InvalidArtifactPinKey {
                key: "service-drive".to_string(),
            })
        );

        Ok(())
    }

    #[test]
    fn artifacts_pins_key_missing_provider_or_name_is_validation_error() -> anyhow::Result<()> {
        for bad_key in ["/service-drive", "phoxal/", "phoxal/service/drive"] {
            let robot = Robot::parse_from_string(&manifest_with_artifacts(&format!(
                "  pins:\n    \"{bad_key}\":\n      path: services/drive"
            )))?;

            let errors = robot
                .validate()
                .expect_err("malformed pin key should fail validation");
            assert!(
                errors.iter().any(|error| matches!(
                    error,
                    super::ValidationError::InvalidArtifactPinKey { key } if key == bad_key
                )),
                "expected InvalidArtifactPinKey for {bad_key:?}, got: {errors:?}"
            );
        }

        Ok(())
    }

    #[test]
    fn artifacts_pins_rejects_unknown_value_form() {
        let error = Robot::parse_from_string(&manifest_with_artifacts(
            r#"  pins:
    phoxal/tool-bus:
      archive: bus.tar.zst"#,
        ))
        .expect_err("unsupported pin form should fail to parse");

        assert!(
            format!("{error:#}").contains("data did not match any variant of untagged enum"),
            "got: {error:#}"
        );
    }

    #[test]
    fn artifacts_pins_rejects_unknown_fields_inside_path_pin() {
        let error = Robot::parse_from_string(&manifest_with_artifacts(
            r#"  pins:
    phoxal/service-drive:
      path: ../framework/service/drive
      rev: 9f2c1e7"#,
        ))
        .expect_err("unknown path pin field should fail to parse");

        assert!(
            format!("{error:#}").contains("data did not match any variant of untagged enum"),
            "got: {error:#}"
        );
    }

    #[test]
    fn artifacts_section_entirely_omitted_by_default() -> anyhow::Result<()> {
        let robot = Robot::parse_from_string(&minimal_manifest(""))?;

        assert!(robot.artifacts.pins.is_empty());

        let yaml = serde_yaml::to_string(&crate::model::robot::Robot::V0(robot))?;
        assert!(
            !yaml.contains("artifacts:"),
            "default artifacts section should be omitted entirely: {yaml}"
        );

        Ok(())
    }

    #[test]
    fn artifacts_rejects_invalid_channel() {
        let error = Robot::parse_from_string(&manifest_with_artifacts("  channel: experimental"))
            .expect_err("invalid artifacts channel should fail to parse");

        assert!(format!("{error:#}").contains("unknown field `channel`"));
    }

    #[test]
    fn artifacts_no_longer_accepts_target_field() {
        let error = Robot::parse_from_string(&manifest_with_artifacts(
            "  target: aarch64-unknown-linux-gnu",
        ))
        .expect_err("artifacts.target should no longer parse");

        assert!(
            format!("{error:#}").contains("unknown field `target`"),
            "got: {error:#}"
        );
    }

    #[test]
    fn artifacts_rejects_channel_and_generation() {
        let error = Robot::parse_from_string(&manifest_with_artifacts(
            r#"  channel: preview
  generation: v2"#,
        ))
        .expect_err("train and API selectors do not belong in robot.yaml");
        assert!(format!("{error:#}").contains("unknown field `channel`"));
    }

    #[test]
    fn robot_manifest_requires_schema_v0() -> anyhow::Result<()> {
        let robot = Robot::parse_from_string(&minimal_manifest(""))?;

        let yaml = serde_yaml::to_string(&crate::model::robot::Robot::V0(robot))?;
        assert!(
            yaml.starts_with("schema: robot/v0\nrobot:\n"),
            "schema should be the first root key: {yaml}"
        );

        Ok(())
    }

    #[test]
    fn empty_robot_id_is_validation_error() -> anyhow::Result<()> {
        let robot = Robot::parse_from_string(
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
