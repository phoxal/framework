use crate::bus::zenoh::TypedSchema;
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

impl TypedSchema for Sample {
    const SCHEMA_NAME: &'static str = "component/capability/encoder";
    const SCHEMA_VERSION: u32 = 1;
}

pub const KIND: &str = "encoder";

crate::bus::topic_leaf! {
    pubsub(component_id: &str, capability_id: &str) {
        path: "component/{}/{}/profile/default",
        payload: Sample
    }
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh::TypedSchema;

    use super::Sample;

    #[test]
    fn schema_contract_does_not_drift() {
        assert_eq!(Sample::SCHEMA_NAME, "component/capability/encoder");
        assert_eq!(Sample::SCHEMA_VERSION, 1);
    }

    #[test]
    fn path_is_stable() {
        assert_eq!(
            super::path("left_wheel", "encoder"),
            "component/left_wheel/encoder/profile/default"
        );
    }
}
