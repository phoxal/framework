//! `ParticipantLaunch` - the bundle -> process launch contract (D53).
//!
//! The generated bundle carries one `ParticipantLaunch` record per participant;
//! the executable takes `--bundle <dir> --participant <id>` instead of a soup of
//! flags/env. The crucial field is `participant_id` (≠ the static artifact id):
//! repeated component-driver instances (e.g. ~13 ToF sensors from one driver
//! artifact) would otherwise collide on the bus (D47/D53).
//!
//! This is an internal launch input, not a public RobotPlan (D49). The first
//! slice synthesizes a default launch for local runs; loading from a real
//! bundle file lands with the CLI slice.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Default bounded shutdown grace, in milliseconds.
pub const DEFAULT_SHUTDOWN_GRACE_MS: u64 = 2000;

/// One participant's launch record.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParticipantLaunch {
    /// The bus-unique participant id (never the static participant/artifact id, D53).
    pub participant_id: String,
    /// The bus namespace (`identity.namespace`).
    pub namespace: String,
    /// The robot id (`identity.id`).
    pub robot_id: String,
    /// The resolved bus profile.
    #[serde(default)]
    pub bus: BusProfile,
    /// The clock mode.
    #[serde(default)]
    pub clock: ClockMode,
    /// The participant's typed config block (`user_runtimes.<id>.config`), if any.
    #[serde(default)]
    pub config: Option<serde_json::Value>,
    /// The bundle root that holds the robot model (`robot.yaml` + components +
    /// structure). Official participants read it via `SetupContext::robot()`; absent
    /// for a bare local run with no model.
    #[serde(default)]
    pub bundle_root: Option<PathBuf>,
    /// The `components.instances` entry this participant drives (D47/D53). A
    /// component driver is launched once per instance, each with a distinct
    /// `participant_id` and its own `component_instance`; read via
    /// `SetupContext::component()`. Absent for non-driver participants.
    #[serde(default)]
    pub component_instance: Option<String>,
    /// Bounded shutdown grace, in milliseconds.
    #[serde(default = "default_grace")]
    pub shutdown_grace_ms: u64,
}

fn default_grace() -> u64 {
    DEFAULT_SHUTDOWN_GRACE_MS
}

impl ParticipantLaunch {
    /// A default launch for local runs: in-process bus, real clock, the given
    /// participant id (defaulting the namespace to `dev`, D38).
    pub fn local(participant_id: impl Into<String>, robot_id: impl Into<String>) -> Self {
        ParticipantLaunch {
            participant_id: participant_id.into(),
            namespace: "dev".to_string(),
            robot_id: robot_id.into(),
            bus: BusProfile::default(),
            clock: ClockMode::Real,
            config: None,
            bundle_root: None,
            component_instance: None,
            shutdown_grace_ms: DEFAULT_SHUTDOWN_GRACE_MS,
        }
    }

    /// Build a launch from the environment, over [`local`](Self::local) defaults.
    ///
    /// This is how an external launcher (`phoxal-cli`, a systemd unit, a
    /// container entrypoint) hands a host-native participant its namespace, bus
    /// endpoints, bundle, typed config, and component instance without recompiling -
    /// each `PHOXAL_*` variable overrides the matching field; unset/empty leaves the
    /// `local` default:
    ///
    /// - `PHOXAL_PARTICIPANT_ID`, `PHOXAL_ROBOT_ID`, `PHOXAL_NAMESPACE`
    /// - `PHOXAL_BUNDLE_ROOT` (path to the resolved robot model)
    /// - `PHOXAL_COMPONENT_INSTANCE` (driver launch, D47/D53)
    /// - `PHOXAL_CONNECT` (comma-separated Zenoh endpoints; empty = in-process)
    /// - `PHOXAL_CONFIG` (the participant's typed config block, as JSON)
    /// - `PHOXAL_CLOCK` (`real` | `simulation`)
    pub fn from_env(default_participant_id: &str, default_robot_id: &str) -> crate::Result<Self> {
        use anyhow::Context;
        use std::env::var;

        let nonempty = |key: &str| var(key).ok().filter(|value| !value.is_empty());

        let mut launch = Self::local(
            nonempty("PHOXAL_PARTICIPANT_ID").unwrap_or_else(|| default_participant_id.to_string()),
            nonempty("PHOXAL_ROBOT_ID").unwrap_or_else(|| default_robot_id.to_string()),
        );
        if let Some(namespace) = nonempty("PHOXAL_NAMESPACE") {
            launch.namespace = namespace;
        }
        if let Some(root) = nonempty("PHOXAL_BUNDLE_ROOT") {
            launch.bundle_root = Some(PathBuf::from(root));
        }
        if let Some(instance) = nonempty("PHOXAL_COMPONENT_INSTANCE") {
            launch.component_instance = Some(instance);
        }
        if let Some(endpoints) = nonempty("PHOXAL_CONNECT") {
            launch.bus.connect_endpoints = endpoints
                .split(',')
                .map(|endpoint| endpoint.trim().to_string())
                .filter(|endpoint| !endpoint.is_empty())
                .collect();
        }
        if let Some(config) = nonempty("PHOXAL_CONFIG") {
            launch.config = Some(
                serde_json::from_str(&config)
                    .context("PHOXAL_CONFIG must be valid JSON for the participant config")?,
            );
        }
        if let Some(clock) = nonempty("PHOXAL_CLOCK") {
            launch.clock = match clock.as_str() {
                "real" => ClockMode::Real,
                "simulation" => ClockMode::Simulation,
                other => {
                    anyhow::bail!("PHOXAL_CLOCK must be 'real' or 'simulation', got '{other}'")
                }
            };
        }
        Ok(launch)
    }

