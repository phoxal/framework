#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Command {
    Velocity(Velocity),
    Position(Position),
    Torque(Torque),
}

pub type Velocity = f32;
pub type Position = f32;
pub type Torque = f32;
