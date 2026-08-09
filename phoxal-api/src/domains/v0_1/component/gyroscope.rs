//! v0.1 component gyroscope payloads.
#![allow(legacy_derive_helpers)]

                /// Raw angular velocity sample in the sensor-local frame in rad/s.
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct Sample {
                    pub angular_velocity: [f32; 3],
                }


