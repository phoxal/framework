use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use phoxal::api::component::v1::capability::profile::ProfileId;
use phoxal::model::component::v1::CapabilityRef;
use phoxal::model::component::v1::capability::{Capability, GnssCoordinateSystem};
use phoxal::model::robot::v1::{ResolvedLocalizeBackend, resolve_localize_backend};
use phoxal::model::v1::Robot;
use tracing::warn;

use crate::runtime::BackendSelection;

pub const ENV_ORB_SLAM3_VOCABULARY: &str = "ORB_SLAM3_VOCABULARY";

pub fn capability_default_profile_topic(
    robot: &Robot,
    capability: &CapabilityRef,
) -> anyhow::Result<String> {
    robot.capability(capability)?;
    Ok(phoxal::api::component::v1::capability::profile_path(
        &capability.component_id,
        &capability.capability_id,
        &ProfileId::default_profile(),
    ))
}

fn capability_profile_topic(capability: &CapabilityRef, profile_id: &ProfileId) -> String {
    phoxal::api::component::v1::capability::profile_path(
        &capability.component_id,
        &capability.capability_id,
        profile_id,
    )
}

pub fn select_backend(
    robot: &Robot,
    orb_slam3_vocabulary_path: Option<&std::path::Path>,
) -> Result<BackendSelection> {
    select_backend_with_settings_dir(robot, orb_slam3_vocabulary_path, None)
}

fn select_backend_with_settings_dir(
    robot: &Robot,
    orb_slam3_vocabulary_path: Option<&Path>,
    settings_dir: Option<&Path>,
) -> Result<BackendSelection> {
    match resolve_localize_backend(&robot.manifest, &robot.components) {
        ResolvedLocalizeBackend::DeadReckoning => Ok(BackendSelection::DeadReckoning),
        ResolvedLocalizeBackend::GnssAnchored { gnss } => Ok(BackendSelection::GnssAnchored {
            gnss_topic: capability_default_profile_topic(robot, &gnss)?,
            coordinate_system: gnss_coordinate_system(robot, &gnss)?,
        }),
        ResolvedLocalizeBackend::OrbSlam3RgbdInertial { camera, depth, imu } => {
            let Some(vocabulary_path) = orb_slam3_vocabulary_path else {
                warn!(
                    "ORB-SLAM3-eligible robot but no vocabulary path configured; falling back to dead-reckoning"
                );
                return Ok(BackendSelection::DeadReckoning);
            };
            let color_intrinsics = intrinsics_for(robot, &camera)?;
            let depth_intrinsics = intrinsics_for(robot, &depth)?;
            let camera_fps = camera_publish_rate_hz(robot, &camera)?;
            let imu_frequency_hz = imu_publish_frequency_hz(robot, &imu)?;
            let camera_topic = default_profile_topic(&camera);
            let depth_topic = default_profile_topic(&depth);
            let camera_optical_to_base = camera_optical_to_base(robot, &camera)?;
            let imu_to_camera_optical = imu_to_camera_optical(robot, &camera, &imu)?;
            let settings_path = write_orb_slam3_settings(
                &color_intrinsics,
                camera_fps,
                imu_frequency_hz,
                imu_to_camera_optical,
                settings_dir,
            )?;

            Ok(BackendSelection::OrbSlam3(Box::new(
                crate::orbslam3::OrbSlam3Config {
                    vocabulary_path: vocabulary_path.to_path_buf(),
                    settings_path,
                    camera_topic,
                    depth_topic,
                    imu_topic: Some(capability_default_profile_topic(robot, &imu)?),
                    inertial: true,
                    color_intrinsics,
                    depth_intrinsics,
                    camera_optical_to_base,
                },
            )))
        }
        ResolvedLocalizeBackend::OrbSlam3Rgbd { camera, depth } => {
            let Some(vocabulary_path) = orb_slam3_vocabulary_path else {
                warn!(
                    "ORB-SLAM3-eligible robot but no vocabulary path configured; falling back to dead-reckoning"
                );
                return Ok(BackendSelection::DeadReckoning);
            };
            let color_intrinsics = intrinsics_for(robot, &camera)?;
            let depth_intrinsics = intrinsics_for(robot, &depth)?;
            let camera_fps = camera_publish_rate_hz(robot, &camera)?;
            let camera_topic = default_profile_topic(&camera);
            let depth_topic = default_profile_topic(&depth);
            let camera_optical_to_base = camera_optical_to_base(robot, &camera)?;
            let settings_path =
                write_orb_slam3_rgbd_settings(&color_intrinsics, camera_fps, settings_dir)?;

            Ok(BackendSelection::OrbSlam3(Box::new(
                crate::orbslam3::OrbSlam3Config {
                    vocabulary_path: vocabulary_path.to_path_buf(),
                    settings_path,
                    camera_topic,
                    depth_topic,
                    imu_topic: None,
                    inertial: false,
                    color_intrinsics,
                    depth_intrinsics,
                    camera_optical_to_base,
                },
            )))
        }
    }
}

