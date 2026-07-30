//! Versioned authored `robot.yaml` documents.

pub mod v0;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

mod strict_yaml;

const ROBOT_FILE: &str = "robot.yaml";

pub fn read_from_dir(path: impl AsRef<Path>) -> Result<v0::Manifest> {
    let manifest = parse_from_dir(path)?;
    manifest.validate().map_err(validation_error)?;
    Ok(manifest)
}

pub fn read_from_path(path: impl AsRef<Path>) -> Result<v0::Manifest> {
    let manifest = parse_from_path(path)?;
    manifest.validate().map_err(validation_error)?;
    Ok(manifest)
}

pub fn read_from_string(text: &str) -> Result<v0::Manifest> {
    let manifest = parse_from_string(text)?;
    manifest.validate().map_err(validation_error)?;
    Ok(manifest)
}

pub fn parse_from_dir(path: impl AsRef<Path>) -> Result<v0::Manifest> {
    parse_from_path(path.as_ref().join(ROBOT_FILE))
}

/// Read one leaf document and compose its ordered direct parents.
pub fn parse_from_path(path: impl AsRef<Path>) -> Result<v0::Manifest> {
    let leaf_path = path.as_ref().canonicalize().with_context(|| {
        format!(
            "failed to resolve robot/v0 document {}",
            path.as_ref().display()
        )
    })?;
    let root = leaf_path
        .parent()
        .context("robot/v0 document must have a parent directory")?
        .to_path_buf();
    let mut leaf = read_yaml_value(&leaf_path)?;
    let parents = take_extends(&mut leaf, &leaf_path)?;
    let mut seen = BTreeSet::new();
    let mut composed = serde_yaml::Value::Mapping(Default::default());

    for relative in parents {
        if relative.is_absolute() {
            bail!(
                "robot extends path must be relative: {}",
                relative.display()
            );
        }
        let parent_path = root.join(&relative).canonicalize().with_context(|| {
            format!(
                "failed to resolve robot/v0 parent {} declared by {}",
                relative.display(),
                leaf_path.display()
            )
        })?;
        if !parent_path.starts_with(&root) {
            bail!(
                "robot parent {} escapes robot directory {}",
                relative.display(),
                root.display()
            );
        }
        if parent_path == leaf_path {
            bail!(
                "robot document cannot extend itself: {}",
                relative.display()
            );
        }
        if !seen.insert(parent_path.clone()) {
            bail!("duplicate robot parent: {}", relative.display());
        }

        let mut parent = read_yaml_value(&parent_path)?;
        if !take_extends(&mut parent, &parent_path)?.is_empty() {
            bail!(
                "robot parent {} declares nested extends; list every parent directly in {}",
                parent_path.display(),
                leaf_path.display()
            );
        }
        deep_merge(&mut composed, parent);
    }
    deep_merge(&mut composed, leaf);

    serde_yaml::from_value(composed).with_context(|| {
        format!(
            "failed to parse composed robot/v0 document {}",
            leaf_path.display()
        )
    })
}

pub fn parse_from_string(text: &str) -> Result<v0::Manifest> {
    strict_yaml::check(text).context("failed to parse robot/v0 document")?;
    let manifest: v0::Manifest =
        serde_yaml::from_str(text).context("failed to parse robot/v0 document")?;
    if !manifest.extends.is_empty() {
        bail!(
            "robot extends requires a file path; use \
             source::robot::read_from_path or source::robot::read_from_dir"
        );
    }
    Ok(manifest)
}

pub fn write_to_dir(manifest: &v0::Manifest, path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    std::fs::create_dir_all(path)
        .with_context(|| format!("failed to create robot directory {}", path.display()))?;
    let destination = path.join(ROBOT_FILE);
    std::fs::write(&destination, serde_yaml::to_string(manifest)?).with_context(|| {
        format!(
            "failed to write robot/v0 document {}",
            destination.display()
        )
    })
}

fn read_yaml_value(path: &Path) -> Result<serde_yaml::Value> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read robot/v0 document {}", path.display()))?;
    strict_yaml::check(&text)
        .with_context(|| format!("failed to parse robot/v0 document {}", path.display()))?;
    serde_yaml::from_str(&text)
        .with_context(|| format!("failed to parse robot/v0 document {}", path.display()))
}

fn take_extends(value: &mut serde_yaml::Value, path: &Path) -> Result<Vec<PathBuf>> {
    let serde_yaml::Value::Mapping(map) = value else {
        bail!("robot document {} must be a mapping", path.display());
    };
    let key = serde_yaml::Value::String("extends".to_string());
    let Some(raw) = map.remove(&key) else {
        return Ok(Vec::new());
    };
    serde_yaml::from_value(raw)
        .with_context(|| format!("invalid extends list in {}", path.display()))
}

fn deep_merge(base: &mut serde_yaml::Value, overlay: serde_yaml::Value) {
    match (base, overlay) {
        (serde_yaml::Value::Mapping(base), serde_yaml::Value::Mapping(overlay)) => {
            for (key, value) in overlay {
                match base.get_mut(&key) {
                    Some(current) => deep_merge(current, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (base, overlay) => *base = overlay,
    }
}

fn validation_error(errors: Vec<v0::ValidationError>) -> anyhow::Error {
    anyhow::anyhow!(
        "Robot errors:\n{}",
        errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::{parse_from_string, read_from_dir};

    #[test]
    fn direct_parents_compose_without_a_dispatcher() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::write(
            dir.path().join("base.robot.yaml"),
            "robot:\n  motion_limits:\n    max_linear_speed_mps: 0.5\n",
        )?;
        std::fs::write(
            dir.path().join("robot.yaml"),
            r#"
schema: robot/v0
extends: [base.robot.yaml]
robot:
  id: rover
  namespace: dev
  kinematic: { kind: omnidirectional, actuators: [drive.motor], encoders: [] }
  motion_limits:
    max_angular_speed_radps: 1.0
  components:
    drive:
      component: drive
      mount_link: base_link
"#,
        )?;
        let manifest = read_from_dir(dir.path())?;
        assert!(manifest.extends.is_empty());
        assert_eq!(manifest.robot.motion_limits.max_linear_speed_mps, 0.5);
        Ok(())
    }

    #[test]
    fn string_parser_requires_the_exact_schema() {
        assert!(parse_from_string("schema: robot/v1\n").is_err());
        assert!(parse_from_string("robot: {}\n").is_err());
    }
}

#[cfg(test)]
mod schema_guard {
    use super::v0::Manifest;

    const SCHEMA_PATH: &str = "../examples/robot.schema.json";

    fn generated_schema_json() -> String {
        let schema = schemars::schema_for!(Manifest);
        serde_json::to_string_pretty(&schema).expect("schema serializes to JSON") + "\n"
    }

    fn schema_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(SCHEMA_PATH)
    }

    #[test]
    fn schema_matches_model() {
        let generated = generated_schema_json();
        let checked_in = std::fs::read_to_string(schema_path()).unwrap_or_default();
        assert_eq!(
            generated,
            checked_in,
            "examples/robot.schema.json is stale relative to \
             phoxal::model::source::robot::v0::Manifest; regenerate it with:\n\n  \
             cargo test -p phoxal schema_guard::print_schema -- --ignored --nocapture > {}\n",
            schema_path().display()
        );
    }

    #[test]
    #[ignore = "prints the schema; run explicitly to regenerate examples/robot.schema.json"]
    fn print_schema() {
        print!("{}", generated_schema_json());
    }
}
