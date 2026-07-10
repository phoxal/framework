//! The `phoxal.catalog/v0` schema - xtask/CI-scoped only (design
//! `organization/tmp/ci-release-refactor/design.md` §3).
//!
//! This is a **full index**: `artifacts[]` holds one entry per published
//! `(package, version)`, accumulating forever. A build-snapshot CI run
//! appends the `(package, version)` pairs it built to whatever the previous
//! catalog already indexed (`xtask/src/catalog/generate.rs`'s merge); nothing
//! is ever replaced, refreshed, or dropped. There is no `revision`/checksum
//! machinery (that guarded the old mutable-manifest model, which this
//! replaces), no `heads`, and no five-per-kind-array split - one `artifacts`
//! array, one shared [`Blob`] download descriptor.
//!
//! This type is **not** the local `phoxal-artifacts.json` the CLI reads (out
//! of scope for the CI refactor - see the design doc §1); it lives in
//! `xtask`, not the `phoxal` library crate, because nothing outside the
//! release pipeline consumes it in this pass.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The catalog's schema identity - a bare, unversioned sanity guard against
/// pointing a reader at an unrelated JSON file.
pub const SCHEMA: &str = "phoxal.catalog/v0";

/// The full artifact index for one `build-*` release stream: the previous
/// catalog's complete `artifacts[]` plus whatever `(package, version)` pairs
/// this run appended.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub schema: String,
    /// Provenance for the CI run that produced this exact catalog file - not
    /// per-entry: an individual [`Artifact`] may have been carried forward
    /// from an older run (see the module docs).
    pub build: BuildProvenance,
    pub artifacts: Vec<Artifact>,
}

impl Catalog {
    pub fn new(build: BuildProvenance, artifacts: Vec<Artifact>) -> Self {
        Self {
            schema: SCHEMA.to_string(),
            build,
            artifacts,
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
/// provider-qualified crate identity - no `kind`, no `-driver`/`-assets`
/// suffix mandated by the shape itself (today's workspace still discovers
/// component drivers and component assets as separate packages - see the
/// generate slice's report - so each may appear as its own entry until the
/// component-as-one-crate migration, design doc §9, lands).
///
/// A service entry carries `targets` only; a driver-full component carries
/// `targets` **and** `assets`; a driver-less component carries `assets`
/// only. Every entry has at least one of the two (a structural invariant
/// `catalog check`/`catalog verify` enforce, not the type itself - a
/// metadata-only catalog legitimately has neither, see those modules).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub package: String,
    pub version: String,
    /// The compiled-in `#[derive(phoxal::Api)]` contract metadata, present
    /// iff the crate has a binary. Empty for asset-only entries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contracts: Vec<Contract>,
    /// The participant's `robot.yaml` config JSON Schema. Always omitted for
    /// now - the real `schemars` schema needs a host-side `build.rs` slice
    /// that is not built yet (design doc §8/§10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<serde_json::Value>,
    /// Target triple -> the built binary tarball for that triple. Empty iff
    /// the crate has no binary (an asset-only component) or the catalog was
    /// generated `--metadata-only` (no tarballs exist yet).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub targets: BTreeMap<String, Blob>,
    /// The target-independent component asset bundle, present iff the crate
    /// ships one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assets: Option<Blob>,
}

/// One graph contract a runtime binary uses: its identity as embedded by
/// `#[derive(phoxal::Api)]` (`name`, verbatim/opaque - carried through as
/// extracted, never canonicalized here) plus the role this artifact plays
/// for it (`"publish"`, `"subscribe"`, `"serve"`, or `"ask"`).
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    pub name: String,
    pub role: String,
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
                contracts: vec![Contract {
                    name: "y2026_1/drive/target".to_string(),
                    role: "publish".to_string(),
                }],
                config_schema: None,
                targets,
                assets: None,
            }],
        );

        let json = serde_json::to_string_pretty(&catalog).expect("serialize");
        assert!(!json.contains("config_schema"));
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
        let json = r#"{"schema":"phoxal.catalog/v0","build":{"tag":"t","run_number":1,"run_id":1,"commit":"c","created_at":"c"},"artifacts":[],"heads":{}}"#;
        let err = serde_json::from_str::<Catalog>(json).unwrap_err();
        assert!(err.to_string().contains("unknown field"));
    }
}
