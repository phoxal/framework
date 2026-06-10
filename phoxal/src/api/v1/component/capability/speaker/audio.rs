use derive_new::new;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, new)]
pub struct Audio {
    #[serde(with = "serde_bytes")]
    #[new(into)]
    data: Vec<u8>,
}

impl Audio {
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

pub const KIND: &str = "speaker/audio";