fn gnss_coordinate_system(
    robot: &Robot,
    capability_ref: &CapabilityRef,
) -> Result<GnssCoordinateSystem> {
    let capability = robot.capability(capability_ref)?;
    let Capability::Gnss(gnss) = capability else {
        bail!(
            "resolved GNSS-anchored capability '{}' must be gnss, found {}",
            capability_ref,
            capability.kind_name()
        );
    };
    Ok(gnss.coordinate_system)
}

fn default_profile_topic(capability_ref: &CapabilityRef) -> String {
    capability_profile_topic(capability_ref, &ProfileId::default_profile())
}

fn camera_optical_to_base(robot: &Robot, camera: &CapabilityRef) -> Result<([f64; 3], [f64; 4])> {
    let transform = phoxal::spatial::sensor::resolve_capability_link_pose_in_frame(
        &robot.manifest,
        &robot.components,
        &robot.structure,
        camera,
        "base_footprint",
    )?;
    let rotation = transform.rotation.quaternion();
    Ok(crate::pose_math::camera_optical_to_base_extrinsic(
        [
            transform.translation.x,
            transform.translation.y,
            transform.translation.z,
        ],
        [rotation.i, rotation.j, rotation.k, rotation.w],
    ))
}

fn imu_to_camera_optical(
    robot: &Robot,
    camera: &CapabilityRef,
    imu: &CapabilityRef,
) -> Result<([f64; 3], [[f64; 3]; 3])> {
    let camera_link_in_base = phoxal::spatial::sensor::resolve_capability_link_pose_in_frame(
        &robot.manifest,
        &robot.components,
        &robot.structure,
        camera,
        "base_footprint",
    )?;
    let imu_link_in_base = phoxal::spatial::sensor::resolve_capability_link_pose_in_frame(
        &robot.manifest,
        &robot.components,
        &robot.structure,
        imu,
        "base_footprint",
    )?;

    let imu_to_camera_link = imu_link_in_base.inverse() * camera_link_in_base;
    let rotation = imu_to_camera_link.rotation.quaternion();
    let (translation, rotation) = crate::pose_math::compose_poses(
        [
            imu_to_camera_link.translation.x,
            imu_to_camera_link.translation.y,
            imu_to_camera_link.translation.z,
        ],
        [rotation.i, rotation.j, rotation.k, rotation.w],
        [0.0, 0.0, 0.0],
        crate::pose_math::CAMERA_OPTICAL_FROM_LINK,
    );

    Ok((
        translation,
        crate::pose_math::rotation_matrix_from_quaternion(rotation),
    ))
}

fn camera_publish_rate_hz(robot: &Robot, capability_ref: &CapabilityRef) -> Result<f64> {
    use anyhow::bail;
    use phoxal::model::component::v1::capability::Capability;

    let capability = robot.capability(capability_ref)?;
    let Capability::Camera(camera) = capability else {
        bail!(
            "resolved ORB-SLAM3 RGB capability '{}' must be camera, found {}",
            capability_ref,
            capability.kind_name()
        );
    };
    validate_publish_rate_hz(camera.publish_rate_hz, capability_ref, "RGB camera")
}

