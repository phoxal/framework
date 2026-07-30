//! Exact `component.yaml` v0 document.

pub mod capability;

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::model::component::Component;
use capability::Capability;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Schema {
    #[serde(rename = "component/v0")]
    V0,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: Schema,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gtin: Option<Gtin>,
    #[serde(default)]
    pub capabilities: BTreeMap<String, Capability>,
}

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

impl Manifest {
    pub fn validate(&self) -> Result<()> {
        self.validate_for_component("<inline>")
    }

    pub fn validate_for_component(&self, component_id: &str) -> Result<()> {
        let mut errors = Vec::new();

        for (capability_id, capability) in &self.capabilities {
            if !crate::model::component::is_valid_token(capability_id) {
                errors.push(format!(
                    "component.yaml for '{component_id}' has invalid capability id \
                     '{capability_id}'; ids may contain only lowercase ASCII letters, digits, \
                     '_' or '-'"
                ));
            }

            let target_id = match capability.target() {
                capability::StructuralTarget::Joint { id }
                | capability::StructuralTarget::Link { id } => id.trim(),
            };
            if target_id.is_empty() {
                errors.push(format!(
                    "component.yaml for '{component_id}' has an empty \
                     capabilities.{capability_id}.target.id"
                ));
            }

            match capability {
                capability::Capability::Motor(motor) if !is_valid_positive(motor.gear_ratio) => {
                    errors.push(format!(
                        "component.yaml for '{component_id}' requires \
                         capabilities.{capability_id}.gear_ratio > 0"
                    ));
                }
                capability::Capability::Encoder(encoder) => {
                    if !is_valid_positive(encoder.gear_ratio) {
                        errors.push(format!(
                            "component.yaml for '{component_id}' requires \
                             capabilities.{capability_id}.gear_ratio > 0"
                        ));
                    }
                    if encoder.counts_per_revolution == 0 {
                        errors.push(format!(
                            "component.yaml for '{component_id}' requires \
                             capabilities.{capability_id}.counts_per_revolution > 0"
                        ));
                    }
                }
                capability::Capability::Camera(camera) => {
                    if camera.width_px == 0 {
                        errors.push(format!(
                            "component.yaml for '{component_id}' requires \
                             capabilities.{capability_id}.width_px > 0"
                        ));
                    }
                    if camera.height_px == 0 {
                        errors.push(format!(
                            "component.yaml for '{component_id}' requires \
                             capabilities.{capability_id}.height_px > 0"
                        ));
                    }
                }
                capability::Capability::Depth(depth) => {
                    if depth.width_px == 0 {
                        errors.push(format!(
                            "component.yaml for '{component_id}' requires \
                             capabilities.{capability_id}.width_px > 0"
                        ));
                    }
                    if depth.height_px == 0 {
                        errors.push(format!(
                            "component.yaml for '{component_id}' requires \
                             capabilities.{capability_id}.height_px > 0"
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

fn is_valid_positive(value: f64) -> bool {
    value.is_finite() && value > 0.0
}

impl From<Manifest> for Component {
    fn from(source: Manifest) -> Self {
        Self {
            gtin: source.gtin.map(Into::into),
            capabilities: source
                .capabilities
                .into_iter()
                .map(|(id, capability)| (id, capability.into()))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Manifest, Schema};

    #[test]
    fn schema_is_required_and_exact() -> anyhow::Result<()> {
        let manifest: Manifest = serde_yaml::from_str("schema: component/v0\ncapabilities: {}\n")?;
        assert_eq!(manifest.schema, Schema::V0);
        assert!(serde_yaml::from_str::<Manifest>("capabilities: {}\n").is_err());
        assert!(serde_yaml::from_str::<Manifest>("schema: component/v1\n").is_err());
        Ok(())
    }
}
