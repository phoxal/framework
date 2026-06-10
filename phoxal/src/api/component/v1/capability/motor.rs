use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    Velocity(Velocity),
    Position(Position),
    Torque(Torque),
}

pub const KIND: &str = "motor";

pub type Velocity = f32;
pub type Position = f32;
pub type Torque = f32;
