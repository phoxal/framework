use crate::api::component::capability::depth::v1::MILLIMETERS_PER_METER;
use crate::api::component::capability::lidar::v1::Scan as LidarData;
use nalgebra::Vector3;
use serde::{Deserialize, Serialize};

use crate::spatial::sensor::ResolvedSensorPose;

const RANGE_HIT_MAX_MARGIN_M: f32 = 0.05;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Ray {
    origin_m: [f32; 3],
    end_m: [f32; 3],
    max_range_m: f32,
    radius_m: f32,
    confidence: f32,
}

impl Ray {
    pub const fn new(
        origin_m: [f32; 3],
        end_m: [f32; 3],
        max_range_m: f32,
        radius_m: f32,
        confidence: f32,
    ) -> Self {
        Self {
            origin_m,
            end_m,
            max_range_m,
            radius_m,
            confidence,
        }
    }

    pub const fn origin_m(&self) -> [f32; 3] {
        self.origin_m
    }

    pub const fn end_m(&self) -> [f32; 3] {
        self.end_m
    }

    pub const fn max_range_m(&self) -> f32 {
        self.max_range_m
    }

    pub const fn radius_m(&self) -> f32 {
        self.radius_m
    }

    pub const fn confidence(&self) -> f32 {
        self.confidence
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RangeRayProjection {
    pub angle_rad: f32,
    pub clear_distance_m: f32,
    pub occupied_distance_m: Option<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DepthRayProjection {
    pub ray: RangeRayProjection,
    pub point_root_m: [f32; 3],
    pub open_end_root_m: [f32; 3],
}

#[derive(Debug, Clone, Copy)]
pub struct DepthGrid<'a> {
    pub samples_mm: &'a [u16],
    pub width_px: u32,
    pub height_px: u32,
}

pub fn sample_range_rays(
    sensor_yaw_rad: f32,
    max_range_m: f32,
    distance_m: f32,
) -> Vec<RangeRayProjection> {
    let occupied_distance_m = is_range_hit(distance_m, max_range_m).then_some(distance_m);
    let clear_distance_m = occupied_distance_m.unwrap_or(max_range_m);

    vec![RangeRayProjection {
        angle_rad: sensor_yaw_rad,
        clear_distance_m,
        occupied_distance_m,
    }]
}

pub fn sample_depth_rays(
    sensor: &ResolvedSensorPose,
    field_of_view_rad: f32,
    max_range_m: f32,
    grid: DepthGrid<'_>,
    sample_columns: usize,
    sample_rows: usize,
) -> Vec<DepthRayProjection> {
    let width = grid.width_px as usize;
    let height = grid.height_px as usize;
    let sampled_rows = sampled_indices(height, sample_rows).collect::<Vec<_>>();

    sampled_indices(width, sample_columns)
        .flat_map(|column| {
            sampled_rows.iter().copied().filter_map(move |row| {
                depth_ray_projection(
                    sensor,
                    field_of_view_rad,
                    max_range_m,
                    grid.samples_mm,
                    [width, height],
                    [column, row],
                )
            })
        })
        .collect()
}

pub fn sample_lidar_rays(
    sensor_yaw_rad: f32,
    field_of_view_rad: f32,
    max_range_m: f32,
    payload: &LidarData,
) -> Vec<RangeRayProjection> {
    match payload {
        LidarData::Ranges(ranges) => ranges
            .ranges
            .iter()
            .enumerate()
            .map(|(index, distance_m)| {
                let occupied_distance_m =
                    (distance_m.is_finite() && *distance_m > 0.0 && *distance_m < max_range_m)
                        .then_some(*distance_m);
                let angle_offset_rad = (if ranges.ranges.len() <= 1 {
                    0.5
                } else {
                    index as f32 / (ranges.ranges.len() - 1) as f32
                } - 0.5)
                    * field_of_view_rad;

                RangeRayProjection {
                    angle_rad: sensor_yaw_rad + angle_offset_rad,
                    clear_distance_m: occupied_distance_m.unwrap_or(max_range_m),
                    occupied_distance_m,
                }
            })
            .collect(),
        LidarData::Points(points) => points
            .points
            .iter()
            .filter_map(|point| {
                let distance_m = (point[0] * point[0] + point[1] * point[1]).sqrt();
                (distance_m.is_finite() && distance_m > 0.0 && distance_m < max_range_m).then_some(
                    RangeRayProjection {
                        angle_rad: sensor_yaw_rad + point[1].atan2(point[0]),
                        clear_distance_m: distance_m,
                        occupied_distance_m: Some(distance_m),
                    },
                )
            })
            .collect(),
    }
}

fn sampled_indices(length: usize, sample_count: usize) -> impl Iterator<Item = usize> {
    let sample_count = sample_count.max(1);
    (0..sample_count).map(move |sample_index| {
        ((((sample_index as f32) + 0.5) * length as f32) / sample_count as f32)
            .floor()
            .clamp(0.0, (length.saturating_sub(1)) as f32) as usize
    })
}

fn depth_ray_projection(
    sensor: &ResolvedSensorPose,
    field_of_view_rad: f32,
    max_range_m: f32,
    samples_mm: &[u16],
    resolution: [usize; 2],
    pixel: [usize; 2],
) -> Option<DepthRayProjection> {
    let [column, row] = pixel;
    let [width, height] = resolution;
    let horizontal_angle_rad =
        -normalized_sample_offset(column, width).clamp(-0.5, 0.5) * field_of_view_rad;
    let vertical_fov_rad = depth_vertical_fov_rad(field_of_view_rad, width, height);
    let vertical_angle_rad =
        -normalized_sample_offset(row, height).clamp(-0.5, 0.5) * vertical_fov_rad;
    let clear_point_sensor =
        point_in_sensor_frame(max_range_m, horizontal_angle_rad, vertical_angle_rad);
    let clear_point_from_sensor_root = sensor.local_rotation.transform_vector(&clear_point_sensor);
    let clear_point_root = clear_point_from_sensor_root
        + Vector3::new(
            sensor.offset_xyz_m[0],
            sensor.offset_xyz_m[1],
            sensor.offset_xyz_m[2],
        );
    let depth_m = f32::from(samples_mm[row * width + column]) / MILLIMETERS_PER_METER;
    if !depth_m.is_finite() || depth_m <= 0.0 || depth_m >= max_range_m {
        return Some(DepthRayProjection {
            ray: RangeRayProjection {
                angle_rad: sensor.yaw_rad + horizontal_angle_rad,
                clear_distance_m: max_range_m,
                occupied_distance_m: None,
            },
            point_root_m: sensor.offset_xyz_m,
            open_end_root_m: [clear_point_root.x, clear_point_root.y, clear_point_root.z],
        });
    }

    let point_sensor = point_in_sensor_frame(depth_m, horizontal_angle_rad, vertical_angle_rad);
    let point_from_sensor_root = sensor.local_rotation.transform_vector(&point_sensor);
    let point_root = point_from_sensor_root
        + Vector3::new(
            sensor.offset_xyz_m[0],
            sensor.offset_xyz_m[1],
            sensor.offset_xyz_m[2],
        );
    let planar_distance_m = point_from_sensor_root.x.hypot(point_from_sensor_root.y);
    if !planar_distance_m.is_finite() || planar_distance_m <= f32::EPSILON {
        return None;
    }

    Some(DepthRayProjection {
        ray: RangeRayProjection {
            angle_rad: point_from_sensor_root.y.atan2(point_from_sensor_root.x),
            clear_distance_m: planar_distance_m.min(max_range_m),
            occupied_distance_m: Some(planar_distance_m.min(max_range_m)),
        },
        point_root_m: [point_root.x, point_root.y, point_root.z],
        open_end_root_m: [clear_point_root.x, clear_point_root.y, clear_point_root.z],
    })
}

fn normalized_sample_offset(index: usize, length: usize) -> f32 {
    if length <= 1 {
        0.0
    } else {
        ((index as f32 + 0.5) / length as f32) - 0.5
    }
}

fn depth_vertical_fov_rad(horizontal_fov_rad: f32, width: usize, height: usize) -> f32 {
    if width == 0 || height == 0 {
        return 0.0;
    }

    2.0 * ((horizontal_fov_rad * 0.5).tan() * (height as f32 / width as f32)).atan()
}

fn point_in_sensor_frame(
    depth_m: f32,
    horizontal_angle_rad: f32,
    vertical_angle_rad: f32,
) -> Vector3<f32> {
    Vector3::new(
        depth_m,
        depth_m * horizontal_angle_rad.tan(),
        depth_m * vertical_angle_rad.tan(),
    )
}

fn is_range_hit(distance_m: f32, max_range_m: f32) -> bool {
    distance_m.is_finite()
        && distance_m > 0.0
        && distance_m < (max_range_m - RANGE_HIT_MAX_MARGIN_M).max(0.0)
}

#[cfg(test)]
mod tests {
    use crate::api::component::capability::depth::v1::Depth as DepthPayload;
    use crate::api::component::capability::lidar::v1::{Points, Ranges, Scan as LidarData};
    use crate::model::component::v1::CapabilityRef;
    use nalgebra::UnitQuaternion;

    use crate::spatial::sensor::{ResolvedSensorKind, ResolvedSensorPose};

    use super::{DepthGrid, sample_depth_rays, sample_lidar_rays, sample_range_rays};

    fn depth_payload(values: &[f32]) -> DepthPayload {
        DepthPayload::from_meters(values.iter().copied()).expect("valid depth")
    }

    fn depth_sensor(offset_xyz_m: [f32; 3]) -> ResolvedSensorPose {
        ResolvedSensorPose {
            capability: CapabilityRef::new("front_camera", "depth"),
            offset_xyz_m,
            yaw_rad: 0.0,
            local_rotation: UnitQuaternion::identity(),
            kind: ResolvedSensorKind::Depth {
                field_of_view_rad: std::f32::consts::FRAC_PI_2,
                max_range_m: 5.0,
                width_px: 1,
                height_px: 1,
            },
        }
    }

    #[test]
    fn sample_range_rays_treats_near_max_range_as_clear() {
        let rays = sample_range_rays(0.0, 4.0, 3.98);
        assert_eq!(rays[0].occupied_distance_m, None);
        assert_eq!(rays[0].clear_distance_m, 4.0);
    }

    #[test]
    fn sample_range_rays_keeps_single_center_beam() {
        let rays = sample_range_rays(1.2, 4.0, 2.0);
        assert_eq!(rays.len(), 1);
        assert!((rays[0].angle_rad - 1.2).abs() < 1e-6);
        assert_eq!(rays[0].occupied_distance_m, Some(2.0));
    }

    #[test]
    fn sample_lidar_rays_handles_ranges_and_points() {
        let ranges = sample_lidar_rays(
            0.0,
            std::f32::consts::PI,
            5.0,
            &LidarData::Ranges(Ranges::new(vec![1.0, 6.0, 2.0])),
        );
        assert_eq!(ranges.len(), 3);
        assert_eq!(ranges[1].occupied_distance_m, None);

        let points = sample_lidar_rays(
            0.0,
            std::f32::consts::PI,
            5.0,
            &LidarData::Points(Points::new(vec![[1.0, 1.0, 0.0]])),
        );
        assert_eq!(points.len(), 1);
        assert!(points[0].occupied_distance_m.is_some());
    }

    #[test]
    fn sample_depth_rays_projects_points_into_root_frame() {
        let payload = depth_payload(&[2.0]);

        let rays = sample_depth_rays(
            &depth_sensor([1.0, 0.0, 0.5]),
            std::f32::consts::FRAC_PI_2,
            5.0,
            DepthGrid {
                samples_mm: payload.samples_mm(),
                width_px: 1,
                height_px: 1,
            },
            1,
            1,
        );
        assert_eq!(rays.len(), 1);
        assert_eq!(rays[0].ray.occupied_distance_m, Some(2.0));
        assert!((rays[0].point_root_m[0] - 3.0).abs() < 1e-6);
        assert!((rays[0].point_root_m[2] - 0.5).abs() < 1e-6);
        assert_eq!(rays[0].open_end_root_m, [6.0, 0.0, 0.5]);
    }

    #[test]
    fn sample_depth_rays_keeps_clear_samples_when_depth_is_out_of_range() {
        let payload = depth_payload(&[9.0]);

        let rays = sample_depth_rays(
            &depth_sensor([0.0, 0.0, 0.0]),
            std::f32::consts::FRAC_PI_2,
            5.0,
            DepthGrid {
                samples_mm: payload.samples_mm(),
                width_px: 1,
                height_px: 1,
            },
            1,
            1,
        );
        assert_eq!(rays[0].ray.occupied_distance_m, None);
        assert_eq!(rays[0].ray.clear_distance_m, 5.0);
    }

    #[test]
    fn sample_depth_rays_projects_clear_end_along_pixel_ray() {
        let payload = depth_payload(&[9.0, 9.0, 9.0]);

        let rays = sample_depth_rays(
            &depth_sensor([0.0, 0.0, 0.0]),
            std::f32::consts::FRAC_PI_2,
            5.0,
            DepthGrid {
                samples_mm: payload.samples_mm(),
                width_px: 1,
                height_px: 3,
            },
            1,
            3,
        );

        assert_eq!(rays.len(), 3);
        assert!(rays[0].open_end_root_m[2] > 0.0);
        assert!(rays[1].open_end_root_m[2].abs() < 1e-6);
        assert!(rays[2].open_end_root_m[2] < 0.0);
    }
}
