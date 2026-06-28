//! `ParticipantLaunch` — the bundle → process launch contract (D53).
//!
//! The generated bundle carries one `ParticipantLaunch` record per participant;
//! the executable takes `--bundle <dir> --participant <id>` instead of a soup of
//! flags/env. The crucial field is `participant_id` (≠ the static artifact id):
//! repeated component-driver instances (e.g. ~13 ToF sensors from one driver
//! artifact) would otherwise collide on the bus (D47/D53).
//!
//! This is an internal launch input, not a public RobotPlan (D49). The first
//! slice synthesizes a default launch for `runtime run`; loading from a real
//! bundle file lands with the CLI slice.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Default bounded shutdown grace, in milliseconds.
pub const DEFAULT_SHUTDOWN_GRACE_MS: u64 = 2000;

/// One participant's launch record.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParticipantLaunch {
    /// The bus-unique participant id (never the static artifact/runtime id, D53).
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
    /// The runtime's typed config block (`user_runtimes.<id>.config`), if any.
    #[serde(default)]
    pub config: Option<serde_json::Value>,
    /// The bundle root that holds the robot model (`robot.yaml` + components +
    /// structure). Official runtimes read it via `SetupContext::robot()`; absent
    /// for a bare `runtime run` with no model.
    #[serde(default)]
    pub bundle_root: Option<PathBuf>,
    /// The `components.instances` entry this participant drives (D47/D53). A
    /// component driver is launched once per instance, each with a distinct
    /// `participant_id` and its own `component_instance`; read via
    /// `SetupContext::component()`. Absent for non-driver runtimes.
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
    /// A default launch for `runtime run`: in-process bus, real clock, the given
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

/// The clock source the runner uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockMode {
    /// Host-monotonic real time.
    #[default]
    Real,
    /// Subscribe the authoritative `simulation/clock` (Webots port).
    Simulation,
}
