use derive_new::new;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, new)]
pub struct Sample {
    magnetic_field: [f32; 3],
}

impl Sample {
    pub const fn magnetic_field(&self) -> &[f32; 3] {
        &self.magnetic_field
    }
}

pub const KIND: &str = "magnetometer";
