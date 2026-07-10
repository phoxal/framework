//! `cargo xtask catalog verify` - parse a `phoxal.catalog/v0` file and check
//! its internal structural invariants. There is no more revision/checksum
//! machinery (the old mutable-manifest model this replaces had one): a
//! `phoxal.catalog/v0` file's integrity is entirely structural (schema
//! identity, `(package, version)` uniqueness, every entry has an output,
//! every `Blob` is well-formed) - see [`validate_structure`].

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;

use super::model::{Catalog, SCHEMA};
use crate::catalog::generate::default_catalog_path;
use crate::workspace::Workspace;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Catalog file to verify; defaults to the standard local generate output.
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,
}

pub fn run(args: Args) -> Result<()> {
    let workspace = Workspace::discover()?;
    let path = args
        .path
        .unwrap_or_else(|| default_catalog_path(&workspace));
    let catalog = verify_catalog_path(&path)?;
    println!(
        "verified catalog {} with {} artifact entries at {}",
        catalog.schema,
        catalog.artifacts.len(),
        path.display()
    );
    Ok(())
}

/// Parses `path` as a `phoxal.catalog/v0` document and validates it
/// structurally with no `--metadata-only` leniency (every entry must carry a
/// real, well-formed output). Used both by `catalog verify` and by `catalog
/// generate --previous-catalog`, since a previous catalog is always a real,
/// fully-built one.
pub(crate) fn verify_catalog_path(path: &Path) -> Result<Catalog> {
    let catalog = load_catalog(path)?;
    validate_structure(&catalog, false)?;
    Ok(catalog)
}

/// Parses `path` as a `phoxal.catalog/v0` document without validating its
/// structural invariants - callers apply their own (possibly
/// `--metadata-only`-lenient) validation afterward via [`validate_structure`].
pub(crate) fn load_catalog(path: &Path) -> Result<Catalog> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("{} is not a valid catalog JSON document", path.display()))
}

/// The structural invariants every `phoxal.catalog/v0` document must satisfy:
///
/// - `schema` is `"phoxal.catalog/v0"`;
/// - `(package, version)` is unique across `artifacts[]`;
/// - unless `metadata_only`: every entry has a `targets` and/or `assets`
///   output, and every [`super::model::Blob`] it carries has a non-empty
///   url, a valid sha256, and a non-zero size. A `--metadata-only` catalog
///   legitimately has neither (no tarballs exist yet - see
///   `catalog::generate`), so both checks are skipped in that mode.
pub(crate) fn validate_structure(catalog: &Catalog, metadata_only: bool) -> Result<()> {
    if catalog.schema != SCHEMA {
        bail!(
            "catalog schema '{}' did not match expected '{}'",
            catalog.schema,
            SCHEMA
        );
    }

    let mut seen = std::collections::BTreeSet::new();
    for artifact in &catalog.artifacts {
        let key = (artifact.package.as_str(), artifact.version.as_str());
        if !seen.insert(key) {
            bail!(
                "catalog contains duplicate entry for {} v{}",
                artifact.package,
                artifact.version
            );
        }

        if metadata_only {
            continue;
        }

        if artifact.targets.is_empty() && artifact.assets.is_none() {
            bail!(
                "catalog entry {} v{} has neither targets nor assets",
                artifact.package,
                artifact.version
            );
        }
        for (triple, blob) in &artifact.targets {
            validate_blob(
                blob,
                &format!(
                    "{} v{} target {}",
                    artifact.package, artifact.version, triple
                ),
            )?;
        }
        if let Some(blob) = &artifact.assets {
            validate_blob(
                blob,
                &format!("{} v{} assets", artifact.package, artifact.version),
            )?;
        }
    }

    Ok(())
}

fn validate_blob(blob: &super::model::Blob, describe: &str) -> Result<()> {
    if blob.url.trim().is_empty() {
        bail!("{describe} has an empty blob url");
    }
    if !super::model::is_sha256(&blob.sha256) {
        bail!(
            "{describe} has an invalid sha256 checksum '{}'",
            blob.sha256
        );
    }
    if blob.size == 0 {
        bail!("{describe} has a zero blob size");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::catalog::model::{Artifact, Blob, BuildProvenance, Contract};

    fn fixture_build() -> BuildProvenance {
        BuildProvenance {
            tag: "build-20260708-0001234".to_string(),
            run_number: 1234,
            run_id: 1,
            commit: "deadbeef".to_string(),
            created_at: "2026-07-08T12:00:00Z".to_string(),
        }
    }

    fn fixture_artifact() -> Artifact {
        let mut targets = BTreeMap::new();
        targets.insert(
            "x86_64-unknown-linux-gnu".to_string(),
            Blob {
                url: "https://example.invalid/a.tar.zst".to_string(),
                sha256: "a".repeat(64),
                size: 42,
            },
        );
        Artifact {
            package: "phoxal/service-drive".to_string(),
            version: "0.1.0".to_string(),
            contracts: vec![Contract {
                name: "y2026_1/drive/target".to_string(),
                role: "publish".to_string(),
            }],
            config_schema: None,
            targets,
            assets: None,
        }
    }

    #[test]
    fn accepts_a_well_formed_catalog() {
        let catalog = Catalog::new(fixture_build(), vec![fixture_artifact()]);
        validate_structure(&catalog, false).expect("valid catalog");
    }

    #[test]
    fn rejects_wrong_schema() {
        let mut catalog = Catalog::new(fixture_build(), vec![fixture_artifact()]);
        catalog.schema = "phoxal-artifacts".to_string();
        let err = validate_structure(&catalog, false).unwrap_err();
        assert!(err.to_string().contains("catalog schema"));
    }

    #[test]
    fn rejects_duplicate_package_version() {
        let entry = fixture_artifact();
        let catalog = Catalog::new(fixture_build(), vec![entry.clone(), entry]);
        let err = validate_structure(&catalog, false).unwrap_err();
        assert!(err.to_string().contains("duplicate entry"));
    }

    #[test]
    fn rejects_entry_with_neither_targets_nor_assets() {
        let mut entry = fixture_artifact();
        entry.targets.clear();
        let catalog = Catalog::new(fixture_build(), vec![entry]);
        let err = validate_structure(&catalog, false).unwrap_err();
        assert!(err.to_string().contains("neither targets nor assets"));
    }

    #[test]
    fn rejects_blob_with_bad_sha256() {
        let mut entry = fixture_artifact();
        for blob in entry.targets.values_mut() {
            blob.sha256 = "not-a-hash".to_string();
        }
        let catalog = Catalog::new(fixture_build(), vec![entry]);
        let err = validate_structure(&catalog, false).unwrap_err();
        assert!(err.to_string().contains("invalid sha256"));
    }

    #[test]
    fn metadata_only_mode_allows_entries_with_no_outputs() {
        let mut entry = fixture_artifact();
        entry.targets.clear();
        let catalog = Catalog::new(fixture_build(), vec![entry]);
        validate_structure(&catalog, true).expect("metadata-only catalog is valid");
    }
}
