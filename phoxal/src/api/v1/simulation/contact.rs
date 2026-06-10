use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Contact {
    pub touching: bool,
    pub links: Vec<String>,
}

pub const SCHEMA: &str = "simulation/robot/contact";
