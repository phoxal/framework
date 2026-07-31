//! Native build requirements declared by a runnable crate.
//!
//! The namespace stays strict so future keys can be added deliberately without
//! reinterpreting declarations that were already published.

use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};

/// A runnable crate's declared native build requirements.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildRequirements {
    /// Debian package selectors, normalized to a sorted, deduplicated set.
    pub apt: BTreeSet<String>,
}

/// Debian package names start with an alphanumeric and contain at least two
/// bytes (Policy 5.6.1); those rules also prevent apt option injection.
fn is_valid_apt_selector(selector: &str) -> bool {
    selector.len() >= 2
        && selector
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && selector
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".+:_=-".contains(&byte))
}

/// Parse a manifest's `[package.metadata.phoxal.build]` table strictly.
///
/// Missing `phoxal`, `build`, or `apt` levels are an empty declaration.
/// Unknown keys are rejected so the namespace can grow deliberately.
pub fn requirements_from_manifest(manifest_source: &str, label: &str) -> Result<BuildRequirements> {
    let manifest: toml::Value = toml::from_str(manifest_source)
        .with_context(|| format!("{label}: manifest is not valid TOML"))?;
    let Some(phoxal) = manifest
        .get("package")
        .and_then(|package| package.get("metadata"))
        .and_then(|metadata| metadata.get("phoxal"))
    else {
        return Ok(BuildRequirements::default());
    };
    let Some(phoxal) = phoxal.as_table() else {
        bail!("{label}: [package.metadata.phoxal] must be a table");
    };
    if phoxal.contains_key("kind") || phoxal.contains_key("id") {
        bail!(
            "{label}: [package.metadata.phoxal] must not declare kind or id; package identity is derived from the directory convention"
        );
    }
    for key in phoxal.keys() {
        if key != "build" {
            bail!(
                "{label}: unknown key {key:?} in [package.metadata.phoxal]; only `build` is allowed"
            );
        }
    }
    let Some(build) = phoxal.get("build") else {
        return Ok(BuildRequirements::default());
    };
    let Some(build) = build.as_table() else {
        bail!("{label}: [package.metadata.phoxal.build] must be a table");
    };
    for key in build.keys() {
        if key != "apt" {
            bail!(
                "{label}: unknown key {key:?} in [package.metadata.phoxal.build]; only `apt` is allowed"
            );
        }
    }
    let Some(apt) = build.get("apt") else {
        return Ok(BuildRequirements::default());
    };
    let Some(entries) = apt.as_array() else {
        bail!("{label}: `apt` must be an array of package selectors");
    };
    let mut packages = BTreeSet::new();
    for entry in entries {
        let Some(selector) = entry.as_str() else {
            bail!("{label}: `apt` entries must be strings");
        };
        if !is_valid_apt_selector(selector) {
            bail!(
                "{label}: invalid apt selector {selector:?}; a selector must start with an ASCII alphanumeric, be at least two bytes, and use only [A-Za-z0-9.+:_=-]"
            );
        }
        packages.insert(selector.to_owned());
    }
    Ok(BuildRequirements { apt: packages })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_tables_mean_an_empty_declaration() -> Result<()> {
        for manifest in [
            "[package]\nname = \"x\"",
            "[package.metadata.phoxal]",
            "[package.metadata.phoxal.build]",
        ] {
            assert_eq!(
                requirements_from_manifest(manifest, "test")?,
                BuildRequirements::default()
            );
        }
        Ok(())
    }

    #[test]
    fn a_valid_declaration_is_sorted_and_deduplicated() -> Result<()> {
        let requirements = requirements_from_manifest(
            "[package.metadata.phoxal.build]\napt = [\"pkg-config\", \"libudev-dev\", \"pkg-config\"]",
            "test",
        )?;
        assert_eq!(
            requirements.apt.into_iter().collect::<Vec<_>>(),
            ["libudev-dev", "pkg-config"]
        );
        Ok(())
    }

    #[test]
    fn an_unknown_phoxal_key_is_rejected() {
        let error =
            requirements_from_manifest("[package.metadata.phoxal.container]", "test").unwrap_err();
        assert!(error.to_string().contains("unknown key \"container\""));
    }

    #[test]
    fn an_unknown_build_key_is_rejected() {
        let error = requirements_from_manifest(
            "[package.metadata.phoxal.build]\ndockerfile = \"x\"",
            "test",
        )
        .unwrap_err();
        assert!(error.to_string().contains("unknown key \"dockerfile\""));
    }

    #[test]
    fn kind_and_id_are_rejected_with_a_specific_message() {
        for key in ["kind", "id"] {
            let source = format!("[package.metadata.phoxal]\n{key} = \"x\"");
            let error = requirements_from_manifest(&source, "test").unwrap_err();
            assert!(
                error.to_string().contains("must not declare kind or id"),
                "{error:#}"
            );
        }
    }

    #[test]
    fn an_invalid_selector_is_rejected() {
        for selector in ["libudev-dev; rm -rf /", "", "-o"] {
            let source = format!("[package.metadata.phoxal.build]\napt = [{selector:?}]");
            let error = requirements_from_manifest(&source, "test").unwrap_err();
            assert!(error.to_string().contains("invalid apt selector"));
        }
    }

    #[test]
    fn untrusted_values_are_escaped_in_errors() {
        let error = requirements_from_manifest(
            "[package.metadata.phoxal.build]\napt = [\"\\u001b]0;owned\\u0007\"]",
            "test",
        )
        .unwrap_err();
        let rendered = error.to_string();
        assert!(!rendered.contains('\u{1b}'));
        assert!(!rendered.contains('\u{7}'));
        assert!(rendered.contains(r"\u{1b}]0;owned\u{7}"));
    }

    #[test]
    fn non_string_entries_are_rejected() {
        let error =
            requirements_from_manifest("[package.metadata.phoxal.build]\napt = [1]", "test")
                .unwrap_err();
        assert!(error.to_string().contains("must be strings"));
    }

    #[test]
    fn non_table_metadata_levels_are_rejected() {
        for (source, expected) in [
            ("[package.metadata]\nphoxal = 1", "phoxal] must be a table"),
            (
                "[package.metadata.phoxal]\nbuild = \"x\"",
                "phoxal.build] must be a table",
            ),
        ] {
            let error = requirements_from_manifest(source, "test").unwrap_err();
            assert!(error.to_string().contains(expected), "{error:#}");
        }
    }
}
