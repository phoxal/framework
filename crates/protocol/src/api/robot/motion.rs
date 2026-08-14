#[derive(
    phoxal_macros::DescribeWire,
    Copy,
    Eq,
    Clone,
    Debug,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Manual,
    Navigation,
    EmergencyStop,
}

#[derive(
    phoxal_macros::DescribeWire,
    Copy,
    Eq,
    Clone,
    Debug,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
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

#[derive(
    phoxal_macros::DescribeWire,
    Copy,
    Eq,
    Clone,
    Debug,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SafetyRuntime {
    Absent,
    Present,
}

#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub struct ManualCommand {
    pub linear_x_mps: f64,
    pub angular_z_radps: f64,
}

/// The sole motion execution decision. A stopped decision carries no source or
/// target, so consumers cannot observe an active source alongside a stop reason.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub enum Decision {
    Active {
        source: Source,
        target: crate::api::robot::drive::Target,
    },
    Stopped {
        reason: ZeroReason,
    },
}

#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
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

phoxal_macros::protocol_fragment! {
    path robot / motion;

    manual: Setpoint<ManualCommand>;
    state: State<State>;
}
