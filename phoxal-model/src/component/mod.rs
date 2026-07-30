//! Canonical component facts used after authored documents are loaded.
//!
//! Runtime consumers use these unversioned values through [`crate::Robot`].

pub mod capability;

use std::collections::BTreeMap;
use std::fmt;

use crate::structure::Structure;
use capability::Capability;

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct Component {
    capabilities: BTreeMap<String, Capability>,
    structure: Structure,
}

impl Component {
    #[doc(hidden)]
    pub fn __new(capabilities: BTreeMap<String, Capability>, structure: Structure) -> Self {
        Self {
            capabilities,
            structure,
        }
    }

    #[must_use]
    pub fn capability(&self, capability_id: &str) -> Option<&Capability> {
        self.capabilities.get(capability_id)
    }

    pub fn capabilities(&self) -> impl ExactSizeIterator<Item = (&str, &Capability)> {
        self.capabilities
            .iter()
            .map(|(id, capability)| (id.as_str(), capability))
    }

    #[must_use]
    pub fn structure(&self) -> &Structure {
        &self.structure
    }
}

#[derive(
    serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(try_from = "String", into = "String")]
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

impl std::str::FromStr for CapabilityRef {
    type Err = crate::ModelError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (component_id, capability_id) = value.split_once('.').ok_or_else(|| {
            crate::ModelError::Invalid(format!(
                "capability reference '{value}' must use component.capability"
            ))
        })?;
        if !is_valid_token(component_id) || !is_valid_token(capability_id) {
            return Err(crate::ModelError::Invalid(format!(
                "capability reference '{value}' is not normalized"
            )));
        }
        Ok(Self::new(component_id, capability_id))
    }
}

impl TryFrom<String> for CapabilityRef {
    type Error = crate::ModelError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
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
    !value.is_empty()
        && value.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || character == '_'
                || character == '-'
        })
}
