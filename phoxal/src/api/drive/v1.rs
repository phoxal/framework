use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Target {
    pub linear_x_mps: f64,
    pub angular_z_radps: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub target: Target,
    pub limited_target: Target,
    pub actuator_authority: ActuatorAuthority,
    pub stop_reason: Option<StopReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActuatorAuthority {
    Active,
    Stopped,
    Degraded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    CommandTimedOut,
    SafetyStop,
    EmergencyStop,
    NoTarget,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActuatorCommands {
    pub commands: Vec<ActuatorCommand>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActuatorCommand {
    pub component_id: String,
    pub capability_id: String,
    pub value: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Saturation {
    pub requested: Target,
    pub limited: Target,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Watchdog {
    pub target_fresh: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Kinematics {
    pub profile: String,
    pub actuator_commands: Vec<ActuatorCommand>,
}
