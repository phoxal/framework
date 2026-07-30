//! Service-owned canonical behavior catalog and execution schema.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

const CATALOG_SCHEMA: &str = "phoxal/behavior-catalog/v0";

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BehaviorDefinition {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BehaviorCatalog {
    schema: String,
    definitions: BTreeMap<String, BehaviorDefinition>,
}

impl Default for BehaviorCatalog {
    fn default() -> Self {
        Self {
            schema: CATALOG_SCHEMA.to_string(),
            definitions: BTreeMap::new(),
        }
    }
}

impl BehaviorCatalog {
    /// Strictly decode the compiled runtime catalog.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let catalog: Self =
            serde_json::from_slice(bytes).context("failed to decode behavior catalog")?;
        if catalog.schema != CATALOG_SCHEMA {
            bail!(
                "unsupported behavior catalog schema '{}'; expected '{CATALOG_SCHEMA}'",
                catalog.schema
            );
        }
        catalog.validate()?;
        Ok(catalog)
    }

    #[cfg(test)]
    pub(crate) fn from_test_documents(documents: &[&str]) -> Result<Self> {
        let mut definitions = BTreeMap::new();
        for document in documents {
            let authored: BehaviorDefinitionFile = serde_json::from_str(document)?;
            let id = authored.id.clone();
            definitions.insert(
                id,
                BehaviorDefinition {
                    content_hash: "test-only".to_string(),
                    authored,
                },
            );
        }
        let catalog = Self {
            schema: CATALOG_SCHEMA.to_string(),
            definitions,
        };
        catalog.validate()?;
        Ok(catalog)
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<&BehaviorDefinition> {
        self.definitions.get(id)
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

    const GOLDEN: &[u8] =
        include_bytes!("../../../phoxal-manifest/tests/golden/behavior-catalog.json");

    #[test]
    fn manifest_compiler_golden_is_accepted_by_the_runtime_consumer() {
        let catalog = BehaviorCatalog::decode(GOLDEN).unwrap();
        assert!(catalog.get("system.root").is_some());
    }
}
