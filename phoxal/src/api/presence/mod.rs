use crate::bus::zenoh::TypedSchema;
use serde::{Deserialize, Serialize};

pub const SCHEMA_NAME: &str = "phoxal-api-presence/v1";
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RuntimeId(pub String);

impl RuntimeId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl std::fmt::Display for RuntimeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Readiness {
    NotStarted,
    Initializing,
    Ready,
    Degraded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heartbeat {
    pub runtime_id: RuntimeId,
    pub readiness: Readiness,
}

impl TypedSchema for Heartbeat {
    const SCHEMA_NAME: &'static str = "runtime/presence/heartbeat";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeReadiness {
    pub runtime_id: RuntimeId,
    pub readiness: Readiness,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub autonomy_ready: bool,
    pub runtimes: Vec<RuntimeReadiness>,
}

impl TypedSchema for Summary {
    const SCHEMA_NAME: &'static str = "runtime/presence/summary";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DebugReadiness {
    pub runtimes: Vec<RuntimeReadiness>,
}

impl TypedSchema for DebugReadiness {
    const SCHEMA_NAME: &'static str = "runtime/presence/debug/readiness";
    const SCHEMA_VERSION: u32 = 1;
}

crate::bus::topic_leaf! {
    pubsub heartbeat {
        path: "runtime/presence/heartbeat",
        payload: Heartbeat
    }
}

crate::bus::topic_leaf! {
    pubsub summary {
        path: "runtime/presence/summary",
        payload: Summary
    }
}

pub mod debug {
    use super::*;

    crate::bus::topic_leaf! {
        pubsub readiness {
            path: "runtime/presence/debug/readiness",
            payload: DebugReadiness
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DebugReadiness, Heartbeat, SCHEMA_NAME, SCHEMA_VERSION, Summary};
    use crate::bus::zenoh::TypedSchema;

    #[test]
    fn schema_contracts_do_not_drift() {
        assert_eq!(Heartbeat::SCHEMA_NAME, "runtime/presence/heartbeat");
        assert_eq!(Heartbeat::SCHEMA_VERSION, 1);
        assert_eq!(Summary::SCHEMA_NAME, "runtime/presence/summary");
        assert_eq!(Summary::SCHEMA_VERSION, 1);
        assert_eq!(
            DebugReadiness::SCHEMA_NAME,
            "runtime/presence/debug/readiness"
        );
        assert_eq!(DebugReadiness::SCHEMA_VERSION, 1);
    }

    #[test]
    fn api_contract_version_is_stable() {
        assert_eq!(SCHEMA_NAME, "phoxal-api-presence/v1");
        assert_eq!(SCHEMA_VERSION, 1);
    }

    #[test]
    fn topic_paths_are_stable() {
        assert_eq!(super::heartbeat::path(), "runtime/presence/heartbeat");
        assert_eq!(super::summary::path(), "runtime/presence/summary");
        assert_eq!(
            super::debug::readiness::path(),
            "runtime/presence/debug/readiness"
        );
    }
}
