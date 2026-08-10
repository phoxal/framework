//! Canonical component facts used after authored documents are loaded.
//!
//! Runtime consumers use these unversioned values through [`crate::Robot`].

pub mod capability;

use std::collections::BTreeMap;

use crate::identity::CapabilityId;
use crate::structure::Structure;
use capability::Capability;

/// One component *type*: the capabilities and structure every instance of it
/// has.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct Component {
    capabilities: BTreeMap<CapabilityId, Capability>,
    structure: Structure,
}

impl Component {
    pub(crate) fn new(
        capabilities: BTreeMap<CapabilityId, Capability>,
        structure: Structure,
    ) -> Self {
        Self {
            capabilities,
            structure,
        }
    }

    /// The named capability, if this type declares it.
    #[must_use]
    pub fn capability(&self, capability_id: &str) -> Option<&Capability> {
        self.capabilities.get(capability_id)
    }

    /// Every declared capability, ordered by capability id.
    pub fn capabilities(&self) -> impl ExactSizeIterator<Item = (&CapabilityId, &Capability)> {
        self.capabilities.iter()
    }

    /// The component's own structure, in component-local identities.
    #[must_use]
    pub fn structure(&self) -> &Structure {
        &self.structure
    }
}
