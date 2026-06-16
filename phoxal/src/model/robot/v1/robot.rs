use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};

use super::{Component, Motion, Role, capability};

const ROBOT_FILE: &str = "robot.yaml";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Robot {
    pub identity: Identity,
    #[serde(default = "default_structure_path")]
    pub structure: PathBuf,
    pub phoxal_runtimes: PhoxalRuntimes,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub user_runtimes: BTreeMap<String, UserRuntime>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, Tool>,
    pub motion: Motion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<Network>,
    pub components: Components,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub id: String,
    pub namespace: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhoxalRuntimes {
    pub version: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub overrides: BTreeMap<String, PlatformRuntimeOverride>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformRuntimeOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserRuntime {
    pub path: PathBuf,
    #[serde(
        default = "default_user_runtime_framework",
        skip_serializing_if = "is_match_platform"
    )]
    pub framework: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<UserRuntimeBuild>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserRuntimeBuild {
    #[serde(default = "default_build_context")]
    pub context: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dockerfile: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Network {
    #[serde(default)]
    pub uplink: Uplink,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<NetworkTls>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Uplink {
    /// Production zenoh endpoints (e.g. "tls/uplink.phoxal.cloud:7447").
    /// Ignored in sim mode - phoxal-cli rewrites the upstream link.
    #[serde(default)]
    pub endpoints: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkTls {
    pub cert: PathBuf,
    pub key: PathBuf,
    pub ca: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Components {
    pub sources: BTreeMap<String, ComponentSource>,
    pub instances: BTreeMap<String, Component>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ComponentSource {
    Git(SourceGit),
    Path(SourcePath),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceGit {
    pub git: String,
    pub tag: String,
    /// Optional subdirectory within the git repository that holds the
    /// component definition (`component.yaml` and friends). Absent means the
    /// component lives at the repository root — the historical single-component
    /// repository layout. A value such as `bno085` selects
    /// `<repo>/bno085/component.yaml`, enabling a shared catalog repository
    /// (e.g. `phoxal/components`) to host many components as subdirectories.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourcePath {
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    EmptyIdentityId,
    EmptyIdentityNamespace,
    UnknownPlatformRuntimeOverride {
        name: String,
    },
    UserRuntimeShadowsPlatformRuntime {
        name: String,
    },
    MissingComponentSource {
        instance: String,
        source: String,
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
            .map(crate::model::robot::Robot::into_v1)
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
            .map(crate::model::robot::Robot::into_v1)
    }

    pub fn write_to_dir(&self, path: impl AsRef<Path>) -> Result<()> {
        crate::model::robot::Robot::V1(self.clone()).write_to_dir(path)
    }

    pub fn validate(&self) -> std::result::Result<(), Vec<ValidationError>> {
        let mut errors = Vec::new();
        self.validate_basics(&mut errors);
        self.validate_component_sources(&mut errors);
        self.validate_component_structure(&mut errors);
        self.validate_driver_structure(&mut errors);
        self.validate_role_hints(&mut errors);
        self.validate_kinematics(&mut errors);
        self.validate_numerics(&mut errors);
        validation_result(errors)
    }

    pub fn validate_with(
        &self,
        platform_runtime_names: &[&str],
    ) -> std::result::Result<(), Vec<ValidationError>> {
        let mut errors = match self.validate() {
            Ok(()) => Vec::new(),
            Err(errors) => errors,
        };
        let platform_runtime_names = platform_runtime_names
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();

        for runtime_name in self.phoxal_runtimes.overrides.keys() {
            if !platform_runtime_names.contains(runtime_name.as_str()) {
                errors.push(ValidationError::UnknownPlatformRuntimeOverride {
                    name: runtime_name.clone(),
                });
            }
        }
        for runtime_name in self.user_runtimes.keys() {
            if platform_runtime_names.contains(runtime_name.as_str()) {
                errors.push(ValidationError::UserRuntimeShadowsPlatformRuntime {
                    name: runtime_name.clone(),
                });
            }
        }

        validation_result(errors)
    }

    #[must_use]
    pub fn robot_id(&self) -> &str {
        &self.identity.id
    }

    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.identity.namespace
    }

    #[must_use]
    pub fn components(&self) -> &BTreeMap<String, Component> {
        &self.components.instances
    }

    #[must_use]
    pub fn component_instance(&self, component_id: &str) -> Option<&Component> {
        self.components.instances.get(component_id)
    }

    #[must_use]
    pub fn parameter(
        &self,
        capability_ref: &crate::model::component::v1::CapabilityRef,
    ) -> Option<&capability::Parameters> {
        self.component_instance(&capability_ref.component_id)
            .and_then(|component| component.parameters.get(&capability_ref.capability_id))
    }

    #[must_use]
    pub fn used_component_types(&self) -> BTreeSet<&str> {
        self.components
            .instances
            .values()
            .map(|component| component.component.as_str())
            .collect()
    }

    fn validate_basics(&self, errors: &mut Vec<ValidationError>) {
        if self.identity.id.trim().is_empty() {
            errors.push(ValidationError::EmptyIdentityId);
        }
        if self.identity.namespace.trim().is_empty() {
            errors.push(ValidationError::EmptyIdentityNamespace);
        }
    }

    fn validate_component_sources(&self, errors: &mut Vec<ValidationError>) {
        for (instance_name, instance) in &self.components.instances {
            if !self.components.sources.contains_key(&instance.component) {
                errors.push(ValidationError::MissingComponentSource {
                    instance: instance_name.clone(),
                    source: instance.component.clone(),
                });
            }
        }
    }
}

impl Components {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty() && self.instances.is_empty()
    }
}

impl Deref for Components {
    type Target = BTreeMap<String, Component>;

    fn deref(&self) -> &Self::Target {
        &self.instances
    }
}

impl DerefMut for Components {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.instances
    }
}

impl<'a> IntoIterator for &'a Components {
    type Item = (&'a String, &'a Component);
    type IntoIter = std::collections::btree_map::Iter<'a, String, Component>;

    fn into_iter(self) -> Self::IntoIter {
        self.instances.iter()
    }
}

impl<'a> IntoIterator for &'a mut Components {
    type Item = (&'a String, &'a mut Component);
    type IntoIter = std::collections::btree_map::IterMut<'a, String, Component>;

    fn into_iter(self) -> Self::IntoIter {
        self.instances.iter_mut()
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyIdentityId => formatter.write_str("identity.id must not be empty"),
            Self::EmptyIdentityNamespace => {
                formatter.write_str("identity.namespace must not be empty")
            }
            Self::UnknownPlatformRuntimeOverride { name } => write!(
                formatter,
                "phoxal_runtimes.overrides.{name} is not a platform runtime"
            ),
            Self::UserRuntimeShadowsPlatformRuntime { name } => {
                write!(formatter, "user_runtimes.{name} shadows a platform runtime")
            }
            Self::MissingComponentSource { instance, source } => write!(
                formatter,
                "components.instances.{instance}.component references missing source '{source}'"
            ),
            Self::InvalidToken { field, value } => write!(
                formatter,
                "{field} value '{value}' must contain only lowercase ASCII letters, digits, '_' or '-'"
            ),
            Self::EmptyComponentType { instance } => write!(
                formatter,
                "components.instances.{instance}.component must not be empty"
            ),
            Self::EmptyMountLink { instance } => {
                write!(
                    formatter,
                    "components.instances.{instance}.mount_link must not be empty"
                )
            }
            Self::EmptyRoleList {
                instance,
                capability,
            } => write!(
                formatter,
                "components.instances.{instance}.roles.{capability} must list at least one role"
            ),
            Self::RepeatedRole {
                instance,
                capability,
                role,
            } => write!(
                formatter,
                "components.instances.{instance}.roles.{capability} repeats role '{role}'"
            ),
            Self::InvalidRuntimeClock { instance } => write!(
                formatter,
                "components.instances.{instance}.driver.runtime_clock_ms must be > 0"
            ),
            Self::InvalidKinematicField { field, message } => {
                write!(formatter, "motion.kinematic.{field} {message}")
            }
            Self::InvalidDirectionSign {
                instance,
                capability,
            } => write!(
                formatter,
                "components.instances.{instance}.parameters.{capability}.direction_sign must be either -1 or 1"
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

fn default_user_runtime_framework() -> String {
    "match-platform".to_string()
}

fn is_match_platform(framework: &str) -> bool {
    framework == "match-platform"
}

fn default_build_context() -> PathBuf {
    PathBuf::from(".")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{Robot, UserRuntimeBuild};

    #[test]
    fn user_runtime_with_only_path_defaults_framework_and_build() -> anyhow::Result<()> {
        let robot = Robot::parse_from_string(
            r#"
version: v1
identity:
  id: test-bot
  namespace: dev
phoxal_runtimes:
  version: "0.9.0"
user_runtimes:
  autonomy:
    path: runtimes/autonomy
motion:
  kinematic:
    kind: omnidirectional
    actuators: []
    encoders: []
components:
  sources: {}
  instances: {}
"#,
        )?;

        let runtime = robot
            .user_runtimes
            .get("autonomy")
            .expect("user runtime should parse");
        assert_eq!(runtime.path, PathBuf::from("runtimes/autonomy"));
        assert_eq!(runtime.framework, "match-platform");
        assert_eq!(runtime.build, None);

        Ok(())
    }

    #[test]
    fn user_runtime_parses_framework_and_full_build_recipe() -> anyhow::Result<()> {
        let robot = Robot::parse_from_string(
            r#"
version: v1
identity:
  id: test-bot
  namespace: dev
phoxal_runtimes:
  version: "0.9.0"
user_runtimes:
  autonomy:
    path: runtimes/autonomy
    framework: "0.9.0"
    build:
      context: container
      dockerfile: Dockerfile.runtime
      target: runtime
motion:
  kinematic:
    kind: omnidirectional
    actuators: []
    encoders: []
components:
  sources: {}
  instances: {}
"#,
        )?;

        let runtime = robot
            .user_runtimes
            .get("autonomy")
            .expect("user runtime should parse");
        assert_eq!(runtime.framework, "0.9.0");
        assert_eq!(
            runtime.build,
            Some(UserRuntimeBuild {
                context: PathBuf::from("container"),
                dockerfile: Some(PathBuf::from("Dockerfile.runtime")),
                target: Some("runtime".to_string()),
            })
        );

        Ok(())
    }

    #[test]
    fn user_runtime_build_rejects_unknown_fields() {
        let error = Robot::parse_from_string(
            r#"
version: v1
identity:
  id: test-bot
  namespace: dev
phoxal_runtimes:
  version: "0.9.0"
user_runtimes:
  autonomy:
    path: runtimes/autonomy
    build:
      bogus: 1
motion:
  kinematic:
    kind: omnidirectional
    actuators: []
    encoders: []
components:
  sources: {}
  instances: {}
"#,
        )
        .expect_err("unknown build fields should fail to parse");

        assert!(
            format!("{error:#}").contains("unknown field `bogus`"),
            "got: {error:#}"
        );
    }

    #[test]
    fn user_runtime_round_trips_and_omits_default_framework_and_empty_build() -> anyhow::Result<()>
    {
        let default_robot = Robot::parse_from_string(
            r#"
version: v1
identity:
  id: test-bot
  namespace: dev
phoxal_runtimes:
  version: "0.9.0"
user_runtimes:
  autonomy:
    path: runtimes/autonomy
motion:
  kinematic:
    kind: omnidirectional
    actuators: []
    encoders: []
components:
  sources: {}
  instances: {}
"#,
        )?;
        let default_yaml = serde_yaml::to_string(&crate::model::robot::Robot::V1(default_robot))?;

        assert!(
            !default_yaml.contains("framework:"),
            "default framework should be omitted: {default_yaml}"
        );
        assert!(
            !default_yaml.contains("build:"),
            "empty build recipe should be omitted: {default_yaml}"
        );

        let robot = Robot::parse_from_string(
            r#"
version: v1
identity:
  id: test-bot
  namespace: dev
phoxal_runtimes:
  version: "0.9.0"
user_runtimes:
  autonomy:
    path: runtimes/autonomy
    framework: "0.9.0"
    build:
      context: container
      dockerfile: Dockerfile.runtime
      target: runtime
motion:
  kinematic:
    kind: omnidirectional
    actuators: []
    encoders: []
components:
  sources: {}
  instances: {}
"#,
        )?;
        let yaml = serde_yaml::to_string(&crate::model::robot::Robot::V1(robot.clone()))?;
        let reparsed = Robot::parse_from_string(&yaml)?;

        assert_eq!(reparsed.user_runtimes, robot.user_runtimes);

        Ok(())
    }
}
