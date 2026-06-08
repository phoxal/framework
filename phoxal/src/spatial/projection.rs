use crate::api::component::v1::capability::depth::Depth;
use crate::api::component::v1::capability::lidar::Scan;
use crate::api::component::v1::capability::range::Sample;

use crate::spatial::ray::{
    DepthGrid, DepthRayProjection, RangeRayProjection, Ray, sample_depth_rays, sample_lidar_rays,
    sample_range_rays,
};
use crate::spatial::sensor::{ResolvedSensorKind, ResolvedSensorPose};

pub const DEPTH_SAMPLE_COLUMNS: usize = 17;
pub const DEPTH_SAMPLE_ROWS: usize = 5;

const DEPTH_POINT_RADIUS_M: f32 = 0.08;
const MIN_RAY_RADIUS_M: f32 = 0.01;
const DEFAULT_CONFIDENCE: f32 = 1.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    UnsupportedSensorKind,
    InvalidPayload,
}

pub fn rays_from_range_sample(
    sensor_pose: &ResolvedSensorPose,
    sample: &Sample,
) -> Result<Vec<Ray>, Error> {
    let ResolvedSensorKind::Range { max_range_m, .. } = sensor_pose.kind else {
        return Err(Error::UnsupportedSensorKind);
    };
    let Some(distance_m) = range_distance_for_ray(sample.distance_m(), max_range_m) else {
        return Err(Error::InvalidPayload);
    };

    Ok(rays_from_projections(
        sensor_pose,
        sample_range_rays(sensor_pose.yaw_rad, max_range_m, distance_m),
        max_range_m,
        DEFAULT_POINT_RADIUS_M,
    ))
}

pub fn rays_from_depth_sample(
    sensor_pose: &ResolvedSensorPose,
    depth: &Depth,
) -> Result<Vec<Ray>, Error> {
    let ResolvedSensorKind::Depth {
        field_of_view_rad,
        max_range_m,
        width_px,
        height_px,
    } = sensor_pose.kind
    else {
        return Err(Error::UnsupportedSensorKind);
    };
    if !complete_depth_grid(depth.samples_mm(), width_px, height_px) {
        return Err(Error::InvalidPayload);
    }

    Ok(depth_rays(
        sensor_pose,
        sample_depth_rays(
            sensor_pose,
            field_of_view_rad,
            max_range_m,
            DepthGrid {
                samples_mm: depth.samples_mm(),
                width_px,
                height_px,
            },
            DEPTH_SAMPLE_COLUMNS,
            DEPTH_SAMPLE_ROWS,
        ),
        max_range_m,
    ))
}

fn complete_depth_grid(samples_mm: &[u16], width_px: u32, height_px: u32) -> bool {
    let Some(expected_len) = (width_px as usize).checked_mul(height_px as usize) else {
        return false;
    };
    expected_len > 0 && samples_mm.len() >= expected_len
}

pub fn rays_from_lidar_sample(
    sensor_pose: &ResolvedSensorPose,
    scan: &Scan,
) -> Result<Vec<Ray>, Error> {
    let ResolvedSensorKind::Lidar {
        field_of_view_rad,
        max_range_m,
    } = sensor_pose.kind
    else {
        return Err(Error::UnsupportedSensorKind);
    };
    if lidar_payload_invalid(scan) {
        return Err(Error::InvalidPayload);
    }

    Ok(rays_from_projections(
        sensor_pose,
        sample_lidar_rays(sensor_pose.yaw_rad, field_of_view_rad, max_range_m, scan),
        max_range_m,
        DEFAULT_POINT_RADIUS_M,
    ))
}

const DEFAULT_POINT_RADIUS_M: f32 = 0.05;

fn rays_from_projections(
    sensor_pose: &ResolvedSensorPose,
    rays: Vec<RangeRayProjection>,
    max_range_m: f32,
    radius_m: f32,
) -> Vec<Ray> {
    rays.into_iter()
        .map(|ray| evidence_ray(sensor_pose, &ray, max_range_m, radius_m))
        .collect()
}

fn depth_rays(
    sensor_pose: &ResolvedSensorPose,
    projections: Vec<DepthRayProjection>,
    max_range_m: f32,
) -> Vec<Ray> {
    projections
        .into_iter()
        .map(|projection| {
            let origin_m = sensor_pose.offset_xyz_m;
            let end_m = if projection.ray.occupied_distance_m.is_some() {
                projection.point_root_m
            } else {
                projection.open_end_root_m
            };
            Ray::new(
                origin_m,
                end_m,
                distance_3d(origin_m, projection.open_end_root_m).max(max_range_m),
                ray_radius(DEPTH_POINT_RADIUS_M),
                DEFAULT_CONFIDENCE,
            )
        })
        .collect()
}

