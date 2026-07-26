//! Lidar capability: publishes `component::lidar::Scan` from the Webots
//! `Lidar` device, as polar ranges or as a cartesian point cloud depending on
//! the component's declared `output`.
//!
//! Geometry and range limits are read off the device rather than the manifest:
//! the world's `Lidar` node is what actually decides them, so reporting the
//! authored numbers instead would let a scan describe a sensor nobody built.

use anyhow::{Result, anyhow};
use phoxal::api;
use phoxal::model::component::v0::capability::LidarOutput;
use webots_rs::device::lidar::{LidarConfig, LidarReading};

use super::{SampledSpec, is_due};

#[derive(Clone, Debug)]
pub(crate) struct LidarSpec {
    pub(crate) sampled: SampledSpec,
    pub(crate) output: LidarOutput,
}

pub(crate) struct NativeLidar {
    lidar: webots_rs::device::lidar::Lidar,
    spec: LidarSpec,
    geometry: Option<api::component::lidar::ScanGeometry>,
    limits: api::component::lidar::RangeLimits,
}

impl NativeLidar {
    pub(crate) fn new(webots: &webots_rs::Webots, spec: &LidarSpec) -> Result<Self> {
        let point_cloud = matches!(spec.output, LidarOutput::Points);
        let lidar = webots
            .lidar(
                spec.sampled.reference.to_string(),
                LidarConfig::new().with_point_cloud(point_cloud),
            )
            .map_err(|error| anyhow!(error))?;
        lidar
            .enable(spec.sampled.sampling_period_ms)
            .map_err(|error| anyhow!(error))?;
        let limits = api::component::lidar::RangeLimits {
            min_m: lidar.get_min_range().map_err(|error| anyhow!(error))? as f32,
            max_m: lidar.get_max_range().map_err(|error| anyhow!(error))? as f32,
        };
        let geometry = scan_geometry(
            lidar.get_fov().map_err(|error| anyhow!(error))?,
            lidar
                .get_horizontal_resolution()
                .map_err(|error| anyhow!(error))?,
        );
        Ok(Self {
            lidar,
            spec: spec.clone(),
            geometry,
            limits,
        })
    }

    pub(crate) fn read_if_due(
        &self,
        step_index: u64,
    ) -> Result<Option<api::component::lidar::Scan>> {
        if !is_due(step_index, self.spec.sampled.publish_every_steps) {
            return Ok(None);
        }
        let reading = self.lidar.reading().map_err(|error| anyhow!(error))?;
        Ok(Some(match reading {
            LidarReading::RangeImage(ranges) => {
                let valid_points = ranges.iter().filter(|range| range.is_finite()).count();
                api::component::lidar::Scan::Ranges(api::component::lidar::Ranges {
                    ranges,
                    geometry: self.geometry,
                    limits: Some(self.limits),
                    quality: Some(api::component::lidar::ScanQuality {
                        valid_points: valid_points as u32,
                    }),
                    health: api::component::lidar::SensorHealth::Nominal,
                })
            }
            LidarReading::PointCloud(cloud) => {
                let points: Vec<[f32; 3]> = cloud
                    .iter()
                    .map(|point| [point.x, point.y, point.z])
                    .collect();
                let valid_points = points
                    .iter()
                    .filter(|point| point.iter().all(|axis| axis.is_finite()))
                    .count();
                api::component::lidar::Scan::Points(api::component::lidar::Points {
                    points,
                    limits: Some(self.limits),
                    quality: Some(api::component::lidar::ScanQuality {
                        valid_points: valid_points as u32,
                    }),
                    health: api::component::lidar::SensorHealth::Nominal,
                })
            }
        }))
    }
}

/// The polar geometry of one horizontal sweep. A single-ray lidar has no
/// increment to report, and a non-finite field of view describes no sweep at
/// all, so both yield `None` rather than an invented angle.
fn scan_geometry(
    fov_rad: f64,
    horizontal_resolution: i32,
) -> Option<api::component::lidar::ScanGeometry> {
    if !fov_rad.is_finite() || fov_rad <= 0.0 || horizontal_resolution < 2 {
        return None;
    }
    Some(api::component::lidar::ScanGeometry {
        angle_min_rad: (-fov_rad / 2.0) as f32,
        angle_increment_rad: (fov_rad / f64::from(horizontal_resolution - 1)) as f32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_spreads_the_field_of_view_across_the_rays() {
        let geometry = scan_geometry(std::f64::consts::PI, 3).expect("a 3-ray sweep has geometry");
        assert_eq!(geometry.angle_min_rad, -(std::f64::consts::PI / 2.0) as f32);
        assert_eq!(
            geometry.angle_increment_rad,
            (std::f64::consts::PI / 2.0) as f32
        );
    }

    #[test]
    fn a_single_ray_or_absent_sweep_reports_no_geometry() {
        assert!(scan_geometry(std::f64::consts::PI, 1).is_none());
        assert!(scan_geometry(0.0, 8).is_none());
        assert!(scan_geometry(f64::NAN, 8).is_none());
    }
}
