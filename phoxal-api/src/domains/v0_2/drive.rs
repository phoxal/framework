//! v0.2 drive payloads.
#![allow(legacy_derive_helpers)]

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    TargetStale,
    TargetNotFinite,
    ActuatorCommandNotFinite,
    EmergencyStop,
    Fault,
}

/// A finite requested or limited planar velocity in the current
/// control wire revision.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    #[serde(deserialize_with = "crate::domains::v0_2::drive::deserialize_finite_target_scalar")]
    pub(crate) linear_x_mps: f32,
    #[serde(deserialize_with = "crate::domains::v0_2::drive::deserialize_finite_target_scalar")]
    pub(crate) angular_z_radps: f32,
}

impl Target {
    pub fn try_new(linear_x_mps: f32, angular_z_radps: f32) -> Result<Self, InvalidTarget> {
        if !linear_x_mps.is_finite() {
            return Err(InvalidTarget::LinearXNotFinite);
        }
        if !angular_z_radps.is_finite() {
            return Err(InvalidTarget::AngularZNotFinite);
        }
        Ok(Self {
            linear_x_mps,
            angular_z_radps,
        })
    }

    #[must_use]
    pub const fn linear_x_mps(&self) -> f32 {
        self.linear_x_mps
    }

    #[must_use]
    pub const fn angular_z_radps(&self) -> f32 {
        self.angular_z_radps
    }

    #[must_use]
    pub const fn stopped() -> Self {
        Self {
            linear_x_mps: 0.0,
            angular_z_radps: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidTarget {
    LinearXNotFinite,
    AngularZNotFinite,
}

impl std::fmt::Display for InvalidTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let field = match self {
            Self::LinearXNotFinite => "linear_x_mps",
            Self::AngularZNotFinite => "angular_z_radps",
        };
        write!(formatter, "control target field {field} must be finite")
    }
}

impl std::error::Error for InvalidTarget {}

/// The drive participant's exclusive control state.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum State {
    Active {
        target: Target,
        limited_target: Target,
    },
    Stopped {
        target: Target,
        reason: StopReason,
    },
}

/// Deserialize a control-target scalar without allowing a non-finite value to
/// enter a lease or a wheel-mixing calculation. The target fields remain plain
/// `f32`s on the wire; their explicitly restricted visibility and this serde
/// hook make serde enforce the same invariant as `Target::try_new`.
pub(crate) fn deserialize_finite_target_scalar<'de, D>(deserializer: D) -> Result<f32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <f32 as serde::Deserialize>::deserialize(deserializer)?;
    value
        .is_finite()
        .then_some(value)
        .ok_or_else(|| serde::de::Error::custom("control target scalar must be finite"))
}
