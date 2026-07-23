//! Immutable descriptors for published framework trains.
//!
//! Published schema revisions remain available under their concrete modules.
//! The root re-exports preserve the original `phoxal.suite/v0` Rust surface for
//! existing readers; new profile-aware consumers use [`v1`] explicitly.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Original artifact-only descriptor schema.
pub mod v0 {
    use super::*;

    /// Schema identity for artifact-only per-release descriptors.
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
}

/// Profile-aware descriptor schema.
pub mod v1 {
    use super::*;

    pub use super::v0::{Artifact, Blob, Kind};

    /// Schema identity for profile-aware per-release descriptors.
    pub const SCHEMA: &str = "phoxal.suite/v1";

    #[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Suite {
        pub schema: String,
        /// Exact framework train version. Launch policy is part of this train,
        /// not another independently resolved version axis.
        pub version: String,
        pub profiles: SuiteProfiles,
        pub artifacts: Vec<Artifact>,
    }

    impl Suite {
        #[must_use]
        pub fn new(
            version: impl Into<String>,
            profiles: SuiteProfiles,
            artifacts: Vec<Artifact>,
        ) -> Self {
            Self {
                schema: SCHEMA.to_string(),
                version: version.into(),
                profiles,
                artifacts,
            }
        }
    }

    #[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct SuiteProfiles {
        pub native: Vec<ArtifactActivation>,
        pub webots: Vec<ArtifactActivation>,
    }

    #[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct ArtifactActivation {
        /// Provider-qualified package identity, such as `phoxal/tool-log`.
        pub package: String,
        pub scope: ActivationScope,
        pub criticality: ActivationCriticality,
    }

    #[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
    #[serde(rename_all = "snake_case")]
    pub enum ActivationScope {
        PerProject,
        PerRobot,
    }

    #[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
    #[serde(rename_all = "snake_case")]
    pub enum ActivationCriticality {
        Required,
        Optional,
    }
}

// Preserve the exact pre-v1 Rust surface as well as its serialized shape.
pub use v0::{Artifact, Blob, Kind, SCHEMA, Suite};

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

    fn artifact() -> v0::Artifact {
        v0::Artifact {
            id: "phoxal/service-drive".into(),
            kind: v0::Kind::Service,
            targets: BTreeMap::from([(
                "aarch64-unknown-linux-gnu".into(),
                v0::Blob {
                    url: "https://example.invalid/drive.tar.zst".into(),
                    sha256: "a".repeat(64),
                    size: 42,
                },
            )]),
            assets: None,
        }
    }

    #[test]
    fn v0_descriptor_keeps_the_original_artifact_only_shape() {
        let suite = v0::Suite::new("0.36.0", vec![artifact()]);
        let json = serde_json::to_string(&suite).unwrap();
        assert!(json.contains("phoxal.suite/v0"));
        assert!(!json.contains("profiles"));
        assert!(!json.contains("api_revision"));
        assert_eq!(serde_json::from_str::<v0::Suite>(&json).unwrap(), suite);

        let with_profiles = json.replacen(
            "\"artifacts\"",
            "\"profiles\":{\"native\":[],\"webots\":[]},\"artifacts\"",
            1,
        );
        assert!(serde_json::from_str::<v0::Suite>(&with_profiles).is_err());
    }

    #[test]
    fn v1_descriptor_round_trips_strict_launch_profiles() {
        let activation = v1::ArtifactActivation {
            package: "phoxal/tool-log".into(),
            scope: v1::ActivationScope::PerRobot,
            criticality: v1::ActivationCriticality::Optional,
        };
        let suite = v1::Suite::new(
            "0.38.0",
            v1::SuiteProfiles {
                native: vec![activation.clone()],
                webots: vec![activation],
            },
            vec![artifact()],
        );
        let json = serde_json::to_string(&suite).unwrap();
        assert!(json.contains("phoxal.suite/v1"));
        assert!(json.contains("\"scope\":\"per_robot\""));
        assert!(json.contains("\"criticality\":\"optional\""));
        assert_eq!(serde_json::from_str::<v1::Suite>(&json).unwrap(), suite);
    }

    #[test]
    fn v1_rejects_unknown_suite_profile_and_activation_fields() {
        let base = r#"{
            "schema":"phoxal.suite/v1",
            "version":"0.38.0",
            "profiles":{
                "native":[{
                    "package":"phoxal/tool-log",
                    "scope":"per_robot",
                    "criticality":"optional"
                }],
                "webots":[]
            },
            "artifacts":[]
        }"#;
        assert!(serde_json::from_str::<v1::Suite>(base).is_ok());
        assert!(
            serde_json::from_str::<v1::Suite>(
                &base.replace(r#""webots":[]"#, r#""webots":[],"future_profile":[]"#)
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<v1::Suite>(&base.replace(
                r#""criticality":"optional""#,
                r#""criticality":"optional","failure_policy":"stop_project""#
            ))
            .is_err()
        );
    }
}
