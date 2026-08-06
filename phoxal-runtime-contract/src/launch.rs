use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{ExecutionId, ExecutionOrigin};

/// The launch ABI a participant binary declares: this launch record plus the
/// environment-variable encoding below.
pub const LAUNCH_ABI: &str = "phoxal/participant-launch/v0";

/// Default bounded shutdown grace, in milliseconds.
pub const DEFAULT_SHUTDOWN_GRACE_MS: u64 = 2000;

/// Participant launch environment variable names.
pub mod env {
    pub const PARTICIPANT_ID: &str = "PHOXAL_PARTICIPANT_ID";
    pub const EXECUTION_ID: &str = "PHOXAL_EXECUTION_ID";
    pub const EXECUTION_ORIGIN: &str = "PHOXAL_EXECUTION_ORIGIN";
    pub const ROBOT_ID: &str = "PHOXAL_ROBOT_ID";
    pub const BUNDLE_ROOT: &str = "PHOXAL_BUNDLE_ROOT";
    pub const COMPONENT_INSTANCE: &str = "PHOXAL_COMPONENT_INSTANCE";
    pub const CONNECT: &str = "PHOXAL_CONNECT";
    pub const CONFIG: &str = "PHOXAL_CONFIG";
    pub const CLOCK: &str = "PHOXAL_CLOCK";

    pub const ALL: &[&str] = &[
        PARTICIPANT_ID,
        EXECUTION_ID,
        EXECUTION_ORIGIN,
        ROBOT_ID,
        BUNDLE_ROOT,
        COMPONENT_INSTANCE,
        CONNECT,
        CONFIG,
        CLOCK,
    ];
}

/// One participant's process launch record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParticipantLaunch {
    pub participant_id: String,
    pub execution: ExecutionId,
    #[serde(default, with = "origin_serde")]
    pub execution_origin: Option<ExecutionOrigin>,
    pub robot_id: String,
    #[serde(default)]
    pub bus: BusProfile,
    #[serde(default)]
    pub clock: ClockMode,
    #[serde(default)]
    pub config: Option<serde_json::Value>,
    #[serde(default)]
    pub bundle_root: Option<PathBuf>,
    #[serde(default)]
    pub component_instance: Option<String>,
    #[serde(default = "default_grace")]
    pub shutdown_grace_ms: u64,
}

const fn default_grace() -> u64 {
    DEFAULT_SHUTDOWN_GRACE_MS
}

impl ParticipantLaunch {
    pub fn local(participant_id: impl Into<String>, robot_id: impl Into<String>) -> Self {
        Self {
            participant_id: participant_id.into(),
            execution: ExecutionId::mint(),
            execution_origin: None,
            robot_id: robot_id.into(),
            bus: BusProfile::default(),
            clock: ClockMode::Real,
            config: None,
            bundle_root: None,
            component_instance: None,
            shutdown_grace_ms: DEFAULT_SHUTDOWN_GRACE_MS,
        }
    }

    pub fn in_execution(mut self, execution: ExecutionId) -> Self {
        self.execution = execution;
        self
    }

    pub fn with_execution_origin(mut self, origin: ExecutionOrigin) -> Self {
        self.execution_origin = Some(origin);
        self
    }

    /// Encode the complete shared environment ABI.
    pub fn encode_env(&self) -> Result<Vec<(&'static str, String)>, LaunchError> {
        let mut values = vec![
            (env::PARTICIPANT_ID, self.participant_id.clone()),
            (env::EXECUTION_ID, self.execution.to_string()),
            (env::ROBOT_ID, self.robot_id.clone()),
            (env::CONNECT, self.bus.connect_endpoints.join(",")),
            (env::CLOCK, self.clock.to_string()),
        ];
        if let Some(origin) = self.execution_origin {
            values.push((env::EXECUTION_ORIGIN, origin.encode()));
        }
        if let Some(root) = &self.bundle_root {
            values.push((env::BUNDLE_ROOT, root.to_string_lossy().into_owned()));
        }
        if let Some(instance) = &self.component_instance {
            values.push((env::COMPONENT_INSTANCE, instance.clone()));
        }
        if let Some(config) = &self.config {
            values.push((env::CONFIG, serde_json::to_string(config)?));
        }
        Ok(values)
    }

