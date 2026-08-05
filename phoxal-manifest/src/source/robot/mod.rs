//! Versioned authored `robot.yaml` documents.
//!
//! The parser deliberately enforces a strict source contract: DTO structs deny
//! unknown fields, `serde_yaml` rejects duplicate mapping keys and does not
//! coerce YAML-1.1-only booleans (`yes`, `no`, `on`, `off`) into typed boolean
//! fields, and the strict YAML reader additionally rejects anchors, aliases, merge
//! keys, explicit tags, and multi-document streams.

pub mod v0;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

mod strict_yaml;

const ROBOT_FILE: &str = "robot.yaml";

/// A versioned authored `robot.yaml` document; the schema tag selects the variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "schema")]
pub enum Manifest {
    #[serde(rename = "robot/v0")]
    V0(v0::Manifest),
}

pub fn read_from_dir(path: impl AsRef<Path>) -> Result<Manifest> {
    let path = path.as_ref();
    let manifest = parse_from_dir(path)?;
    let location = path.join(ROBOT_FILE);
    let Manifest::V0(body) = &manifest;
    body.validate()
        .map_err(|errors| validation_error(&location.display().to_string(), errors))?;
    Ok(manifest)
}

pub fn read_from_path(path: impl AsRef<Path>) -> Result<Manifest> {
    let path = path.as_ref();
    let manifest = parse_from_path(path)?;
    let Manifest::V0(body) = &manifest;
    body.validate()
        .map_err(|errors| validation_error(&path.display().to_string(), errors))?;
    Ok(manifest)
}

pub fn read_from_string(text: &str) -> Result<Manifest> {
    let manifest = parse_from_string(text)?;
    let Manifest::V0(body) = &manifest;
    body.validate()
        .map_err(|errors| validation_error("<inline robot.yaml>", errors))?;
    Ok(manifest)
}

pub fn parse_from_dir(path: impl AsRef<Path>) -> Result<Manifest> {
    parse_from_path(path.as_ref().join(ROBOT_FILE))
}

/// Read one leaf document and compose its ordered direct parents.
///
/// Parent maps merge recursively in declaration order. Sequences and scalar
/// values replace earlier values. Nested composition is rejected so the leaf
/// remains the single deterministic authority for parent order.
pub fn parse_from_path(path: impl AsRef<Path>) -> Result<Manifest> {
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

pub fn parse_from_string(text: &str) -> Result<Manifest> {
    strict_yaml::check(text).context("failed to parse robot/v0 document")?;
    let manifest: Manifest =
        serde_yaml::from_str(text).context("failed to parse robot/v0 document")?;
    let Manifest::V0(body) = &manifest;
    if !body.extends.is_empty() {
        bail!(
            "robot extends requires a file path; use \
             source::robot::read_from_path or source::robot::read_from_dir"
        );
    }
    Ok(manifest)
}

pub fn write_to_dir(manifest: &Manifest, path: impl AsRef<Path>) -> Result<()> {
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

pub(crate) fn validation_error(location: &str, errors: Vec<v0::ValidationError>) -> anyhow::Error {
    anyhow::anyhow!(
        "invalid robot/v0 document {location}:\n{}",
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

    const COMPOSED_LEAF: &str = r#"
schema: robot/v0
extends: [base.robot.yaml, host.robot.yaml]
robot:
  id: rover
  namespace: dev
  kinematic: { kind: omnidirectional, actuators: [drive.motor], encoders: [] }
  motion_limits: { max_linear_speed_mps: 0.5, max_angular_speed_radps: 1.0 }
  components:
    drive:
      component: drive
      mount_link: base_link
"#;

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
        let super::Manifest::V0(manifest) = manifest;
        assert!(manifest.extends.is_empty());
        assert_eq!(manifest.robot.motion_limits.max_linear_speed_mps, 0.5);
        Ok(())
    }

    #[test]
    fn string_parser_requires_the_exact_schema() {
        assert!(parse_from_string("schema: robot/v1\n").is_err());
        assert!(parse_from_string("robot: {}\n").is_err());
    }

    /// The schema tag alone selects the variant: a structurally valid body
    /// tagged with a future version fails on the tag, not the body, and a
    /// round trip through the enum keeps `schema:` as the first emitted key.
    #[test]
    fn future_schema_tag_fails_on_the_variant_not_the_body() -> anyhow::Result<()> {
        let valid_body_future_tag =
            COMPOSED_LEAF.replacen("schema: robot/v0", "schema: robot/v1", 1);
        let error = parse_from_string(&valid_body_future_tag)
            .expect_err("a valid body under an unknown schema tag must still fail");
        assert!(format!("{error:#}").contains("robot/v1"), "got: {error:#}");

        let manifest = parse_from_string(
            COMPOSED_LEAF
                .replacen("extends: [base.robot.yaml, host.robot.yaml]\n", "", 1)
                .as_str(),
        )?;
        let serialized = serde_yaml::to_string(&manifest)?;
        assert!(
            serialized.starts_with("schema: robot/v0\n"),
            "schema tag must round-trip as the first key: {serialized}"
        );
        Ok(())
    }

    #[test]
    fn nested_parent_extends_is_rejected() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::write(
            dir.path().join("base.robot.yaml"),
            "extends: [other.robot.yaml]\n",
        )?;
        std::fs::write(dir.path().join("host.robot.yaml"), "{}\n")?;
        std::fs::write(dir.path().join("robot.yaml"), COMPOSED_LEAF)?;
        let error = read_from_dir(dir.path()).expect_err("nested extends must fail");
        assert!(format!("{error:#}").contains("declares nested extends"));
        Ok(())
    }

    #[test]
    fn string_parser_rejects_unresolvable_extends() {
        let error =
            parse_from_string(COMPOSED_LEAF).expect_err("string parsing cannot resolve parents");
        assert!(format!("{error:#}").contains(
            "robot extends requires a file path; use \
                 source::robot::read_from_path or source::robot::read_from_dir"
        ));
    }

    #[test]
    fn duplicate_and_escaping_parents_are_rejected() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let robot_dir = temp.path().join("robot");
        std::fs::create_dir(&robot_dir)?;
        std::fs::write(robot_dir.join("base.robot.yaml"), "{}\n")?;
        std::fs::write(robot_dir.join("host.robot.yaml"), "{}\n")?;
        std::fs::write(
            robot_dir.join("robot.yaml"),
            COMPOSED_LEAF.replace(
                "base.robot.yaml, host.robot.yaml",
                "base.robot.yaml, base.robot.yaml",
            ),
        )?;
        let duplicate = read_from_dir(&robot_dir).expect_err("duplicate parent must fail");
        assert!(format!("{duplicate:#}").contains("duplicate robot parent"));

        std::fs::write(
            robot_dir.join("robot.yaml"),
            COMPOSED_LEAF.replace("base.robot.yaml, host.robot.yaml", "../outside.robot.yaml"),
        )?;
        std::fs::write(temp.path().join("outside.robot.yaml"), "{}\n")?;
        let escaping = read_from_dir(&robot_dir).expect_err("escaping parent must fail");
        assert!(format!("{escaping:#}").contains("escapes robot directory"));
        Ok(())
    }
}
