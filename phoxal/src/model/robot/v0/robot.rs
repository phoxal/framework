use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use super::{Component, KinematicConfig, Role, capability};

const ROBOT_FILE: &str = "robot.yaml";

/// Top-level `robot:` section - the robot model, and exactly what
/// `ctx.robot()` exposes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Robot {
    pub robot: RobotSection,
    /// Framework-resolved packages: channel, generation ceiling, and the
    /// unified provider-qualified pins map. Optional; a day-0 robot may omit
    /// this section entirely.
    #[serde(default, skip_serializing_if = "Artifacts::is_default")]
    pub artifacts: Artifacts,
    /// User services: project-local source crates built by `phoxal-cli` for
    /// run/sim/deploy. Official services are never declared here.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub services: BTreeMap<String, UserService>,
    /// Bus wiring - listen endpoints and the optional uplink. Never robot
    /// model; participants never parse bus facts from `robot.yaml`.
    #[serde(default, skip_serializing_if = "Bus::is_empty")]
    pub bus: Bus,
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
    /// Component instance map: instance-id -> instance (was
    /// `components.instances`; `components.sources` no longer exists -
    /// official components resolve from the catalog, forks are
    /// `artifacts.pins` entries).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub components: BTreeMap<String, Component>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
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

/// `artifacts:` (was `phoxal_artifacts`) - framework-resolved packages.
///
/// The whole section is optional; a day-0 robot can omit it entirely.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Artifacts {
    /// Release channel used when resolving official framework packages.
    #[serde(default)]
    pub channel: Channel,
    /// Deprecated API version ceiling or pin for official package
    /// resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<String>,
    /// Unified fail-closed pin map keyed by provider-qualified package id
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
        self.channel == Channel::Stable && self.generation.is_none() && self.pins.is_empty()
    }
}

/// One `artifacts.pins` entry. Accepts a version string, a git reference, a
/// `sha256:...` checksum string, or a local path override.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum ArtifactPin {
    /// Resolve this package from a local source checkout (dev-overlay-only).
    Path(ArtifactPathPin),
    /// Resolve this package from a git repository at a pinned revision.
    Git(ArtifactGitPin),
    /// Pin to an exact `sha256:...` checksum.
    Sha256(Sha256Pin),
    /// Pin to a `vX.Y.Z` / semver version string.
    Version(VersionPin),
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

/// A `sha256:...` checksum pin, validated at parse time to carry the
/// `sha256:` prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sha256Pin(pub String);

impl Serialize for Sha256Pin {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Sha256Pin {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value.starts_with("sha256:") && value.len() > "sha256:".len() {
            Ok(Self(value))
        } else {
            Err(serde::de::Error::custom(
                "sha256 pin must be a string of the form 'sha256:<digest>'",
            ))
        }
    }
}

/// Test-only: `Sha256Pin` hand-rolls `Serialize`/`Deserialize` (above) rather
/// than deriving them, so `#[derive(schemars::JsonSchema)]` cannot see it;
/// this manual impl keeps the generated `robot.schema.json` matching what the
/// hand-rolled `Deserialize` actually accepts (see `docs/GETTING_STARTED.md`
/// and the drift-guard test in `robot.rs`).
#[cfg(test)]
impl schemars::JsonSchema for Sha256Pin {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Sha256Pin".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": "^sha256:.+$"
        })
    }
}

/// A `vX.Y.Z` / semver version pin, validated at parse time so it cannot
/// collide with a `sha256:...` string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionPin(pub String);

impl Serialize for VersionPin {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for VersionPin {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if is_version_pin_string(&value) {
            Ok(Self(value))
        } else {
            Err(serde::de::Error::custom(
                "version pin must be a 'vX.Y.Z' or semver-shaped string",
            ))
        }
    }
}

/// Test-only: see the parallel `Sha256Pin` impl above for why this is manual.
#[cfg(test)]
impl schemars::JsonSchema for VersionPin {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "VersionPin".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": "^v?[0-9]+\\.[0-9]+\\.[0-9]+([-+].+)?$"
        })
    }
}

