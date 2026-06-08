use std::collections::{BTreeMap, HashMap};

use crate::model::component::v1::capability::Capability;
use crate::model::component::v1::{CapabilityRef, Component as SourceComponent};
use crate::model::robot::v1::Robot;
use crate::model::structure::Structure;
use anyhow::{Result, bail};
use nalgebra::{Isometry3, UnitQuaternion};

use crate::spatial::frame::{extract_link_transforms, resolve_target_link};

const DEFAULT_DEPTH_FOV_RAD: f32 = std::f32::consts::FRAC_PI_2;
const DEFAULT_DEPTH_MAX_RANGE_M: f32 = 5.0;
const DEFAULT_LIDAR_FOV_RAD: f32 = std::f32::consts::TAU;
const DEFAULT_LIDAR_MAX_RANGE_M: f32 = 10.0;

#[derive(Debug, Clone)]
pub struct ResolvedSensorPose {
    pub capability: CapabilityRef,
    pub offset_xyz_m: [f32; 3],
    pub yaw_rad: f32,
    pub local_rotation: UnitQuaternion<f32>,
    pub kind: ResolvedSensorKind,
}

#[derive(Debug, Clone)]
pub enum ResolvedSensorKind {
    Range {
        field_of_view_rad: f32,
        max_range_m: f32,
    },
    Depth {
        field_of_view_rad: f32,
        max_range_m: f32,
        width_px: u32,
        height_px: u32,
    },
    Lidar {
        field_of_view_rad: f32,
        max_range_m: f32,
    },
}

pub fn resolve_sensor_poses(
    model: &Robot,
    components: &BTreeMap<String, SourceComponent>,
    structure: &Structure,
    devices: &[CapabilityRef],
) -> Result<Vec<ResolvedSensorPose>> {
    resolve_sensor_poses_in_frame(
        model,
        components,
        structure,
        devices,
        structure.root_link_name()?,
    )
}

pub fn resolve_sensor_poses_in_frame(
    model: &Robot,
    components: &BTreeMap<String, SourceComponent>,
    structure: &Structure,
    devices: &[CapabilityRef],
    target_frame: &str,
) -> Result<Vec<ResolvedSensorPose>> {
    let link_transforms = extract_link_transforms(structure)?;
    let target_to_root = link_transforms
        .get(target_frame)
        .copied()
        .ok_or_else(|| anyhow::anyhow!("missing target frame '{target_frame}' transform"))?
        .inverse();
    devices
        .iter()
        .map(|capability_ref| {
            resolve_sensor_pose_with_transforms(
                model,
                components,
                structure,
                &link_transforms,
                target_to_root,
                capability_ref,
            )
        })
        .collect()
}

pub fn resolve_capability_link_pose_in_frame(
    model: &Robot,
    components: &BTreeMap<String, SourceComponent>,
    structure: &Structure,
    capability: &CapabilityRef,
    target_frame: &str,
) -> Result<Isometry3<f64>> {
    let link_transforms = extract_link_transforms(structure)?;
    let target_to_root = link_transforms
        .get(target_frame)
        .copied()
        .ok_or_else(|| anyhow::anyhow!("missing target frame '{target_frame}' transform"))?
        .inverse();

    resolve_capability_link_pose_with_transforms(
        model,
        components,
        structure,
        &link_transforms,
        target_to_root,
        capability,
    )
}

fn resolve_sensor_pose_with_transforms(
    model: &Robot,
    components: &BTreeMap<String, SourceComponent>,
    structure: &Structure,
    link_transforms: &HashMap<String, Isometry3<f64>>,
    target_to_root: Isometry3<f64>,
    capability_ref: &CapabilityRef,
) -> Result<ResolvedSensorPose> {
    let capability = configuration_capability(model, components, capability_ref)?;
    let kind = resolved_sensor_kind(capability, capability_ref)?;
    let transform = resolve_capability_link_pose_with_transforms(
        model,
        components,
        structure,
        link_transforms,
        target_to_root,
        capability_ref,
    )?;
    let (_, _, yaw_rad) = transform.rotation.euler_angles();

    Ok(ResolvedSensorPose {
        capability: capability_ref.clone(),
        offset_xyz_m: [
            transform.translation.x as f32,
            transform.translation.y as f32,
            transform.translation.z as f32,
        ],
        yaw_rad: yaw_rad as f32,
        local_rotation: UnitQuaternion::from_quaternion(nalgebra::Quaternion::new(
            transform.rotation.w as f32,
            transform.rotation.i as f32,
            transform.rotation.j as f32,
            transform.rotation.k as f32,
        )),
        kind,
    })
}

