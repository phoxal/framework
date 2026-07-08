use crate::model::simulation::capability;
use anyhow::{Result, bail};
use derive_new::new;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Represents the top-level configuration loaded from `simulation.yaml`
#[derive(Debug, Clone, Serialize, Deserialize, new)]
#[serde(deny_unknown_fields)]
pub struct Simulation {
    #[serde(default)]
    pub capabilities: BTreeMap<String, capability::Capability>,
    #[serde(default)]
    pub links: BTreeMap<String, Link>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Link {
    #[serde(default)]
    pub contact_material: Option<String>,
}

impl Simulation {
    pub fn validate(&self) -> Result<()> {
        let mut errors = Vec::new();

        for (capability_id, capability) in &self.capabilities {
            if !crate::model::component::v0::is_valid_token(capability_id) {
                errors.push(format!(
                    "simulation.capabilities.{capability_id} must use a valid capability token"
                ));
            }
            capability.validate(
                &format!("simulation.capabilities.{capability_id}"),
                &mut errors,
            );
        }

        for (link_name, link) in &self.links {
            if link_name.trim().is_empty() {
                errors.push("simulation.links contains an empty link name".to_string());
            }
            if let Some(contact_material) = &link.contact_material
                && contact_material.trim().is_empty()
            {
                errors.push(format!(
                    "simulation.links.{link_name}.contact_material must not be empty"
                ));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            bail!("Simulation errors:\n{}", errors.join("\n"))
        }
    }
}
