use crate::api::robot::map::GridWireError;

/// Why safety is stopping or limiting body motion. The
/// map/footprint reasons are deliberately distinct so a fail
/// closed decision remains diagnosable without parsing text.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintReason {
    WorldUnavailable,
    MapUnavailable,
    MapStale,
    MapPartial,
    MapRevisionInvalid,
    UnknownOccupancy,
    FootprintUnavailable,
    FootprintMismatch,
    FootprintObstacle,
    DrivableSpaceUnavailable,
    LocalizationUnavailable,
    LocalizationUncertain,
    ObstacleProximity,
    RangeSensorFault,
    DriveFault,
    BatteryLow,
    BatteryCritical,
    BatteryUnavailable,
    BatteryStale,
    SpeedZone,
    OperatorPolicy,
}

/// Additional provenance category for the compiled footprint
/// checks. The source is still the safety participant, but the
/// category tells operators which safety input failed.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintSourceKind {
    WorldModel,
    Map,
    Localization,
    Range,
    Drive,
    Battery,
    Footprint,
    Operator,
}

/// Typed origin of one constraint.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub struct ConstraintSource {
    pub kind: ConstraintSourceKind,
    pub participant_id: String,
    pub component_id: Option<String>,
    pub capability_id: Option<String>,
}

/// A constraint is one shape only: a `Limited` item carries
/// effective limits and a `Stopped` item carries no contradictory
/// limit fields. This prevents a consumer from having to choose
/// between `stop = true` and a nonzero limit.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(try_from = "crate::api::robot::safety::SafetyConstraintWire")]
pub enum Constraint {
    Limited {
        reason: ConstraintReason,
        source: ConstraintSource,
        max_linear_speed_mps: f32,
        max_angular_speed_radps: f32,
        observed_value: Option<f32>,
        valid_from: ::phoxal_bus::RobotInstant,
        expires_at: ::phoxal_bus::RobotInstant,
    },
    Stopped {
        reason: ConstraintReason,
        source: ConstraintSource,
        observed_value: Option<f32>,
        valid_from: ::phoxal_bus::RobotInstant,
        expires_at: ::phoxal_bus::RobotInstant,
    },
}

/// The sole safety permission consumed by motion. Its variants
/// are mutually exclusive and carry exactly the fields that make
/// sense for that decision.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(try_from = "crate::api::robot::safety::SafetyMotionPermissionWire")]
pub enum MotionPermission {
    Clear,
    Limited {
        effective_linear_speed_mps: f32,
        effective_angular_speed_radps: f32,
        reasons: Vec<ConstraintReason>,
    },
    Stopped {
        reasons: Vec<ConstraintReason>,
    },
}

#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(try_from = "crate::api::robot::safety::SafetyMotionConstraintsWire")]
pub struct MotionConstraints {
    pub sequence: u64,
    pub permission: MotionPermission,
    pub constraints: Vec<Constraint>,
    pub expires_at: ::phoxal_bus::RobotInstant,
}

#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub constraints: MotionConstraints,
}

fn deserialize_finite_nonnegative_safety_limit<'de, D>(deserializer: D) -> Result<f32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <f32 as serde::Deserialize>::deserialize(deserializer)?;
    (value.is_finite() && value >= 0.0)
        .then_some(value)
        .ok_or_else(|| {
            serde::de::Error::custom("safety speed limit must be finite and nonnegative")
        })
}

fn deserialize_optional_finite_safety_value<'de, D>(
    deserializer: D,
) -> Result<Option<f32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <Option<f32> as serde::Deserialize>::deserialize(deserializer)?;
    value
        .map(|value| {
            value
                .is_finite()
                .then_some(value)
                .ok_or_else(|| serde::de::Error::custom("safety observed value must be finite"))
        })
        .transpose()
}

#[doc(hidden)]
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub enum SafetyMotionPermissionWire {
    Clear,
    Limited {
        #[serde(
            deserialize_with = "crate::api::robot::safety::deserialize_finite_nonnegative_safety_limit"
        )]
        effective_linear_speed_mps: f32,
        #[serde(
            deserialize_with = "crate::api::robot::safety::deserialize_finite_nonnegative_safety_limit"
        )]
        effective_angular_speed_radps: f32,
        reasons: Vec<crate::api::robot::safety::ConstraintReason>,
    },
    Stopped {
        reasons: Vec<crate::api::robot::safety::ConstraintReason>,
    },
}

#[doc(hidden)]
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub enum SafetyConstraintWire {
    Limited {
        reason: crate::api::robot::safety::ConstraintReason,
        source: crate::api::robot::safety::ConstraintSource,
        #[serde(
            deserialize_with = "crate::api::robot::safety::deserialize_finite_nonnegative_safety_limit"
        )]
        max_linear_speed_mps: f32,
        #[serde(
            deserialize_with = "crate::api::robot::safety::deserialize_finite_nonnegative_safety_limit"
        )]
        max_angular_speed_radps: f32,
        #[serde(
            deserialize_with = "crate::api::robot::safety::deserialize_optional_finite_safety_value"
        )]
        observed_value: Option<f32>,
        valid_from: ::phoxal_bus::RobotInstant,
        expires_at: ::phoxal_bus::RobotInstant,
    },
    Stopped {
        reason: crate::api::robot::safety::ConstraintReason,
        source: crate::api::robot::safety::ConstraintSource,
        #[serde(
            deserialize_with = "crate::api::robot::safety::deserialize_optional_finite_safety_value"
        )]
        observed_value: Option<f32>,
        valid_from: ::phoxal_bus::RobotInstant,
        expires_at: ::phoxal_bus::RobotInstant,
    },
}

