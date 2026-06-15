pub mod v1;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum Request {
    #[serde(rename = "1")]
    V1(v1::Request),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum Response {
    #[serde(rename = "1")]
    V1(v1::Response),
}
