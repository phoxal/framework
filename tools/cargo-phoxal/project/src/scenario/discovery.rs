//! Scenario file discovery.
//!
//! Walks `<robot-root>/scenarios/` and returns one
//! [`DiscoveredScenario`] per file-form or module-form root. The two
//! layouts:
//!
//! - `scenarios/<name>.rs`
//! - `scenarios/<name>/mod.rs`
//!
//! are mutually exclusive at the `<name>` stem; simultaneous presence is
//! reported as a [`DiscoveryError::AmbiguousModule`] with both locations.
//!
//! Modules are not recursively walked: nested `mod` declarations inside
//! `mod.rs` are ordinary Rust submodules that follow normal Rust rules.
//! The discovery pass only sees the *root* of each scenario.

use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// One scenario source root, after filesystem discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredScenario {
    /// Path of the `.rs` file relative to the robot root.
    pub relative_path: PathBuf,
    /// Rust module identifier usable inside the generated harness.
    /// For `forward_turn_stop.rs` this is `_scenario_forward_turn_stop`
    /// (the generated harness lives under a private namespace).
    pub module_identifier: String,
}

/// Failure modes for scenario discovery.
#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("scenario root `{0}` does not exist or is not a directory")]
    MissingRoot(PathBuf),
    #[error("scenario module stem `{stem}` is ambiguous: both {rs} and {mod_dir} exist")]
    AmbiguousModule {
        stem: String,
        rs: PathBuf,
        mod_dir: PathBuf,
    },
    #[error("scenario module stem `{0}` is not a valid Rust identifier")]
    InvalidIdentifier(String),
    #[error("filesystem error while discovering scenarios at `{path}`: {source}")]
    Filesystem {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Discover scenarios under `<robot_root>/scenarios`. Missing directory
/// is *not* an error: it returns an empty list (the registry is then
/// empty until scenarios are added).
pub fn discover_scenarios(robot_root: &Path) -> Result<Vec<DiscoveredScenario>, DiscoveryError> {
    let scenarios_dir = robot_root.join("scenarios");
    let metadata = match fs::metadata(&scenarios_dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(DiscoveryError::Filesystem {
                path: scenarios_dir,
                source,
            });
        }
    };
    if !metadata.is_dir() {
        return Err(DiscoveryError::MissingRoot(scenarios_dir));
    }

    let entries = fs::read_dir(&scenarios_dir).map_err(|source| DiscoveryError::Filesystem {
        path: scenarios_dir.clone(),
        source,
    })?;

    let mut stems: Vec<(String, Option<PathBuf>, Option<PathBuf>)> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| DiscoveryError::Filesystem {
            path: scenarios_dir.clone(),
            source,
        })?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let path = entry.path();
        let metadata = entry
            .metadata()
            .map_err(|source| DiscoveryError::Filesystem {
                path: path.clone(),
                source,
            })?;
        if metadata.is_dir() {
            let mod_rs = path.join("mod.rs");
            if mod_rs.is_file() {
                stems.push((name.to_owned(), None, Some(mod_rs)));
            }
        } else if metadata.is_file() && name.ends_with(".rs") && name != "mod.rs" {
            let stem = name.trim_end_matches(".rs").to_owned();
            stems.push((stem, Some(path), None));
        }
    }

    // Detect ambiguity: same stem from both `.rs` and `mod.rs`.
    let mut by_stem: std::collections::BTreeMap<String, (Option<PathBuf>, Option<PathBuf>)> =
        std::collections::BTreeMap::new();
    for (stem, rs, mod_dir) in stems {
        let entry = by_stem.entry(stem.clone()).or_default();
        if rs.is_some() {
            entry.0 = rs;
        }
        if mod_dir.is_some() {
            entry.1 = mod_dir;
        }
    }

    let mut out = Vec::new();
    for (stem, (rs, mod_dir)) in by_stem {
        match (rs, mod_dir) {
            (Some(rs), Some(mod_dir)) => {
                return Err(DiscoveryError::AmbiguousModule {
                    stem,
                    rs,
                    mod_dir: mod_dir.join("mod.rs"),
                });
            }
            (Some(rs), None) => {
                let module_identifier = module_identifier(&stem)?;
                out.push(DiscoveredScenario {
                    relative_path: rs
                        .strip_prefix(robot_root)
                        .unwrap_or(rs.as_path())
                        .to_path_buf(),
                    module_identifier,
                });
            }
            (None, Some(mod_dir)) => {
                let module_identifier = module_identifier(&stem)?;
                let relative = mod_dir
                    .strip_prefix(robot_root)
                    .unwrap_or(mod_dir.as_path())
                    .to_path_buf();
                out.push(DiscoveredScenario {
                    relative_path: relative,
                    module_identifier,
                });
            }
            (None, None) => {}
        }
    }

    // Deterministic order: sort by the relative path.
    out.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    Ok(out)
}

