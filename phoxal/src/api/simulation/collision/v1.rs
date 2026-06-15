use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Collision {
    pub collided: bool,
    pub pairs: Vec<[String; 2]>,
}

pub const SCHEMA: &str = "simulation/robot/collision";
