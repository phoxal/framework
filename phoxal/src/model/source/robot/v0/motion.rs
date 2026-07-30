use serde::{Deserialize, Serialize};

use std::fmt;
use std::str::FromStr;

use anyhow::{Result, bail};

use crate::model::component::is_valid_token;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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

/// Robot-wide planar motion limits enforced independently by `motion` and
/// `drive`. These are authored facts, not per-runtime configuration.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct MotionLimits {
    pub max_linear_speed_mps: f64,
    pub max_angular_speed_radps: f64,
}

/// The robot's kinematic model - a direct field of `robot:` (was
/// `motion.kinematic`; the `motion:` wrapper is gone).
///
/// Every `CapabilityRef` field carries a test-only
/// `#[schemars(with = "String")]` override. The module-local
/// [`CapabilityRef`] uses custom string serde and does not derive `JsonSchema`;
/// it serializes as a plain `"<component>.<capability>"` string
/// (`#[serde(try_from = "String", into = "String")]`), so treating it as
/// `String` in the generated JSON Schema is exact, not an approximation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KinematicConfig {
    Differential {
        #[cfg_attr(test, schemars(with = "Vec<String>"))]
        left_actuators: Vec<CapabilityRef>,
        #[cfg_attr(test, schemars(with = "Vec<String>"))]
        right_actuators: Vec<CapabilityRef>,
        #[cfg_attr(test, schemars(with = "Vec<String>"))]
        left_encoders: Vec<CapabilityRef>,
        #[cfg_attr(test, schemars(with = "Vec<String>"))]
        right_encoders: Vec<CapabilityRef>,
        wheel_radius_m: f64,
        wheel_base_m: f64,
    },
    Mecanum {
        #[cfg_attr(test, schemars(with = "String"))]
        front_left_actuator: CapabilityRef,
        #[cfg_attr(test, schemars(with = "String"))]
        front_right_actuator: CapabilityRef,
        #[cfg_attr(test, schemars(with = "String"))]
        rear_left_actuator: CapabilityRef,
        #[cfg_attr(test, schemars(with = "String"))]
        rear_right_actuator: CapabilityRef,
        wheel_radius_m: f64,
        wheel_base_m: f64,
        track_m: f64,
    },
    Ackermann {
        #[cfg_attr(test, schemars(with = "String"))]
        steering_actuator: CapabilityRef,
        #[cfg_attr(test, schemars(with = "String"))]
        drive_actuator: CapabilityRef,
        #[cfg_attr(test, schemars(with = "Option<String>"))]
        steering_encoder: Option<CapabilityRef>,
        #[cfg_attr(test, schemars(with = "Option<String>"))]
        drive_encoder: Option<CapabilityRef>,
        wheel_base_m: f64,
        track_m: f64,
        max_steering_angle_rad: f64,
    },
    Omnidirectional {
        #[cfg_attr(test, schemars(with = "Vec<String>"))]
        actuators: Vec<CapabilityRef>,
        #[cfg_attr(test, schemars(with = "Vec<String>"))]
        encoders: Vec<CapabilityRef>,
    },
}
