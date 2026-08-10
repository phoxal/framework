#[derive(Copy, Eq, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensorHealth {
    Nominal,
    Degraded,
    Fault,
}
#[derive(Copy, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScanGeometry {
    pub angle_min_rad: f32,
    pub angle_increment_rad: f32,
}
#[derive(Copy, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RangeLimits {
    pub min_m: f32,
    pub max_m: f32,
}
#[derive(Copy, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScanQuality {
    pub valid_points: u32,
}
/// A polar return is either a physical range or an explicit invalid reading.
#[derive(Copy, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RangeSample {
    Valid(f32),
    Invalid,
}
/// A cartesian return is likewise explicit about a missing/invalid reading.
#[derive(Copy, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointSample {
    Valid([f32; 3]),
    Invalid,
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Ranges {
    pub ranges: Vec<RangeSample>,
    pub geometry: Option<ScanGeometry>,
    pub limits: Option<RangeLimits>,
    pub quality: Option<ScanQuality>,
    pub health: SensorHealth,
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Points {
    pub points: Vec<PointSample>,
    pub limits: Option<RangeLimits>,
    pub quality: Option<ScanQuality>,
    pub health: SensorHealth,
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", try_from = "ScanWire")]
pub enum Scan {
    Ranges(Ranges),
    Points(Points),
}
#[derive(serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ScanWire {
    Ranges(Ranges),
    Points(Points),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidScan(&'static str);
impl std::fmt::Display for InvalidScan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for InvalidScan {}
fn checked_limits(limits: Option<RangeLimits>) -> Result<Option<RangeLimits>, InvalidScan> {
    if let Some(l) = limits
        && !(l.min_m.is_finite() && l.max_m.is_finite() && l.min_m >= 0.0 && l.min_m <= l.max_m)
    {
        return Err(InvalidScan(
            "lidar limits must be finite, nonnegative, and ordered",
        ));
    }
    Ok(limits)
}
impl Scan {
    pub fn ranges(
        ranges: Vec<RangeSample>,
        geometry: Option<ScanGeometry>,
        limits: Option<RangeLimits>,
        quality: Option<ScanQuality>,
        health: SensorHealth,
    ) -> Result<Self, InvalidScan> {
        let limits = checked_limits(limits)?;
        if geometry.is_some_and(|g| {
            !g.angle_min_rad.is_finite()
                || !g.angle_increment_rad.is_finite()
                || g.angle_increment_rad == 0.0
        }) {
            return Err(InvalidScan(
                "lidar geometry must be finite with nonzero increment",
            ));
        }
        let valid = ranges
            .iter()
            .filter_map(|r| match r {
                RangeSample::Valid(v) => Some(*v),
                RangeSample::Invalid => None,
            })
            .count();
        if ranges
            .iter()
            .filter_map(|r| match r {
                RangeSample::Valid(v) => Some(*v),
                RangeSample::Invalid => None,
            })
            .any(|v| {
                !v.is_finite() || v < 0.0 || limits.is_some_and(|l| v < l.min_m || v > l.max_m)
            })
        {
            return Err(InvalidScan(
                "lidar valid ranges must be finite and inside limits",
            ));
        }
        if quality.is_some_and(|q| usize::try_from(q.valid_points).ok() != Some(valid)) {
            return Err(InvalidScan(
                "lidar valid point count must match explicit valid returns",
            ));
        }
        Ok(Self::Ranges(Ranges {
            ranges,
            geometry,
            limits,
            quality,
            health,
        }))
    }
    pub fn points(
        points: Vec<PointSample>,
        limits: Option<RangeLimits>,
        quality: Option<ScanQuality>,
        health: SensorHealth,
    ) -> Result<Self, InvalidScan> {
        let limits = checked_limits(limits)?;
        if points
            .iter()
            .filter_map(|point| match point {
                PointSample::Valid(value) => Some(value),
                PointSample::Invalid => None,
            })
            .flatten()
            .any(|v| !v.is_finite())
        {
            return Err(InvalidScan("lidar points must be finite"));
        }
        if points
            .iter()
            .filter_map(|point| match point {
                PointSample::Valid(value) => Some(value),
                PointSample::Invalid => None,
            })
            .any(|point| {
                limits.is_some_and(|limits| {
                    let norm_m = (f64::from(point[0]).powi(2)
                        + f64::from(point[1]).powi(2)
                        + f64::from(point[2]).powi(2))
                    .sqrt();
                    norm_m < f64::from(limits.min_m) || norm_m > f64::from(limits.max_m)
                })
            })
        {
            return Err(InvalidScan(
                "lidar valid points must be inside radial limits",
            ));
        }
        let valid = points
            .iter()
            .filter(|point| matches!(point, PointSample::Valid(_)))
            .count();
        if quality.is_some_and(|q| usize::try_from(q.valid_points).ok() != Some(valid)) {
            return Err(InvalidScan("lidar valid point count must match points"));
        }
        Ok(Self::Points(Points {
            points,
            limits,
            quality,
            health,
        }))
    }
}
impl TryFrom<ScanWire> for Scan {
    type Error = InvalidScan;
    fn try_from(v: ScanWire) -> Result<Self, Self::Error> {
        match v {
            ScanWire::Ranges(r) => {
                Self::ranges(r.ranges, r.geometry, r.limits, r.quality, r.health)
            }
            ScanWire::Points(p) => Self::points(p.points, p.limits, p.quality, p.health),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn constructor_rejects_nan_and_counts_explicit_invalids() {
        assert!(
            Scan::ranges(
                vec![RangeSample::Valid(f32::NAN)],
                None,
                None,
                None,
                SensorHealth::Nominal
            )
            .is_err()
        );
        assert!(
            Scan::ranges(
                vec![RangeSample::Valid(1.0), RangeSample::Invalid],
                None,
                None,
                Some(ScanQuality { valid_points: 1 }),
                SensorHealth::Nominal
            )
            .is_ok()
        );
    }

    #[test]
    fn cartesian_points_must_obey_the_declared_radial_limits() {
        let limits = Some(RangeLimits {
            min_m: 0.5,
            max_m: 2.0,
        });
        assert!(
            Scan::points(
                vec![PointSample::Valid([3.0, 0.0, 0.0])],
                limits,
                None,
                SensorHealth::Nominal,
            )
            .is_err()
        );
        assert!(
            Scan::points(
                vec![PointSample::Valid([1.0, 1.0, 0.0])],
                limits,
                None,
                SensorHealth::Nominal,
            )
            .is_ok()
        );
    }
}

phoxal_macros::phoxal_api_fragment! {
    path robot / component(instance) / lidar(capability);

    topic scan: Sample<Scan>;
}
