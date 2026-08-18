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

/// One operator's driving intent, expressed as a fraction of what this robot
/// was authored to do rather than in physical units.
///
/// Both fields are a fraction of the robot's authored maximum in `-1.0..=1.0`:
/// `linear` positive is forward, `angular` positive is counter-clockwise (left),
/// matching the body-twist convention `robot/drive` realizes. The robot scales
/// them by its own authored motion limits, so a client drives without knowing
/// the wheel base, the wheel radius, or any limit - physics stays where the
/// kinematics already live. A magnitude past 1 is full deflection, not a
/// protocol violation; a non-finite value names no deflection at all and stops
/// the robot.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub struct ManualCommand {
    pub linear: f64,
    pub angular: f64,
}

/// The sole motion execution decision. A stopped decision carries no source or
/// target, so consumers cannot observe an active source alongside a stop reason.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub enum Decision {
    Active {
        source: Source,
        target: crate::robot::drive::Target,
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
