//! The standard inertial measurement vocabulary.
//!
//! Producer-independent identities under `phoxal.component.imu.v1`: every
//! IMU-bearing driver (for example BNO085 and OAK-D Lite) and the native
//! simulator publish these exact shapes, so consumers bind to the concept
//! rather than a vendor. Units are SI throughout; the quaternion order is
//! [w, x, y, z].

#[phoxal::messages(package = "phoxal.component.imu.v1")]
mod v1 {
    use crate::contracts::geometry::{Quaternion, Vector3};

    /// One accelerometer sample in m/s², including gravity.
    pub struct AccelerometerSample {
        #[phoxal(tag = 1)]
        pub linear_acceleration_mps2: Option<Vector3>,
    }

    /// One gyroscope sample in rad/s.
    pub struct GyroscopeSample {
        #[phoxal(tag = 1)]
        pub angular_velocity_radps: Option<Vector3>,
    }

    /// One fused IMU sample: orientation in the sensor's mount frame,
    /// angular velocity and linear acceleration in the same frame, and the
    /// stable identifier of that frame.
    pub struct ImuSample {
        #[phoxal(tag = 1)]
        pub orientation: Option<Quaternion>,
        #[phoxal(tag = 2)]
        pub angular_velocity_radps: Option<Vector3>,
        #[phoxal(tag = 3)]
        pub linear_acceleration_mps2: Option<Vector3>,
        #[phoxal(tag = 4)]
        pub sensor_frame_id: String,
    }
}

pub use v1::*;

#[cfg(test)]
mod tests {
    use prost::Name;

    use super::{AccelerometerSample, GyroscopeSample, ImuSample};
    use crate::schema::MessageSchema;

    #[test]
    fn retains_public_protobuf_identity_and_schema() {
        assert_eq!(ImuSample::PACKAGE, "phoxal.component.imu.v1");
        assert_eq!(ImuSample::WIRE_NAME, "phoxal.component.imu.v1.ImuSample");
        assert!(ImuSample::retain_schema() > 0);
        assert!(AccelerometerSample::retain_schema() > 0);
        assert!(GyroscopeSample::retain_schema() > 0);
    }
}
