//! v0.1 component magnetometer payloads.
#![allow(legacy_derive_helpers)]

                /// Raw magnetic-field sample in the sensor-local frame.
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct Sample {
                    pub magnetic_field: [f32; 3],
                }


