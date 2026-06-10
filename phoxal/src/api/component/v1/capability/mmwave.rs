use derive_new::new;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, new)]
pub struct Scan {
    #[new(into)]
    detections: Vec<Detection>,
}

impl Scan {
    pub fn detections(&self) -> &[Detection] {
        &self.detections
    }
}

pub const KIND: &str = "mmwave";

#[derive(Debug, Clone, Serialize, Deserialize, new)]
pub struct Detection {
    pub position: [f32; 3],
    pub velocity: [f32; 3],
    pub snr: f32,
}
