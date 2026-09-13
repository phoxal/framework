//! Generated world messages, typed ports, and domain validation.

use std::collections::HashSet;

include!(concat!(env!("OUT_DIR"), "/phoxal.world.v1.rs"));

/// The kinematics-owned odometry payload admitted at the world boundary.
pub use phoxal_kinematics::OdometryState;

/// Public typed ports owned by the World Protobuf service.
pub use world::ports;

/// The original descriptor closure for independent language generation and inspection.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/phoxal-descriptors.bin"));

/// Maximum UTF-8 byte length of execution-scoped world identifiers.
pub const MAX_ID_BYTES: usize = 64;

/// A world message violates the version-one domain contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ValidationError {
    /// An identifier is empty or exceeds its bound.
    #[error("{0} must contain 1 to {MAX_ID_BYTES} UTF-8 bytes")]
    InvalidId(&'static str),
    /// A spatial scalar is not finite.
    #[error("{0} must be finite")]
    NonFinite(&'static str),
    /// A yaw is outside the canonical interval.
    #[error("yaw_rad must be finite and within [-pi, pi]")]
    InvalidYaw,
    /// Confidence is outside the closed unit interval.
    #[error("confidence must be finite and within [0, 1]")]
    InvalidConfidence,
    /// A bounds value has no positive extent.
    #[error("world bounds must be finite and have positive extent")]
    InvalidBounds,
    /// A grid's dimensions and cells disagree.
    #[error("world grid cells do not match its dimensions")]
    InvalidGrid,
    /// A world state contains an unspecified or repeated reason.
    #[error("world unavailable reasons must be known, distinct, and bounded")]
    InvalidReasons,
    /// A response has no selected result or an invalid unavailable reason.
    #[error("world window response is missing or invalid")]
    InvalidResponse,
}

impl WorldBelief {
    /// Validates a world belief and its availability relation.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_id(&self.frame_id, "frame_id")?;
        validate_pose(self.x_m, self.y_m, self.yaw_rad)?;
        validate_confidence(self.confidence)
    }
}

impl WorldRevision {
    /// Validates a revision marker.
    pub const fn validate(&self) -> Result<(), ValidationError> {
        Ok(())
    }
}

impl Bounds {
    /// Validates finite bounds with positive width and height.
    pub fn validate(&self) -> Result<(), ValidationError> {
        for (value, field) in [
            (self.min_x_m, "min_x_m"),
            (self.min_y_m, "min_y_m"),
            (self.max_x_m, "max_x_m"),
            (self.max_y_m, "max_y_m"),
        ] {
            validate_finite(value, field)?;
        }
        (self.min_x_m < self.max_x_m && self.min_y_m < self.max_y_m)
            .then_some(())
            .ok_or(ValidationError::InvalidBounds)
    }
}

