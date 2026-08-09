pub(crate) mod accelerometer;
pub(crate) mod battery;
pub(crate) mod camera;
pub(crate) mod depth;
pub(crate) mod emergency_stop;
pub(crate) mod encoder;
pub(crate) mod gnss;
pub(crate) mod gyroscope;
pub(crate) mod imu;
pub(crate) mod led;
pub(crate) mod lidar;
pub(crate) mod magnetometer;
pub(crate) mod microphone;
pub(crate) mod mmwave;
pub(crate) mod motor;
pub(crate) mod range;
pub(crate) mod speaker;

phoxal_macros::phoxal_api_fragment_group! {
    fragments {
        accelerometer;
        battery;
        camera;
        depth;
        emergency_stop;
        encoder;
        gnss;
        gyroscope;
        imu;
        led;
        lidar;
        magnetometer;
        microphone;
        mmwave;
        motor;
        range;
        speaker;
    }
}
