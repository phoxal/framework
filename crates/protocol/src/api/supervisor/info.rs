//! The immutable facts of one running bundle.
//!
//! These are the answers that cannot change while an execution lives: which
//! robot is running, on which clock, and - when the bundle admits manual
//! driving at all - the geometry and limits a manual driver has to respect.
//! Everything that can change belongs to the snapshot instead.

use phoxal_runtime_contract::clock::Clock;
use phoxal_runtime_contract::identity::RobotId;

/// Empty request for the immutable facts of one running bundle.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct InfoRequest {}

/// The manual-drive envelope this bundle admits, absent when the bundle offers
/// no manual driving at all.
///
/// Every value is finite on decode: a driver cannot be handed a limit it has no
/// way to respect.
#[derive(
    phoxal_macros::DescribeWire, Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct ManualDrive {
    #[serde(deserialize_with = "crate::api::supervisor::info::deserialize_finite_f64")]
    pub wheel_base_m: f64,
    #[serde(deserialize_with = "crate::api::supervisor::info::deserialize_finite_f64")]
    pub max_linear_speed_mps: f64,
    #[serde(deserialize_with = "crate::api::supervisor::info::deserialize_finite_f64")]
    pub max_angular_speed_radps: f64,
}

/// The facts a supervisor advertises about the bundle it is running.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct Info {
    pub robot: RobotId,
    pub clock: Clock,
    pub manual_drive: Option<ManualDrive>,
}

fn deserialize_finite_f64<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <f64 as serde::Deserialize>::deserialize(deserializer)?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(serde::de::Error::custom(
            "expected a finite floating-point value",
        ))
    }
}

phoxal_macros::protocol_fragment! {
    path supervisor / info;

    query self: InfoRequest => Info;
}

#[cfg(test)]
mod tests {
    use super::{Clock, Info, ManualDrive, RobotId};

    fn info(manual_drive: Option<ManualDrive>) -> Info {
        Info {
            robot: RobotId::new("rover").expect("valid robot id"),
            clock: Clock::Real,
            manual_drive,
        }
    }

    #[test]
    fn info_round_trips_with_and_without_a_manual_drive_envelope() {
        for value in [
            info(None),
            info(Some(ManualDrive {
                wheel_base_m: 0.32,
                max_linear_speed_mps: 0.8,
                max_angular_speed_radps: 1.5,
            })),
        ] {
            let encoded = rmp_serde::to_vec_named(&value).expect("info encodes");
            assert_eq!(
                rmp_serde::from_slice::<Info>(&encoded).expect("info decodes"),
                value
            );
        }
    }

    #[test]
    fn a_non_finite_manual_drive_limit_is_rejected_on_decode() {
        let malformed = rmp_serde::to_vec_named(&std::collections::BTreeMap::from([
            ("wheel_base_m", 0.32_f64),
            ("max_linear_speed_mps", 0.8),
            ("max_angular_speed_radps", f64::NAN),
        ]))
        .expect("messagepack permits the hostile input");
        assert!(rmp_serde::from_slice::<ManualDrive>(&malformed).is_err());
    }
}
