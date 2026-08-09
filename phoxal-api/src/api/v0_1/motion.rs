#[derive(Copy, Eq, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Manual,
    Navigation,
    EmergencyStop,
}

#[derive(Copy, Eq, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ZeroReason {
    NoCandidate,
    NavigationCandidateStale,
    ManualCandidateNotFinite,
    NavigationCandidateNotFinite,
    EmergencyStopEngaged,
    SafetyConstraintsUnavailable,
    SafetyProtectiveStop,
}

#[derive(Copy, Eq, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetyRuntime {
    Absent,
    Present,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ManualCommand {
    pub linear_x_mps: f64,
    pub angular_z_radps: f64,
}

/// The sole motion execution decision. A stopped decision carries no source or
/// target, so consumers cannot observe an active source alongside a stop reason.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Decision {
    Active {
        source: Source,
        target: crate::api::v0_1::drive::Target,
    },
    Stopped {
        reason: ZeroReason,
    },
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct State {
    pub decision: Decision,
    /// How long ago motion observed the live manual command, on
    /// its own host clock. `None` when no manual command is live.
    pub manual_observed_age_ns: Option<u64>,
    pub autonomous_candidate_age_ns: Option<u64>,
    pub safety_constraints_age_ns: Option<u64>,
    pub safety_runtime: SafetyRuntime,
    pub component_estop_blocked: bool,
    pub active_safety_constraints: Vec<super::safety::Constraint>,
    pub safety_permission: super::safety::MotionPermission,
}

phoxal_macros::phoxal_api_fragment! {
    path motion;

    version v0_1;

    command manual: Setpoint<ManualCommand>;
    topic state: State<State>;
}
