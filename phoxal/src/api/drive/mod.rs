pub mod v1;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum Target {
    #[serde(rename = "1")]
    V1(v1::Target),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum State {
    #[serde(rename = "1")]
    V1(v1::State),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum ActuatorCommands {
    #[serde(rename = "1")]
    V1(v1::ActuatorCommands),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum Saturation {
    #[serde(rename = "1")]
    V1(v1::Saturation),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum Watchdog {
    #[serde(rename = "1")]
    V1(v1::Watchdog),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum Kinematics {
    #[serde(rename = "1")]
    V1(v1::Kinematics),
}
