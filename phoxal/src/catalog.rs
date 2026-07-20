//! The shared `phoxal.catalog/v0` wire schema.
//!
//! This is a **full location/integrity index**: `artifacts[]` holds one entry
//! per published `(package, version)`, accumulating versions forever. A
//! build-snapshot CI run upserts every `(package, version)` it rebuilt into the
//! previous catalog (`xtask/src/catalog/generate.rs`'s merge), so unchanged
//! versions retain their prior immutable locations while older versions remain
//! indexed. There is no
//! `revision`/checksum machinery (that guarded the old mutable-manifest model,
//! which this replaces), and no five-per-kind-array split - one `artifacts`
//! array, one shared [`Blob`] download descriptor. Contract and config-schema
//! metadata stay in participant binaries' embedded metadata sections; the
//! catalog does not duplicate them.
//!
//! `heads` is the one exception to "full index, nothing computed": the
//! coherence-gate design doc §4 whole-set snapshot pointers. The producer
//! computes them from the full packaged participant set, or extends a previous
//! coherent head after the PR gate checks the full set and the changed binaries
//! are cross-target validated - see [`Heads`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The catalog's schema identity - a bare, unversioned sanity guard against
/// pointing a reader at an unrelated JSON file.
pub const SCHEMA: &str = "phoxal.catalog/v0";

/// Canonical statically launched platform-service inventory. The CLI consumes
/// this constant directly; xtask verifies it against workspace artifacts.
pub const OFFICIAL_SERVICES: &[(&str, &str)] = &[
    ("asset", "phoxal/service-asset"),
    ("behavior", "phoxal/service-behavior"),
    ("drive", "phoxal/service-drive"),
    ("frame", "phoxal/service-frame"),
    ("joint", "phoxal/service-joint"),
    ("localize", "phoxal/service-localize"),
    ("map", "phoxal/service-map"),
    ("motion", "phoxal/service-motion"),
    ("navigation", "phoxal/service-navigation"),
    ("odometry", "phoxal/service-odometry"),
    ("perception", "phoxal/service-perception"),
    ("power", "phoxal/service-power"),
    ("safety", "phoxal/service-safety"),
    ("video", "phoxal/service-video"),
];

/// The full artifact index for one `build-*` release stream: the previous
/// catalog's complete `artifacts[]` with this run's rebuilt `(package, version)`
/// pairs upserted.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub schema: String,
    /// Provenance for the CI run that produced this exact catalog file - not
    /// per-entry: an individual [`Artifact`] may have been carried forward
    /// from an older run (see the module docs).
    pub build: BuildProvenance,
    pub artifacts: Vec<Artifact>,
    /// The coherence-gate design doc §4 whole-set snapshot pointers,
    /// recomputed by every `catalog generate` run - see [`Heads`].
    pub heads: Heads,
}

impl Catalog {
    pub fn new(build: BuildProvenance, artifacts: Vec<Artifact>, heads: Heads) -> Self {
        Self {
            schema: SCHEMA.to_string(),
            build,
            artifacts,
            heads,
        }
    }
}

/// The coherence-gate design doc §4 whole-set snapshot pointers - **whole-set
/// snapshot pointers, not per-binary versions** (design doc §4: a per-binary
/// draft was rejected because holding back one binary can strand its
/// counterparts transitively, and a snapshot pointer is coherent by
/// construction with no fault attribution needed).
///
/// Both fields are a `build-*` release tag (e.g. `"build-20260710-0000123"`,
/// [`BuildProvenance::tag`]), never a per-package version.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Heads {
    /// The newest catalog whose full latest-version set passed the coherence
    /// gate. A full rebuild checks packaged metadata directly; an artifact-only
    /// build extends an already coherent latest catalog after the release PR's
    /// full-set check and cross-target validation of changed binaries. Empty iff
    /// no coherent snapshot has ever been published.
    pub stable: String,
    /// The newest build snapshot, always - regardless of coherence. Empty
    /// only for a `--metadata-only` catalog (the PR gate, not a publish -
    /// there is no build snapshot to point at).
    pub nightly: String,
}

impl Heads {
    /// Both heads empty - the `--metadata-only` catalog shape (no build
    /// snapshot exists yet to point at).
    #[must_use]
    pub fn empty() -> Self {
        Self {
            stable: String::new(),
            nightly: String::new(),
        }
    }
}