/// Convert a filesystem stem into a private module identifier used inside
/// the generated harness. The generated harness lives under a single
/// `scenarios` module, so the user's struct names cannot collide with
/// reserved words or std types.
pub fn module_identifier(stem: &str) -> Result<String, DiscoveryError> {
    let mut chars = stem.chars();
    let Some(first) = chars.next() else {
        return Err(DiscoveryError::InvalidIdentifier(stem.to_owned()));
    };
    if !first.is_ascii_alphabetic() && first != '_' {
        return Err(DiscoveryError::InvalidIdentifier(stem.to_owned()));
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(DiscoveryError::InvalidIdentifier(stem.to_owned()));
    }
    // Prefix with `_scenario_` to avoid colliding with the user's struct
    // names inside the harness module.
    Ok(format!("_scenario_{stem}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn discovers_both_layouts_and_dedupes_by_stem() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"r\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let scenarios = dir.path().join("scenarios");
        fs::create_dir_all(&scenarios).unwrap();
        fs::write(
            scenarios.join("forward_turn_stop.rs"),
            "// empty scenario\n",
        )
        .unwrap();
        fs::create_dir_all(scenarios.join("from_mod")).unwrap();
        fs::write(
            scenarios.join("from_mod").join("mod.rs"),
            "// empty scenario\n",
        )
        .unwrap();

        let found = discover_scenarios(dir.path()).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(
            found[0].relative_path,
            Path::new("scenarios/forward_turn_stop.rs")
        );
        assert_eq!(found[0].module_identifier, "_scenario_forward_turn_stop");
        assert_eq!(
            found[1].relative_path,
            Path::new("scenarios/from_mod/mod.rs")
        );
        assert_eq!(found[1].module_identifier, "_scenario_from_mod");
    }

    #[test]
    fn rejects_ambiguous_stem() {
        let dir = tempfile::tempdir().unwrap();
        let scenarios = dir.path().join("scenarios");
        fs::create_dir_all(&scenarios).unwrap();
        fs::write(scenarios.join("dup.rs"), "//").unwrap();
        fs::create_dir_all(scenarios.join("dup")).unwrap();
        fs::write(scenarios.join("dup").join("mod.rs"), "//").unwrap();
        let error = discover_scenarios(dir.path()).unwrap_err();
        assert!(matches!(error, DiscoveryError::AmbiguousModule { .. }));
    }

    #[test]
    fn missing_root_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let found = discover_scenarios(dir.path()).unwrap();
        assert!(found.is_empty());
    }

    #[test]
    fn rejects_invalid_identifier() {
        assert!(matches!(
            module_identifier("9start"),
            Err(DiscoveryError::InvalidIdentifier(_))
        ));
        assert!(matches!(
            module_identifier("with-dash"),
            Err(DiscoveryError::InvalidIdentifier(_))
        ));
        assert!(module_identifier("forward_turn_stop").is_ok());
    }
}