    /// Decode values obtained from CLI flags or environment variables.
    pub fn decode(values: LaunchEnv) -> Result<Self, LaunchError> {
        let mut launch = Self::local(values.participant_id, values.robot_id);
        if let Some(value) = nonempty(values.execution_id) {
            launch.execution = ExecutionId::parse(&value)
                .map_err(|source| LaunchError::ExecutionId { value, source })?;
        }
        if let Some(value) = nonempty(values.execution_origin) {
            launch.execution_origin = Some(
                ExecutionOrigin::decode(&value)
                    .ok_or_else(|| LaunchError::ExecutionOrigin(value.clone()))?,
            );
        }
        launch.bus.connect_endpoints = nonempty(values.connect)
            .map(|endpoints| {
                endpoints
                    .split(',')
                    .map(str::trim)
                    .filter(|endpoint| !endpoint.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        launch.config = match nonempty(values.config) {
            Some(value) => Some(
                serde_json::from_str(&value)
                    .map_err(|source| LaunchError::Config { value, source })?,
            ),
            None => None,
        };
        launch.bundle_root = values
            .bundle_root
            .filter(|path| !path.as_os_str().is_empty());
        launch.component_instance = nonempty(values.component_instance);
        launch.clock = values.clock;
        Ok(launch)
    }
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.is_empty())
}

/// Raw values supplied by a runtime-specific CLI parser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchEnv {
    pub participant_id: String,
    pub execution_id: Option<String>,
    pub execution_origin: Option<String>,
    pub robot_id: String,
    pub bundle_root: Option<PathBuf>,
    pub component_instance: Option<String>,
    pub connect: Option<String>,
    pub config: Option<String>,
    pub clock: ClockMode,
}

/// A malformed process-boundary launch record.
#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error("PHOXAL_EXECUTION_ID is invalid ('{value}'): {source}")]
    ExecutionId {
        value: String,
        source: crate::InvalidIdentity,
    },
    #[error("PHOXAL_EXECUTION_ORIGIN is malformed: '{0}'")]
    ExecutionOrigin(String),
    #[error("PHOXAL_CONFIG is not valid JSON: {source}")]
    Config {
        value: String,
        source: serde_json::Error,
    },
    #[error("could not encode launch JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// A resolved bus profile.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BusProfile {
    #[serde(default)]
    pub connect_endpoints: Vec<String>,
}

/// The participant scheduler's clock mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockMode {
    #[default]
    Real,
    Simulation,
    Clockless,
}

impl std::fmt::Display for ClockMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Real => "real",
            Self::Simulation => "simulation",
            Self::Clockless => "clockless",
        })
    }
}

mod origin_serde {
    use super::ExecutionOrigin;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(
        value: &Option<ExecutionOrigin>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value.map(ExecutionOrigin::encode).serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<ExecutionOrigin>, D::Error> {
        let Some(rendered) = Option::<String>::deserialize(deserializer)? else {
            return Ok(None);
        };
        ExecutionOrigin::decode(&rendered)
            .map(Some)
            .ok_or_else(|| serde::de::Error::custom("malformed execution origin"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_env_round_trip_uses_bundle_root_only() {
        let launch = ParticipantLaunch {
            bundle_root: Some(PathBuf::from("/bundle")),
            execution_origin: Some(ExecutionOrigin::mint()),
            ..ParticipantLaunch::local("drive", "robot")
        };
        let encoded = launch.encode_env().unwrap();
        assert!(
            encoded
                .iter()
                .any(|(key, value)| { *key == env::BUNDLE_ROOT && value == "/bundle" })
        );
        assert!(!encoded.iter().any(|(key, _)| *key == "PHOXAL_ROBOT_ROOT"));
    }
}
