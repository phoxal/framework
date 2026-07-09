//! The Phoxal artifact catalog schema (`phoxal-artifacts.json`).
//!
//! This is the single source of truth for the wire shape of the framework's
//! artifact catalog: the discoverable set of official packages (services,
//! component drivers, component assets, tools, simulators), the release
//! channels they publish on, and - for every runtime binary - the contract
//! and config metadata a consumer (the CLI, `robot.yaml` validation) needs
//! without a second round trip to the artifact itself.
//!
//! Two structural choices keep this lean:
//!
//! 1. **Five typed per-kind arrays** ([`Manifest::assets`], [`Manifest::services`],
//!    [`Manifest::drivers`], [`Manifest::tools`], [`Manifest::simulators`])
//!    instead of one tagged `entries` list with a `kind` discriminant: the array
//!    an entry lives in *is* its kind.
//! 2. **Inlined `emit-apis` metadata.** [`ArtifactEntry::contracts`] and
//!    [`ArtifactEntry::config_schema`] are copied straight from the artifact's
//!    own `emit-apis` report at catalog generation time. There is no separate
//!    `.emit-apis.json` sidecar to fetch afterward - the catalog is the whole
//!    answer. There is no `bus_abi` field (D1): the wire ABI dissolved into the
//!    generation-qualified key, so there is nothing left to track as a
//!    cross-artifact axis.
//!
//! `xtask` (`cargo xtask catalog generate`) is the only writer of this schema
//! today; this module carries the types and the revision/checksum machinery,
//! not the workspace-discovery or packaging policy that produces a manifest -
//! that stays in the tool that owns the release process.
//!
//! `api_generation` is deliberately per-[`ArtifactEntry`]: a participant authors
//! against exactly one dated API generation (compile-enforced). Unlike the old
//! model, [`Contract::family`] IS generation-qualified now (D1): the generation
//! is folded into the contract's own name/key, not tracked as a separate axis,
//! so contract identity alone (not a `schema_id` hash) decides compatibility.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The catalog's schema identity - a bare, unversioned sanity guard against
/// pointing a reader at an unrelated JSON file. There is no `format_version`
/// and no version suffix: the framework and CLI move in lockstep and carry no
/// back-compat for pre-1.0 catalogs, so there is never more than one shape.
pub const SCHEMA: &str = "phoxal-artifacts";

/// One immutable revision of the artifact catalog.
///
/// `revision` is the sole integrity field: a `"sha256:<hex>"` content hash
/// computed over the canonical JSON serialization of the manifest with
/// `revision` itself emptied first (see [`Manifest::finalize`] /
/// [`Manifest::verify`]).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: String,
    pub revision: String,
    /// Target-independent component asset bundles (meshes, URDF, `component.yaml`, ...).
    pub assets: Vec<AssetEntry>,
    pub services: Vec<ArtifactEntry>,
    /// The optional target-specific checked driver binary for a component.
    pub drivers: Vec<ArtifactEntry>,
    pub tools: Vec<ArtifactEntry>,
    pub simulators: Vec<ArtifactEntry>,
}

impl Manifest {
    /// Builds an unfinalized manifest (`revision` empty). Call [`Self::finalize`]
    /// before writing it out.
    pub fn new(
        assets: Vec<AssetEntry>,
        services: Vec<ArtifactEntry>,
        drivers: Vec<ArtifactEntry>,
        tools: Vec<ArtifactEntry>,
        simulators: Vec<ArtifactEntry>,
    ) -> Self {
        Self {
            schema: SCHEMA.to_string(),
            revision: String::new(),
            assets,
            services,
            drivers,
            tools,
            simulators,
        }
    }

    /// The total number of entries across all five arrays.
    pub fn total_entries(&self) -> usize {
        self.assets.len()
            + self.services.len()
            + self.drivers.len()
            + self.tools.len()
            + self.simulators.len()
    }

    /// Every runtime binary entry (service, driver, tool, simulator) across
    /// the four [`ArtifactEntry`] arrays, in that order. Excludes
    /// [`Self::assets`], which carry no contracts/API generation.
    pub fn artifact_entries(&self) -> impl Iterator<Item = &ArtifactEntry> {
        self.services
            .iter()
            .chain(&self.drivers)
            .chain(&self.tools)
            .chain(&self.simulators)
    }

    /// Computes and records `revision` over the canonical bytes of `self` with
    /// `revision` emptied first, consuming `self`.
    pub fn finalize(mut self) -> Result<Self> {
        let checksum = self.content_sha256()?;
        self.revision = format!("sha256:{checksum}");
        Ok(self)
    }

    /// Verifies the schema identity and that `revision` matches the manifest's
    /// own content. This is a structural tamper/consistency check only; it does
    /// not enforce business rules (non-empty package/version, contract name
    /// format, ...) - callers that need those apply their own policy on top of
    /// a verified manifest.
    pub fn verify(&self) -> Result<()> {
        if self.schema != SCHEMA {
            bail!(
                "catalog schema '{}' did not match expected '{}'",
                self.schema,
                SCHEMA
            );
        }
        let expected = self.content_sha256()?;
        let expected_revision = format!("sha256:{expected}");
        if self.revision != expected_revision {
            bail!(
                "catalog revision '{}' did not match computed '{}'",
                self.revision,
                expected_revision
            );
        }
        Ok(())
    }

