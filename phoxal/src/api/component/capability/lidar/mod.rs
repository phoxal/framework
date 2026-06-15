pub mod v1;

use serde::{Deserialize, Serialize};

fn wire_eq<T: Serialize>(left: &T, right: &T) -> bool {
    match (
        rmp_serde::to_vec_named(left),
        rmp_serde::to_vec_named(right),
    ) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum Scan {
    #[serde(rename = "1")]
    V1(v1::Scan),
}

impl PartialEq for Scan {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::V1(left), Self::V1(right)) => wire_eq(left, right),
        }
    }
}
