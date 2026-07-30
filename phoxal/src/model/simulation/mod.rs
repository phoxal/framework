//! Canonical simulation facts used after authored documents are loaded.

pub mod capability;

use std::collections::BTreeMap;

use capability::Capability;

#[derive(Debug, Clone, PartialEq)]
pub struct Simulation {
    pub capabilities: BTreeMap<String, Capability>,
    pub links: BTreeMap<String, Link>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub contact_material: Option<String>,
}

impl From<crate::model::source::simulation::v0::Manifest> for Simulation {
    fn from(value: crate::model::source::simulation::v0::Manifest) -> Self {
        Self {
            capabilities: value
                .capabilities
                .into_iter()
                .map(|(id, capability)| (id, capability.into()))
                .collect(),
            links: value
                .links
                .into_iter()
                .map(|(id, link)| {
                    (
                        id,
                        Link {
                            contact_material: link.contact_material,
                        },
                    )
                })
                .collect(),
        }
    }
}