    /// The content hash over the canonical bytes of `self` with `revision`
    /// emptied first.
    fn content_sha256(&self) -> Result<String> {
        let mut unsigned = self.clone();
        unsigned.revision.clear();
        let bytes = serde_json::to_vec(&unsigned).map_err(|error| {
            anyhow::anyhow!("failed to serialize catalog for checksum: {error}")
        })?;
        Ok(to_hex(&Sha256::digest(bytes)))
    }
}

/// A release channel an artifact version is published on.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Stable,
    Preview,
}

/// A built artifact for one triple (or the target-independent scope): a
/// tarball filename plus its digest. No metadata sub-object - a plain asset
/// and checksum.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub tarball: String,
    pub sha256: String,
}

/// One graph contract a runtime binary uses: nothing beyond its
/// generation-qualified identity (`family`, e.g. `"y2026_1::drive::Target"`,
/// D1) - no `schema_id`, no `topic_template`, no `direction`. Those are a
/// property of the API generation's own contract manifest, not of a specific
/// artifact's catalog entry.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    pub family: String,
}

/// A target-independent component asset bundle: meshes, URDF, `component.yaml`,
/// `simulation.yaml`, and other files a component needs regardless of target
/// architecture. Carries no contracts, no `api_generation`, no config, and no
/// bus ABI - it is not a launched participant.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssetEntry {
    pub package: String,
    pub version: String,
    /// Always exactly the target-independent scope key today; a map so the
    /// wire shape matches [`ArtifactEntry::artifacts`].
    pub artifacts: BTreeMap<String, Artifact>,
    pub channels: BTreeMap<Channel, String>,
}

/// One runtime binary: a service, component driver, tool, or simulator (which
/// of the five [`Manifest`] arrays it lives in *is* its kind).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactEntry {
    pub package: String,
    pub version: String,
    /// The one dated API generation this binary authors against (e.g.
    /// `"y2026_1"`). Correctly per-entry: do not fold this into
    /// [`Contract::family`] - see the module docs.
    pub api_generation: String,
    /// Inlined from the artifact's own `emit-apis` report.
    pub contracts: Vec<Contract>,
    /// Inlined JSON Schema for this participant's `robot.yaml` config, from
    /// `emit-apis`.
    pub config_schema: Option<serde_json::Value>,
    /// Triple -> built tarball. The consumer reads supported triples from
    /// these keys; empty is valid (e.g. a metadata-only catalog).
    pub artifacts: BTreeMap<String, Artifact>,
    pub channels: BTreeMap<Channel, String>,
    /// Contract families that changed vs. the previous API generation -
    /// upgrade-preview signal, kept from the prior schema.
    pub changed_contracts: Vec<String>,
}

/// Whether `value` is a valid `schema_id`: 16 lowercase hex characters.
pub fn is_schema_id(value: &str) -> bool {
    value.len() == 16
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

/// Whether `value` is a valid lowercase sha256 hex digest.
pub fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact_entry(package: &str) -> ArtifactEntry {
        let mut channels = BTreeMap::new();
        channels.insert(Channel::Stable, "0.1.0".to_string());
        ArtifactEntry {
            package: package.to_string(),
            version: "0.1.0".to_string(),
            api_generation: "y2026_1".to_string(),
            contracts: vec![Contract {
                family: "y2026_1::drive::Target".to_string(),
            }],
            config_schema: Some(serde_json::json!({ "type": "object" })),
            artifacts: BTreeMap::new(),
            channels,
            changed_contracts: Vec::new(),
        }
    }

    #[test]
    fn finalize_then_verify_round_trips() -> Result<()> {
        let manifest = Manifest::new(
            Vec::new(),
            vec![artifact_entry("phoxal/service-drive")],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .finalize()?;
        manifest.verify()?;
        assert!(manifest.revision.starts_with("sha256:"));

        let json = serde_json::to_string_pretty(&manifest)?;
        let reparsed: Manifest = serde_json::from_str(&json)?;
        assert_eq!(manifest, reparsed);
        reparsed.verify()?;
        Ok(())
    }

    #[test]
    fn verify_rejects_edited_content() -> Result<()> {
        let mut manifest = Manifest::new(
            Vec::new(),
            vec![artifact_entry("phoxal/service-drive")],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .finalize()?;
        manifest.services[0].version = "0.2.0".to_string();
        let err = manifest.verify().unwrap_err();
        assert!(err.to_string().contains("did not match computed"));
        Ok(())
    }

    #[test]
    fn verify_rejects_wrong_schema() -> Result<()> {
        let mut manifest =
            Manifest::new(Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new()).finalize()?;
        manifest.schema = "phoxal-artifacts/999".to_string();
        let err = manifest.verify().unwrap_err();
        assert!(err.to_string().contains("catalog schema"));
        Ok(())
    }

    #[test]
    fn schema_id_and_sha256_format_checks() {
        assert!(is_schema_id("0123456789abcdef"));
        assert!(!is_schema_id("0123456789ABCDEF"));
        assert!(!is_schema_id("too-short"));
        assert!(is_sha256(&"a".repeat(64)));
        assert!(!is_sha256(&"A".repeat(64)));
    }
}
