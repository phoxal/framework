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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_version: Option<String>,
    pub identity: Identity,
    #[serde(default = "default_structure_path")]
    pub structure: PathBuf,
    #[serde(default, skip_serializing_if = "PhoxalArtifacts::is_default")]
    pub phoxal_artifacts: PhoxalArtifacts,
    pub phoxal_participants: PhoxalParticipants,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub user_participants: BTreeMap<String, UserParticipant>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, Tool>,
    pub motion: Motion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<Network>,
    pub components: Components,
    #[serde(default, skip_serializing_if = "Bus::is_empty")]
    pub bus: Bus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub id: String,
    pub namespace: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    #[default]
    Stable,
    Preview,
}

impl Channel {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Preview => "preview",
        }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhoxalArtifacts {
    /// Release channel used when resolving official framework artifacts.
    #[serde(default)]
    pub channel: Channel,
    /// Optional legacy target triple override; native-shape manifests infer this elsewhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Optional API generation ceiling or pin for official artifact resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<String>,
    /// Unified fail-closed pin map keyed by kind-qualified artifact id.
    ///
    /// Keys use the resolved artifact id format, such as `service-drive`,
    /// `driver-ddsm115`, `tool-router`, or `simulator-webots`. The CLI validates
    /// prefixes, existence in the resolved graph, dev-overlay-only path pins, and
    /// whether each key is actually used; the model only rejects empty keys.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pins: BTreeMap<String, ArtifactPin>,
}

