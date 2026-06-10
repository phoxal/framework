use super::target::Target;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct State {
    pub target: Target,
    pub limited_target: Target,
    pub actuator_authority: ActuatorAuthority,
    pub stop_reason: Option<StopReason>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActuatorAuthority {
    Active,
    Stopped,
    Degraded,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    CommandTimedOut,
    SafetyStop,
    EmergencyStop,
    NoTarget,
}
