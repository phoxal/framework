//! Generated safety messages, typed ports, and domain validation.
//!
//! Safety owns `SafetyStatus` and the assessment algorithms that publish the
//! constraints endpoint. The protective-constraint payload
//! (`ConstraintReason`, `Constraint`, `Permission`, `MotionConstraints`) is
//! owned by Motion and re-exported here from `phoxal_motion`; its pure
//! validation lives in the Motion contract crate, where the orphan rules
//! permit it. Safety composes those types with world/range evidence and
//! validates the resulting `SafetyStatus`.

use std::collections::HashSet;

include!(concat!(env!("OUT_DIR"), "/phoxal.safety.v1.rs"));

/// Canonical metric range consumed directly from sensor owners.
pub use phoxal_robotics::RangeSample;

/// Canonical motion status consumed by the safety assessment.
pub use phoxal_motion::MotionStatus;
/// Canonical protective-constraint payload owned by Motion.
pub use phoxal_motion::{Constraint, ConstraintReason, MotionConstraints, Permission};
/// Re-export the constraint-validation error from the Motion contract so
/// callers can match it without depending on `phoxal_motion` directly.
pub use phoxal_motion::ConstraintValidationError;
/// Canonical world products consumed by the safety assessment.
pub use phoxal_service_world::{WorldBelief, WorldRevision};

/// Public typed ports owned by the Safety Protobuf service.
pub use safety::ports;

/// The original descriptor closure for independent language generation and inspection.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/phoxal-descriptors.bin"));

/// A safety message violates the version-one domain contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ValidationError {
    /// Reasons contain an unspecified or duplicate value.
    #[error("safety reasons must be known, distinct, and bounded")]
    InvalidReasons,
    /// A reason set contradicts the protective state.
    #[error("safety status contradicts its protective state")]
    Inconsistent,
    /// The Motion-owned payload failed its own validation.
    #[error("motion constraints payload is invalid: {0}")]
    ConstraintPayload(#[from] ConstraintValidationError),
}

impl SafetyStatus {
    /// Validates the status reason set and its consistency with the
    /// declared protective state. The constraint-side validation lives on
    /// `MotionConstraints` in the Motion contract crate.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.reasons.len() > 16 {
            return Err(ValidationError::InvalidReasons);
        }
        let mut reasons = HashSet::with_capacity(self.reasons.len());
        for reason in &self.reasons {
            let reason = ConstraintReason::try_from(*reason)
                .ok()
                .filter(|reason| *reason != ConstraintReason::Unspecified)
                .ok_or(ValidationError::InvalidReasons)?;
            if !reasons.insert(reason) {
                return Err(ValidationError::InvalidReasons);
            }
        }
        if self.protective_state_clear && !self.reasons.is_empty() {
            return Err(ValidationError::Inconsistent);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use phoxal::port::{PortDescriptor, PortKind};
    use prost::Name;

    use super::*;

    #[test]
    fn generated_ports_retain_names_kinds_and_message_identity() {
        assert_eq!(ports::WORLD.name(), "world");
        assert_eq!(ports::WORLD_REVISION.name(), "world_revision");
        assert_eq!(ports::RANGES.name(), "ranges");
        assert_eq!(ports::MOTION.name(), "motion");
        assert_eq!(ports::CONSTRAINTS.name(), "constraints");
        assert_eq!(ports::STATUS.name(), "status");
        assert_eq!(
            <phoxal::port::Sample<RangeSample> as PortDescriptor>::KIND,
            PortKind::Sample
        );
        assert_eq!(
            <phoxal::port::State<MotionConstraints> as PortDescriptor>::KIND,
            PortKind::State
        );
        assert_eq!(WorldBelief::PACKAGE, "phoxal.world.v1");
        assert_eq!(MotionStatus::PACKAGE, "phoxal.motion.v1");
        // The Motion-owned payload remains in its owner proto package after
        // the protective-constraint ownership move.
        assert_eq!(MotionConstraints::PACKAGE, "phoxal.motion.v1");
        assert_eq!(ConstraintReason::Unspecified as i32, 0);
        assert_eq!(Permission::Stopped as i32, 3);
        assert!(!FILE_DESCRIPTOR_SET.is_empty());
    }

    #[test]
    fn safety_status_validation_runs_independently_of_payload_validation() {
        SafetyStatus {
            protective_state_clear: true,
            sequence: 0,
            reasons: Vec::new(),
        }
        .validate()
        .expect("clear state has no reasons");
        let inconsistent = SafetyStatus {
            protective_state_clear: true,
            sequence: 0,
            reasons: vec![ConstraintReason::MapUnavailable as i32],
        };
        assert_eq!(inconsistent.validate(), Err(ValidationError::Inconsistent));
    }

    #[test]
    fn rejects_duplicate_or_unspecified_status_reasons() {
        let dup = SafetyStatus {
            protective_state_clear: false,
            sequence: 1,
            reasons: vec![
                ConstraintReason::MapUnavailable as i32,
                ConstraintReason::MapUnavailable as i32,
            ],
        };
        assert_eq!(dup.validate(), Err(ValidationError::InvalidReasons));
        let unknown = SafetyStatus {
            protective_state_clear: false,
            sequence: 1,
            reasons: vec![ConstraintReason::Unspecified as i32],
        };
        assert_eq!(unknown.validate(), Err(ValidationError::InvalidReasons));
    }
}