impl PhoxalArtifacts {
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.channel == Channel::Stable
            && self.target.is_none()
            && self.generation.is_none()
            && self.pins.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ArtifactPin {
    /// Resolve this artifact from a local source checkout.
    Path(ArtifactPathPin),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactPathPin {
    /// Project-relative or overlay-relative source path for this artifact.
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhoxalParticipants {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub images: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserParticipant {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
    #[serde(
        default = "default_user_participant_framework",
        skip_serializing_if = "is_match_platform"
    )]
    pub framework: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<UserParticipantBuild>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserParticipantBuild {
    #[serde(default = "default_build_context")]
    pub context: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dockerfile: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bus {
    /// Router listen endpoints merged into the default localhost listen set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub listen: Vec<String>,
    /// Optional upstream router connection used for deployed robots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uplink: Option<BusUplink>,
}

impl Bus {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.listen.is_empty() && self.uplink.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BusUplink {
    /// Upstream Zenoh endpoint the site router should connect to.
    pub connect: String,
    /// Optional project-local mTLS material for the upstream connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<BusMtlsAuth>,
    /// Capped retry backoff; retry is forever and never gates readiness.
    #[serde(default, skip_serializing_if = "BusRetry::is_default")]
    pub retry: BusRetry,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BusMtlsAuth {
    /// Project-local root CA certificate path used to verify the upstream router.
    pub ca: PathBuf,
    /// Project-local client certificate path identifying this robot/site.
    pub cert: PathBuf,
    /// Project-local client private-key path paired with `cert`.
    pub key: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BusRetry {
    /// Initial retry delay in milliseconds.
    #[serde(default = "default_bus_retry_initial_ms")]
    pub initial_ms: u64,
    /// Maximum retry delay in milliseconds.
    #[serde(default = "default_bus_retry_max_ms")]
    pub max_ms: u64,
}

impl Default for BusRetry {
    fn default() -> Self {
        Self {
            initial_ms: default_bus_retry_initial_ms(),
            max_ms: default_bus_retry_max_ms(),
        }
    }
}

impl BusRetry {
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
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
    /// component lives at the repository root - the historical single-component
    /// repository layout. Catalog components now live in this repository under
    /// `component/<name>`; robots select one by setting `git` to
    /// `https://github.com/phoxal/framework` and `directory` to
    /// `component/<name>`.
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
    EmptyApiVersion,
    EmptyIdentityId,
    EmptyIdentityNamespace,
    UnknownPlatformParticipantImage {
        name: String,
    },
    UserParticipantShadowsPlatformParticipant {
        name: String,
    },
    EmptyUserParticipantImage {
        participant: String,
    },
    EmptyBusListenEndpoint {
        index: usize,
    },
    UnsupportedBusListenEndpoint {
        endpoint: String,
    },
    NonLoopbackTcpBusListenEndpoint {
        endpoint: String,
    },
    EmptyBusUplinkConnect,
    EmptyBusUplinkAuthPath {
        field: String,
    },
    AbsoluteBusUplinkAuthPath {
        field: String,
        path: PathBuf,
    },
    InvalidBusRetryBackoff,
    EmptyArtifactPinKey,
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
        self.validate_bus(&mut errors);
        self.validate_artifact_pins(&mut errors);
        self.validate_user_participants(&mut errors);
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
        platform_participant_names: &[&str],
    ) -> std::result::Result<(), Vec<ValidationError>> {
        let mut errors = match self.validate() {
            Ok(()) => Vec::new(),
            Err(errors) => errors,
        };
        let platform_participant_names = platform_participant_names
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();

        for participant_name in self.phoxal_participants.images.keys() {
            if !platform_participant_names.contains(participant_name.as_str()) {
                errors.push(ValidationError::UnknownPlatformParticipantImage {
                    name: participant_name.clone(),
                });
            }
        }
        for participant_name in self.user_participants.keys() {
            if platform_participant_names.contains(participant_name.as_str()) {
                errors.push(ValidationError::UserParticipantShadowsPlatformParticipant {
                    name: participant_name.clone(),
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
        if self
            .api_version
            .as_deref()
            .is_some_and(|api_version| api_version.trim().is_empty())
        {
            errors.push(ValidationError::EmptyApiVersion);
        }
        if self.identity.id.trim().is_empty() {
            errors.push(ValidationError::EmptyIdentityId);
        }
        if self.identity.namespace.trim().is_empty() {
            errors.push(ValidationError::EmptyIdentityNamespace);
        }
    }

    fn validate_artifact_pins(&self, errors: &mut Vec<ValidationError>) {
        if self
            .phoxal_artifacts
            .pins
            .keys()
            .any(|artifact_id| artifact_id.trim().is_empty())
        {
            errors.push(ValidationError::EmptyArtifactPinKey);
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

    fn validate_bus(&self, errors: &mut Vec<ValidationError>) {
        for (index, endpoint) in self.bus.listen.iter().enumerate() {
            validate_bus_listen_endpoint(index, endpoint, errors);
        }

        if let Some(uplink) = &self.bus.uplink {
            if uplink.connect.trim().is_empty() {
                errors.push(ValidationError::EmptyBusUplinkConnect);
            }
            if let Some(auth) = &uplink.auth {
                validate_project_local_path("ca", &auth.ca, errors);
                validate_project_local_path("cert", &auth.cert, errors);
                validate_project_local_path("key", &auth.key, errors);
            }
            if uplink.retry.initial_ms == 0
                || uplink.retry.max_ms == 0
                || uplink.retry.initial_ms > uplink.retry.max_ms
            {
                errors.push(ValidationError::InvalidBusRetryBackoff);
            }
        }
    }

    fn validate_user_participants(&self, errors: &mut Vec<ValidationError>) {
        for (participant, config) in &self.user_participants {
            if config
                .image
                .as_deref()
                .is_some_and(|image| image.trim().is_empty())
            {
                errors.push(ValidationError::EmptyUserParticipantImage {
                    participant: participant.clone(),
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
            Self::EmptyApiVersion => formatter.write_str("api_version must not be empty"),
            Self::EmptyIdentityId => formatter.write_str("identity.id must not be empty"),
            Self::EmptyIdentityNamespace => {
                formatter.write_str("identity.namespace must not be empty")
            }
            Self::UnknownPlatformParticipantImage { name } => write!(
                formatter,
                "phoxal_participants.images.{name} is not a platform participant"
            ),
            Self::UserParticipantShadowsPlatformParticipant { name } => {
                write!(
                    formatter,
                    "user_participants.{name} shadows a platform participant"
                )
            }
            Self::EmptyUserParticipantImage { participant } => {
                write!(
                    formatter,
                    "user_participants.{participant}.image must not be empty"
                )
            }
            Self::EmptyBusListenEndpoint { index } => {
                write!(formatter, "bus.listen[{index}] must not be empty")
            }
            Self::UnsupportedBusListenEndpoint { endpoint } => write!(
                formatter,
                "bus.listen endpoint '{endpoint}' must use serial/ or tcp/ on day one"
            ),
            Self::NonLoopbackTcpBusListenEndpoint { endpoint } => write!(
                formatter,
                "bus.listen TCP endpoint '{endpoint}' must bind loopback until listen auth ships"
            ),
            Self::EmptyBusUplinkConnect => {
                formatter.write_str("bus.uplink.connect must not be empty")
            }
            Self::EmptyBusUplinkAuthPath { field } => {
                write!(formatter, "bus.uplink.auth.{field} must not be empty")
            }
            Self::AbsoluteBusUplinkAuthPath { field, path } => write!(
                formatter,
                "bus.uplink.auth.{field} path '{}' must be project-local",
                path.display()
            ),
            Self::InvalidBusRetryBackoff => formatter.write_str(
                "bus.uplink.retry initial_ms and max_ms must be > 0 with initial_ms <= max_ms",
            ),
            Self::EmptyArtifactPinKey => {
                formatter.write_str("phoxal_artifacts.pins keys must not be empty")
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

fn default_user_participant_framework() -> String {
    "match-platform".to_string()
}

fn is_match_platform(framework: &str) -> bool {
    framework == "match-platform"
}

fn default_build_context() -> PathBuf {
    PathBuf::from(".")
}

fn default_bus_retry_initial_ms() -> u64 {
    1_000
}

fn default_bus_retry_max_ms() -> u64 {
    30_000
}

fn validate_bus_listen_endpoint(index: usize, endpoint: &str, errors: &mut Vec<ValidationError>) {
    let endpoint = endpoint.trim();
    if endpoint.is_empty() {
        errors.push(ValidationError::EmptyBusListenEndpoint { index });
    } else if endpoint.starts_with("serial/") {
        // Zenoh owns serial endpoint parsing; day-one framework validation only
        // gates the accepted listen schemes.
    } else if let Some(rest) = endpoint.strip_prefix("tcp/") {
        if !tcp_endpoint_is_loopback(rest) {
            errors.push(ValidationError::NonLoopbackTcpBusListenEndpoint {
                endpoint: endpoint.to_string(),
            });
        }
    } else {
        errors.push(ValidationError::UnsupportedBusListenEndpoint {
            endpoint: endpoint.to_string(),
        });
    }
}

fn tcp_endpoint_is_loopback(endpoint_tail: &str) -> bool {
    let host = endpoint_tail
        .strip_prefix('[')
        .and_then(|tail| tail.split_once(']').map(|(host, _rest)| host))
        .or_else(|| endpoint_tail.split_once(':').map(|(host, _port)| host))
        .unwrap_or(endpoint_tail);

    host == "localhost" || host == "::1" || host == "127.0.0.1" || host.starts_with("127.")
}

fn validate_project_local_path(field: &str, path: &Path, errors: &mut Vec<ValidationError>) {
    if path.as_os_str().is_empty() {
        errors.push(ValidationError::EmptyBusUplinkAuthPath {
            field: field.to_string(),
        });
    } else if path.is_absolute() {
        errors.push(ValidationError::AbsoluteBusUplinkAuthPath {
            field: field.to_string(),
            path: path.to_path_buf(),
        });
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{ArtifactPathPin, ArtifactPin, Channel, Robot, UserParticipantBuild};

    fn robot_yaml_with_phoxal_artifacts(phoxal_artifacts: &str) -> String {
        format!(
            r#"
schema: v0
identity:
  id: test-bot
  namespace: dev
phoxal_artifacts:
{phoxal_artifacts}
phoxal_participants: {{}}
motion:
  kinematic:
    kind: omnidirectional
    actuators: []
    encoders: []
components:
  sources: {{}}
  instances: {{}}
"#
        )
    }

    #[test]
    fn user_participant_with_only_path_defaults_framework_and_build() -> anyhow::Result<()> {
        let robot = Robot::parse_from_string(
            r#"
schema: v0
api_version: y2026_1
identity:
  id: test-bot
  namespace: dev
phoxal_participants: {}
user_participants:
  autonomy:
    path: participants/autonomy
motion:
  kinematic:
    kind: omnidirectional
    actuators:
    - drive.motor
    encoders: []
components:
  sources:
    drive:
      path: ../component/drive
  instances:
    drive:
      component: drive
      mount_link: drive_link
      parameters:
        motor:
          kind: motor
"#,
        )?;

        let participant = robot
            .user_participants
            .get("autonomy")
            .expect("user participant should parse");
        assert_eq!(participant.path, PathBuf::from("participants/autonomy"));
        assert_eq!(participant.framework, "match-platform");
        assert_eq!(participant.image, None);
        assert_eq!(participant.config, None);
        assert_eq!(participant.build, None);
        assert!(robot.bus.is_empty());

        Ok(())
    }

    #[test]
    fn user_participant_parses_framework_and_full_build_recipe() -> anyhow::Result<()> {
        let robot = Robot::parse_from_string(
            r#"
schema: v0
api_version: y2026_1
identity:
  id: test-bot
  namespace: dev
phoxal_participants: {}
user_participants:
  autonomy:
    path: participants/autonomy
    framework: "0.9.0"
    build:
      context: container
      dockerfile: Dockerfile.participant
      target: participant
motion:
  kinematic:
    kind: omnidirectional
    actuators:
    - drive.motor
    encoders: []
components:
  sources: {}
  instances: {}
"#,
        )?;

        let participant = robot
            .user_participants
            .get("autonomy")
            .expect("user participant should parse");
        assert_eq!(participant.framework, "0.9.0");
        assert_eq!(
            participant.build,
            Some(UserParticipantBuild {
                context: PathBuf::from("container"),
                dockerfile: Some(PathBuf::from("Dockerfile.participant")),
                target: Some("participant".to_string()),
            })
        );

        Ok(())
    }

    #[test]
    fn bus_listen_and_uplink_parse_and_validate() -> anyhow::Result<()> {
        let robot = Robot::parse_from_string(
            r#"
schema: v0
api_version: y2026_1
identity:
  id: test-bot
  namespace: dev
phoxal_participants: {}
user_participants:
  autonomy:
    path: participants/autonomy
    image: ghcr.io/acme/autonomy@sha256:abc
    config:
      max_linear_speed_mps: 0.6
      enabled: true
motion:
  kinematic:
    kind: omnidirectional
    actuators:
    - drive.motor
    encoders: []
components:
  sources:
    drive:
      path: ../component/drive
  instances:
    drive:
      component: drive
      mount_link: drive_link
      parameters:
        motor:
          kind: motor
bus:
  listen:
  - serial//dev/ttyACM0#baudrate=115200
  - tcp/127.0.0.1:7448
  uplink:
    connect: tls/uplink.phoxal.cloud:7447
    auth:
      ca: identity/ca.pem
      cert: identity/robot.pem
      key: identity/robot.key
    retry:
      initial_ms: 2000
      max_ms: 10000
"#,
        )?;

        let participant = robot
            .user_participants
            .get("autonomy")
            .expect("user participant should parse");
        assert_eq!(
            participant.image.as_deref(),
            Some("ghcr.io/acme/autonomy@sha256:abc")
        );
        assert_eq!(
            participant
                .config
                .as_ref()
                .and_then(|config| config.get("enabled"))
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert_eq!(
            robot.bus.listen,
            vec![
                "serial//dev/ttyACM0#baudrate=115200".to_string(),
                "tcp/127.0.0.1:7448".to_string(),
            ]
        );
        let uplink = robot.bus.uplink.as_ref().expect("uplink should parse");
        assert_eq!(uplink.connect, "tls/uplink.phoxal.cloud:7447");
        assert_eq!(uplink.retry.initial_ms, 2000);
        assert_eq!(uplink.retry.max_ms, 10000);
        assert_eq!(
            uplink.auth.as_ref().map(|auth| auth.cert.as_path()),
            Some(Path::new("identity/robot.pem"))
        );
        robot
            .validate()
            .expect("bus listen and uplink should validate");

        Ok(())
    }

    #[test]
    fn bus_rejects_non_loopback_tcp_listen() -> anyhow::Result<()> {
        let robot = Robot::parse_from_string(
            r#"
schema: v0
api_version: y2026_1
identity:
  id: test-bot
  namespace: dev
phoxal_participants: {}
motion:
  kinematic:
    kind: omnidirectional
    actuators: []
    encoders: []
components:
  sources: {}
  instances: {}
bus:
  listen:
  - tcp/0.0.0.0:7447
"#,
        )?;

        let errors = robot
            .validate()
            .expect_err("non-loopback TCP listen should fail validation");
        assert!(
            errors.contains(&super::ValidationError::NonLoopbackTcpBusListenEndpoint {
                endpoint: "tcp/0.0.0.0:7447".to_string(),
            })
        );

        Ok(())
    }

    #[test]
    fn user_participant_rejects_retired_bus_profile_field() {
        let error = Robot::parse_from_string(
            r#"
schema: v0
api_version: y2026_1
identity:
  id: test-bot
  namespace: dev
phoxal_participants: {}
user_participants:
  autonomy:
    path: participants/autonomy
    bus_profile: default
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
        .expect_err("retired bus_profile field should fail to parse");

        assert!(
            format!("{error:#}").contains("unknown field `bus_profile`"),
            "got: {error:#}"
        );
    }

    #[test]
    fn user_participant_build_rejects_unknown_fields() {
        let error = Robot::parse_from_string(
            r#"
schema: v0
api_version: y2026_1
identity:
  id: test-bot
  namespace: dev
phoxal_participants: {}
user_participants:
  autonomy:
    path: participants/autonomy
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
    fn user_participant_round_trips_and_omits_default_framework_and_empty_build()
    -> anyhow::Result<()> {
        let default_robot = Robot::parse_from_string(
            r#"
schema: v0
api_version: y2026_1
identity:
  id: test-bot
  namespace: dev
phoxal_participants: {}
user_participants:
  autonomy:
    path: participants/autonomy
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
schema: v0
api_version: y2026_1
identity:
  id: test-bot
  namespace: dev
phoxal_participants: {}
user_participants:
  autonomy:
    path: participants/autonomy
    framework: "0.9.0"
    build:
      context: container
      dockerfile: Dockerfile.participant
      target: participant
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

        assert_eq!(reparsed.user_participants, robot.user_participants);

        Ok(())
    }

    #[test]
    fn phoxal_artifacts_defaults_to_stable_without_pin_or_target() -> anyhow::Result<()> {
        let robot = Robot::parse_from_string(
            r#"
schema: v0
identity:
  id: test-bot
  namespace: dev
phoxal_participants: {}
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

        assert_eq!(robot.api_version, None);
        assert_eq!(robot.phoxal_artifacts.channel, Channel::Stable);
        assert_eq!(robot.phoxal_artifacts.channel.as_str(), "stable");
        assert_eq!(robot.phoxal_artifacts.channel.to_string(), "stable");
        assert_eq!(robot.phoxal_artifacts.target, None);
        assert_eq!(robot.phoxal_artifacts.generation, None);
        assert!(robot.phoxal_participants.images.is_empty());

        Ok(())
    }

    #[test]
    fn phoxal_artifacts_rejects_invalid_channel() {
        let error = Robot::parse_from_string(
            r#"
schema: v0
api_version: y2026_1
identity:
  id: test-bot
  namespace: dev
phoxal_artifacts:
  channel: experimental
phoxal_participants: {}
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
        .expect_err("invalid phoxal_artifacts channel should fail to parse");

        assert!(
            format!("{error:#}").contains("unknown variant `experimental`"),
            "got: {error:#}"
        );
    }

    #[test]
    fn phoxal_artifacts_parses_preview_target_and_generation_pin() -> anyhow::Result<()> {
        let robot = Robot::parse_from_string(
            r#"
schema: v0
identity:
  id: test-bot
  namespace: dev
phoxal_artifacts:
  channel: preview
  target: aarch64-unknown-linux-gnu
  generation: y2026_2
phoxal_participants: {}
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

        assert_eq!(robot.phoxal_artifacts.channel, Channel::Preview);
        assert_eq!(
            robot.phoxal_artifacts.target.as_deref(),
            Some("aarch64-unknown-linux-gnu")
        );
        assert_eq!(
            robot.phoxal_artifacts.generation.as_deref(),
            Some("y2026_2")
        );

        Ok(())
    }

    #[test]
    fn phoxal_artifacts_pins_path_entry_round_trips() -> anyhow::Result<()> {
        let robot = Robot::parse_from_string(&robot_yaml_with_phoxal_artifacts(
            r#"  pins:
    service-drive:
      path: ../framework/service/drive"#,
        ))?;

        assert_eq!(
            robot.phoxal_artifacts.pins.get("service-drive"),
            Some(&ArtifactPin::Path(ArtifactPathPin {
                path: PathBuf::from("../framework/service/drive"),
            }))
        );

        let yaml = serde_yaml::to_string(&crate::model::robot::Robot::V1(robot.clone()))?;
        assert!(
            yaml.contains("pins:\n    service-drive:\n      path: ../framework/service/drive"),
            "path pin should serialize in the unified pins map: {yaml}"
        );

        let reparsed = Robot::parse_from_string(&yaml)?;
        assert_eq!(reparsed.phoxal_artifacts.pins, robot.phoxal_artifacts.pins);

        Ok(())
    }

    #[test]
    fn phoxal_artifacts_pins_unknown_value_forms_are_errors() {
        for pin in [
            r#"  pins:
    service-drive: v0.8.4"#,
            r#"  pins:
    service-drive: "sha256:222222""#,
            r#"  pins:
    driver-ddsm115:
      git: https://github.com/you/ddsm115
      rev: 9f2c1e7"#,
            r#"  pins:
    tool-router:
      archive: router.tar.zst"#,
        ] {
            let error = Robot::parse_from_string(&robot_yaml_with_phoxal_artifacts(pin))
                .expect_err("unsupported pin form should fail to parse");

            assert!(
                format!("{error:#}").contains("data did not match any variant of untagged enum"),
                "got: {error:#}"
            );
        }
    }

    #[test]
    fn phoxal_artifacts_pins_rejects_unknown_fields_inside_path_pin() {
        let error = Robot::parse_from_string(&robot_yaml_with_phoxal_artifacts(
            r#"  pins:
    service-drive:
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
    fn phoxal_artifacts_empty_pins_are_absent_from_serialization() -> anyhow::Result<()> {
        let robot = Robot::parse_from_string(&robot_yaml_with_phoxal_artifacts(
            r#"  channel: preview
  pins: {}"#,
        ))?;

        let yaml = serde_yaml::to_string(&crate::model::robot::Robot::V1(robot))?;

        assert!(
            yaml.contains("phoxal_artifacts:\n  channel: preview"),
            "non-default artifacts section should serialize: {yaml}"
        );
        assert!(
            !yaml.contains("pins:"),
            "empty pins map should be omitted from serialization: {yaml}"
        );

        Ok(())
    }

    #[test]
    fn phoxal_artifacts_empty_pin_key_is_validation_error() -> anyhow::Result<()> {
        let robot = Robot::parse_from_string(&robot_yaml_with_phoxal_artifacts(
            r#"  pins:
    "":
      path: ../framework/service/drive"#,
        ))?;

        let errors = robot
            .validate()
            .expect_err("empty artifact pin key should fail validation");

        assert!(errors.contains(&super::ValidationError::EmptyArtifactPinKey));
        assert_eq!(
            super::ValidationError::EmptyArtifactPinKey.to_string(),
            "phoxal_artifacts.pins keys must not be empty"
        );

        Ok(())
    }

    #[test]
    fn phoxal_participants_rejects_old_version_field() {
        let error = Robot::parse_from_string(
            r#"
schema: v0
api_version: y2026_1
identity:
  id: test-bot
  namespace: dev
phoxal_participants:
  version: "latest"
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
        .expect_err("old phoxal_participants version field should fail to parse");

        assert!(
            format!("{error:#}").contains("unknown field `version`"),
            "got: {error:#}"
        );
    }

    #[test]
    fn phoxal_participants_images_parse_and_validate_against_platform_participants()
    -> anyhow::Result<()> {
        let robot = Robot::parse_from_string(
            r#"
schema: v0
api_version: y2026_1
identity:
  id: test-bot
  namespace: dev
phoxal_artifacts:
  channel: preview
phoxal_participants:
  images:
    drive: ghcr.io/phoxal/runtime-drive:y2026_1-v0.8.4
motion:
  kinematic:
    kind: omnidirectional
    actuators:
    - drive.motor
    encoders: []
components:
  sources: {}
  instances: {}
"#,
        )?;

        assert_eq!(robot.phoxal_artifacts.channel, Channel::Preview);
        assert_eq!(
            robot.phoxal_participants.images.get("drive"),
            Some(&"ghcr.io/phoxal/runtime-drive:y2026_1-v0.8.4".to_string())
        );
        robot
            .validate_with(&["drive"])
            .expect("known platform image key should validate");

        let errors = robot
            .validate_with(&["odometry"])
            .expect_err("unknown platform image key should fail validation");

        assert!(
            errors.contains(&super::ValidationError::UnknownPlatformParticipantImage {
                name: "drive".to_string(),
            })
        );
        assert_eq!(
            super::ValidationError::UnknownPlatformParticipantImage {
                name: "drive".to_string(),
            }
            .to_string(),
            "phoxal_participants.images.drive is not a platform participant"
        );

        Ok(())
    }

    #[test]
    fn robot_manifest_requires_schema_v0_and_allows_omitted_api_version() -> anyhow::Result<()> {
        let robot = Robot::parse_from_string(
            r#"
schema: v0
identity:
  id: test-bot
  namespace: dev
phoxal_participants: {}
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

        assert_eq!(robot.api_version, None);

        let yaml = serde_yaml::to_string(&crate::model::robot::Robot::V1(robot))?;
        assert!(
            yaml.starts_with("schema: v0\nidentity:\n"),
            "schema should be the first root key and api_version should be omitted by default: {yaml}"
        );

        let old_manifest_error = Robot::parse_from_string(
            r#"
version: v1
identity:
  id: test-bot
  namespace: dev
phoxal_participants: {}
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
        .expect_err("old version discriminator should no longer parse");

        assert!(
            format!("{old_manifest_error:#}").contains("schema"),
            "got: {old_manifest_error:#}"
        );

        Ok(())
    }

    #[test]
    fn present_blank_api_version_is_validation_error() -> anyhow::Result<()> {
        let robot = Robot::parse_from_string(
            r#"
schema: v0
api_version: " "
identity:
  id: test-bot
  namespace: dev
phoxal_participants: {}
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

        let errors = robot
            .validate()
            .expect_err("blank api_version should fail validation");

        assert!(errors.contains(&super::ValidationError::EmptyApiVersion));
        assert_eq!(
            super::ValidationError::EmptyApiVersion.to_string(),
            "api_version must not be empty"
        );

        Ok(())
    }
}
