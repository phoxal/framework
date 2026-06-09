use crate::bus::zenoh::TypedSchema;
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

impl TypedSchema for Frame {
    const SCHEMA_NAME: &'static str = "component/capability/microphone";
    const SCHEMA_VERSION: u32 = 1;
}

pub const KIND: &str = "microphone";

crate::bus::topic_leaf! {
    pubsub(component_id: &str, capability_id: &str) {
        path: "component/{}/{}/profile/default",
        payload: Frame
    }
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh::TypedSchema;

    use super::Frame;

    #[test]
    fn schema_contract_does_not_drift() {
        assert_eq!(Frame::SCHEMA_NAME, "component/capability/microphone");
        assert_eq!(Frame::SCHEMA_VERSION, 1);
    }

    #[test]
    fn path_is_stable() {
        assert_eq!(
            super::path("mic_array", "audio"),
            "component/mic_array/audio/profile/default"
        );
    }
}