fn range_distance_for_ray(distance_m: f32, max_range_m: f32) -> Option<f32> {
    if distance_m < 0.0 {
        None
    } else if distance_m.is_finite() {
        Some(distance_m)
    } else {
        Some(max_range_m)
    }
}

fn lidar_payload_invalid(payload: &Scan) -> bool {
    match payload {
        Scan::Ranges(ranges) => {
            ranges.ranges.is_empty() || ranges.ranges.iter().any(|range| *range < 0.0)
        }
        Scan::Points(points) => points
            .points
            .iter()
            .any(|point| point.iter().any(|value| !value.is_finite())),
    }
}

fn distance_3d(left: [f32; 3], right: [f32; 3]) -> f32 {
    let dx = left[0] - right[0];
    let dy = left[1] - right[1];
    let dz = left[2] - right[2];
    (dx * dx + dy * dy + dz * dz).sqrt()
}

fn evidence_ray(
    sensor_pose: &ResolvedSensorPose,
    ray: &RangeRayProjection,
    max_range_m: f32,
    radius_m: f32,
) -> Ray {
    Ray::new(
        sensor_pose.offset_xyz_m,
        project_planar_ray(sensor_pose, ray.angle_rad, ray.clear_distance_m),
        max_range_m,
        ray_radius(radius_m),
        DEFAULT_CONFIDENCE,
    )
}

fn ray_radius(radius_m: f32) -> f32 {
    if radius_m.is_finite() {
        radius_m.max(MIN_RAY_RADIUS_M)
    } else {
        MIN_RAY_RADIUS_M
    }
}

