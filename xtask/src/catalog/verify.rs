use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;

use crate::catalog::generate::default_catalog_path;
use crate::catalog::schema::CatalogRevision;
use crate::workspace::Workspace;

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(value_name = "PATH_OR_REV")]
    pub rev: String,
}

pub fn run(args: Args) -> Result<()> {
    let workspace = Workspace::discover()?;
    let path = resolve_path_or_revision(&workspace, &args.rev)?;
    let revision = verify_catalog_path(&path)?;
    println!(
        "verified catalog {} with {} entries at {}",
        revision.revision,
        revision.entries.len(),
        path.display()
    );
    Ok(())
}

pub(crate) fn verify_catalog_path(path: &Path) -> Result<CatalogRevision> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let revision: CatalogRevision = serde_json::from_str(&text)
        .with_context(|| format!("{} is not a valid catalog JSON document", path.display()))?;
    revision.verify()?;
    Ok(revision)
}

fn resolve_path_or_revision(workspace: &Workspace, value: &str) -> Result<PathBuf> {
    let path = PathBuf::from(value);
    if path.is_file() {
        return Ok(path);
    }
    if path.components().count() > 1 {
        bail!("catalog path does not exist: {}", path.display());
    }

    let default_path = default_catalog_path(workspace);
    if default_path.is_file() {
        let revision = verify_catalog_path(&default_path)?;
        if revision.revision == value || revision.integrity.sha256 == value {
            return Ok(default_path);
        }
    }

    bail!(
        "catalog revision '{value}' is not available locally; pass a catalog path or generate {} first",
        default_path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::generate::tests_support::fixture_catalog;

    #[test]
    fn verify_rejects_bad_checksum() -> Result<()> {
        let mut catalog = fixture_catalog()?;
        catalog.entries[0].package = "phoxal-service-edited".to_string();
        let err = catalog.verify().unwrap_err();
        assert_error_contains(&err, "checksum mismatch");
        Ok(())
    }

    #[test]
    fn verify_rejects_wrong_format_version() -> Result<()> {
        let mut catalog = fixture_catalog()?;
        catalog.format_version = 999;
        catalog = catalog.finalize()?;
        let err = catalog.verify().unwrap_err();
        assert_error_contains(&err, "format_version");
        Ok(())
    }

    fn assert_error_contains(error: &anyhow::Error, needle: &str) {
        assert!(
            error
                .chain()
                .any(|cause| cause.to_string().contains(needle)),
            "expected error chain to contain {needle:?}, got {error:?}"
        );
    }
}
