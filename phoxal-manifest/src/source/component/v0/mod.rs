//! Exact `component.yaml` v0 document.

pub mod capability;

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use phoxal_model::identity::is_valid_token;
use serde::{Deserialize, Serialize};

use capability::{Capability, StructuralTarget};

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gtin: Option<Gtin>,
    #[serde(default)]
    pub capabilities: BTreeMap<String, Capability>,
}

/// A GS1 Global Trade Item Number, exactly 13 digits.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(try_from = "String", into = "String")]
#[schemars(with = "String", inline)]
pub struct Gtin(String);

/// Why a string is not a [`Gtin`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid GTIN '{value}': must be exactly 13 digits")]
pub struct InvalidGtin {
    pub value: String,
}

impl Gtin {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Gtin {
    type Err = InvalidGtin;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let trimmed = value.trim();
        if trimmed.len() != 13 || !trimmed.chars().all(|character| character.is_ascii_digit()) {
            return Err(InvalidGtin {
                value: value.to_string(),
            });
        }
        Ok(Self(trimmed.to_string()))
    }
}

impl TryFrom<String> for Gtin {
    type Error = InvalidGtin;

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
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One authored rule a `component.yaml` v0 document broke.
///
/// `component` is the type the document defines. It is only known when the
/// document was loaded as a named component type, so it is optional rather
/// than invented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    /// A capability id is not a normalized token.
    InvalidCapabilityId {
        component: Option<String>,
        capability: String,
    },
    /// A capability names no structural item to attach to.
    EmptyTarget {
        component: Option<String>,
        capability: String,
    },
    /// A numeric field that scales a physical quantity is not positive.
    NotPositive {
        component: Option<String>,
        capability: String,
        field: &'static str,
    },
}

impl ValidationError {
    /// The same rejection, attributed to the component type it came from.
    #[must_use]
    pub fn in_component(self, component_type: &str) -> Self {
        let named = Some(component_type.to_string());
        match self {
            Self::InvalidCapabilityId { capability, .. } => Self::InvalidCapabilityId {
                component: named,
                capability,
            },
            Self::EmptyTarget { capability, .. } => Self::EmptyTarget {
                component: named,
                capability,
            },
            Self::NotPositive {
                capability, field, ..
            } => Self::NotPositive {
                component: named,
                capability,
                field,
            },
        }
    }

    fn component(&self) -> Option<&str> {
        match self {
            Self::InvalidCapabilityId { component, .. }
            | Self::EmptyTarget { component, .. }
            | Self::NotPositive { component, .. } => component.as_deref(),
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(component) = self.component() {
            write!(formatter, "component.yaml for '{component}' ")?;
        }
        match self {
            Self::InvalidCapabilityId { capability, .. } => write!(
                formatter,
                "has invalid capability id '{capability}'; ids may contain only lowercase ASCII \
                 letters, digits, '_' or '-'"
            ),
            Self::EmptyTarget { capability, .. } => write!(
                formatter,
                "has an empty capabilities.{capability}.target.id"
            ),
            Self::NotPositive {
                capability, field, ..
            } => write!(formatter, "requires capabilities.{capability}.{field} > 0"),
        }
    }
}

impl Manifest {
    /// Every rule this document breaks, or `Ok(())` when it breaks none.
    ///
    /// # Errors
    ///
    /// Returns every [`ValidationError`] at once.
    pub fn validate(&self) -> Result<(), Vec<ValidationError>> {
        let mut errors = Vec::new();

        for (capability_id, capability) in &self.capabilities {
            let not_positive = |field: &'static str| ValidationError::NotPositive {
                component: None,
                capability: capability_id.clone(),
                field,
            };
            if !is_valid_token(capability_id) {
                errors.push(ValidationError::InvalidCapabilityId {
                    component: None,
                    capability: capability_id.clone(),
                });
            }

            let (StructuralTarget::Joint { id } | StructuralTarget::Link { id }) =
                capability.target();
            if id.trim().is_empty() {
                errors.push(ValidationError::EmptyTarget {
                    component: None,
                    capability: capability_id.clone(),
                });
            }

            match capability {
                Capability::Motor(motor) if !is_valid_positive(motor.gear_ratio) => {
                    errors.push(not_positive("gear_ratio"));
                }
                Capability::Encoder(encoder) => {
                    if !is_valid_positive(encoder.gear_ratio) {
                        errors.push(not_positive("gear_ratio"));
                    }
                    if encoder.counts_per_revolution == 0 {
                        errors.push(not_positive("counts_per_revolution"));
                    }
                }
                Capability::Camera(camera) => {
                    if camera.width_px == 0 {
                        errors.push(not_positive("width_px"));
                    }
                    if camera.height_px == 0 {
                        errors.push(not_positive("height_px"));
                    }
                }
                Capability::Depth(depth) => {
                    if depth.width_px == 0 {
                        errors.push(not_positive("width_px"));
                    }
                    if depth.height_px == 0 {
                        errors.push(not_positive("height_px"));
                    }
                }
                _ => {}
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

fn is_valid_positive(value: f64) -> bool {
    value.is_finite() && value > 0.0
}

#[cfg(test)]
mod tests {
    use crate::source::component::Manifest;

    #[test]
    fn schema_is_required_and_exact() -> anyhow::Result<()> {
        let manifest: Manifest =
            serde_yaml::from_str("schema: phoxal/component/v0\ncapabilities: {}\n")?;
        assert!(matches!(manifest, Manifest::V0(_)));
        assert!(serde_yaml::from_str::<Manifest>("capabilities: {}\n").is_err());
        assert!(serde_yaml::from_str::<Manifest>("schema: phoxal/component/v1\n").is_err());
        Ok(())
    }
}
