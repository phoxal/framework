//! v0.2 motor payloads.
#![allow(legacy_derive_helpers)]

/// A per-actuator command in the current control revision.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Command {
    Position(
        #[serde(
            deserialize_with = "crate::domains::v0_2::component::motor::deserialize_finite_motor_scalar"
        )]
        f32,
    ),
    Velocity(
        #[serde(
            deserialize_with = "crate::domains::v0_2::component::motor::deserialize_finite_motor_scalar"
        )]
        f32,
    ),
    Torque(
        #[serde(
            deserialize_with = "crate::domains::v0_2::component::motor::deserialize_finite_motor_scalar"
        )]
        f32,
    ),
    Stop,
}

/// Reject non-finite v0.2 motor command scalars during wire deserialization.
pub(crate) fn deserialize_finite_motor_scalar<'de, D>(deserializer: D) -> Result<f32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <f32 as serde::Deserialize>::deserialize(deserializer)?;
    value
        .is_finite()
        .then_some(value)
        .ok_or_else(|| serde::de::Error::custom("motor command scalar must be finite"))
}
