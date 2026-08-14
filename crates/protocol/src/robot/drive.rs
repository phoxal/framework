#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    TargetStale,
    TargetNotFinite,
    ActuatorCommandNotFinite,
    EmergencyStop,
    Fault,
}

/// A finite requested or limited planar velocity.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct Target {
    #[serde(deserialize_with = "crate::robot::drive::deserialize_finite_target_scalar")]
    pub(crate) linear_x_mps: f32,
    #[serde(deserialize_with = "crate::robot::drive::deserialize_finite_target_scalar")]
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
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
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


#[cfg(test)]
mod tests {
    use phoxal_runtime_contract::wire_schema::{DescribeWire, WireSchema};

    use super::{State, StopReason, Target};

    /// The control target's scalars stay plain `f32`s on the wire: the
    /// finiteness hook beside them narrows what decodes and never changes what
    /// is written, so it must not appear in the declared shape.
    #[test]
    fn a_decode_side_finiteness_hook_is_not_part_of_the_declared_shape() {
        assert_eq!(
            Target::wire_schema().canonical_json(),
            concat!(
                r#"{"fields":[{"name":"angular_z_radps","presence":"required","schema":{"kind":"f32"}},"#,
                r#"{"name":"linear_x_mps","presence":"required","schema":{"kind":"f32"}}],"kind":"struct"}"#,
            )
        );
        let json = serde_json::to_value(Target::stopped()).expect("a target serializes");
        assert_eq!(Target::wire_schema().conforms(&json), Ok(()));
    }

    /// The externally tagged control state and the `snake_case` stop reasons
    /// are read from the declaration, so a rename rule cannot mean one thing
    /// here and another on the wire.
    #[test]
    fn the_declared_state_shape_is_the_shape_serde_writes() {
        let state = State::Stopped {
            target: Target::stopped(),
            reason: StopReason::EmergencyStop,
        };
        let json = serde_json::to_value(&state).expect("a drive state serializes");
        assert_eq!(State::wire_schema().conforms(&json), Ok(()));
        assert!(json.get("Stopped").is_some(), "{json}");
        assert_eq!(json["Stopped"]["reason"], "emergency_stop");

        let reasons = StopReason::wire_schema();
        let WireSchema::Enum { variants, .. } = &reasons else {
            panic!("a stop reason is a sum type: {reasons:?}");
        };
        assert!(
            variants
                .iter()
                .any(|variant| variant.name == "emergency_stop"),
            "{variants:?}"
        );
    }
}
