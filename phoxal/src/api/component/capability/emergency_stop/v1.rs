use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub engaged: bool,
}

pub const KIND: &str = "emergency_stop";