fn resolve_capability_link_pose_with_transforms(
    model: &Robot,
    components: &BTreeMap<String, SourceComponent>,
    structure: &Structure,
    link_transforms: &HashMap<String, Isometry3<f64>>,
    target_to_root: Isometry3<f64>,
    capability_ref: &CapabilityRef,
) -> Result<Isometry3<f64>> {
    let capability = configuration_capability(model, components, capability_ref)?;
    let namespaced_target = capability.target().namespaced(&capability_ref.component_id);
    let link_id = match resolve_target_link(&namespaced_target, structure) {
        Ok(link_id) => link_id,
        Err(target_error) => {
            let Some(model_component) = model.component_instance(&capability_ref.component_id)
            else {
                return Err(target_error);
            };
            structure
                .link(&model_component.mount_link)
                .map(|link| link.name.as_str())
                .ok_or(target_error)?
        }
    };
    let transform = target_to_root
        * link_transforms
            .get(link_id)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("missing transform for sensor link '{link_id}'"))?;
    Ok(transform)
}

fn configuration_capability<'a>(
    model: &'a Robot,
    components: &'a BTreeMap<String, SourceComponent>,
    capability_ref: &CapabilityRef,
) -> Result<&'a Capability> {
    let model_component = model
        .component_instance(&capability_ref.component_id)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "component instance '{}' is not defined in robot.yaml",
                capability_ref.component_id
            )
        })?;
    components
        .get(&model_component.component)
        .and_then(|component| component.capabilities.get(&capability_ref.capability_id))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "capability '{}' is not defined in component.yaml",
                capability_ref
            )
        })
}

