//! Public kinematics contract library.
//!
//! Generated Protobuf messages, the original descriptor closure, the typed
//! public port references, and the pure validation of the public payload all
//! live here. The executable Runtime state, the parsed validator inputs, and
//! the launched process live behind the binary target and are not re-exported.

use std::collections::HashSet;

include!(concat!(env!("OUT_DIR"), "/phoxal.kinematics.v1.rs"));

/// The shared encoder payload used by physical producers and this service's
/// encoder input port.
pub use phoxal_robotics::EncoderSample;

/// Public typed ports owned by the Kinematics Protobuf service.
pub use kinematics::ports;

/// The original descriptor closure for independent language generation and inspection.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/phoxal-descriptors.bin"));

/// Maximum UTF-8 byte length of execution-scoped kinematics identifiers.
pub const MAX_ID_BYTES: usize = 64;

/// A kinematics message violates the version-one domain contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ValidationError {
    /// Available evidence must retain its original capture time.
    #[error("available evidence is missing capture provenance")]
    MissingCapture,

    /// An identifier is empty or exceeds its bound.
    #[error("{0} must contain 1 to {MAX_ID_BYTES} UTF-8 bytes")]
    InvalidId(&'static str),
    /// A physical quantity is not finite.
    #[error("{0} must be finite")]
    NonFinite(&'static str),
    /// A yaw is outside the canonical interval.
    #[error("yaw_rad must be finite and within [-pi, pi]")]
    InvalidYaw,
    /// The message contains an unspecified or repeated unavailable reason.
    #[error("unavailable reasons must be known, distinct, and bounded")]
    InvalidReasons,
    /// A frame tree contains contradictory transform identities.
    #[error("frame tree contains duplicate or self-referential transforms")]
    InvalidFrameTree,
}

impl JointState {
    /// Validates one measured joint state.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_id(&self.joint_id, "joint_id")?;
        validate_finite(self.position_rad, "position_rad")?;
        validate_finite(self.velocity_radps, "velocity_radps")?;
        if let Some(effort_nm) = self.effort_nm {
            validate_finite(effort_nm, "effort_nm")?;
        }
        Ok(())
    }
}

impl OdometryState {
    /// Tests the age of the original supporting capture in one execution timeline.
    /// Missing provenance and future captures are never fresh.
    #[must_use]
    pub fn capture_is_fresh_at(&self, now_nanos: u64, max_age_nanos: u64) -> bool {
        self.oldest_capture_time_nanos
            .and_then(|capture| now_nanos.checked_sub(capture))
            .is_some_and(|age| age <= max_age_nanos)
    }

    /// Validates pose, velocity, revision, and availability fields.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.available && self.oldest_capture_time_nanos.is_none() {
            return Err(ValidationError::MissingCapture);
        }

        validate_finite(self.x_m, "x_m")?;
        validate_finite(self.y_m, "y_m")?;
        validate_yaw(self.yaw_rad)?;
        validate_finite(self.linear_x_mps, "linear_x_mps")?;
        validate_finite(self.angular_z_radps, "angular_z_radps")
    }
}

impl FrameTransform {
    /// Validates one model-backed frame edge.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_id(&self.parent_frame_id, "parent_frame_id")?;
        validate_id(&self.child_frame_id, "child_frame_id")?;
        if self.parent_frame_id == self.child_frame_id {
            return Err(ValidationError::InvalidFrameTree);
        }
        validate_finite(self.x_m, "x_m")?;
        validate_finite(self.y_m, "y_m")?;
        validate_yaw(self.yaw_rad)
    }
}

impl FrameTree {
    /// Validates bounded, uniquely identified frame edges.
    pub fn validate(&self) -> Result<(), ValidationError> {
        let mut children = HashSet::with_capacity(self.transforms.len());
        for transform in &self.transforms {
            transform.validate()?;
            if !children.insert(transform.child_frame_id.as_str()) {
                return Err(ValidationError::InvalidFrameTree);
            }
        }
        Ok(())
    }
}

