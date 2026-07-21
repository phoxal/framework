//! Authored behavior definitions, the official action/condition registry, and
//! deterministic robot-root catalog validation.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const BEHAVIORS_DIR: &str = "behaviors";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BehaviorDefinitionFile {
    pub schema: String,
    pub id: String,
    pub version: String,
    #[serde(default)]
    pub inputs: BTreeMap<String, ValueType>,
    #[serde(default)]
    pub blackboard: BTreeMap<String, ValueType>,
    pub root: Node,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BehaviorDefinition {
    pub source: PathBuf,
    pub content_hash: String,
    pub authored: BehaviorDefinitionFile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueType {
    Bool,
    Integer,
    Number,
    String,
    Pose,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Node {
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

impl Node {
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Sequence { id, .. }
            | Self::Selector { id, .. }
            | Self::ReactiveSelector { id, .. }
            | Self::Condition { id, .. }
            | Self::Action { id, .. }
            | Self::Wait { id, .. }
            | Self::Timeout { id, .. }
            | Self::Retry { id, .. }
            | Self::Subtree { id, .. } => id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionSpec {
    pub id: &'static str,
    pub args: &'static [(&'static str, ValueType, bool)],
    pub claims: &'static [&'static str],
    pub cancellable: bool,
    pub timeout_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConditionSpec {
    pub id: &'static str,
    pub args: &'static [(&'static str, ValueType, bool)],
    pub reads: &'static [&'static str],
}

pub const ACTIONS: &[ActionSpec] = &[
    ActionSpec {
        id: "behavior.idle",
        args: &[],
        claims: &[],
        cancellable: false,
        timeout_required: false,
    },
    ActionSpec {
        id: "behavior.dispatch_request",
        args: &[],
        claims: &[],
        cancellable: true,
        timeout_required: false,
    },
    ActionSpec {
        id: "navigation.goto_pose",
        args: &[("pose", ValueType::Pose, true)],
        claims: &["navigation"],
        cancellable: true,
        timeout_required: true,
    },
    ActionSpec {
        id: "navigation.stop",
        args: &[],
        claims: &["navigation"],
        cancellable: false,
        timeout_required: false,
    },
    ActionSpec {
        id: "host.shutdown",
        args: &[],
        claims: &["host_power"],
        cancellable: false,
        timeout_required: false,
    },
];

pub const CONDITIONS: &[ConditionSpec] = &[
    ConditionSpec {
        id: "motion.manual_active",
        args: &[],
        reads: &["v0.1/motion/state"],
    },
    ConditionSpec {
        id: "localization.confident",
        args: &[("min_confidence", ValueType::Number, true)],
        reads: &["v0.1/localize/state"],
    },
    ConditionSpec {
        id: "map.ready",
        args: &[],
        reads: &["v0.1/map/revision"],
    },
    ConditionSpec {
        id: "safety.clear",
        args: &[],
        reads: &["v0.1/safety/state"],
    },
];

#[derive(Debug, Clone, Default)]
pub struct BehaviorCatalog {
    definitions: BTreeMap<String, BehaviorDefinition>,
}

