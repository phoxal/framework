pub mod v1;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum JointState {
    #[serde(rename = "1")]
    V1(v1::JointState),
}