fn imu_publish_frequency_hz(robot: &Robot, capability_ref: &CapabilityRef) -> Result<f64> {
    use anyhow::bail;
    use phoxal::model::component::v1::capability::Capability;

    let capability = robot.capability(capability_ref)?;
    let Capability::Imu(imu) = capability else {
        bail!(
            "resolved ORB-SLAM3 IMU capability '{}' must be imu, found {}",
            capability_ref,
            capability.kind_name()
        );
    };
    validate_publish_rate_hz(imu.publish_rate_hz, capability_ref, "IMU")
}

fn validate_publish_rate_hz(
    publish_rate_hz: f64,
    capability_ref: &CapabilityRef,
    label: &str,
) -> Result<f64> {
    use anyhow::bail;

    if !publish_rate_hz.is_finite() || publish_rate_hz <= 0.0 {
        bail!(
            "resolved ORB-SLAM3 {label} capability '{capability_ref}' requires publish_rate_hz > 0, got {publish_rate_hz}"
        );
    }
    Ok(publish_rate_hz)
}

fn intrinsics_for(
    robot: &Robot,
    capability_ref: &CapabilityRef,
) -> Result<crate::settings::CameraIntrinsics> {
    use anyhow::bail;
    use phoxal::model::component::v1::capability::Capability;

    let capability = robot.capability(capability_ref)?;
    let (width_px, height_px, field_of_view_rad) = match capability {
        Capability::Camera(camera) => (camera.width_px, camera.height_px, camera.field_of_view_rad),
        Capability::Depth(depth) => (depth.width_px, depth.height_px, depth.field_of_view_rad),
        Capability::Motor(_)
        | Capability::Encoder(_)
        | Capability::Accelerometer(_)
        | Capability::Gyroscope(_)
        | Capability::Magnetometer(_)
        | Capability::Imu(_)
        | Capability::Gnss(_)
        | Capability::Range(_)
        | Capability::Lidar(_)
        | Capability::Mmwave(_)
        | Capability::Microphone(_)
        | Capability::Speaker(_)
        | Capability::Battery(_)
        | Capability::EmergencyStop(_)
        | Capability::Led(_) => {
            bail!(
                "resolved ORB-SLAM3 capability '{}' must be camera or depth, found {}",
                capability_ref,
                capability.kind_name()
            );
        }
    };
    let Some(horizontal_fov_rad) = field_of_view_rad else {
        bail!("resolved ORB-SLAM3 capability '{capability_ref}' requires field_of_view_rad");
    };

    crate::settings::CameraIntrinsics::from_horizontal_fov(width_px, height_px, horizontal_fov_rad)
}

fn write_orb_slam3_settings(
    intrinsics: &crate::settings::CameraIntrinsics,
    camera_fps: f64,
    imu_frequency_hz: f64,
    imu_to_camera_optical: ([f64; 3], [[f64; 3]; 3]),
    settings_dir: Option<&Path>,
) -> Result<std::path::PathBuf> {
    use anyhow::Context as _;

    use crate::settings::render_rgbd_inertial_settings;

    let settings = render_rgbd_inertial_settings(
        intrinsics,
        1000.0,
        camera_fps,
        imu_frequency_hz,
        imu_to_camera_optical,
    );

    let settings_dir = orb_slam3_settings_dir(settings_dir);
    std::fs::create_dir_all(&settings_dir).with_context(|| {
        format!(
            "failed to create ORB-SLAM3 settings directory {}",
            settings_dir.display()
        )
    })?;
    let settings_path = settings_dir.join("rgbd-inertial.yaml");
    std::fs::write(&settings_path, settings).with_context(|| {
        format!(
            "failed to write ORB-SLAM3 settings file {}",
            settings_path.display()
        )
    })?;

    Ok(settings_path)
}

