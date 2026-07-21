//! Immutable descriptor for one published framework train.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Schema identity for per-release descriptors.
pub const SCHEMA: &str = "phoxal.suite/v0";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Suite {
    pub schema: String,
    /// Exact framework train version. This is intentionally the only version
    /// axis in the descriptor; API revisions are selected by that train.
    pub version: String,
    pub artifacts: Vec<Artifact>,
}

impl Suite {
    #[must_use]
    pub fn new(version: impl Into<String>, artifacts: Vec<Artifact>) -> Self {
        Self {
            schema: SCHEMA.to_string(),
            version: version.into(),
            artifacts,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub id: String,
    pub kind: Kind,
    pub targets: BTreeMap<String, Blob>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assets: Option<Blob>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Service,
    Component,
    Tool,
    Simulator,
    Infrastructure,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Blob {
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

#[must_use]
pub fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_has_one_train_version_and_no_api_axis() {
        let suite = Suite::new(
            "0.36.0",
            vec![Artifact {
                id: "phoxal/service-drive".into(),
                kind: Kind::Service,
                targets: BTreeMap::from([(
                    "aarch64-unknown-linux-gnu".into(),
                    Blob {
                        url: "https://example.invalid/drive.tar.zst".into(),
                        sha256: "a".repeat(64),
                        size: 42,
                    },
                )]),
                assets: None,
            }],
        );
        let json = serde_json::to_string(&suite).unwrap();
        assert!(json.contains("phoxal.suite/v0"));
        assert!(!json.contains("api_revision"));
        assert_eq!(serde_json::from_str::<Suite>(&json).unwrap(), suite);
    }
}
