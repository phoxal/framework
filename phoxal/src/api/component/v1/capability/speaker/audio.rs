use crate::bus::zenoh::TypedSchema;
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

impl TypedSchema for Audio {
    const SCHEMA_NAME: &'static str = "component/capability/speaker/audio";
    const SCHEMA_VERSION: u32 = 1;
}

pub const KIND: &str = "speaker/audio";

crate::bus::topic_leaf! {
    pubsub(component_id: &str, capability_id: &str) {
        path: "component/{}/speaker/audio/{}/data",
        payload: Audio
    }
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh::TypedSchema;

    use super::Audio;

    #[test]
    fn schema_contract_does_not_drift() {
        assert_eq!(Audio::SCHEMA_NAME, "component/capability/speaker/audio");
        assert_eq!(Audio::SCHEMA_VERSION, 1);
    }

    #[test]
    fn path_is_stable() {
        assert_eq!(
            super::path("speaker", "left"),
            "component/speaker/speaker/audio/left/data"
        );
    }
}
