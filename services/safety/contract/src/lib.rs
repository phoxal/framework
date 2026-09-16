//! Generated safety messages, typed ports, and domain validation.

use std::collections::HashSet;

include!(concat!(env!("OUT_DIR"), "/phoxal.safety.v1.rs"));

/// Canonical metric range consumed directly from sensor owners.
pub use phoxal_robotics::RangeSample;

/// Canonical motion status consumed by the safety assessment.
pub use phoxal_motion::MotionStatus;
/// Canonical world products consumed by the safety assessment.
pub use phoxal_world::{WorldBelief, WorldRevision};

/// Public typed ports owned by the Safety Protobuf service.
pub use safety::ports;

/// The original descriptor closure for independent language generation and inspection.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/phoxal-descriptors.bin"));

/// A safety message violates the version-one domain contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ValidationError {
    /// A constraint has an invalid reason or limit shape.
    #[error("safety constraint is invalid")]
    InvalidConstraint,
    /// A constraints product has contradictory permission or validity.
    #[error("safety constraints are incoherent")]
    InvalidProduct,
    /// Reasons contain an unspecified or duplicate value.
    #[error("safety reasons must be known, distinct, and bounded")]
    InvalidReasons,
}

impl Constraint {
    /// Validates one protective constraint's finite optional limits.
    pub fn validate(&self) -> Result<(), ValidationError> {
        let reason = ConstraintReason::try_from(self.reason)
            .ok()
            .filter(|reason| *reason != ConstraintReason::Unspecified)
            .ok_or(ValidationError::InvalidConstraint)?;
        let _ = reason;
        for value in [
            self.max_linear_speed_mps,
            self.max_angular_speed_radps,
            self.observed_value,
        ]
        .into_iter()
        .flatten()
        {
            if !value.is_finite() || value < 0.0 {
                return Err(ValidationError::InvalidConstraint);
            }
        }
        if matches!(
            reason,
            ConstraintReason::ObstacleProximity
                | ConstraintReason::RangeUnavailable
                | ConstraintReason::RangeFault
                | ConstraintReason::WorldUnavailable
                | ConstraintReason::LocalizationUncertain
                | ConstraintReason::MapUnavailable
                | ConstraintReason::MapBlocked
                | ConstraintReason::MotionUnavailable
                | ConstraintReason::MotionFault
        ) && self.max_linear_speed_mps.is_none()
            && self.max_angular_speed_radps.is_none()
            && self.observed_value.is_none()
        {
            return Err(ValidationError::InvalidConstraint);
        }
        Ok(())
    }
}

impl MotionConstraints {
    /// Validates permission, bounded reasons, and the expiry interval.
    pub fn validate(&self) -> Result<(), ValidationError> {
        let permission = Permission::try_from(self.permission)
            .ok()
            .filter(|permission| *permission != Permission::Unspecified)
            .ok_or(ValidationError::InvalidProduct)?;
        if permission != Permission::Stopped
            && self
                .oldest_capture_time_nanos
                .is_none_or(|capture| capture > self.valid_from_nanos)
        {
            return Err(ValidationError::InvalidProduct);
        }
        if self.expires_at_nanos < self.valid_from_nanos {
            return Err(ValidationError::InvalidProduct);
        }
        if self.constraints.len() > 16 {
            return Err(ValidationError::InvalidProduct);
        }
        let mut reasons = HashSet::with_capacity(self.constraints.len());
        let mut has_limit = false;
        for constraint in &self.constraints {
            constraint.validate()?;
            let reason = ConstraintReason::try_from(constraint.reason)
                .map_err(|_| ValidationError::InvalidConstraint)?;
            if !reasons.insert(reason) {
                return Err(ValidationError::InvalidProduct);
            }
            if permission == Permission::Limited
                && (reason != ConstraintReason::ObstacleProximity
                    || (constraint.max_linear_speed_mps.is_none()
                        && constraint.max_angular_speed_radps.is_none()))
            {
                return Err(ValidationError::InvalidProduct);
            }
            has_limit |= constraint.max_linear_speed_mps.is_some()
                || constraint.max_angular_speed_radps.is_some();
        }
        match permission {
            Permission::Clear if !self.constraints.is_empty() => {
                Err(ValidationError::InvalidProduct)
            }
            Permission::Limited if self.constraints.is_empty() || !has_limit => {
                Err(ValidationError::InvalidProduct)
            }
            Permission::Stopped if self.constraints.is_empty() => {
                Err(ValidationError::InvalidProduct)
            }
            _ => Ok(()),
        }
    }
}

impl SafetyStatus {
    /// Validates the status reason set.
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
            return Err(ValidationError::InvalidReasons);
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
        assert!(!FILE_DESCRIPTOR_SET.is_empty());
    }

    #[test]
    fn accepts_a_clear_product_and_rejects_a_contradictory_one() {
        MotionConstraints {
            sequence: 1,
            permission: Permission::Clear as i32,
            constraints: Vec::new(),
            oldest_capture_time_nanos: Some(0),
            valid_from_nanos: 10,
            expires_at_nanos: 20,
        }
        .validate()
        .expect("clear has no constraints");
        let bad = MotionConstraints {
            sequence: 1,
            permission: Permission::Clear as i32,
            constraints: vec![Constraint {
                reason: ConstraintReason::MapUnavailable as i32,
                max_linear_speed_mps: None,
                max_angular_speed_radps: None,
                observed_value: Some(0.0),
            }],
            oldest_capture_time_nanos: Some(0),
            valid_from_nanos: 10,
            expires_at_nanos: 20,
        };
        assert_eq!(bad.validate(), Err(ValidationError::InvalidProduct));
    }
}
