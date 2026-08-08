//! Versioned authored `robot.yaml` documents.
//!
//! The parser enforces a strict source contract: DTO structs deny unknown
//! fields, `serde_yaml` rejects duplicate mapping keys and does not coerce
//! YAML-1.1-only booleans (`yes`, `no`, `on`, `off`) into typed boolean fields,
//! and the strict YAML reader additionally rejects anchors, aliases, merge
//! keys, explicit tags, and multi-document streams.
//!
//! A leaf document may compose ordered parents through `extends`. Parents are
//! resolved relative to the leaf's own directory, which is why composition only
//! exists on [`Manifest::load`]: text handed to [`Manifest::parse`] has no
//! directory to resolve against.

pub mod v0;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::source::document::{ComposeError, Document, DocumentKind, Origin, SourceError};
use crate::source::{document_path, strict_yaml};

/// A versioned authored `robot.yaml` document; the schema tag selects the variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "schema")]
pub enum Manifest {
    #[serde(rename = "phoxal/robot/v0")]
    V0(v0::Manifest),
}

impl Document for Manifest {
    const KIND: DocumentKind = DocumentKind::Robot;

    fn check(&self) -> Result<(), super::Violations> {
        let Self::V0(body) = self;
        body.validate().map_err(super::Violations::Robot)
    }

    fn precheck(text: &str, origin: &Origin) -> Result<(), SourceError> {
        strict_yaml::check(text).map_err(|source| SourceError::StrictYaml {
            kind: Self::KIND,
            origin: origin.clone(),
            source,
        })
    }
}

impl Manifest {
    /// Parse and validate one complete robot document from its exact text.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::UnresolvableExtends`] when the text declares
    /// parents, which only [`Manifest::load`] can resolve, plus whatever the
    /// strict parse and the document's own rules reject.
    pub fn parse(text: &str) -> Result<Self, SourceError> {
        let manifest = Self::read_text(text, Origin::Text)?;
        let Self::V0(body) = &manifest;
        if !body.extends.is_empty() {
            return Err(SourceError::UnresolvableExtends {
                kind: Self::KIND,
                origin: Origin::Text,
            });
        }
        Ok(manifest)
    }

    /// Read, compose and validate the robot document at `path`, which is either
    /// the document file itself or the directory that holds it.
    ///
    /// Declared parents are deep-merged in declaration order and the leaf wins;
    /// sequence and scalar values replace earlier values. Nested composition is
    /// refused so the leaf stays the single deterministic authority for parent
    /// order.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Read`], [`SourceError::StrictYaml`],
    /// [`SourceError::Compose`], [`SourceError::Parse`] or
    /// [`SourceError::Invalid`], in the order those stages run.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, SourceError> {
        let leaf = document_path(path.as_ref(), Self::KIND);
        let leaf = leaf.canonicalize().map_err(|source| SourceError::Read {
            kind: Self::KIND,
            path: leaf.clone(),
            source,
        })?;
        let composed = compose(&leaf)?;
        let origin = Origin::File(leaf);
        let manifest: Self =
            serde_yaml::from_value(composed).map_err(|source| SourceError::Parse {
                kind: Self::KIND,
                origin: origin.clone(),
                source,
            })?;
        manifest
            .check()
            .map_err(|violations| SourceError::Invalid { origin, violations })?;
        Ok(manifest)
    }

    /// Write the document into `directory` as `robot.yaml`.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Write`] when the directory or file cannot be
    /// written.
    pub fn write_to_dir(&self, directory: impl AsRef<Path>) -> Result<(), SourceError> {
        self.write_dir(directory.as_ref())
    }
}

/// Merge every direct parent of `leaf`, in declaration order, under the leaf.
fn compose(leaf: &Path) -> Result<serde_yaml::Value, SourceError> {
    let root = leaf
        .parent()
        .unwrap_or(Path::new("."))
        .canonicalize()
        .map_err(|source| SourceError::Read {
            kind: DocumentKind::Robot,
            path: leaf.to_path_buf(),
            source,
        })?;

    let compose = || -> Result<serde_yaml::Value, ComposeError> {
        let mut document = read_value(leaf)?;
        let parents = take_extends(&mut document, leaf)?;
        let mut seen = BTreeSet::new();
        let mut composed = serde_yaml::Value::Mapping(serde_yaml::Mapping::default());

        for relative in parents {
            if relative.is_absolute() {
                return Err(ComposeError::AbsoluteParent { path: relative });
            }
            let parent = root.join(&relative).canonicalize().map_err(|source| {
                ComposeError::UnresolvedParent {
                    path: relative.clone(),
                    source,
                }
            })?;
            if !parent.starts_with(&root) {
                return Err(ComposeError::EscapingParent {
                    path: relative,
                    root: root.clone(),
                });
            }
            if parent == leaf {
                return Err(ComposeError::SelfParent { path: relative });
            }
            if !seen.insert(parent.clone()) {
                return Err(ComposeError::DuplicateParent { path: relative });
            }

            let mut value = read_value(&parent)?;
            if !take_extends(&mut value, &parent)?.is_empty() {
                return Err(ComposeError::NestedExtends { path: parent });
            }
            deep_merge(&mut composed, value);
        }
        deep_merge(&mut composed, document);
        Ok(composed)
    };

    compose().map_err(|source| SourceError::Compose {
        kind: DocumentKind::Robot,
        leaf: leaf.to_path_buf(),
        source,
    })
}

