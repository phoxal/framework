use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pose {
    pub frame_id: String,
    pub translation_m: [f64; 3],
    pub rotation_xyzw: [f64; 4],
}

pub const SCHEMA: &str = "simulation/robot/pose";