#[doc(hidden)]
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SafetyMotionConstraintsWire {
    pub sequence: u64,
    pub permission: SafetyMotionPermissionWire,
    pub constraints: Vec<SafetyConstraintWire>,
    pub expires_at: ::phoxal_bus::RobotInstant,
}

impl TryFrom<SafetyMotionPermissionWire> for crate::api::robot::safety::MotionPermission {
    type Error = GridWireError;

    fn try_from(value: SafetyMotionPermissionWire) -> Result<Self, Self::Error> {
        match value {
            SafetyMotionPermissionWire::Clear => Ok(Self::Clear),
            SafetyMotionPermissionWire::Limited {
                effective_linear_speed_mps,
                effective_angular_speed_radps,
                reasons,
            } if !reasons.is_empty() => Ok(Self::Limited {
                effective_linear_speed_mps,
                effective_angular_speed_radps,
                reasons,
            }),
            SafetyMotionPermissionWire::Stopped { reasons } if !reasons.is_empty() => {
                Ok(Self::Stopped { reasons })
            }
            _ => Err(GridWireError(
                "safety permission must carry reasons for limited or stopped motion",
            )),
        }
    }
}

impl TryFrom<SafetyConstraintWire> for crate::api::robot::safety::Constraint {
    type Error = GridWireError;

    fn try_from(value: SafetyConstraintWire) -> Result<Self, Self::Error> {
        let (source, valid_from, expires_at) = match &value {
            SafetyConstraintWire::Limited {
                source,
                valid_from,
                expires_at,
                ..
            }
            | SafetyConstraintWire::Stopped {
                source,
                valid_from,
                expires_at,
                ..
            } => (source, *valid_from, *expires_at),
        };
        let validity = valid_from.checked_cmp(expires_at);
        if source.participant_id.trim().is_empty()
            || !validity.is_ok_and(|order| order != std::cmp::Ordering::Greater)
        {
            return Err(GridWireError(
                "safety constraint source and validity interval are invalid",
            ));
        }
        Ok(match value {
            SafetyConstraintWire::Limited {
                reason,
                source,
                max_linear_speed_mps,
                max_angular_speed_radps,
                observed_value,
                valid_from,
                expires_at,
            } => Self::Limited {
                reason,
                source,
                max_linear_speed_mps,
                max_angular_speed_radps,
                observed_value,
                valid_from,
                expires_at,
            },
            SafetyConstraintWire::Stopped {
                reason,
                source,
                observed_value,
                valid_from,
                expires_at,
            } => Self::Stopped {
                reason,
                source,
                observed_value,
                valid_from,
                expires_at,
            },
        })
    }
}

impl TryFrom<SafetyMotionConstraintsWire> for crate::api::robot::safety::MotionConstraints {
    type Error = GridWireError;

    fn try_from(value: SafetyMotionConstraintsWire) -> Result<Self, Self::Error> {
        let permission = value.permission.try_into()?;
        let constraints = value
            .constraints
            .into_iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<crate::api::robot::safety::Constraint>, GridWireError>>()?;
        let expected = expected_safety_permission(&constraints)?;
        if permission != expected {
            return Err(GridWireError(
                "safety permission disagrees with its constraints",
            ));
        }
        for constraint in &constraints {
            let (valid_from, expires_at) = match constraint {
                crate::api::robot::safety::Constraint::Limited {
                    valid_from,
                    expires_at,
                    ..
                }
                | crate::api::robot::safety::Constraint::Stopped {
                    valid_from,
                    expires_at,
                    ..
                } => (valid_from, expires_at),
            };
            if value.expires_at.checked_cmp(*expires_at).is_err()
                || !value
                    .expires_at
                    .checked_cmp(*expires_at)
                    .is_ok_and(|order| order != std::cmp::Ordering::Less)
            {
                return Err(GridWireError(
                    "safety product expiry must cover every constraint",
                ));
            }
            if valid_from.checked_cmp(value.expires_at).is_err() {
                return Err(GridWireError("safety product uses mixed timelines"));
            }
        }
        Ok(Self {
            sequence: value.sequence,
            permission,
            constraints,
            expires_at: value.expires_at,
        })
    }
}

fn expected_safety_permission(
    constraints: &[crate::api::robot::safety::Constraint],
) -> Result<crate::api::robot::safety::MotionPermission, GridWireError> {
    let reasons = constraints
        .iter()
        .map(|constraint| match constraint {
            crate::api::robot::safety::Constraint::Limited { reason, .. }
            | crate::api::robot::safety::Constraint::Stopped { reason, .. } => reason.clone(),
        })
        .collect::<Vec<_>>();
    if constraints.iter().any(|constraint| {
        matches!(
            constraint,
            crate::api::robot::safety::Constraint::Stopped { .. }
        )
    }) {
        return Ok(crate::api::robot::safety::MotionPermission::Stopped { reasons });
    }
    if constraints.is_empty() {
        return Ok(crate::api::robot::safety::MotionPermission::Clear);
    }
    let mut linear = f32::MAX;
    let mut angular = f32::MAX;
    for constraint in constraints {
        let crate::api::robot::safety::Constraint::Limited {
            max_linear_speed_mps,
            max_angular_speed_radps,
            ..
        } = constraint
        else {
            return Err(GridWireError("safety constraint shape is contradictory"));
        };
        linear = linear.min(*max_linear_speed_mps);
        angular = angular.min(*max_angular_speed_radps);
    }
    Ok(crate::api::robot::safety::MotionPermission::Limited {
        effective_linear_speed_mps: linear,
        effective_angular_speed_radps: angular,
        reasons,
    })
}

phoxal_macros::protocol_fragment! {
    path robot / safety;

    topic constraints: State<MotionConstraints>;
    topic state: State<State>;
}
