//! Producer-independent spatial quantities, shared by sensors and services.
//!
//! Frames are right-handed; mobile robot body axes are x forward, y left,
//! z up. Distances use metres, angles radians, and time seconds. A containing
//! contract identifies the reference frame and observation time. These value
//! types do not assert a world frame or invent their own timestamp.

#[phoxal::messages(package = "phoxal.geometry.v1")]
mod v1 {
    /// A vector with units determined by its containing field.
    /// Unlike a point, changing frame origin does not translate a vector.
    pub struct Vector3 {
        #[phoxal(tag = 1)]
        pub x: f64,
        #[phoxal(tag = 2)]
        pub y: f64,
        #[phoxal(tag = 3)]
        pub z: f64,
    }

    /// A location in metres relative to the containing contract's frame.
    pub struct Point3 {
        #[phoxal(tag = 1)]
        pub x: f64,
        #[phoxal(tag = 2)]
        pub y: f64,
        #[phoxal(tag = 3)]
        pub z: f64,
    }

    /// A unit quaternion, with scalar component w and vector components x/y/z.
    /// Producers normalize it; the all-zero default is not a valid rotation.
    pub struct Quaternion {
        #[phoxal(tag = 1)]
        pub w: f64,
        #[phoxal(tag = 2)]
        pub x: f64,
        #[phoxal(tag = 3)]
        pub y: f64,
        #[phoxal(tag = 4)]
        pub z: f64,
    }

    /// Position and orientation in one reference frame.
    /// Missing quantities are unknown, not zero or the identity rotation.
    pub struct Pose {
        #[phoxal(tag = 1)]
        pub position_m: Option<Point3>,
        #[phoxal(tag = 2)]
        pub orientation: Option<Quaternion>,
    }

    /// Linear and angular velocity, expressed in the same frame.
    pub struct Twist {
        #[phoxal(tag = 1)]
        pub linear_mps: Option<Vector3>,
        #[phoxal(tag = 2)]
        pub angular_radps: Option<Vector3>,
    }

    /// Force and torque expressed in one frame, about that frame's origin.
    pub struct Wrench {
        #[phoxal(tag = 1)]
        pub force_n: Option<Vector3>,
        #[phoxal(tag = 2)]
        pub torque_nm: Option<Vector3>,
    }
}

pub use v1::*;
