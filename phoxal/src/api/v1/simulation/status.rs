use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Status {
    pub epoch: u64,
    pub step: u64,
    pub time_ns: u64,
    pub dt_ns: u64,
}
