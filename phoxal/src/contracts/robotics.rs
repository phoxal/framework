//! Shared physical and robotics vocabulary for Phoxal contracts.
//!
//! Contains the producer-independent odometry product and the shared
//! domain validation of the robotics vocabulary. It starts no runtime,
//! transport, hardware driver, or simulator.

/// One integrated planar odometry state.
///
/// This is a producer-independent robotics concept: whichever participant
/// integrates the pose (a kinematics service, a localization product, or a
/// hardware component) publishes this exact identity, and composition
/// selects the producer. Semantics:
///
/// - **Pose** (`x_m`, `y_m`, `yaw_rad`): the planar SE(2) pose of the
///   robot body in the producer's local odometry frame — the frame whose
///   origin and heading the producer fixed when it started integrating.
///   The standard does not claim a world or map frame; a named-frame pose
///   is a different, future message.
/// - **Twist** (`linear_x_mps`, `angular_z_radps`): the body-frame
///   forward velocity along the body x axis and the counter-clockwise yaw
///   rate around the body z axis, planar.
/// - **Revision**: the producer's monotonically non-decreasing
///   integration revision; consumers use it for change detection only.
/// - **Validity** (`available`): false means the producer currently
///   derives no usable estimate (for example lost encoder input); the
///   fields still carry the last integrated values and must not be
///   treated as fresh measurements.
/// - **Capture time** (`oldest_capture_time_nanos`): the oldest source
///   observation time, in nanoseconds on the execution timeline, that
///   this integrated state rests on. Republishing or deriving further
///   state preserves it; it is never the publication time.
#[phoxal::message(package = "phoxal.robotics.v1")]
pub struct OdometryState {
    #[phoxal(tag = 1)]
    pub x_m: f64,
    #[phoxal(tag = 2)]
    pub y_m: f64,
    #[phoxal(tag = 3)]
    pub yaw_rad: f64,
    #[phoxal(tag = 4)]
    pub linear_x_mps: f64,
    #[phoxal(tag = 5)]
    pub angular_z_radps: f64,
    #[phoxal(tag = 6)]
    pub revision: u64,
    #[phoxal(tag = 7)]
    pub available: bool,
    #[phoxal(tag = 8)]
    pub oldest_capture_time_nanos: Option<u64>,
}

/// A shared robotics value violates its public domain contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ValidationError {
    /// Range bounds or a reported valid distance are incoherent.
    #[error("range distance and limits must be finite, nonnegative, and coherent")]
    InvalidRange,
    /// An encoder quantity is present but is not finite.
    #[error("encoder {field} must be finite when present")]
    NonFiniteEncoderValue {
        /// The invalid field name.
        field: &'static str,
    },
}

pub(crate) fn validate_optional_finite(
    value: Option<f64>,
    field: &'static str,
) -> Result<(), ValidationError> {
    if value.is_some_and(|value| !value.is_finite()) {
        return Err(ValidationError::NonFiniteEncoderValue { field });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use prost::Name;

    use super::OdometryState;
    use crate::schema::MessageSchema;

    #[test]
    fn retains_public_protobuf_identity_and_schema() {
        assert_eq!(OdometryState::PACKAGE, "phoxal.robotics.v1");
        assert_eq!(OdometryState::NAME, "OdometryState");
        assert_eq!(OdometryState::WIRE_NAME, "phoxal.robotics.v1.OdometryState");
        assert!(OdometryState::retain_schema() > 0);
    }
}
