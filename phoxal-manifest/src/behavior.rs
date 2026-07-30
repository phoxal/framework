//! Authored behavior documents and deterministic catalog compilation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const CATALOG_SCHEMA: &str = "phoxal/behavior-catalog/v0";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BehaviorDocument {
    schema: String,
    id: String,
    version: String,
    #[serde(default)]
    inputs: BTreeMap<String, ValueType>,
    #[serde(default)]
    blackboard: BTreeMap<String, ValueType>,
    root: Node,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ValueType {
    Bool,
    Integer,
    Number,
    String,
    Pose,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Node {
    Sequence {
        id: String,
        children: Vec<Node>,
    },
    Selector {
        id: String,
        children: Vec<Node>,
    },
    ReactiveSelector {
        id: String,
        children: Vec<Node>,
    },
    Condition {
        id: String,
        condition: String,
        #[serde(default)]
        args: BTreeMap<String, serde_json::Value>,
    },
    Action {
        id: String,
        action: String,
        #[serde(default)]
        args: BTreeMap<String, serde_json::Value>,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    Wait {
        id: String,
        duration_ms: u64,
    },
    Timeout {
        id: String,
        timeout_ms: u64,
        child: Box<Node>,
    },
    Retry {
        id: String,
        attempts: u32,
        child: Box<Node>,
    },
    Subtree {
        id: String,
        behavior: String,
        #[serde(default)]
        args: BTreeMap<String, serde_json::Value>,
    },
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct CompiledCatalog {
    schema: &'static str,
    definitions: BTreeMap<String, CompiledDefinition>,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct CompiledDefinition {
    content_hash: String,
    authored: BehaviorDocument,
}

pub(crate) fn compile(root: &Path) -> Result<Vec<u8>> {
    let mut files = Vec::new();
    collect_yaml(&root.join("behaviors"), &mut files)?;
    files.sort();
    let mut definitions = BTreeMap::new();
    for path in files {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read behavior {}", path.display()))?;
        let authored: BehaviorDocument = serde_yaml::from_str(&text)
            .with_context(|| format!("failed to parse behavior {}", path.display()))?;
        if authored.schema != "behavior/v0" {
            bail!(
                "{}: unsupported behavior schema '{}'",
                path.display(),
                authored.schema
            );
        }
        validate_identifier(&authored.id, "behavior id")?;
        validate_node(&authored.root)?;
        let canonical = serde_json::to_vec(&authored)?;
        let content_hash = format!("sha256:{:x}", Sha256::digest(canonical));
        let id = authored.id.clone();
        if definitions
            .insert(
                id.clone(),
                CompiledDefinition {
                    content_hash,
                    authored,
                },
            )
            .is_some()
        {
            bail!(
                "duplicate behavior id '{id}' (latest file: {})",
                path.display()
            );
        }
    }
    let mut bytes = serde_json::to_vec_pretty(&CompiledCatalog {
        schema: CATALOG_SCHEMA,
        definitions,
    })
    .context("failed to encode compiled behavior catalog")?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn validate_node(node: &Node) -> Result<()> {
    let (id, children): (&str, &[Node]) = match node {
        Node::Sequence { id, children }
        | Node::Selector { id, children }
        | Node::ReactiveSelector { id, children } => (id, children),
        Node::Timeout { id, child, .. } | Node::Retry { id, child, .. } => {
            validate_node(child)?;
            (id, &[])
        }
        Node::Condition { id, .. }
        | Node::Action { id, .. }
        | Node::Wait { id, .. }
        | Node::Subtree { id, .. } => (id, &[]),
    };
    validate_identifier(id, "behavior node id")?;
    for child in children {
        validate_node(child)?;
    }
    Ok(())
}

fn collect_yaml(path: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(path)
        .with_context(|| format!("failed to read behavior directory {}", path.display()))?
    {
        let path = entry?.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            bail!(
                "behavior source tree contains forbidden symlink {}",
                path.display()
            );
        }
        if metadata.is_dir() {
            collect_yaml(&path, out)?;
        } else if matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("yaml" | "yml")
        ) {
            out.push(path);
        }
    }
    Ok(())
}

fn validate_identifier(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
    {
        bail!("{label} '{value}' must use only ASCII letters, digits, '.', '_' or '-'");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOLDEN: &[u8] = include_bytes!("../tests/golden/behavior-catalog.json");

    #[test]
    fn compiled_catalog_matches_the_cross_crate_golden() {
        let root = tempfile::tempdir().unwrap();
        let behaviors = root.path().join("behaviors");
        std::fs::create_dir(&behaviors).unwrap();
        std::fs::write(
            behaviors.join("root.yaml"),
            "schema: behavior/v0\nid: system.root\nversion: \"1\"\nroot:\n  type: wait\n  id: settle\n  duration_ms: 10\n",
        )
        .unwrap();
        assert_eq!(compile(root.path()).unwrap(), GOLDEN);
    }
}
