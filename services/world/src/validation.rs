use std::collections::HashSet;

use phoxal_service_kinematics::OdometryState;
use phoxal_service_world::{
    Bounds, GridWindow, Occupancy, UnavailableReason, WindowRequest, WindowResponse,
    WindowUnavailableReason, WorldBelief, WorldRevision, WorldStatus, window_response,
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
    #[error("confidence must be finite and within [0, 1]")]
    InvalidConfidence,
    #[error("world bounds must be finite and have positive extent")]
    InvalidBounds,
    #[error("world grid cells do not match its dimensions")]
    InvalidGrid,
    #[error("world unavailable reasons must be known, distinct, and bounded")]
    InvalidReasons,
    #[error("world window response is missing or invalid")]
    InvalidResponse,
}

pub fn capture_is_fresh_at(capture: Option<u64>, now_nanos: u64, max_age_nanos: u64) -> bool {
    capture
        .and_then(|capture| now_nanos.checked_sub(capture))
        .is_some_and(|age| age <= max_age_nanos)
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

pub fn belief(value: &WorldBelief) -> Result<(), ValidationError> {
    if value.available && value.oldest_capture_time_nanos.is_none() {
        return Err(ValidationError::MissingCapture);
    }
    validate_id(&value.frame_id, "frame_id")?;
    validate_finite(value.x_m, "x_m")?;
    validate_finite(value.y_m, "y_m")?;
    validate_yaw(value.yaw_rad)?;
    (value.confidence.is_finite() && (0.0..=1.0).contains(&value.confidence))
        .then_some(())
        .ok_or(ValidationError::InvalidConfidence)
}

pub fn revision(value: &WorldRevision) -> Result<(), ValidationError> {
    if value.available && value.oldest_capture_time_nanos.is_none() {
        return Err(ValidationError::MissingCapture);
    }
    Ok(())
}

pub fn bounds(value: &Bounds) -> Result<(), ValidationError> {
    for (value, field) in [
        (value.min_x_m, "min_x_m"),
        (value.min_y_m, "min_y_m"),
        (value.max_x_m, "max_x_m"),
        (value.max_y_m, "max_y_m"),
    ] {
        validate_finite(value, field)?;
    }
    (value.min_x_m < value.max_x_m && value.min_y_m < value.max_y_m)
        .then_some(())
        .ok_or(ValidationError::InvalidBounds)
}

pub fn window(value: &GridWindow) -> Result<(), ValidationError> {
    validate_id(&value.frame_id, "frame_id")?;
    validate_finite(value.origin_x_m, "origin_x_m")?;
    validate_finite(value.origin_y_m, "origin_y_m")?;
    if !value.resolution_m.is_finite() || value.resolution_m <= 0.0 {
        return Err(ValidationError::NonFinite("resolution_m"));
    }
    if value.width == 0 || value.height == 0 {
        return Err(ValidationError::InvalidGrid);
    }
    let expected = usize::try_from(value.width)
        .ok()
        .and_then(|width| {
            usize::try_from(value.height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .ok_or(ValidationError::InvalidGrid)?;
    if value.cells.len() != expected {
        return Err(ValidationError::InvalidGrid);
    }
    let requested = value
        .requested
        .as_ref()
        .ok_or(ValidationError::InvalidGrid)?;
    let covered = value.covered.as_ref().ok_or(ValidationError::InvalidGrid)?;
    for cell in &value.cells {
        let _ = Occupancy::try_from(*cell)
            .ok()
            .filter(|occupancy| *occupancy != Occupancy::Unspecified)
            .ok_or(ValidationError::InvalidGrid)?;
    }
    bounds(requested)?;
    bounds(covered)?;
    let width_m = f64::from(value.width) * value.resolution_m;
    let height_m = f64::from(value.height) * value.resolution_m;
    let epsilon = value.resolution_m * 1.0e-9;
    if (covered.min_x_m - value.origin_x_m).abs() > epsilon
        || (covered.min_y_m - value.origin_y_m).abs() > epsilon
        || (covered.max_x_m - covered.min_x_m - width_m).abs() > epsilon
        || (covered.max_y_m - covered.min_y_m - height_m).abs() > epsilon
        || requested.min_x_m < covered.min_x_m
        || requested.min_y_m < covered.min_y_m
        || requested.max_x_m > covered.max_x_m
        || requested.max_y_m > covered.max_y_m
    {
        return Err(ValidationError::InvalidGrid);
    }
    Ok(())
}

pub fn window_request(value: &WindowRequest) -> Result<(), ValidationError> {
    bounds(
        value
            .requested
            .as_ref()
            .ok_or(ValidationError::InvalidResponse)?,
    )
}

pub fn window_response(value: &WindowResponse) -> Result<(), ValidationError> {
    match value
        .result
        .as_ref()
        .ok_or(ValidationError::InvalidResponse)?
    {
        window_response::Result::Window(value) => window(value),
        window_response::Result::Unavailable(value) => {
            let _ = WindowUnavailableReason::try_from(value.reason)
                .ok()
                .filter(|reason| *reason != WindowUnavailableReason::Unspecified)
                .ok_or(ValidationError::InvalidResponse)?;
            Ok(())
        }
    }
}

pub fn status(value: &WorldStatus) -> Result<(), ValidationError> {
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
    fn validates_coherent_free_window_shape() {
        let requested = Bounds {
            min_x_m: 0.0,
            min_y_m: 0.0,
            max_x_m: 0.2,
            max_y_m: 0.2,
        };
        let value = GridWindow {
            frame_id: "map".into(),
            origin_x_m: 0.0,
            origin_y_m: 0.0,
            resolution_m: 0.1,
            width: 2,
            height: 2,
            cells: vec![Occupancy::Free as i32; 4],
            revision: 1,
            requested: Some(requested),
            covered: Some(requested),
        };
        assert!(window(&value).is_ok());
    }
}
