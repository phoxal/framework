//! Native build requirements declared by a runnable crate.
//!
//! The namespace stays strict so future keys can be added deliberately without
//! reinterpreting declarations that were already published.

use std::collections::BTreeSet;

/// A runnable crate's declared native build requirements.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildRequirements {
    /// Debian package selectors, normalized to a sorted, deduplicated set.
    pub apt: BTreeSet<String>,
}

/// Why a Cargo manifest's `[package.metadata.phoxal]` table is not a
/// declaration this crate can honour.
///
/// `label` is what the caller calls the manifest - usually its path - and it
/// leads every message so a workspace-wide scan says which crate is at fault.
#[derive(Debug, thiserror::Error)]
pub enum BuildRequirementsError {
    #[error("{label}: manifest is not valid TOML: {source}")]
    Toml {
        label: String,
        #[source]
        source: toml::de::Error,
    },

    #[error("{label}: [{table}] must be a table")]
    NotATable { label: String, table: &'static str },

    /// Package identity is derived from the directory convention, so declaring
    /// it here would create a second, disagreeing source of truth.
    #[error(
        "{label}: [package.metadata.phoxal] must not declare kind or id; package identity is \
         derived from the directory convention"
    )]
    DeclaredIdentity { label: String },

    #[error("{label}: unknown key {key:?} in [{table}]; only `{allowed}` is allowed")]
    UnknownKey {
        label: String,
        table: &'static str,
        key: String,
        allowed: &'static str,
    },

    #[error("{label}: `apt` must be an array of package selectors")]
    AptNotAnArray { label: String },

    #[error("{label}: `apt` entries must be strings")]
    AptEntryNotAString { label: String },

    /// A selector that does not spell a Debian package name.
    #[error(
        "{label}: invalid apt selector {selector:?}; a Debian package name starts with an ASCII \
         alphanumeric, is at least two bytes, and uses only [A-Za-z0-9.+:_=-]"
    )]
    InvalidSelector { label: String, selector: String },
}

const PHOXAL_TABLE: &str = "package.metadata.phoxal";
const BUILD_TABLE: &str = "package.metadata.phoxal.build";

impl BuildRequirements {
    /// Parse a Cargo manifest's `[package.metadata.phoxal.build]` table
    /// strictly.
    ///
    /// Missing `phoxal`, `build`, or `apt` levels are an empty declaration.
    /// Unknown keys are rejected so the namespace can grow deliberately without
    /// reinterpreting declarations that were already published.
    ///
    /// # Errors
    ///
    /// Returns the first [`BuildRequirementsError`] the manifest triggers.
    pub fn from_manifest(
        manifest_source: &str,
        label: &str,
    ) -> Result<Self, BuildRequirementsError> {
        let manifest: toml::Value =
            toml::from_str(manifest_source).map_err(|source| BuildRequirementsError::Toml {
                label: label.to_string(),
                source,
            })?;
        let Some(phoxal) = manifest
            .get("package")
            .and_then(|package| package.get("metadata"))
            .and_then(|metadata| metadata.get("phoxal"))
        else {
            return Ok(Self::default());
        };
        let phoxal = table(phoxal, label, PHOXAL_TABLE)?;
        if phoxal.contains_key("kind") || phoxal.contains_key("id") {
            return Err(BuildRequirementsError::DeclaredIdentity {
                label: label.to_string(),
            });
        }
        only_key(phoxal.keys(), label, PHOXAL_TABLE, "build")?;

        let Some(build) = phoxal.get("build") else {
            return Ok(Self::default());
        };
        let build = table(build, label, BUILD_TABLE)?;
        only_key(build.keys(), label, BUILD_TABLE, "apt")?;

        let Some(apt) = build.get("apt") else {
            return Ok(Self::default());
        };
        let Some(entries) = apt.as_array() else {
            return Err(BuildRequirementsError::AptNotAnArray {
                label: label.to_string(),
            });
        };
        let mut packages = BTreeSet::new();
        for entry in entries {
            let Some(selector) = entry.as_str() else {
                return Err(BuildRequirementsError::AptEntryNotAString {
                    label: label.to_string(),
                });
            };
            if !is_valid_apt_selector(selector) {
                return Err(BuildRequirementsError::InvalidSelector {
                    label: label.to_string(),
                    selector: selector.to_owned(),
                });
            }
            packages.insert(selector.to_owned());
        }
        Ok(Self { apt: packages })
    }
}

/// Debian package names start with an alphanumeric and contain at least two
/// bytes (Policy 5.6.1).
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

fn table<'a>(
    value: &'a toml::Value,
    label: &str,
    name: &'static str,
) -> Result<&'a toml::Table, BuildRequirementsError> {
    value
        .as_table()
        .ok_or_else(|| BuildRequirementsError::NotATable {
            label: label.to_string(),
            table: name,
        })
}

fn only_key<'a>(
    keys: impl Iterator<Item = &'a String>,
    label: &str,
    table: &'static str,
    allowed: &'static str,
) -> Result<(), BuildRequirementsError> {
    for key in keys {
        if key != allowed {
            return Err(BuildRequirementsError::UnknownKey {
                label: label.to_string(),
                table,
                key: key.clone(),
                allowed,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::BuildRequirements;

    #[test]
    fn absent_tables_mean_an_empty_declaration() -> anyhow::Result<()> {
        for manifest in [
            "[package]\nname = \"x\"",
            "[package.metadata.phoxal]",
            "[package.metadata.phoxal.build]",
        ] {
            assert_eq!(
                BuildRequirements::from_manifest(manifest, "test")?,
                BuildRequirements::default()
            );
        }
        Ok(())
    }

    #[test]
    fn a_valid_declaration_is_sorted_and_deduplicated() -> anyhow::Result<()> {
        let requirements = BuildRequirements::from_manifest(
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
        let error = BuildRequirements::from_manifest("[package.metadata.phoxal.container]", "test")
            .unwrap_err();
        assert!(error.to_string().contains("unknown key \"container\""));
    }

    #[test]
    fn an_unknown_build_key_is_rejected() {
        let error = BuildRequirements::from_manifest(
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
            let error = BuildRequirements::from_manifest(&source, "test").unwrap_err();
            assert!(
                error.to_string().contains("must not declare kind or id"),
                "{error:#}"
            );
        }
    }

    #[test]
    fn an_invalid_selector_is_rejected() {
        for selector in ["", "x", ".libudev-dev", "libudev dev"] {
            let source = format!("[package.metadata.phoxal.build]\napt = [{selector:?}]");
            let error = BuildRequirements::from_manifest(&source, "test").unwrap_err();
            assert!(error.to_string().contains("invalid apt selector"));
        }
    }

    #[test]
    fn error_text_escapes_control_characters() {
        let error = BuildRequirements::from_manifest(
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
            BuildRequirements::from_manifest("[package.metadata.phoxal.build]\napt = [1]", "test")
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
            let error = BuildRequirements::from_manifest(source, "test").unwrap_err();
            assert!(error.to_string().contains(expected), "{error:#}");
        }
    }
}
