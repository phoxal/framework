//! Canonical component facts used after authored documents are loaded.
//!
//! Runtime consumers use these unversioned values through [`crate::model::Robot`].

pub mod capability;

use std::collections::BTreeMap;

use crate::model::identity::CapabilityId;
use crate::model::simulation::Simulation;
use crate::model::structure::Structure;
use capability::Capability;

/// One component *type*: the capabilities and structure every instance of it
/// has, and how a simulated world models it.
///
/// The simulation lives on the type rather than in a parallel map keyed the same
/// way, because it is only ever meaningful together with the capabilities it
/// models: a simulated capability that names none of them is the one error the
/// pairing makes impossible to write.
#[derive(phoxal_macros::DescribeWire, serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct Component {
    capabilities: BTreeMap<CapabilityId, Capability>,
    structure: Structure,
    simulation: Option<Simulation>,
}

impl Component {
    pub(crate) fn new(
        capabilities: BTreeMap<CapabilityId, Capability>,
        structure: Structure,
        simulation: Option<Simulation>,
    ) -> Self {
        Self {
            capabilities,
            structure,
            simulation,
        }
    }

    /// How a simulated world models this type, when a document authored one.
    #[must_use]
    pub const fn simulation(&self) -> Option<&Simulation> {
        self.simulation.as_ref()
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