impl KinematicsStatus {
    /// Validates availability reasons and their bounded cardinality.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.unavailable_reasons.len() > 4 {
            return Err(ValidationError::InvalidReasons);
        }
        let mut reasons = HashSet::with_capacity(self.unavailable_reasons.len());
        for reason in &self.unavailable_reasons {
            let reason = UnavailableReason::try_from(*reason)
                .ok()
                .filter(|reason| *reason != UnavailableReason::Unspecified)
                .ok_or(ValidationError::InvalidReasons)?;
            if !reasons.insert(reason) {
                return Err(ValidationError::InvalidReasons);
            }
        }
        if self.available && !self.unavailable_reasons.is_empty() {
            return Err(ValidationError::InvalidReasons);
        }
        Ok(())
    }
}

impl LookupFrameRequest {
    /// Validates the requested frame edge.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_id(&self.parent_frame_id, "parent_frame_id")?;
        validate_id(&self.child_frame_id, "child_frame_id")
    }
}

impl LookupFrameResponse {
    /// Validates a found transform when one is present.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if let Some(transform) = &self.transform {
            transform.validate()?;
        }
        Ok(())
    }
}

fn validate_id(value: &str, field: &'static str) -> Result<(), ValidationError> {
    if value.is_empty() || value.len() > MAX_ID_BYTES {
        return Err(ValidationError::InvalidId(field));
    }
    Ok(())
}

fn validate_finite(value: f64, field: &'static str) -> Result<(), ValidationError> {
    value
        .is_finite()
        .then_some(())
        .ok_or(ValidationError::NonFinite(field))
}

fn validate_yaw(value: f64) -> Result<(), ValidationError> {
    (value.is_finite() && (-std::f64::consts::PI..=std::f64::consts::PI).contains(&value))
        .then_some(())
        .ok_or(ValidationError::InvalidYaw)
}

#[cfg(test)]
mod tests {
    use phoxal::port::{PortDescriptor, PortKind};
    use prost::Name;

    use super::*;

    #[test]
    fn generated_ports_retain_names_kinds_and_message_identity() {
        assert_eq!(ports::ENCODER_MEASUREMENTS.name(), "encoder_measurements");
        assert_eq!(ports::JOINTS.name(), "joints");
        assert_eq!(ports::ODOMETRY.name(), "odometry");
        assert_eq!(ports::FRAMES.name(), "frames");
        assert_eq!(ports::STATUS.name(), "status");
        assert_eq!(ports::LOOKUP_FRAME.name(), "lookup_frame");
        assert_eq!(
            <phoxal::port::Sample<EncoderSample> as PortDescriptor>::KIND,
            PortKind::Sample
        );
        assert_eq!(
            <phoxal::port::Read<LookupFrameRequest, LookupFrameResponse> as PortDescriptor>::KIND,
            PortKind::Read
        );
        assert_eq!(OdometryState::PACKAGE, "phoxal.kinematics.v1");
        assert!(!FILE_DESCRIPTOR_SET.is_empty());
    }

    #[test]
    fn finite_measurements_and_frame_trees_are_accepted() {
        EncoderSample {
            position_rad: Some(0.0),
            velocity_radps: Some(-0.0),
        }
        .validate()
        .expect("zero is a measurement");
        FrameTree {
            transforms: vec![FrameTransform {
                parent_frame_id: "odom".into(),
                child_frame_id: "base".into(),
                x_m: 0.0,
                y_m: 0.0,
                yaw_rad: 0.0,
            }],
            revision: 1,
        }
        .validate()
        .expect("one frame edge is valid");
    }

    #[test]
    fn rejects_invalid_measurement_and_duplicate_frame_children() {
        let duplicate = FrameTree {
            transforms: vec![
                FrameTransform {
                    parent_frame_id: "odom".into(),
                    child_frame_id: "base".into(),
                    x_m: 0.0,
                    y_m: 0.0,
                    yaw_rad: 0.0,
                },
                FrameTransform {
                    parent_frame_id: "map".into(),
                    child_frame_id: "base".into(),
                    x_m: 0.0,
                    y_m: 0.0,
                    yaw_rad: 0.0,
                },
            ],
            revision: 1,
        };
        assert_eq!(duplicate.validate(), Err(ValidationError::InvalidFrameTree));
    }
}
