use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;
use phoxal::catalog::Manifest;

use crate::catalog::generate::default_catalog_path;
use crate::workspace::Workspace;

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(value_name = "PATH_OR_REV")]
    pub rev: String,
}

pub fn run(args: Args) -> Result<()> {
    let workspace = Workspace::discover()?;
    let path = resolve_path_or_revision(&workspace, &args.rev)?;
    let manifest = verify_catalog_path(&path)?;
    println!(
        "verified catalog {} with {} entries at {}",
        manifest.revision,
        manifest.total_entries(),
        path.display()
    );
    Ok(())
}

pub(crate) fn verify_catalog_path(path: &Path) -> Result<Manifest> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let manifest: Manifest = serde_json::from_str(&text)
        .with_context(|| format!("{} is not a valid catalog JSON document", path.display()))?;
    manifest.verify()?;
    Ok(manifest)
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
        let manifest = verify_catalog_path(&default_path)?;
        let bare_hex = manifest.revision.strip_prefix("sha256:");
        if manifest.revision == value || bare_hex == Some(value) {
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
        catalog.services[0].package = "phoxal/service-edited".to_string();
        let err = catalog.verify().unwrap_err();
        assert_error_contains(&err, "did not match computed");
        Ok(())
    }

    #[test]
    fn verify_rejects_wrong_schema() -> Result<()> {
        let mut catalog = fixture_catalog()?;
        catalog.schema = "phoxal-artifacts/999".to_string();
        let err = catalog.verify().unwrap_err();
        assert_error_contains(&err, "catalog schema");
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
