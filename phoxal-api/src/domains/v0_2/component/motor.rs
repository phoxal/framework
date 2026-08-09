//! v0.2 motor payloads.
#![allow(legacy_derive_helpers)]

                /// A per-actuator command in the current control revision.
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub enum Command {
                    Position(#[serde(deserialize_with = "crate::deserialize_finite_motor_scalar")] f32),
                    Velocity(#[serde(deserialize_with = "crate::deserialize_finite_motor_scalar")] f32),
                    Torque(#[serde(deserialize_with = "crate::deserialize_finite_motor_scalar")] f32),
                    Stop,
                }

