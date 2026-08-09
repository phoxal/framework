//! v0.2 component payloads.

pub mod accelerometer;
pub mod battery;
pub mod camera;
pub mod depth;
pub mod encoder;
pub mod gnss;
pub mod gyroscope;
pub mod imu;
pub mod lidar;
pub mod magnetometer;
pub mod mmwave;
pub mod motor;
pub mod range;
#[allow(unused_imports)]
pub use crate::domains::v0_1::component::{emergency_stop, led, microphone, speaker};