impl BehaviorCatalog {
    pub fn load(robot_root: &Path) -> Result<Self> {
        let root = robot_root.join(BEHAVIORS_DIR);
        let mut files = Vec::new();
        collect_yaml(&root, &mut files)?;
        files.sort();
        let mut definitions = BTreeMap::new();
        for path in files {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read behavior {}", path.display()))?;
            let authored: BehaviorDefinitionFile = serde_yaml::from_str(&text)
                .with_context(|| format!("failed to parse behavior {}", path.display()))?;
            if authored.schema != "behavior/v0" {
                bail!(
                    "{}: unsupported behavior schema '{}'",
                    path.display(),
                    authored.schema
                );
            }
            validate_identifier(&authored.id, "behavior id")?;
            let canonical = serde_json::to_vec(&authored)?;
            let content_hash = format!("sha256:{:x}", Sha256::digest(canonical));
            let id = authored.id.clone();
            if definitions
                .insert(
                    id.clone(),
                    BehaviorDefinition {
                        source: path.clone(),
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
        let catalog = Self { definitions };
        catalog.validate()?;
        Ok(catalog)
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<&BehaviorDefinition> {
        self.definitions.get(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &BehaviorDefinition)> {
        self.definitions
            .iter()
            .map(|(id, definition)| (id.as_str(), definition))
    }

    pub fn validate_root(&self, root: &str) -> Result<()> {
        let definition = self
            .get(root)
            .with_context(|| format!("configured behavior root '{root}' does not exist"))?;
        if !definition.authored.inputs.is_empty() {
            bail!("root behavior '{root}' requires arguments; roots must be argument-free in v0");
        }
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        for (id, definition) in &self.definitions {
            let mut node_ids = BTreeSet::new();
            validate_node(
                id,
                &definition.authored.inputs,
                &definition.authored.root,
                &mut node_ids,
                self,
            )?;
        }
        for id in self.definitions.keys() {
            detect_cycle(id, id, self, &mut Vec::new())?;
        }
        Ok(())
    }
}

fn validate_node(
    behavior_id: &str,
    inputs: &BTreeMap<String, ValueType>,
    node: &Node,
    node_ids: &mut BTreeSet<String>,
    catalog: &BehaviorCatalog,
) -> Result<()> {
    validate_identifier(node.id(), "node id")?;
    if !node_ids.insert(node.id().to_string()) {
        bail!(
            "behavior '{behavior_id}' has duplicate node id '{}'",
            node.id()
        );
    }
    match node {
        Node::Sequence { children, .. }
        | Node::Selector { children, .. }
        | Node::ReactiveSelector { children, .. } => {
            if children.is_empty() {
                bail!(
                    "behavior '{behavior_id}' node '{}' must have children",
                    node.id()
                );
            }
            for child in children {
                validate_node(behavior_id, inputs, child, node_ids, catalog)?;
            }
        }
        Node::Condition {
            condition, args, ..
        } => {
            let spec = CONDITIONS.iter().find(|spec| spec.id == condition).with_context(|| {
                format!(
                    "behavior '{behavior_id}' node '{}': unknown condition '{condition}'; available: {}",
                    node.id(),
                    CONDITIONS.iter().map(|spec| spec.id).collect::<Vec<_>>().join(", ")
                )
            })?;
            validate_args(behavior_id, node.id(), inputs, args, spec.args)?;
        }
        Node::Action {
            action,
            args,
            timeout_ms,
            ..
        } => {
            let spec = ACTIONS.iter().find(|spec| spec.id == action).with_context(|| {
                format!(
                    "behavior '{behavior_id}' node '{}': unknown action '{action}'; available: {}",
                    node.id(),
                    ACTIONS.iter().map(|spec| spec.id).collect::<Vec<_>>().join(", ")
                )
            })?;
            validate_args(behavior_id, node.id(), inputs, args, spec.args)?;
            if spec.timeout_required && timeout_ms.is_none() {
                bail!(
                    "behavior '{behavior_id}' node '{}': action '{action}' requires timeout_ms",
                    node.id()
                );
            }
        }
        Node::Wait { duration_ms, .. } if *duration_ms == 0 => {
            bail!(
                "behavior '{behavior_id}' node '{}': duration_ms must be > 0",
                node.id()
            );
        }
        Node::Timeout {
            timeout_ms, child, ..
        } => {
            if *timeout_ms == 0 {
                bail!(
                    "behavior '{behavior_id}' node '{}': timeout_ms must be > 0",
                    node.id()
                );
            }
            validate_node(behavior_id, inputs, child, node_ids, catalog)?;
        }
        Node::Retry {
            attempts, child, ..
        } => {
            if *attempts == 0 {
                bail!(
                    "behavior '{behavior_id}' node '{}': attempts must be > 0",
                    node.id()
                );
            }
            validate_node(behavior_id, inputs, child, node_ids, catalog)?;
        }
        Node::Subtree { behavior, args, .. } => {
            let target = catalog.get(behavior).with_context(|| {
                format!(
                    "behavior '{behavior_id}' node '{}': missing subtree '{behavior}'",
                    node.id()
                )
            })?;
            for (required, kind) in &target.authored.inputs {
                let Some(value) = args.get(required) else {
                    bail!(
                        "behavior '{behavior_id}' node '{}': subtree '{behavior}' requires arg '{required}'",
                        node.id()
                    );
                };
                validate_bound_value(behavior_id, node.id(), inputs, required, value, *kind)?;
            }
            for name in args.keys() {
                if !target.authored.inputs.contains_key(name) {
                    bail!(
                        "behavior '{behavior_id}' node '{}': subtree '{behavior}' has unknown arg '{name}'",
                        node.id()
                    );
                }
            }
        }
        Node::Wait { .. } => {}
    }
    Ok(())
}

fn validate_args(
    behavior_id: &str,
    node_id: &str,
    inputs: &BTreeMap<String, ValueType>,
    args: &BTreeMap<String, serde_json::Value>,
    specs: &[(&str, ValueType, bool)],
) -> Result<()> {
    for (name, kind, required) in specs {
        let Some(value) = args.get(*name) else {
            if *required {
                bail!("behavior '{behavior_id}' node '{node_id}': missing required arg '{name}'");
            }
            continue;
        };
        validate_bound_value(behavior_id, node_id, inputs, name, value, *kind)?;
    }
    for name in args.keys() {
        if !specs.iter().any(|(known, _, _)| name == known) {
            bail!("behavior '{behavior_id}' node '{node_id}': unknown arg '{name}'");
        }
    }
    Ok(())
}

fn validate_bound_value(
    behavior_id: &str,
    node_id: &str,
    inputs: &BTreeMap<String, ValueType>,
    name: &str,
    value: &serde_json::Value,
    kind: ValueType,
) -> Result<()> {
    if let Some(reference) = input_reference(value) {
        let actual = inputs.get(reference).with_context(|| {
            format!(
                "behavior '{behavior_id}' node '{node_id}': arg '{name}' references unknown input '{reference}'"
            )
        })?;
        if *actual != kind {
            bail!(
                "behavior '{behavior_id}' node '{node_id}': arg '{name}' expects {kind:?}, but input '{reference}' is {actual:?}"
            );
        }
    } else if !value_matches(value, kind) {
        bail!("behavior '{behavior_id}' node '{node_id}': arg '{name}' must be {kind:?}");
    }
    Ok(())
}

fn input_reference(value: &serde_json::Value) -> Option<&str> {
    value
        .as_str()
        .and_then(|value| value.strip_prefix("${input."))
        .and_then(|value| value.strip_suffix('}'))
}

fn value_matches(value: &serde_json::Value, kind: ValueType) -> bool {
    match kind {
        ValueType::Bool => value.is_boolean(),
        ValueType::Integer => value.as_i64().is_some(),
        ValueType::Number => value.is_number(),
        ValueType::String => value.is_string(),
        ValueType::Pose => value.as_object().is_some_and(|object| {
            object.get("x_m").is_some_and(serde_json::Value::is_number)
                && object.get("y_m").is_some_and(serde_json::Value::is_number)
        }),
    }
}

fn detect_cycle(
    origin: &str,
    current: &str,
    catalog: &BehaviorCatalog,
    stack: &mut Vec<String>,
) -> Result<()> {
    if stack.iter().any(|id| id == current) {
        stack.push(current.to_string());
        bail!(
            "behavior subtree cycle from '{origin}': {}",
            stack.join(" -> ")
        );
    }
    stack.push(current.to_string());
    let definition = catalog.get(current).expect("validated subtree exists");
    let mut subtrees = Vec::new();
    collect_subtrees(&definition.authored.root, &mut subtrees);
    for child in subtrees {
        detect_cycle(origin, child, catalog, stack)?;
    }
    stack.pop();
    Ok(())
}

fn collect_subtrees<'a>(node: &'a Node, out: &mut Vec<&'a str>) {
    match node {
        Node::Sequence { children, .. }
        | Node::Selector { children, .. }
        | Node::ReactiveSelector { children, .. } => {
            for child in children {
                collect_subtrees(child, out);
            }
        }
        Node::Timeout { child, .. } | Node::Retry { child, .. } => collect_subtrees(child, out),
        Node::Subtree { behavior, .. } => out.push(behavior),
        Node::Condition { .. } | Node::Action { .. } | Node::Wait { .. } => {}
    }
}

fn collect_yaml(path: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(path)
        .with_context(|| format!("failed to read behavior directory {}", path.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
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

    fn write(root: &Path, name: &str, contents: &str) {
        let path = root.join("behaviors").join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn catalog_hashes_and_validates_a_deterministic_definition() {
        let temp = tempfile::tempdir().unwrap();
        write(
            temp.path(),
            "root.yaml",
            r#"
schema: behavior/v0
id: system.root
version: 1.0.0
root:
  type: sequence
  id: root
  children:
    - type: wait
      id: settle
      duration_ms: 10
    - type: condition
      id: map_ready
      condition: map.ready
"#,
        );
        let first = BehaviorCatalog::load(temp.path()).unwrap();
        let second = BehaviorCatalog::load(temp.path()).unwrap();
        let first = first.get("system.root").unwrap();
        let second = second.get("system.root").unwrap();
        assert_eq!(first.content_hash, second.content_hash);
        assert!(first.content_hash.starts_with("sha256:"));
    }

    #[test]
    fn validation_names_unknown_registry_entries() {
        let temp = tempfile::tempdir().unwrap();
        write(
            temp.path(),
            "bad.yaml",
            r#"
schema: behavior/v0
id: bad
version: 1
root:
  type: action
  id: typo
  action: navigation.goto_zone
"#,
        );
        let error = BehaviorCatalog::load(temp.path()).unwrap_err().to_string();
        assert!(error.contains("unknown action 'navigation.goto_zone'"));
        assert!(error.contains("navigation.goto_pose"));
    }

    #[test]
    fn subtree_cycles_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        write(
            temp.path(),
            "a.yaml",
            "schema: behavior/v0\nid: a\nversion: 1\nroot: { type: subtree, id: call_b, behavior: b }\n",
        );
        write(
            temp.path(),
            "b.yaml",
            "schema: behavior/v0\nid: b\nversion: 1\nroot: { type: subtree, id: call_a, behavior: a }\n",
        );
        let error = BehaviorCatalog::load(temp.path()).unwrap_err().to_string();
        assert!(error.contains("subtree cycle"));
    }

    #[test]
    fn subtree_args_are_typed_and_may_bind_parent_inputs() {
        let temp = tempfile::tempdir().unwrap();
        write(
            temp.path(),
            "child.yaml",
            "schema: behavior/v0\nid: child\nversion: 1\ninputs: { target: pose }\nroot:\n  type: action\n  id: goto\n  action: navigation.goto_pose\n  timeout_ms: 1000\n  args: { pose: '${input.target}' }\n",
        );
        write(
            temp.path(),
            "parent.yaml",
            "schema: behavior/v0\nid: parent\nversion: 1\ninputs: { destination: pose }\nroot:\n  type: subtree\n  id: child\n  behavior: child\n  args: { target: '${input.destination}' }\n",
        );
        assert!(BehaviorCatalog::load(temp.path()).is_ok());

        write(
            temp.path(),
            "parent.yaml",
            "schema: behavior/v0\nid: parent\nversion: 1\ninputs: { destination: string }\nroot:\n  type: subtree\n  id: child\n  behavior: child\n  args: { target: '${input.destination}', extra: 1 }\n",
        );
        let error = BehaviorCatalog::load(temp.path()).unwrap_err().to_string();
        assert!(error.contains("expects Pose") || error.contains("unknown arg"));
    }
}