fn write_orb_slam3_rgbd_settings(
    intrinsics: &crate::settings::CameraIntrinsics,
    camera_fps: f64,
    settings_dir: Option<&Path>,
) -> Result<std::path::PathBuf> {
    use anyhow::Context as _;

    use crate::settings::render_rgbd_settings;

    let settings = render_rgbd_settings(intrinsics, 1000.0, camera_fps);

    let settings_dir = orb_slam3_settings_dir(settings_dir);
    std::fs::create_dir_all(&settings_dir).with_context(|| {
        format!(
            "failed to create ORB-SLAM3 settings directory {}",
            settings_dir.display()
        )
    })?;
    let settings_path = settings_dir.join("rgbd.yaml");
    std::fs::write(&settings_path, settings).with_context(|| {
        format!(
            "failed to write ORB-SLAM3 settings file {}",
            settings_path.display()
        )
    })?;

    Ok(settings_path)
}

fn orb_slam3_settings_dir(settings_dir: Option<&Path>) -> PathBuf {
    settings_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::temp_dir().join("orb-slam3"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use phoxal::model::component::v1::CapabilityRef;
    use phoxal::model::component::v1::capability::{Capability, Gnss, StructuralTarget};
    use phoxal::model::robot::v1::{Component, Role};
    use phoxal::model::v1::Robot;

    use super::{
        GnssCoordinateSystem, capability_default_profile_topic, default_profile_topic,
        imu_to_camera_optical, select_backend, select_backend_with_settings_dir,
    };
    use crate::runtime::BackendSelection;

    #[test]
    fn capability_default_profile_topic_matches_blueprint_shape() {
        let robot = fixture_robot();

        let imu_topic =
            match capability_default_profile_topic(&robot, &CapabilityRef::new("imu", "imu")) {
                Ok(topic) => topic,
                Err(error) => panic!("imu topic resolution failed: {error:#}"),
            };

        assert_eq!(imu_topic, "component/imu/imu/profile/default");
    }

    #[test]
    fn camera_and_depth_topics_request_default_profile() {
        assert_eq!(
            default_profile_topic(&CapabilityRef::new("front_camera", "depth")),
            "component/front_camera/depth/profile/default"
        );
        assert_eq!(
            default_profile_topic(&CapabilityRef::new("front_camera", "rgb")),
            "component/front_camera/rgb/profile/default"
        );
    }

    #[test]
    fn selector_returns_gnss_anchored_for_gnss_localization_robot() {
        let mut robot = fixture_robot();
        component_roles_mut(&mut robot, "front_camera").clear();
        component_roles_mut(&mut robot, "imu").clear();
        add_gnss_localization_component(&mut robot);

        let backend = match select_backend(&robot, None) {
            Ok(backend) => backend,
            Err(error) => panic!("selector failed: {error:#}"),
        };
        let BackendSelection::GnssAnchored {
            gnss_topic,
            coordinate_system,
        } = backend
        else {
            panic!("expected GNSS-anchored backend");
        };

        assert_eq!(gnss_topic, "component/gnss/gnss/profile/default");
        assert_eq!(coordinate_system, GnssCoordinateSystem::Local);
    }

    #[test]
    fn feature_on_no_paths_returns_dead_reckoning() {
        let robot = fixture_robot();

        let backend = match select_backend(&robot, None) {
            Ok(backend) => backend,
            Err(error) => panic!("selector failed: {error:#}"),
        };
        assert!(matches!(backend, BackendSelection::DeadReckoning));
    }

    #[test]
    fn feature_on_with_paths_returns_orb_slam3() {
        let robot = fixture_robot();
        let vocabulary_path = PathBuf::from("/tmp/orb-vocabulary.txt");
        let (settings_root, settings_dir) = temp_settings_dir();

        let backend = match select_backend_with_settings_dir(
            &robot,
            Some(&vocabulary_path),
            Some(&settings_dir),
        ) {
            Ok(backend) => backend,
            Err(error) => panic!("selector failed: {error:#}"),
        };
        let _settings_root = settings_root;
        let BackendSelection::OrbSlam3(config) = backend else {
            panic!("expected ORB-SLAM3 backend");
        };

        assert_eq!(
            config.camera_topic,
            "component/front_camera/rgb/profile/default"
        );
        assert_eq!(
            config.depth_topic,
            "component/front_camera/depth/profile/default"
        );
        assert_eq!(
            config.imu_topic.as_deref(),
            Some("component/imu/imu/profile/default")
        );
        assert!(config.inertial);
        assert_eq!(config.vocabulary_path, vocabulary_path);
        assert!(
            config
                .settings_path
                .ends_with("orb-slam3/rgbd-inertial.yaml")
        );
    }

    #[test]
    fn inertial_settings_yaml_embeds_resolved_imu_extrinsic() {
        let robot = fixture_robot();
        let vocabulary_path = PathBuf::from("/tmp/orb-vocabulary.txt");
        let (settings_root, settings_dir) = temp_settings_dir();

        let backend = match select_backend_with_settings_dir(
            &robot,
            Some(&vocabulary_path),
            Some(&settings_dir),
        ) {
            Ok(backend) => backend,
            Err(error) => panic!("selector failed: {error:#}"),
        };
        let _settings_root = settings_root;
        let BackendSelection::OrbSlam3(config) = backend else {
            panic!("expected ORB-SLAM3 backend");
        };
        assert!(
            config.inertial,
            "fixture must produce an inertial backend for this test"
        );

        let yaml = match std::fs::read_to_string(&config.settings_path) {
            Ok(text) => text,
            Err(error) => panic!(
                "failed to read settings file at {}: {error:#}",
                config.settings_path.display()
            ),
        };

        assert!(
            !yaml.contains(
                "1.0, 0.0, 0.0, 0.0,\n          0.0, 1.0, 0.0, 0.0,\n          0.0, 0.0, 1.0, 0.0,\n          0.0, 0.0, 0.0, 1.0]"
            ),
            "IMU.T_b_c1 must not be the previously hardcoded identity:\n{yaml}"
        );

        let matrix = parse_imu_t_b_c1(&yaml);
        let (expected_translation, expected_rotation) = imu_to_camera_optical(
            &robot,
            &CapabilityRef::new("front_camera", "rgb"),
            &CapabilityRef::new("imu", "imu"),
        )
        .expect("imu_to_camera_optical must succeed against the fixture");

        let expected = [
            [
                expected_rotation[0][0],
                expected_rotation[0][1],
                expected_rotation[0][2],
                expected_translation[0],
            ],
            [
                expected_rotation[1][0],
                expected_rotation[1][1],
                expected_rotation[1][2],
                expected_translation[1],
            ],
            [
                expected_rotation[2][0],
                expected_rotation[2][1],
                expected_rotation[2][2],
                expected_translation[2],
            ],
            [0.0, 0.0, 0.0, 1.0],
        ];

        for r in 0..4 {
            for c in 0..4 {
                assert!(
                    (matrix[r][c] - expected[r][c]).abs() < 1e-6,
                    "IMU.T_b_c1[{r}][{c}] = {actual} did not match expected {expected_val} (tolerance 1e-6):\n{yaml}",
                    actual = matrix[r][c],
                    expected_val = expected[r][c],
                );
            }
        }

        let is_identity = (matrix[0][0] - 1.0).abs() < 1e-9
            && (matrix[1][1] - 1.0).abs() < 1e-9
            && (matrix[2][2] - 1.0).abs() < 1e-9
            && matrix[0][1].abs() < 1e-9
            && matrix[0][2].abs() < 1e-9
            && matrix[1][0].abs() < 1e-9
            && matrix[1][2].abs() < 1e-9
            && matrix[2][0].abs() < 1e-9
            && matrix[2][1].abs() < 1e-9
            && matrix[0][3].abs() < 1e-9
            && matrix[1][3].abs() < 1e-9
            && matrix[2][3].abs() < 1e-9;
        assert!(
            !is_identity,
            "fixture has a non-trivial IMU<->camera offset; identity matrix means resolution was skipped:\n{yaml}",
        );
    }

    #[test]
    fn feature_on_with_rgbd_only_sensor_mix_returns_orb_slam3_rgbd() {
        let mut robot = fixture_robot();
        component_roles_mut(&mut robot, "imu").insert("imu".to_string(), vec![Role::Odometry]);
        let vocabulary_path = PathBuf::from("/tmp/orb-vocabulary.txt");
        let (settings_root, settings_dir) = temp_settings_dir();

        let backend = match select_backend_with_settings_dir(
            &robot,
            Some(&vocabulary_path),
            Some(&settings_dir),
        ) {
            Ok(backend) => backend,
            Err(error) => panic!("selector failed: {error:#}"),
        };
        let _settings_root = settings_root;
        let BackendSelection::OrbSlam3(config) = backend else {
            panic!("expected ORB-SLAM3 backend");
        };

        assert_eq!(
            config.camera_topic,
            "component/front_camera/rgb/profile/default"
        );
        assert_eq!(
            config.depth_topic,
            "component/front_camera/depth/profile/default"
        );
        assert!(config.imu_topic.is_none());
        assert!(!config.inertial);
        assert_eq!(config.vocabulary_path, vocabulary_path);
        assert!(config.settings_path.ends_with("orb-slam3/rgbd.yaml"));
    }

    fn parse_imu_t_b_c1(yaml: &str) -> [[f64; 4]; 4] {
        let start_marker = "IMU.T_b_c1: !!opencv-matrix";
        let start = yaml.find(start_marker).expect("IMU.T_b_c1 block present");
        let after_block = &yaml[start..];
        let data_start_rel = after_block.find("data:").expect("data: line present");
        let after_data = &after_block[data_start_rel..];
        let open = after_data.find('[').expect("data block opens with [");
        let close = after_data[open..]
            .find(']')
            .expect("data block closes with ]");
        let inside = &after_data[open + 1..open + close];

        let values: Vec<f64> = inside
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.parse::<f64>().expect("matrix entry parses as f64"))
            .collect();
        assert_eq!(
            values.len(),
            16,
            "IMU.T_b_c1 data block must have 16 entries"
        );

        let mut matrix = [[0.0_f64; 4]; 4];
        for (idx, value) in values.into_iter().enumerate() {
            matrix[idx / 4][idx % 4] = value;
        }
        matrix
    }

    fn temp_settings_dir() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().expect("selector test settings tempdir should be created");
        let settings_dir = root.path().join("orb-slam3");
        (root, settings_dir)
    }

    fn fixture_robot() -> Robot {
        let bundle_root = fixture_bundle_root();

        match Robot::read_from_dir(&bundle_root) {
            Ok(robot) => robot,
            Err(error) => panic!(
                "failed to read fixture robot from {}: {error:#}",
                bundle_root.display()
            ),
        }
    }

    fn fixture_bundle_root() -> PathBuf {
        let manifest_dir = match std::env::var("CARGO_MANIFEST_DIR") {
            Ok(value) => PathBuf::from(value),
            Err(error) => panic!("CARGO_MANIFEST_DIR is not set: {error}"),
        };
        let workspace_root = match manifest_dir.parent().and_then(|path| path.parent()) {
            Some(path) => path,
            None => panic!(
                "runtime/localize CARGO_MANIFEST_DIR must live two levels below the workspace root: {}",
                manifest_dir.display()
            ),
        };
        workspace_root
            .join("fixture")
            .join("robot")
            .join("rgbd-imu-diff-drive")
    }

    fn component_roles_mut<'a>(
        robot: &'a mut Robot,
        component_id: &str,
    ) -> &'a mut BTreeMap<String, Vec<Role>> {
        match robot.manifest.components.get_mut(component_id) {
            Some(component) => &mut component.roles,
            None => panic!("fixture missing {component_id} component instance"),
        }
    }

    fn add_gnss_localization_component(robot: &mut Robot) {
        robot.components.insert(
            "zed_f9p".to_string(),
            phoxal::model::component::v1::Component::new(BTreeMap::from([(
                "gnss".to_string(),
                Capability::Gnss(Gnss {
                    target: StructuralTarget::Link {
                        id: "sensor_link".to_string(),
                    },
                    publish_rate_hz: 10.0,
                    coordinate_system: Default::default(),
                }),
            )])),
        );
        robot.manifest.components.insert(
            "gnss".to_string(),
            Component {
                component: "zed_f9p".to_string(),
                mount_link: "gnss_mount".to_string(),
                driver: None,
                roles: BTreeMap::from([("gnss".to_string(), vec![Role::Localization])]),
                parameters: BTreeMap::new(),
            },
        );
    }
}