/// Accepts `v1.2.3` and bare semver `1.2.3` (with optional pre-release/build
/// metadata), rejecting the `sha256:` prefix so the untagged enum can
/// distinguish the two string forms unambiguously.
fn is_version_pin_string(value: &str) -> bool {
    if value.starts_with("sha256:") {
        return false;
    }
    let rest = value.strip_prefix('v').unwrap_or(value);
    let core = rest.split(['-', '+']).next().unwrap_or(rest);
    let mut segments = core.split('.');
    let Some(major) = segments.next() else {
        return false;
    };
    let Some(minor) = segments.next() else {
        return false;
    };
    let Some(patch) = segments.next() else {
        return false;
    };
    if segments.next().is_some() {
        return false;
    }
    [major, minor, patch]
        .into_iter()
        .all(|segment| !segment.is_empty() && segment.chars().all(|c| c.is_ascii_digit()))
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
#[cfg_attr(test, derive(schemars::JsonSchema))]
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
#[cfg_attr(test, derive(schemars::JsonSchema))]
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
#[cfg_attr(test, derive(schemars::JsonSchema))]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    EmptyRobotId,
    EmptyRobotNamespace,
    EmptyUserServiceConfig {
        service: String,
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
        self.validate_bus(&mut errors);
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

    use super::{
        ArtifactGitPin, ArtifactPathPin, ArtifactPin, Channel, Robot, Sha256Pin, VersionPin,
    };

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
artifacts:
  channel: stable
  generation: v2
  pins:
    phoxal/service-drive: v0.8.4
services:
  avoid-obstacles:
    path: ./services/avoid-obstacles
    config: { max_linear_speed_mps: 0.6 }
bus:
  uplink: { connect: "tls/root.example.io:7447" }
  listen: ["serial//dev/ttyACM0#baudrate=115200"]
"#;
        let robot = Robot::parse_from_string(yaml)?;

        assert_eq!(robot.robot.id, "rover");
        assert_eq!(robot.robot.namespace, "dev");
        assert_eq!(robot.robot.structure, PathBuf::from("structure.urdf"));
        assert_eq!(robot.artifacts.channel, Channel::Stable);
        assert_eq!(robot.artifacts.generation.as_deref(), Some("v2"));
        assert_eq!(
            robot.artifacts.pins.get("phoxal/service-drive"),
            Some(&ArtifactPin::Version(VersionPin("v0.8.4".to_string())))
        );
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
            robot
                .bus
                .uplink
                .as_ref()
                .map(|uplink| uplink.connect.as_str()),
            Some("tls/root.example.io:7447")
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
        assert!(robot.bus.is_empty());

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
    fn bus_listen_and_uplink_parse_and_validate() -> anyhow::Result<()> {
        let robot = Robot::parse_from_string(&minimal_manifest(
            r#"bus:
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
        ))?;

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
        let robot = Robot::parse_from_string(&minimal_manifest(
            r#"bus:
  listen:
  - tcp/0.0.0.0:7447
"#,
        ))?;

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
    fn robot_section_rejects_identity_wrapper() {
        let error = Robot::parse_from_string(
            r#"
schema: robot/v0
robot:
  identity:
    id: test-bot
    namespace: dev
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
    fn artifacts_pins_version_form_parses() -> anyhow::Result<()> {
        let robot = Robot::parse_from_string(&manifest_with_artifacts(
            r#"  pins:
    phoxal/service-drive: v0.8.4"#,
        ))?;

        assert_eq!(
            robot.artifacts.pins.get("phoxal/service-drive"),
            Some(&ArtifactPin::Version(VersionPin("v0.8.4".to_string())))
        );

        Ok(())
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
    fn artifacts_pins_sha256_form_parses() -> anyhow::Result<()> {
        let robot = Robot::parse_from_string(&manifest_with_artifacts(
            r#"  pins:
    phoxal/tool-router: "sha256:2222222222222222222222222222222222222222222222222222222222222222""#,
        ))?;

        assert_eq!(
            robot.artifacts.pins.get("phoxal/tool-router"),
            Some(&ArtifactPin::Sha256(Sha256Pin(
                "sha256:2222222222222222222222222222222222222222222222222222222222222222"
                    .to_string()
            )))
        );

        Ok(())
    }

    #[test]
    fn artifacts_pins_string_forms_disambiguate_and_round_trip() -> anyhow::Result<()> {
        let robot = Robot::parse_from_string(&manifest_with_artifacts(
            r#"  pins:
    phoxal/service-drive: v0.8.4
    phoxal/service-frame: 0.8.4
    phoxal/tool-router: "sha256:2222222222222222222222222222222222222222222222222222222222222222""#,
        ))?;

        assert_eq!(
            robot.artifacts.pins.get("phoxal/service-drive"),
            Some(&ArtifactPin::Version(VersionPin("v0.8.4".to_string())))
        );
        assert_eq!(
            robot.artifacts.pins.get("phoxal/service-frame"),
            Some(&ArtifactPin::Version(VersionPin("0.8.4".to_string())))
        );
        assert_eq!(
            robot.artifacts.pins.get("phoxal/tool-router"),
            Some(&ArtifactPin::Sha256(Sha256Pin(
                "sha256:2222222222222222222222222222222222222222222222222222222222222222"
                    .to_string()
            )))
        );

        let yaml = serde_yaml::to_string(&crate::model::robot::Robot::V0(robot.clone()))?;
        let reparsed = Robot::parse_from_string(&yaml)?;
        assert_eq!(reparsed.artifacts.pins, robot.artifacts.pins);

        Ok(())
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
    service-drive: v0.8.4"#,
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
                "  pins:\n    \"{bad_key}\": v1.0.0"
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
    phoxal/tool-router:
      archive: router.tar.zst"#,
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
    fn artifacts_empty_pins_are_absent_from_serialization() -> anyhow::Result<()> {
        let robot = Robot::parse_from_string(&manifest_with_artifacts(
            r#"  channel: preview
  pins: {}"#,
        ))?;

        let yaml = serde_yaml::to_string(&crate::model::robot::Robot::V0(robot))?;

        assert!(
            yaml.contains("artifacts:\n  channel: preview"),
            "non-default artifacts section should serialize: {yaml}"
        );
        assert!(
            !yaml.contains("pins:"),
            "empty pins map should be omitted from serialization: {yaml}"
        );

        Ok(())
    }

    #[test]
    fn artifacts_section_entirely_omitted_by_default() -> anyhow::Result<()> {
        let robot = Robot::parse_from_string(&minimal_manifest(""))?;

        assert_eq!(robot.artifacts.channel, Channel::Stable);
        assert_eq!(robot.artifacts.generation, None);
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

        assert!(
            format!("{error:#}").contains("unknown variant `experimental`"),
            "got: {error:#}"
        );
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
    fn artifacts_parses_preview_channel_and_generation_pin() -> anyhow::Result<()> {
        let robot = Robot::parse_from_string(&manifest_with_artifacts(
            r#"  channel: preview
  generation: v2"#,
        ))?;

        assert_eq!(robot.artifacts.channel, Channel::Preview);
        assert_eq!(robot.artifacts.generation.as_deref(), Some("v2"));

        Ok(())
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