    /// Set the bundle root (where the robot model lives) for this launch.
    pub fn with_bundle_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.bundle_root = Some(root.into());
        self
    }

    /// Set the component instance this participant drives (driver launch, D47/D53).
    pub fn with_component_instance(mut self, instance: impl Into<String>) -> Self {
        self.component_instance = Some(instance.into());
        self
    }
}

/// A resolved bus profile: where to connect. Empty endpoints = in-process.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BusProfile {
    /// Zenoh connect endpoints; empty means in-process (local sim / tests).
    #[serde(default)]
    pub connect_endpoints: Vec<String>,
}

/// The clock source the runner uses. The default runner currently supports
/// only [`ClockMode::Real`]; a launch requesting [`ClockMode::Simulation`]
/// is rejected until the Webots/simulation clock source lands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockMode {
    /// Host-monotonic real time.
    #[default]
    Real,
    /// Subscribe the authoritative `simulation/clock` (Webots port).
    Simulation,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    const KEYS: &[&str] = &[
        "PHOXAL_PARTICIPANT_ID",
        "PHOXAL_ROBOT_ID",
        "PHOXAL_NAMESPACE",
        "PHOXAL_BUNDLE_ROOT",
        "PHOXAL_COMPONENT_INSTANCE",
        "PHOXAL_CONNECT",
        "PHOXAL_CONFIG",
        "PHOXAL_CLOCK",
    ];

    fn clear_env() {
        // SAFETY: tests touching process env are `#[serial]`, so no other thread
        // reads/writes these vars concurrently.
        for key in KEYS {
            unsafe { std::env::remove_var(key) };
        }
    }

    #[test]
    #[serial]
    fn from_env_with_nothing_set_matches_local_defaults() {
        clear_env();
        let launch = ParticipantLaunch::from_env("my-participant", "robot").unwrap();
        assert_eq!(launch.participant_id, "my-participant");
        assert_eq!(launch.robot_id, "robot");
        assert_eq!(launch.namespace, "dev");
        assert_eq!(launch.bundle_root, None);
        assert_eq!(launch.config, None);
        assert!(launch.bus.connect_endpoints.is_empty());
        assert_eq!(launch.clock, ClockMode::Real);
    }

    #[test]
    #[serial]
    fn from_env_overrides_each_field() {
        clear_env();
        // SAFETY: serialized test; see clear_env.
        unsafe {
            std::env::set_var("PHOXAL_PARTICIPANT_ID", "tof-3");
            std::env::set_var("PHOXAL_NAMESPACE", "lab");
            std::env::set_var("PHOXAL_BUNDLE_ROOT", "/bundle");
            std::env::set_var("PHOXAL_COMPONENT_INSTANCE", "tof_front");
            std::env::set_var("PHOXAL_CONNECT", "tcp/127.0.0.1:7447, tcp/127.0.0.1:7448");
            std::env::set_var("PHOXAL_CONFIG", r#"{"rate_hz":10}"#);
            std::env::set_var("PHOXAL_CLOCK", "simulation");
        }
        let launch = ParticipantLaunch::from_env("default-id", "robot").unwrap();
        assert_eq!(launch.participant_id, "tof-3");
        assert_eq!(launch.namespace, "lab");
        assert_eq!(
            launch.bundle_root.as_deref(),
            Some(std::path::Path::new("/bundle"))
        );
        assert_eq!(launch.component_instance.as_deref(), Some("tof_front"));
        assert_eq!(
            launch.bus.connect_endpoints,
            vec![
                "tcp/127.0.0.1:7447".to_string(),
                "tcp/127.0.0.1:7448".to_string()
            ]
        );
        assert_eq!(launch.config, Some(serde_json::json!({"rate_hz": 10})));
        assert_eq!(launch.clock, ClockMode::Simulation);
        clear_env();
    }

    #[test]
    #[serial]
    fn from_env_rejects_invalid_config_json_and_clock() {
        clear_env();
        // SAFETY: serialized test; see clear_env.
        unsafe { std::env::set_var("PHOXAL_CONFIG", "not json") };
        assert!(ParticipantLaunch::from_env("id", "robot").is_err());
        unsafe {
            std::env::remove_var("PHOXAL_CONFIG");
            std::env::set_var("PHOXAL_CLOCK", "wallclock");
        }
        assert!(ParticipantLaunch::from_env("id", "robot").is_err());
        clear_env();
    }
}
