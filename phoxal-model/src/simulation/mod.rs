//! Canonical simulation facts used after authored documents are loaded.

pub mod capability;

use std::collections::BTreeMap;

use capability::Capability;

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Simulation {
    capabilities: BTreeMap<String, Capability>,
    links: BTreeMap<String, Link>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Link {
    contact_material: Option<String>,
}

impl Simulation {
    #[doc(hidden)]
    pub fn __new(
        capabilities: BTreeMap<String, Capability>,
        links: BTreeMap<String, Option<String>>,
    ) -> Self {
        Self {
            capabilities,
            links: links
                .into_iter()
                .map(|(id, contact_material)| (id, Link { contact_material }))
                .collect(),
        }
    }

    pub fn capabilities(&self) -> impl ExactSizeIterator<Item = (&str, &Capability)> {
        self.capabilities
            .iter()
            .map(|(id, capability)| (id.as_str(), capability))
    }

    pub fn capability(&self, id: &str) -> Option<&Capability> {
        self.capabilities.get(id)
    }

    pub fn links(&self) -> impl ExactSizeIterator<Item = (&str, &Link)> {
        self.links.iter().map(|(id, link)| (id.as_str(), link))
    }
}

impl Link {
    #[must_use]
    pub fn contact_material(&self) -> Option<&str> {
        self.contact_material.as_deref()
    }
}
