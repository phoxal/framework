use derive_new::new;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, new)]
pub struct Frame {
    #[new(into)]
    data: Vec<u8>,
}

impl Frame {
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

pub const KIND: &str = "microphone";