/// Read one document as an untyped YAML value, after the strict-subset check.
///
/// Composition happens before the typed parse, so a parent that is not a robot
/// document on its own - a fragment carrying only `robot.motion_limits`, say -
/// still composes.
fn read_value(path: &Path) -> Result<serde_yaml::Value, ComposeError> {
    let text = std::fs::read_to_string(path).map_err(|source| ComposeError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    strict_yaml::check(&text).map_err(|source| ComposeError::StrictYaml {
        path: path.to_path_buf(),
        source,
    })?;
    serde_yaml::from_str(&text).map_err(|source| ComposeError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

fn take_extends(value: &mut serde_yaml::Value, path: &Path) -> Result<Vec<PathBuf>, ComposeError> {
    let serde_yaml::Value::Mapping(map) = value else {
        return Err(ComposeError::NotAMapping {
            path: path.to_path_buf(),
        });
    };
    let key = serde_yaml::Value::String("extends".to_string());
    let Some(raw) = map.remove(&key) else {
        return Ok(Vec::new());
    };
    serde_yaml::from_value(raw).map_err(|source| ComposeError::MalformedExtends {
        path: path.to_path_buf(),
        source,
    })
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

#[cfg(test)]
mod tests {
    use super::Manifest;

    const COMPOSED_LEAF: &str = r#"
schema: phoxal/robot/v0
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
schema: phoxal/robot/v0
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
        let Manifest::V0(manifest) = Manifest::load(dir.path())?;
        assert!(manifest.extends.is_empty());
        assert_eq!(manifest.robot.motion_limits.max_linear_speed_mps, 0.5);
        Ok(())
    }

    #[test]
    fn string_parser_requires_the_exact_schema() {
        assert!(Manifest::parse("schema: phoxal/robot/v1\n").is_err());
        assert!(Manifest::parse("robot: {}\n").is_err());
    }

    /// The schema tag alone selects the variant: a structurally valid body
    /// tagged with a future version fails on the tag, not the body, and a
    /// round trip through the enum keeps `schema:` as the first emitted key.
    #[test]
    fn future_schema_tag_fails_on_the_variant_not_the_body() -> anyhow::Result<()> {
        let valid_body_future_tag =
            COMPOSED_LEAF.replacen("schema: phoxal/robot/v0", "schema: phoxal/robot/v1", 1);
        let error = Manifest::parse(&valid_body_future_tag)
            .expect_err("a valid body under an unknown schema tag must still fail");
        assert!(
            error.to_string().contains("phoxal/robot/v1"),
            "got: {error}"
        );

        let manifest = Manifest::parse(
            COMPOSED_LEAF
                .replacen("extends: [base.robot.yaml, host.robot.yaml]\n", "", 1)
                .as_str(),
        )?;
        let serialized = serde_yaml::to_string(&manifest)?;
        assert!(
            serialized.starts_with("schema: phoxal/robot/v0\n"),
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
        let error = Manifest::load(dir.path()).expect_err("nested extends must fail");
        assert!(
            error.to_string().contains("declares nested extends"),
            "{error}"
        );
        Ok(())
    }

    /// Parents name paths, so they only exist relative to a document on disk.
    #[test]
    fn parsing_text_that_declares_parents_is_refused() {
        let error =
            Manifest::parse(COMPOSED_LEAF).expect_err("text parsing cannot resolve parents");
        assert!(
            matches!(error, super::SourceError::UnresolvableExtends { .. }),
            "{error}"
        );
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
        let duplicate = Manifest::load(&robot_dir).expect_err("duplicate parent must fail");
        assert!(
            duplicate.to_string().contains("duplicate parent"),
            "{duplicate}"
        );

        std::fs::write(
            robot_dir.join("robot.yaml"),
            COMPOSED_LEAF.replace("base.robot.yaml, host.robot.yaml", "../outside.robot.yaml"),
        )?;
        std::fs::write(temp.path().join("outside.robot.yaml"), "{}\n")?;
        let escaping = Manifest::load(&robot_dir).expect_err("escaping parent must fail");
        assert!(
            escaping.to_string().contains("escapes directory"),
            "{escaping}"
        );
        Ok(())
    }
}