impl GridWindow {
    /// Validates the self-describing immutable grid window.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_id(&self.frame_id, "frame_id")?;
        validate_finite(self.origin_x_m, "origin_x_m")?;
        validate_finite(self.origin_y_m, "origin_y_m")?;
        if !self.resolution_m.is_finite() || self.resolution_m <= 0.0 {
            return Err(ValidationError::NonFinite("resolution_m"));
        }
        if self.width == 0 || self.height == 0 {
            return Err(ValidationError::InvalidGrid);
        }
        let expected = usize::try_from(self.width)
            .ok()
            .and_then(|width| {
                usize::try_from(self.height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .ok_or(ValidationError::InvalidGrid)?;
        if self.cells.len() != expected {
            return Err(ValidationError::InvalidGrid);
        }
        let requested = self
            .requested
            .as_ref()
            .ok_or(ValidationError::InvalidGrid)?;
        let covered = self.covered.as_ref().ok_or(ValidationError::InvalidGrid)?;
        for cell in &self.cells {
            let occupancy = Occupancy::try_from(*cell)
                .ok()
                .filter(|occupancy| *occupancy != Occupancy::Unspecified)
                .ok_or(ValidationError::InvalidGrid)?;
            let _ = occupancy;
        }
        requested.validate()?;
        covered.validate()?;
        let width_m = f64::from(self.width) * self.resolution_m;
        let height_m = f64::from(self.height) * self.resolution_m;
        let epsilon = self.resolution_m * 1.0e-9;
        if (covered.min_x_m - self.origin_x_m).abs() > epsilon
            || (covered.min_y_m - self.origin_y_m).abs() > epsilon
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
}

impl WindowRequest {
    /// Validates a bounded window request.
    pub fn validate(&self) -> Result<(), ValidationError> {
        self.requested
            .as_ref()
            .ok_or(ValidationError::InvalidResponse)?
            .validate()
    }
}

impl WindowResponse {
    /// Validates a selected window or explicit unavailable result.
    pub fn validate(&self) -> Result<(), ValidationError> {
        match self
            .result
            .as_ref()
            .ok_or(ValidationError::InvalidResponse)?
        {
            window_response::Result::Window(window) => window.validate(),
            window_response::Result::Unavailable(unavailable) => {
                let reason = WindowUnavailableReason::try_from(unavailable.reason)
                    .ok()
                    .filter(|reason| *reason != WindowUnavailableReason::Unspecified)
                    .ok_or(ValidationError::InvalidResponse)?;
                let _ = reason;
                Ok(())
            }
        }
    }
}

impl WorldStatus {
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

fn validate_pose(x_m: f64, y_m: f64, yaw_rad: f64) -> Result<(), ValidationError> {
    validate_finite(x_m, "x_m")?;
    validate_finite(y_m, "y_m")?;
    if !yaw_rad.is_finite() || !(-std::f64::consts::PI..=std::f64::consts::PI).contains(&yaw_rad) {
        return Err(ValidationError::InvalidYaw);
    }
    Ok(())
}

fn validate_confidence(value: f32) -> Result<(), ValidationError> {
    (value.is_finite() && (0.0..=1.0).contains(&value))
        .then_some(())
        .ok_or(ValidationError::InvalidConfidence)
}

#[cfg(test)]
mod tests {
    use phoxal_port::{PortDescriptor, PortKind};
    use prost::Name;

    use super::*;

    #[test]
    fn generated_ports_retain_names_kinds_and_message_identity() {
        assert_eq!(ports::POSE.name(), "pose");
        assert_eq!(ports::BELIEF.name(), "belief");
        assert_eq!(ports::REVISION.name(), "revision");
        assert_eq!(ports::WINDOW.name(), "window");
        assert_eq!(ports::STATUS.name(), "status");
        assert_eq!(
            <phoxal_port::Sample<OdometryState> as PortDescriptor>::KIND,
            PortKind::Sample
        );
        assert_eq!(
            <phoxal_port::Read<WindowRequest, WindowResponse> as PortDescriptor>::KIND,
            PortKind::Read
        );
        assert_eq!(WorldBelief::PACKAGE, "phoxal.world.v1");
        assert!(!FILE_DESCRIPTOR_SET.is_empty());
    }

    #[test]
    fn validates_coherent_free_window_shape() {
        let requested = Bounds {
            min_x_m: 0.0,
            min_y_m: 0.0,
            max_x_m: 0.2,
            max_y_m: 0.2,
        };
        GridWindow {
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
        }
        .validate()
        .expect("grid shape is coherent");
    }

    #[test]
    fn accepts_a_covered_superset_for_a_bounded_request() {
        GridWindow {
            frame_id: "map".into(),
            origin_x_m: 0.0,
            origin_y_m: 0.0,
            resolution_m: 0.1,
            width: 4,
            height: 4,
            cells: vec![Occupancy::Free as i32; 16],
            revision: 1,
            requested: Some(Bounds {
                min_x_m: 0.1,
                min_y_m: 0.1,
                max_x_m: 0.3,
                max_y_m: 0.3,
            }),
            covered: Some(Bounds {
                min_x_m: 0.0,
                min_y_m: 0.0,
                max_x_m: 0.4,
                max_y_m: 0.4,
            }),
        }
        .validate()
        .expect("a provider may return a covered superset");
    }

    #[test]
    fn rejects_invalid_pose_and_grid_shape() {
        let bounds = Bounds {
            min_x_m: 0.0,
            min_y_m: 0.0,
            max_x_m: 0.2,
            max_y_m: 0.2,
        };
        let bad = GridWindow {
            frame_id: "map".into(),
            origin_x_m: 0.0,
            origin_y_m: 0.0,
            resolution_m: 0.1,
            width: 2,
            height: 2,
            cells: vec![Occupancy::Free as i32; 3],
            revision: 1,
            requested: Some(bounds),
            covered: Some(bounds),
        };
        assert_eq!(bad.validate(), Err(ValidationError::InvalidGrid));
    }
}
