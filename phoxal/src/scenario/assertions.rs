use crate::api::localize::v1::{LocalizationRevision, LocalizationRevisionId};
use crate::api::map::v1::{MapRevision, MapRevisionId};
use crate::api::simulation::v1::pose::Pose;
use anyhow::{Result, bail};

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Meters(pub f64);

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Radians(pub f64);

pub trait EpochRevision {
    fn epoch(&self) -> u64;
    fn sequence(&self) -> u64;
}

impl EpochRevision for LocalizationRevisionId {
    fn epoch(&self) -> u64 {
        self.epoch
    }

    fn sequence(&self) -> u64 {
        self.sequence
    }
}

impl EpochRevision for MapRevisionId {
    fn epoch(&self) -> u64 {
        self.epoch
    }

    fn sequence(&self) -> u64 {
        self.sequence
    }
}

pub fn assert_forward_delta(
    start: &Pose,
    end: &Pose,
    expected: Meters,
    tolerance: Meters,
) -> Result<()> {
    let actual = end.translation_m[0] - start.translation_m[0];
    assert_close("forward delta", actual, expected.0, tolerance.0)
}

pub fn assert_lateral_drift(start: &Pose, end: &Pose, max_drift: Meters) -> Result<()> {
    let drift = (end.translation_m[1] - start.translation_m[1]).abs();
    if drift <= max_drift.0 {
        Ok(())
    } else {
        bail!(
            "lateral drift {drift:.6} m exceeds maximum {:.6} m",
            max_drift.0
        )
    }
}

pub fn assert_yaw_drift(start: &Pose, end: &Pose, max_drift: Radians) -> Result<()> {
    let drift = angle_delta(
        yaw_from_xyzw(start.rotation_xyzw),
        yaw_from_xyzw(end.rotation_xyzw),
    );
    if drift.abs() <= max_drift.0 {
        Ok(())
    } else {
        bail!(
            "yaw drift {:.6} rad exceeds maximum {:.6} rad",
            drift.abs(),
            max_drift.0
        )
    }
}

pub fn assert_revision_linked(map: &MapRevision, localize: &LocalizationRevision) -> Result<()> {
    if map.built_from_localize_revision == localize.revision_id {
        Ok(())
    } else {
        bail!(
            "map revision {:?} is built from localization revision {:?}, expected {:?}",
            map.map_revision_id,
            map.built_from_localize_revision,
            localize.revision_id
        )
    }
}

pub fn assert_revision_monotonic<T>(revisions: &[T]) -> Result<()>
where
    T: EpochRevision + std::fmt::Debug,
{
    for window in revisions.windows(2) {
        let previous = &window[0];
        let current = &window[1];
        if current.epoch() != previous.epoch() {
            bail!("revision epoch changed from {previous:?} to {current:?}");
        }
        if current.sequence() <= previous.sequence() {
            bail!("revision sequence did not increase from {previous:?} to {current:?}");
        }
    }
    Ok(())
}

fn assert_close(name: &str, actual: f64, expected: f64, tolerance: f64) -> Result<()> {
    validate_finite(name, actual)?;
    validate_finite("expected", expected)?;
    validate_finite("tolerance", tolerance)?;
    if tolerance < 0.0 {
        bail!("tolerance must be non-negative, got {tolerance}");
    }

    let delta = (actual - expected).abs();
    if delta <= tolerance {
        Ok(())
    } else {
        bail!(
            "{name} {actual:.6} differs from expected {expected:.6} by {delta:.6}, tolerance {tolerance:.6}"
        )
    }
}

fn validate_finite(name: &str, value: f64) -> Result<()> {
    if value.is_finite() {
        Ok(())
    } else {
        bail!("{name} must be finite, got {value}")
    }
}

fn yaw_from_xyzw(rotation_xyzw: [f64; 4]) -> f64 {
    let [x, y, z, w] = rotation_xyzw;
    let siny_cosp = 2.0 * (w * z + x * y);
    let cosy_cosp = 1.0 - 2.0 * (y * y + z * z);
    siny_cosp.atan2(cosy_cosp)
}

fn angle_delta(start: f64, end: f64) -> f64 {
    let two_pi = std::f64::consts::TAU;
    (end - start + std::f64::consts::PI).rem_euclid(two_pi) - std::f64::consts::PI
}

#[cfg(test)]
mod tests {
    use crate::api::localize::v1::{
        AffectedKeyframeSummary, LocalizationRevision, LocalizationRevisionCause,
        LocalizationRevisionId,
    };
    use crate::api::map::v1::{MapRevision, MapRevisionCause, MapRevisionId};
    use crate::api::simulation::v1::pose::Pose;

    use super::{
        Meters, assert_forward_delta, assert_lateral_drift, assert_revision_linked,
        assert_revision_monotonic, assert_yaw_drift,
    };

    #[test]
    fn forward_delta_uses_x_axis() {
        let start = pose([1.0, 2.0, 0.0], [0.0, 0.0, 0.0, 1.0]);
        let end = pose([2.05, 2.2, 0.0], [0.0, 0.0, 0.0, 1.0]);

        assert_forward_delta(&start, &end, Meters(1.0), Meters(0.1)).unwrap();
    }

    #[test]
    fn lateral_drift_rejects_excess_y_axis_motion() {
        let start = pose([0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]);
        let end = pose([0.0, 0.3, 0.0], [0.0, 0.0, 0.0, 1.0]);

        assert!(assert_lateral_drift(&start, &end, Meters(0.1)).is_err());
    }

    #[test]
    fn yaw_drift_wraps_across_pi() {
        let start = pose([0.0, 0.0, 0.0], yaw_quaternion(std::f64::consts::PI - 0.01));
        let end = pose(
            [0.0, 0.0, 0.0],
            yaw_quaternion(-std::f64::consts::PI + 0.01),
        );

        assert_yaw_drift(&start, &end, super::Radians(0.03)).unwrap();
    }

    #[test]
    fn linked_revisions_must_match() {
        let localize = LocalizationRevision {
            revision_id: LocalizationRevisionId {
                epoch: 1,
                sequence: 7,
            },
            previous_revision_id: None,
            cause: LocalizationRevisionCause::SensorIntegration,
            affected_keyframes: AffectedKeyframeSummary {
                keyframe_ids: Vec::new(),
                region: None,
            },
            inline_correction_available: false,
            correction_fetch_required: false,
        };
        let map = MapRevision {
            map_revision_id: MapRevisionId {
                epoch: 1,
                sequence: 3,
            },
            previous_map_revision_id: None,
            built_from_localize_revision: localize.revision_id,
            cause: MapRevisionCause::SensorIntegration,
            affected_region: None,
        };

        assert_revision_linked(&map, &localize).unwrap();
    }

    #[test]
    fn monotonic_revision_rejects_equal_sequence() {
        let revisions = [
            LocalizationRevisionId {
                epoch: 1,
                sequence: 2,
            },
            LocalizationRevisionId {
                epoch: 1,
                sequence: 2,
            },
        ];

        assert!(assert_revision_monotonic(&revisions).is_err());
    }

    fn pose(translation_m: [f64; 3], rotation_xyzw: [f64; 4]) -> Pose {
        Pose {
            frame_id: "map".to_string(),
            translation_m,
            rotation_xyzw,
        }
    }

    fn yaw_quaternion(yaw_rad: f64) -> [f64; 4] {
        [0.0, 0.0, (yaw_rad / 2.0).sin(), (yaw_rad / 2.0).cos()]
    }
}
