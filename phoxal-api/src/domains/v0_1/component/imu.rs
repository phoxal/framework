//! v0.1 component imu payloads.
#![allow(legacy_derive_helpers)]

                #[derive(Copy, Eq)]
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                #[serde(rename_all = "snake_case")]
                pub enum SensorHealth {
                    Nominal,
                    Degraded,
                    Fault,
                }

                #[derive(Copy)]
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct Bias {
                    pub angular_velocity_radps: [f32; 3],
                    pub linear_acceleration_mps2: [f32; 3],
                }

                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct Sample {
                    pub orientation: Option<[f32; 4]>,
                    pub angular_velocity_radps: [f32; 3],
                    pub linear_acceleration_mps2: [f32; 3],
                    pub covariance: Option<[f32; 9]>,
                    pub noise_density: Option<[f32; 3]>,
                    pub sensor_frame_id: Option<String>,
                    pub health: SensorHealth,
                    pub bias: Option<Bias>,
                }


