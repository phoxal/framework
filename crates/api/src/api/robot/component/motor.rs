/// A per-actuator control command.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub enum Command {
    Position(
        #[serde(
            deserialize_with = "crate::api::robot::component::motor::deserialize_finite_motor_scalar"
        )]
        f32,
    ),
    Velocity(
        #[serde(
            deserialize_with = "crate::api::robot::component::motor::deserialize_finite_motor_scalar"
        )]
        f32,
    ),
    Torque(
        #[serde(
            deserialize_with = "crate::api::robot::component::motor::deserialize_finite_motor_scalar"
        )]
        f32,
    ),
    Stop,
}

/// Reject non-finite motor command scalars during wire deserialization.
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

phoxal_macros::phoxal_api_fragment! {
    path robot / component(instance) / motor(capability);

    command command: Setpoint<Command>;
}