fn resolved_sensor_kind(
    capability: &Capability,
    capability_ref: &CapabilityRef,
) -> Result<ResolvedSensorKind> {
    match capability {
        Capability::Range(cfg) => Ok(ResolvedSensorKind::Range {
            field_of_view_rad: cfg.field_of_view_rad as f32,
            max_range_m: cfg.max_range_m as f32,
        }),
        Capability::Depth(cfg) => Ok(ResolvedSensorKind::Depth {
            field_of_view_rad: cfg
                .field_of_view_rad
                .unwrap_or(f64::from(DEFAULT_DEPTH_FOV_RAD)) as f32,
            max_range_m: cfg
                .max_range_m
                .unwrap_or(f64::from(DEFAULT_DEPTH_MAX_RANGE_M)) as f32,
            width_px: cfg.width_px,
            height_px: cfg.height_px,
        }),
        Capability::Lidar(cfg) => Ok(ResolvedSensorKind::Lidar {
            field_of_view_rad: cfg
                .horizontal_fov_rad
                .unwrap_or(f64::from(DEFAULT_LIDAR_FOV_RAD)) as f32,
            max_range_m: cfg
                .max_range_m
                .unwrap_or(f64::from(DEFAULT_LIDAR_MAX_RANGE_M)) as f32,
        }),
        _ => bail!(
            "capability '{}' must be a range, depth, or lidar capability",
            capability_ref
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use crate::model::component::v1::CapabilityRef;
    use crate::model::component::v1::capability::{
        Capability, Depth, Lidar, LidarOutput, Range, StructuralTarget,
    };
    use crate::model::structure::Structure;
    use anyhow::{Context, Result};

    use super::{
        DEFAULT_DEPTH_FOV_RAD, DEFAULT_DEPTH_MAX_RANGE_M, DEFAULT_LIDAR_FOV_RAD,
        DEFAULT_LIDAR_MAX_RANGE_M, ResolvedSensorKind, resolve_capability_link_pose_in_frame,
        resolve_sensor_poses_in_frame, resolved_sensor_kind,
    };

    #[test]
    fn resolve_sensor_pose_falls_back_to_model_mount_link_when_namespaced_target_absent()
    -> Result<()> {
        let bundle_root = workspace_root()
            .join("fixture")
            .join("robot")
            .join("rgbd-imu-diff-drive");
        let model = crate::model::robot::v1::Robot::read_from_dir(&bundle_root)?;
        let components = source_components(&bundle_root, &model)?;
        let structure = Structure::read_from_dir(&bundle_root)?;
        assert!(
            structure.link("front_center_tof__sensor_link").is_none(),
            "fixture should exercise the mount-link fallback"
        );

        let sensors = resolve_sensor_poses_in_frame(
            &model,
            &components,
            &structure,
            &[CapabilityRef::new("front_center_tof", "range")],
            "base_footprint",
        )?;

        assert_eq!(sensors.len(), 1);
        assert_eq!(
            sensors[0].capability,
            CapabilityRef::new("front_center_tof", "range")
        );
        assert!(sensors[0].offset_xyz_m[2] > 0.0);
        Ok(())
    }

    #[test]
    fn resolve_capability_link_pose_resolves_rgb_camera_link_in_base_footprint() -> Result<()> {
        let bundle_root = workspace_root()
            .join("fixture")
            .join("robot")
            .join("rgbd-imu-diff-drive");
        let model = crate::model::robot::v1::Robot::read_from_dir(&bundle_root)?;
        let components = source_components(&bundle_root, &model)?;
        let structure = Structure::read_from_dir(&bundle_root)?;

        let transform = resolve_capability_link_pose_in_frame(
            &model,
            &components,
            &structure,
            &CapabilityRef::new("front_camera", "rgb"),
            "base_footprint",
        )?;

        assert!(
            transform.translation.x > 0.0,
            "front camera rgb link should be forward of base_footprint: {}",
            transform.translation.x
        );
        assert!(
            transform.translation.z > 0.0,
            "front camera rgb link should be above base_footprint: {}",
            transform.translation.z
        );
        Ok(())
    }

    #[test]
    fn resolved_sensor_kind_uses_depth_and_lidar_defaults() {
        let depth = Capability::Depth(Depth {
            target: StructuralTarget::Link {
                id: "sensor_link".to_string(),
            },
            publish_rate_hz: 30.0,
            width_px: 640,
            height_px: 400,
            field_of_view_rad: None,
            min_range_m: None,
            max_range_m: None,
        });
        let lidar = Capability::Lidar(Lidar {
            target: StructuralTarget::Link {
                id: "sensor_link".to_string(),
            },
            publish_rate_hz: 10.0,
            output: LidarOutput::Ranges,
            horizontal_resolution_rad: None,
            vertical_resolution_rad: None,
            horizontal_fov_rad: None,
            vertical_fov_rad: None,
            min_range_m: None,
            max_range_m: None,
        });
        let range = Capability::Range(Range {
            target: StructuralTarget::Link {
                id: "sensor_link".to_string(),
            },
            publish_rate_hz: 20.0,
            field_of_view_rad: 0.3,
            min_range_m: 0.1,
            max_range_m: 4.0,
        });

        assert!(matches!(
            resolved_sensor_kind(&depth, &CapabilityRef::new("c", "d")).expect("depth"),
            ResolvedSensorKind::Depth {
                field_of_view_rad,
                max_range_m,
                width_px,
                height_px,
            } if (field_of_view_rad - DEFAULT_DEPTH_FOV_RAD).abs() < 1e-6
                && (max_range_m - DEFAULT_DEPTH_MAX_RANGE_M).abs() < 1e-6
                && width_px == 640
                && height_px == 400
        ));
        assert!(matches!(
            resolved_sensor_kind(&lidar, &CapabilityRef::new("c", "l")).expect("lidar"),
            ResolvedSensorKind::Lidar {
                field_of_view_rad,
                max_range_m
            } if (field_of_view_rad - DEFAULT_LIDAR_FOV_RAD).abs() < 1e-6 && (max_range_m - DEFAULT_LIDAR_MAX_RANGE_M).abs() < 1e-6
        ));
        assert!(matches!(
            resolved_sensor_kind(&range, &CapabilityRef::new("c", "r")).expect("range"),
            ResolvedSensorKind::Range {
                field_of_view_rad,
                max_range_m
            } if (field_of_view_rad - 0.3).abs() < 1e-6 && (max_range_m - 4.0).abs() < 1e-6
        ));
    }

    fn source_components(
        bundle_root: &Path,
        model: &crate::model::robot::v1::Robot,
    ) -> Result<BTreeMap<String, crate::model::component::v1::Component>> {
        let fixture_root = bundle_root
            .parent()
            .and_then(Path::parent)
            .context("fixture bundle root must live under fixture/robot")?;
        model
            .used_component_types()
            .into_iter()
            .map(|component_type| {
                let component = crate::model::component::Component::read_from_dir(
                    fixture_root.join("component").join(component_type),
                )?
                .as_v1()
                .context("fixture components must use component.yaml version v1")?
                .clone();
                Ok((component_type.to_string(), component))
            })
            .collect()
    }

    fn workspace_root() -> PathBuf {
        let manifest_dir = match std::env::var("CARGO_MANIFEST_DIR") {
            Ok(value) => PathBuf::from(value),
            Err(error) => panic!("CARGO_MANIFEST_DIR is not set: {error}"),
        };
        // phoxal sits one level below the workspace root.
        let workspace_root = match manifest_dir.parent() {
            Some(path) => path,
            None => panic!(
                "phoxal CARGO_MANIFEST_DIR must live one level below the workspace root: {}",
                manifest_dir.display()
            ),
        };
        workspace_root.to_path_buf()
    }
}
