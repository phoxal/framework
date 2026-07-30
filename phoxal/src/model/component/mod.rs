//! Canonical component facts used after authored documents are loaded.
//!
//! Exact `component.yaml` document versions live under
//! [`crate::model::source::component`]. Runtime consumers use these
//! unversioned values through [`crate::model::Robot`].

pub mod capability;

use anyhow::{Result, bail};
use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use capability::Capability;

#[derive(Debug, Clone)]
pub struct Component {
    pub gtin: Option<Gtin>,
    pub capabilities: BTreeMap<String, Capability>,
}

impl Component {
    #[must_use]
    pub fn capability(&self, capability_id: &str) -> Option<&Capability> {
        self.capabilities.get(capability_id)
    }
}

/// A Global Trade Item Number (GTIN-13).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Gtin(String);

impl From<crate::model::source::component::v0::Gtin> for Gtin {
    fn from(value: crate::model::source::component::v0::Gtin) -> Self {
        Self(value.to_string())
    }
}

impl Gtin {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Gtin {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let trimmed = value.trim();
        if trimmed.len() != 13 || !trimmed.chars().all(|character| character.is_ascii_digit()) {
            bail!("invalid GTIN '{value}': must be exactly 13 digits");
        }
        Ok(Self(trimmed.to_string()))
    }
}

impl TryFrom<String> for Gtin {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self> {
        value.parse()
    }
}

impl From<Gtin> for String {
    fn from(value: Gtin) -> Self {
        value.0
    }
}

impl fmt::Display for Gtin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CapabilityRef {
    pub component_id: String,
    pub capability_id: String,
}

impl CapabilityRef {
    #[must_use]
    pub fn new(component_id: impl Into<String>, capability_id: impl Into<String>) -> Self {
        Self {
            component_id: component_id.into(),
            capability_id: capability_id.into(),
        }
    }
}

impl fmt::Display for CapabilityRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.component_id, self.capability_id)
    }
}

impl FromStr for CapabilityRef {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let Some((component_id, capability_id)) = value.split_once('.') else {
            bail!("invalid capability reference '{value}', must be 'component.capability'");
        };
        if capability_id.contains('.')
            || !is_valid_token(component_id)
            || !is_valid_token(capability_id)
        {
            bail!(
                "invalid capability reference '{value}', component and capability ids must \
                 contain only lowercase ASCII letters, digits, '_' or '-'"
            );
        }
        Ok(Self::new(component_id, capability_id))
    }
}

impl TryFrom<String> for CapabilityRef {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self> {
        value.parse()
    }
}

impl From<CapabilityRef> for String {
    fn from(value: CapabilityRef) -> Self {
        value.to_string()
    }
}

#[must_use]
pub fn is_valid_token(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && trimmed.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || character == '_'
                || character == '-'
        })
}
