pub mod capability;

use crate::model::component::v1::capability::Capability;
use anyhow::{Result, bail};
use derive_new::new;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

pub fn is_valid_token(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return false;
    }
    trimmed.chars().all(|character| {
        character.is_ascii_lowercase()
            || character.is_ascii_digit()
            || character == '_'
            || character == '-'
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, new)]
#[serde(deny_unknown_fields)]
pub struct Component {
    /// Global Trade Item Number identifying the physical part this component
    /// models. Optional: custom/robot-local components need not have one;
    /// catalog components carry it as a stable identity fact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[new(default)]
    pub gtin: Option<Gtin>,
    #[serde(default)]
    pub capabilities: BTreeMap<String, Capability>,
}

/// A Global Trade Item Number (GTIN-13). Validated on parse so a component
/// definition cannot carry a malformed identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Gtin(String);

impl Gtin {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Gtin {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        if trimmed.len() != 13 || !trimmed.chars().all(|character| character.is_ascii_digit()) {
            bail!("invalid GTIN '{}': must be exactly 13 digits", s);
        }
        Ok(Self(trimmed.to_string()))
    }
}

impl TryFrom<String> for Gtin {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<Gtin> for String {
    fn from(value: Gtin) -> Self {
        value.0
    }
}

impl fmt::Display for Gtin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CapabilityRef {
    pub component_id: String,
    pub capability_id: String,
}

impl CapabilityRef {
    pub fn new(component_id: impl Into<String>, capability_id: impl Into<String>) -> Self {
        Self {
            component_id: component_id.into(),
            capability_id: capability_id.into(),
        }
    }
}

impl fmt::Display for CapabilityRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.component_id, self.capability_id)
    }
}

impl FromStr for CapabilityRef {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split('.').collect();
        if parts.len() != 2 {
            bail!(
                "invalid capability reference '{}', must be 'component.capability'",
                s
            );
        }
        if !is_valid_token(parts[0]) || !is_valid_token(parts[1]) {
            bail!(
                "invalid capability reference '{}', component and capability ids must contain only lowercase ASCII letters, digits, '_' or '-'",
                s
            );
        }
        Ok(Self::new(parts[0], parts[1]))
    }
}

impl TryFrom<String> for CapabilityRef {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<CapabilityRef> for String {
    fn from(value: CapabilityRef) -> Self {
        value.to_string()
    }
}

impl Component {
    #[must_use]
    pub fn capability(&self, capability_id: &str) -> Option<&Capability> {
        self.capabilities.get(capability_id)
    }

    pub fn validate(&self) -> Result<()> {
        self.validate_for_component("<inline>")
    }

    pub fn validate_for_component(&self, _component_id: &str) -> Result<()> {
        let mut errors = Vec::new();

        for (capability_id, capability) in &self.capabilities {
            if !is_valid_token(capability_id) {
                errors.push(format!(
                    "capability id '{}' must contain only lowercase ASCII letters, digits, '_' or '-'",
                    capability_id
                ));
            }

            let target_id = match capability.target() {
                capability::StructuralTarget::Joint { id }
                | capability::StructuralTarget::Link { id } => id.trim(),
            };
            if target_id.is_empty() {
                errors.push(format!(
                    "capabilities.{capability_id}.target.id must not be empty"
                ));
            }
            match capability {
                capability::Capability::Camera(camera) => {
                    if camera.width_px == 0 {
                        errors.push(format!("capabilities.{capability_id}.width_px must be > 0"));
                    }
                    if camera.height_px == 0 {
                        errors.push(format!(
                            "capabilities.{capability_id}.height_px must be > 0"
                        ));
                    }
                }
                capability::Capability::Depth(depth) => {
                    if depth.width_px == 0 {
                        errors.push(format!("capabilities.{capability_id}.width_px must be > 0"));
                    }
                    if depth.height_px == 0 {
                        errors.push(format!(
                            "capabilities.{capability_id}.height_px must be > 0"
                        ));
                    }
                }
                _ => {}
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            bail!("{}", errors.join("\n"))
        }
    }
}
