use derive_new::new;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, new)]
pub enum Command {
    On,
    Off,
}

pub const KIND: &str = "led";
