use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::workspace::ArtifactKind;

pub(crate) const CATALOG_SCHEMA: &str = "phoxal.artifact-catalog/v0";
pub(crate) const CATALOG_FORMAT_VERSION: u32 = 1;
pub(crate) const CHECKSUM_CANONICALIZATION: &str = "json-v1-empty-revision-and-integrity-sha256";

/// One immutable catalog revision.
///
/// Phase 2 produces local, checksummed revisions from workspace discovery and
/// `cargo xtask release package` outputs. Native release plan #01 will feed this
/// same schema with real per-triple CI assets, fill in signatures and the
/// per-asset engine versions that are not yet embedded in today's `emit-apis`
/// document, then publish immutable revisions behind an atomic ref.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CatalogRevision {
    pub schema: String,
    pub format_version: u32,
    pub revision: String,
    pub integrity: CatalogIntegrity,
    pub provenance: CatalogProvenance,
    pub signature: Option<CatalogSignature>,
    pub entries: Vec<CatalogEntry>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CatalogIntegrity {
    pub algorithm: String,
    pub canonicalization: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CatalogProvenance {
    pub generator: String,
    pub official_set: String,
    pub emit_apis: String,
    pub release_assets: String,
    pub plan_01: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CatalogSignature {
    pub scheme: String,
    pub key_id: String,
    pub signature: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CatalogEntry {
    pub artifact_id: String,
    pub kind: ArtifactKind,
    pub package: String,
    pub version: String,
    pub api_generation: String,
    pub contract_uses: Vec<ContractUse>,
    pub target_triples: Vec<String>,
    pub release_assets: BTreeMap<String, ReleaseAsset>,
    pub launch_facts: LaunchFacts,
    pub engine_versions: EngineVersions,
    pub channels: BTreeMap<Channel, String>,
    pub status: BTreeMap<String, ArtifactStatus>,
    pub changed_contracts: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Channel {
    Stable,
    Preview,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ArtifactStatus {
    Released,
    Pending,
    Failed,
    Blocked,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContractUse {
    pub family: String,
    pub topic_template: String,
    pub direction: String,
    pub schema_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReleaseAsset {
    pub asset: String,
    pub sha256: String,
    pub metadata: ReleaseAssetMetadata,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReleaseAssetMetadata {
    pub emit_apis: String,
    pub emit_apis_sha256: String,
    pub sha256_file: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LaunchFacts {
    pub participant_kind: String,
    pub participant_class: String,
    pub router: RouterFacts,
    pub systemd: SystemdFacts,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RouterFacts {
    pub needs_zenoh_router: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SystemdFacts {
    pub groups: Vec<String>,
    pub devices: Vec<String>,
    pub source: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EngineVersions {
    pub phoxal: Option<String>,
    pub phoxal_bus: Option<String>,
    pub zenoh: Option<String>,
}

impl CatalogRevision {
    pub(crate) fn new(provenance: CatalogProvenance, entries: Vec<CatalogEntry>) -> Self {
        Self {
            schema: CATALOG_SCHEMA.to_string(),
            format_version: CATALOG_FORMAT_VERSION,
            revision: String::new(),
            integrity: CatalogIntegrity {
                algorithm: "sha256".to_string(),
                canonicalization: CHECKSUM_CANONICALIZATION.to_string(),
                sha256: String::new(),
            },
            provenance,
            signature: None,
            entries,
        }
    }

    pub(crate) fn finalize(mut self) -> Result<Self> {
        self.revision.clear();
        self.integrity.sha256.clear();
        let checksum = self.semantic_sha256()?;
        self.revision = format!("sha256:{checksum}");
        self.integrity.sha256 = checksum;
        Ok(self)
    }

    pub(crate) fn verify(&self) -> Result<()> {
        if self.schema != CATALOG_SCHEMA {
            bail!(
                "catalog schema '{}' did not match expected '{}'",
                self.schema,
                CATALOG_SCHEMA
            );
        }
        if self.format_version != CATALOG_FORMAT_VERSION {
            bail!(
                "catalog format_version {} did not match expected {}",
                self.format_version,
                CATALOG_FORMAT_VERSION
            );
        }
        if self.integrity.algorithm != "sha256" {
            bail!(
                "catalog integrity algorithm '{}' did not match expected 'sha256'",
                self.integrity.algorithm
            );
        }
        if self.integrity.canonicalization != CHECKSUM_CANONICALIZATION {
            bail!(
                "catalog checksum canonicalization '{}' did not match expected '{}'",
                self.integrity.canonicalization,
                CHECKSUM_CANONICALIZATION
            );
        }
        let expected = self.semantic_sha256()?;
        if self.integrity.sha256 != expected {
            bail!(
                "catalog checksum mismatch: recorded {}, computed {}",
                self.integrity.sha256,
                expected
            );
        }
        let expected_revision = format!("sha256:{expected}");
        if self.revision != expected_revision {
            bail!(
                "catalog revision '{}' did not match integrity '{}'",
                self.revision,
                expected_revision
            );
        }
        validate_entries(&self.entries)?;
        Ok(())
    }

    fn semantic_sha256(&self) -> Result<String> {
        let mut unsigned = self.clone();
        unsigned.revision.clear();
        unsigned.integrity.sha256.clear();
        let bytes =
            serde_json::to_vec(&unsigned).context("failed to serialize catalog for checksum")?;
        Ok(hex::encode(Sha256::digest(bytes)))
    }
}

fn validate_entries(entries: &[CatalogEntry]) -> Result<()> {
    if entries.is_empty() {
        bail!("catalog entries must not be empty");
    }

    let mut seen = BTreeSet::new();
    for entry in entries {
        if !seen.insert((&entry.artifact_id, &entry.version)) {
            bail!(
                "catalog contains duplicate entry for {} v{}",
                entry.artifact_id,
                entry.version
            );
        }
        if entry.contract_uses.is_empty() && entry.kind != ArtifactKind::Tool {
            bail!("{} contract_uses must not be empty", entry.artifact_id);
        }
        for contract in &entry.contract_uses {
            if contract.family.trim().is_empty() {
                bail!("{} has an empty contract family", entry.artifact_id);
            }
            if contract.topic_template.trim().is_empty() {
                bail!("{} has an empty topic_template", entry.artifact_id);
            }
            if !is_schema_id(&contract.schema_id) {
                bail!(
                    "{} has invalid schema_id '{}'",
                    entry.artifact_id,
                    contract.schema_id
                );
            }
        }
        if entry.target_triples.is_empty() {
            bail!("{} target_triples must not be empty", entry.artifact_id);
        }
        for triple in &entry.target_triples {
            let status = entry.status.get(triple).with_context(|| {
                format!(
                    "{} is missing status for target {triple}",
                    entry.artifact_id
                )
            })?;
            if *status == ArtifactStatus::Released && !entry.release_assets.contains_key(triple) {
                bail!(
                    "{} target {triple} is released but has no release asset",
                    entry.artifact_id
                );
            }
        }
        for triple in entry.release_assets.keys() {
            if !entry.target_triples.contains(triple) {
                bail!(
                    "{} release asset target {triple} is not in target_triples",
                    entry.artifact_id
                );
            }
        }
        if entry.channels.is_empty() {
            bail!("{} channels must not be empty", entry.artifact_id);
        }
    }

    Ok(())
}

pub(crate) fn is_schema_id(value: &str) -> bool {
    value.len() == 16
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: ArtifactKind, contract_uses: Vec<ContractUse>) -> CatalogEntry {
        let artifact_id = match kind {
            ArtifactKind::Service => "frame",
            ArtifactKind::Driver => "bno085",
            ArtifactKind::Tool => "tool-router",
            ArtifactKind::Simulator => "sim",
        }
        .to_string();
        let mut channels = BTreeMap::new();
        channels.insert(Channel::Stable, "0.1.0".to_string());
        let mut status = BTreeMap::new();
        status.insert("aarch64-apple-darwin".to_string(), ArtifactStatus::Pending);
        CatalogEntry {
            artifact_id,
            kind,
            package: "package".to_string(),
            version: "0.1.0".to_string(),
            api_generation: "y2026_1".to_string(),
            contract_uses,
            target_triples: vec!["aarch64-apple-darwin".to_string()],
            release_assets: BTreeMap::new(),
            launch_facts: LaunchFacts {
                participant_kind: kind.emit_apis_kind().to_string(),
                participant_class: match kind {
                    ArtifactKind::Tool => "privileged",
                    _ => "checked",
                }
                .to_string(),
                router: RouterFacts {
                    needs_zenoh_router: true,
                },
                systemd: SystemdFacts {
                    groups: Vec::new(),
                    devices: Vec::new(),
                    source: "test".to_string(),
                },
            },
            engine_versions: EngineVersions {
                phoxal: None,
                phoxal_bus: None,
                zenoh: None,
            },
            channels,
            status,
            changed_contracts: Vec::new(),
        }
    }

    fn contract() -> ContractUse {
        ContractUse {
            family: "frame::LookupRequest".to_string(),
            topic_template: "frame/lookup".to_string(),
            direction: "query_request".to_string(),
            schema_id: "0123456789abcdef".to_string(),
        }
    }

    #[test]
    fn checked_artifact_requires_contract_uses() {
        let err = validate_entries(&[entry(ArtifactKind::Service, Vec::new())])
            .expect_err("checked service should require graph contracts");
        assert!(err.to_string().contains("contract_uses"));
    }

    #[test]
    fn tool_artifact_may_have_empty_contract_uses() {
        validate_entries(&[entry(ArtifactKind::Tool, Vec::new())])
            .expect("privileged setup-only tools can have no graph contracts");
        validate_entries(&[entry(ArtifactKind::Tool, vec![contract()])])
            .expect("tools with graph contracts still validate");
    }
}
