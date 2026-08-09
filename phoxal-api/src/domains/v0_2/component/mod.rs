//! v0.2 component payloads.

pub mod camera;
pub mod motor;
#[allow(unused_imports)]
pub use crate::domains::v0_1::component::{
    accelerometer, battery, depth, emergency_stop, encoder, gnss, gyroscope, imu, led, lidar,
    magnetometer, microphone, mmwave, range, speaker,
};
