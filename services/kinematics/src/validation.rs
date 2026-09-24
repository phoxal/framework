use std::collections::HashSet;

use crate::api::kinematics::v1::{
    FrameTransform, FrameTree, JointState, KinematicsStatus, LookupFrameRequest,
    LookupFrameResponse, OdometryState, UnavailableReason,
};

pub const MAX_ID_BYTES: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ValidationError {
    #[error("available evidence is missing capture provenance")]
    MissingCapture,
    #[error("{0} must contain 1 to {MAX_ID_BYTES} UTF-8 bytes")]
    InvalidId(&'static str),
    #[error("{0} must be finite")]
    NonFinite(&'static str),
    #[error("yaw_rad must be finite and within [-pi, pi]")]
    InvalidYaw,
    #[error("unavailable reasons must be known, distinct, and bounded")]
    InvalidReasons,
    #[error("frame tree contains duplicate or self-referential transforms")]
    InvalidFrameTree,
}

pub fn joint(value: &JointState) -> Result<(), ValidationError> {
    validate_id(&value.joint_id, "joint_id")?;
    validate_finite(value.position_rad, "position_rad")?;
    validate_finite(value.velocity_radps, "velocity_radps")?;
    if let Some(effort_nm) = value.effort_nm {
        validate_finite(effort_nm, "effort_nm")?;
    }
    Ok(())
}

pub fn odometry(value: &OdometryState) -> Result<(), ValidationError> {
    if value.available && value.oldest_capture_time_nanos.is_none() {
        return Err(ValidationError::MissingCapture);
    }
    validate_finite(value.x_m, "x_m")?;
    validate_finite(value.y_m, "y_m")?;
    validate_yaw(value.yaw_rad)?;
    validate_finite(value.linear_x_mps, "linear_x_mps")?;
    validate_finite(value.angular_z_radps, "angular_z_radps")
}

pub fn frame_transform(value: &FrameTransform) -> Result<(), ValidationError> {
    validate_id(&value.parent_frame_id, "parent_frame_id")?;
    validate_id(&value.child_frame_id, "child_frame_id")?;
    if value.parent_frame_id == value.child_frame_id {
        return Err(ValidationError::InvalidFrameTree);
    }
    validate_finite(value.x_m, "x_m")?;
    validate_finite(value.y_m, "y_m")?;
    validate_yaw(value.yaw_rad)
}

pub fn frame_tree(value: &FrameTree) -> Result<(), ValidationError> {
    let mut children = HashSet::with_capacity(value.transforms.len());
    for transform in &value.transforms {
        frame_transform(transform)?;
        if !children.insert(transform.child_frame_id.as_str()) {
            return Err(ValidationError::InvalidFrameTree);
        }
    }
    Ok(())
}

pub fn status(value: &KinematicsStatus) -> Result<(), ValidationError> {
    if value.unavailable_reasons.len() > 4 {
        return Err(ValidationError::InvalidReasons);
    }
    let mut reasons = HashSet::with_capacity(value.unavailable_reasons.len());
    for reason in &value.unavailable_reasons {
        let reason = UnavailableReason::try_from(*reason)
            .ok()
            .filter(|reason| *reason != UnavailableReason::Unspecified)
            .ok_or(ValidationError::InvalidReasons)?;
        if !reasons.insert(reason) {
            return Err(ValidationError::InvalidReasons);
        }
    }
    if value.available && !value.unavailable_reasons.is_empty() {
        return Err(ValidationError::InvalidReasons);
    }
    Ok(())
}

pub fn lookup_request(value: &LookupFrameRequest) -> Result<(), ValidationError> {
    validate_id(&value.parent_frame_id, "parent_frame_id")?;
    validate_id(&value.child_frame_id, "child_frame_id")
}

pub fn lookup_response(value: &LookupFrameResponse) -> Result<(), ValidationError> {
    if let Some(transform) = &value.transform {
        frame_transform(transform)?;
    }
    Ok(())
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
    use super::*;

    #[test]
    fn accepts_finite_frame_tree() {
        let tree = FrameTree {
            transforms: vec![FrameTransform {
                parent_frame_id: "odom".into(),
                child_frame_id: "base".into(),
                x_m: 0.0,
                y_m: 0.0,
                yaw_rad: 0.0,
            }],
            revision: 1,
        };
        assert!(frame_tree(&tree).is_ok());
    }

    #[test]
    fn rejects_duplicate_frame_children() {
        let tree = FrameTree {
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
        assert_eq!(frame_tree(&tree), Err(ValidationError::InvalidFrameTree));
    }
}
