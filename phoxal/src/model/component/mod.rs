//! Canonical component facts used after authored documents are loaded.
//!
//! Exact `component.yaml` document versions live under
//! [`crate::model::source::component`]. Runtime consumers use these
//! unversioned values through [`crate::model::Robot`].

pub mod capability;

use std::collections::BTreeMap;
use std::fmt;

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