fn project_planar_ray(
    sensor_pose: &ResolvedSensorPose,
    angle_rad: f32,
    distance_m: f32,
) -> [f32; 3] {
    let sensor_angle_rad = angle_rad - sensor_pose.yaw_rad;
    let point_sensor = nalgebra::Vector3::new(
        distance_m * sensor_angle_rad.cos(),
        distance_m * sensor_angle_rad.sin(),
        0.0,
    );
    let point_root = sensor_pose.local_rotation.transform_vector(&point_sensor)
        + nalgebra::Vector3::new(
            sensor_pose.offset_xyz_m[0],
            sensor_pose.offset_xyz_m[1],
            sensor_pose.offset_xyz_m[2],
        );

    [point_root.x, point_root.y, point_root.z]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::component::v1::capability::depth::Depth as DepthPayload;
    use crate::model::component::v1::CapabilityRef;
    use nalgebra::UnitQuaternion;

    fn range_pose(yaw_rad: f32, pitch_rad: f32, roll_rad: f32) -> ResolvedSensorPose {
        ResolvedSensorPose {
            capability: CapabilityRef::new("range", "range"),
            offset_xyz_m: [0.0, 0.0, 0.2],
            yaw_rad,
            local_rotation: UnitQuaternion::from_euler_angles(roll_rad, pitch_rad, yaw_rad),
            kind: ResolvedSensorKind::Range {
                field_of_view_rad: 0.0,
                max_range_m: 4.0,
            },
        }
    }

    #[test]
    fn downward_range_projection_reaches_lower_z() {
        let pose = range_pose(0.0, 0.6, 0.0);
        let point = project_planar_ray(&pose, 0.0, 0.5);

        assert!(point[2] < pose.offset_xyz_m[2]);
    }

    #[test]
    fn horizontal_range_projection_keeps_sensor_z() {
        let pose = range_pose(0.0, 0.0, 0.0);
        let point = project_planar_ray(&pose, 0.0, 0.5);

        assert!((point[2] - pose.offset_xyz_m[2]).abs() < 1e-5);
    }

    #[test]
    fn yaw_rotates_projection_left() {
        let pose = range_pose(std::f32::consts::FRAC_PI_2, 0.0, 0.0);
        let point = project_planar_ray(&pose, pose.yaw_rad, 0.5);

        assert!(point[1] > 0.49);
        assert!(point[0].abs() < 1e-4);
    }

    #[test]
    fn yaw_rotates_projection_backward() {
        let pose = range_pose(std::f32::consts::PI, 0.0, 0.0);
        let point = project_planar_ray(&pose, pose.yaw_rad, 0.5);

        assert!(point[0] < -0.49);
        assert!(point[1].abs() < 1e-4);
    }

    #[test]
    fn upward_pitch_raises_projection_z() {
        let pose = range_pose(0.0, -0.6, 0.0);
        let point = project_planar_ray(&pose, 0.0, 0.5);

        assert!(point[2] > pose.offset_xyz_m[2]);
    }

    #[test]
    fn lateral_sample_angle_respects_sensor_yaw() {
        let pose = range_pose(std::f32::consts::FRAC_PI_2, 0.0, 0.0);
        let point = project_planar_ray(&pose, pose.yaw_rad + std::f32::consts::FRAC_PI_4, 0.5);

        assert!(point[0] < -0.3);
        assert!(point[1] > 0.3);
    }

    #[test]
    fn range_snapshot_keeps_single_truthful_beam() {
        let pose = range_pose(0.4, 0.0, 0.0);
        let rays = rays_from_range_sample(
            &pose,
            &crate::api::component::v1::capability::range::Sample::new(1.5),
        )
        .expect("valid range sample");

        assert_eq!(rays.len(), 1);
        assert_eq!(rays[0].origin_m(), pose.offset_xyz_m);
        assert_eq!(rays[0].max_range_m(), 4.0);
        assert!(rays[0].end_m()[0] < rays[0].max_range_m());
    }

    #[test]
    fn range_no_hit_emits_ray_at_max_range() {
        let pose = range_pose(0.0, 0.0, 0.0);
        let rays = rays_from_range_sample(
            &pose,
            &crate::api::component::v1::capability::range::Sample::new(4.0),
        )
        .expect("valid range sample");

        assert_eq!(rays.len(), 1);
        assert_eq!(rays[0].max_range_m(), 4.0);
        assert!((rays[0].end_m()[0] - 4.0).abs() < 1e-5);
    }

    #[test]
    fn range_nan_becomes_max_range_clear_ray() {
        assert_eq!(range_distance_for_ray(f32::NAN, 4.0), Some(4.0));
    }

    #[test]
    fn depth_samples_preserve_endpoint_radius_and_confidence() {
        let pose = ResolvedSensorPose {
            capability: CapabilityRef::new("front_camera", "depth"),
            offset_xyz_m: [0.0, 0.0, 0.0],
            yaw_rad: 0.0,
            local_rotation: UnitQuaternion::identity(),
            kind: ResolvedSensorKind::Depth {
                field_of_view_rad: std::f32::consts::FRAC_PI_2,
                max_range_m: 5.0,
                width_px: 2,
                height_px: 1,
            },
        };
        let payload = DepthPayload::from_meters([2.0, 9.0]).expect("valid depth");

        let rays = depth_rays(
            &pose,
            sample_depth_rays(
                &pose,
                std::f32::consts::FRAC_PI_2,
                5.0,
                DepthGrid {
                    samples_mm: payload.samples_mm(),
                    width_px: 2,
                    height_px: 1,
                },
                2,
                1,
            ),
            5.0,
        );

        assert_eq!(rays.len(), 2);
        assert!(rays.iter().any(|ray| ray.end_m()[0] < ray.max_range_m()));
        assert!(rays.iter().any(|ray| {
            (distance_3d(ray.origin_m(), ray.end_m()) - ray.max_range_m()).abs() < 1e-5
        }));
        assert!(rays.iter().all(|ray| {
            ray.radius_m() == DEPTH_POINT_RADIUS_M && ray.confidence() == DEFAULT_CONFIDENCE
        }));
    }

    #[test]
    fn depth_off_axis_observed_ray_keeps_per_ray_max_range() {
        let pose = ResolvedSensorPose {
            capability: CapabilityRef::new("front_camera", "depth"),
            offset_xyz_m: [0.0, 0.0, 0.0],
            yaw_rad: 0.0,
            local_rotation: UnitQuaternion::identity(),
            kind: ResolvedSensorKind::Depth {
                field_of_view_rad: std::f32::consts::FRAC_PI_2,
                max_range_m: 5.0,
                width_px: 2,
                height_px: 1,
            },
        };
        let payload = DepthPayload::from_meters([4.9, 4.9]).expect("valid depth");

        let rays = depth_rays(
            &pose,
            sample_depth_rays(
                &pose,
                std::f32::consts::FRAC_PI_2,
                5.0,
                DepthGrid {
                    samples_mm: payload.samples_mm(),
                    width_px: 2,
                    height_px: 1,
                },
                2,
                1,
            ),
            5.0,
        );

        assert!(
            rays.iter()
                .all(|ray| { distance_3d(ray.origin_m(), ray.end_m()) < ray.max_range_m() - 0.05 })
        );
    }

    #[test]
    fn short_depth_payload_is_invalid() {
        let pose = ResolvedSensorPose {
            capability: CapabilityRef::new("front_camera", "depth"),
            offset_xyz_m: [0.0, 0.0, 0.0],
            yaw_rad: 0.0,
            local_rotation: UnitQuaternion::identity(),
            kind: ResolvedSensorKind::Depth {
                field_of_view_rad: std::f32::consts::FRAC_PI_2,
                max_range_m: 5.0,
                width_px: 2,
                height_px: 2,
            },
        };

        let error = rays_from_depth_sample(&pose, &DepthPayload::new(vec![1_000]))
            .expect_err("short depth payload should be invalid");

        assert_eq!(error, Error::InvalidPayload);
    }
}
