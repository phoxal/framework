//! Generated safety messages, typed ports, and domain validation.

use std::collections::HashSet;
use std::path::PathBuf;

include!(concat!(env!("OUT_DIR"), "/phoxal.safety.v1.rs"));

/// Canonical motion status consumed by the safety assessment.
pub use phoxal_motion::MotionStatus;
/// Canonical world products consumed by the safety assessment.
pub use phoxal_world::{WorldBelief, WorldRevision};

/// Returns the packaged Protobuf include root for downstream contract owners.
#[must_use]
pub fn proto_include_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("proto")
}

/// Public typed ports owned by the Safety Protobuf service.
pub use safety::ports;

/// The original descriptor closure for independent language generation and inspection.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/phoxal-descriptors.bin"));

/// Maximum UTF-8 byte length of execution-scoped safety identifiers.
pub const MAX_ID_BYTES: usize = 64;

/// A safety message violates the version-one domain contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ValidationError {
    /// An identifier is empty or exceeds its bound.
    #[error("{0} must contain 1 to {MAX_ID_BYTES} UTF-8 bytes")]
    InvalidId(&'static str),
    /// A measured scalar is not finite.
    #[error("{0} must be finite")]
    NonFinite(&'static str),
    /// A yaw is outside the canonical interval.
    #[error("yaw_rad must be finite and within [-pi, pi]")]
    InvalidYaw,
    /// Confidence is outside the closed unit interval.
    #[error("confidence must be finite and within [0, 1]")]
    InvalidConfidence,
    /// A range distance is invalid.
    #[error("distance_m must be finite and nonnegative")]
    InvalidDistance,
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

impl RangeObservation {
    /// Validates one measured range observation.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_id(&self.sensor_id, "sensor_id")?;
        if !self.distance_m.is_finite() || self.distance_m < 0.0 {
            return Err(ValidationError::InvalidDistance);
        }
        Ok(())
    }
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

fn validate_id(value: &str, field: &'static str) -> Result<(), ValidationError> {
    if value.is_empty() || value.len() > MAX_ID_BYTES {
        return Err(ValidationError::InvalidId(field));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use phoxal_port::{PortDescriptor, PortKind};
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
            <phoxal_port::Sample<RangeObservation> as PortDescriptor>::KIND,
            PortKind::Sample
        );
        assert_eq!(
            <phoxal_port::State<MotionConstraints> as PortDescriptor>::KIND,
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
            valid_from_nanos: 10,
            expires_at_nanos: 20,
        };
        assert_eq!(bad.validate(), Err(ValidationError::InvalidProduct));
    }

    #[test]
    fn rejects_nonfinite_world_and_range_values() {
        assert_eq!(
            (RangeObservation {
                sensor_id: "front".into(),
                distance_m: -1.0,
                valid: true,
            })
            .validate(),
            Err(ValidationError::InvalidDistance)
        );
    }
}