/// Provenance for the CI run that produced a catalog file.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildProvenance {
    /// The `build-*` release tag this run's fresh artifacts were uploaded to
    /// (e.g. `"build-20260708-0001234"`).
    pub tag: String,
    /// GitHub's monotonic per-workflow run counter.
    pub run_number: u64,
    /// The workflow run id (a permalink to the run).
    pub run_id: u64,
    /// The commit sha the catalog was cut from.
    pub commit: String,
    /// RFC3339 timestamp of the run.
    pub created_at: String,
}

/// One `(package, version)` entry in the full index. `package` is the
/// provider-qualified crate identity. Components are one flattened
/// `phoxal/component-<id>` entry carrying both `targets` (binary) and
/// `assets`; no `kind` or `-driver`/`-assets` suffix is encoded by the shape.
///
/// Services, tools, simulators, and infrastructure carry `targets`; a
/// component carries `targets` **and** `assets`. Every published entry has at
/// least one of the two (a structural invariant `catalog check`/`catalog
/// verify` enforce, not the type itself). A metadata-only catalog legitimately
/// has neither.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub package: String,
    pub version: String,
    /// Target triple -> the built binary tarball for that triple. Empty only
    /// when the catalog was generated `--metadata-only` (no tarballs exist).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub targets: BTreeMap<String, Blob>,
    /// The component asset bundle, present iff the crate ships one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assets: Option<Blob>,
}

/// One download primitive: every `targets{}` value and `assets` is a `Blob`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Blob {
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

/// Whether `value` is a valid lowercase sha256 hex digest.
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
    fn round_trips_through_json() {
        let mut targets = BTreeMap::new();
        targets.insert(
            "aarch64-unknown-linux-musl".to_string(),
            Blob {
                url: "https://example.invalid/a.tar.zst".to_string(),
                sha256: "a".repeat(64),
                size: 42,
            },
        );
        let catalog = Catalog::new(
            BuildProvenance {
                tag: "build-20260708-0001234".to_string(),
                run_number: 1234,
                run_id: 987_654_321,
                commit: "deadbeef".to_string(),
                created_at: "2026-07-08T12:00:00Z".to_string(),
            },
            vec![Artifact {
                package: "phoxal/service-drive".to_string(),
                version: "0.19.8".to_string(),
                targets,
                assets: None,
            }],
            Heads {
                stable: "build-20260708-0001234".to_string(),
                nightly: "build-20260708-0001234".to_string(),
            },
        );

        let json = serde_json::to_string_pretty(&catalog).expect("serialize");
        assert!(!json.contains("\"assets\""));
        let reparsed: Catalog = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(catalog, reparsed);
    }

    #[test]
    fn sha256_format_check() {
        assert!(is_sha256(&"a".repeat(64)));
        assert!(!is_sha256(&"A".repeat(64)));
        assert!(!is_sha256("short"));
    }

    #[test]
    fn deny_unknown_fields_rejects_stray_keys() {
        let json = r#"{"schema":"phoxal.catalog/v0","build":{"tag":"t","run_number":1,"run_id":1,"commit":"c","created_at":"c"},"artifacts":[],"heads":{"stable":"","nightly":""},"bogus":true}"#;
        let err = serde_json::from_str::<Catalog>(json).unwrap_err();
        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn removed_artifact_metadata_fields_are_rejected() {
        let with_contracts =
            r#"{"package":"phoxal/service-drive","version":"0.1.0","contracts":[],"targets":{}}"#;
        let err = serde_json::from_str::<Artifact>(with_contracts).unwrap_err();
        assert!(err.to_string().contains("unknown field `contracts`"));

        let with_config_schema = r#"{"package":"phoxal/service-drive","version":"0.1.0","config_schema":null,"targets":{}}"#;
        let err = serde_json::from_str::<Artifact>(with_config_schema).unwrap_err();
        assert!(err.to_string().contains("unknown field `config_schema`"));
    }

    #[test]
    fn heads_round_trips_and_rejects_unknown_fields() {
        let heads = Heads {
            stable: "build-1".to_string(),
            nightly: "build-2".to_string(),
        };
        let json = serde_json::to_string(&heads).expect("serialize");
        let reparsed: Heads = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(heads, reparsed);

        let err =
            serde_json::from_str::<Heads>(r#"{"stable":"","nightly":"","extra":1}"#).unwrap_err();
        assert!(err.to_string().contains("unknown field"));
    }
}
