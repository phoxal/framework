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
