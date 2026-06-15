use derive_new::new;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, new)]
pub struct Sample {
    ticks: i64,
}

impl Sample {
    pub const fn ticks(&self) -> i64 {
        self.ticks
    }
}

pub const KIND: &str = "encoder";
