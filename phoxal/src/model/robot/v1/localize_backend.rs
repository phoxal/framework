use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model::component::v1::CapabilityRef;
use crate::model::component::v1::capability::Capability;

use super::{Robot, Role};

/// Resolved-fact identity of the localization backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalizeBackendKind {
    DeadReckoning,
    OrbSlam3Rgbd,
    OrbSlam3RgbdInertial,
    GnssAnchored,
}

impl LocalizeBackendKind {
    /// Backend family.
    #[must_use]
    pub const fn family(self) -> &'static str {
        match self {
            Self::DeadReckoning => "proprioceptive",
            Self::OrbSlam3Rgbd => "vslam_rgbd",
            Self::OrbSlam3RgbdInertial => "vio_rgbd",
            Self::GnssAnchored => "gnss",
        }
    }

    /// Backend name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::DeadReckoning => "dead_reckoning",
            Self::OrbSlam3Rgbd => "orb_slam3_rgbd",
            Self::OrbSlam3RgbdInertial => "orb_slam3",
            Self::GnssAnchored => "gnss_anchored",
        }
    }

    /// Pinned backend version, part of the resolved localize-backend identity.
    /// Placeholder pins to refine once backend conformance is versioned.
    #[must_use]
    pub const fn version(self) -> &'static str {
        match self {
            Self::DeadReckoning => "builtin-v1",
            Self::OrbSlam3Rgbd => "orb-slam3-rgbd-v1",
            Self::OrbSlam3RgbdInertial => "orb-slam3-rgbd-inertial-v1",
            Self::GnssAnchored => "gnss-anchored-v1",
        }
    }
}

/// Backend resolved from the robot's localization-tagged sensing roles, carrying the
/// concrete capability references the runtime needs to subscribe to inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedLocalizeBackend {
    DeadReckoning,
    OrbSlam3Rgbd {
        camera: CapabilityRef,
        depth: CapabilityRef,
    },
    OrbSlam3RgbdInertial {
        camera: CapabilityRef,
        depth: CapabilityRef,
        imu: CapabilityRef,
    },
    GnssAnchored {
        gnss: CapabilityRef,
    },
}

impl ResolvedLocalizeBackend {
    #[must_use]
    pub const fn kind(&self) -> LocalizeBackendKind {
        match self {
            Self::DeadReckoning => LocalizeBackendKind::DeadReckoning,
            Self::OrbSlam3Rgbd { .. } => LocalizeBackendKind::OrbSlam3Rgbd,
            Self::OrbSlam3RgbdInertial { .. } => LocalizeBackendKind::OrbSlam3RgbdInertial,
            Self::GnssAnchored { .. } => LocalizeBackendKind::GnssAnchored,
        }
    }
}

#[derive(Debug, Default)]
struct InputCandidates {
    camera: Vec<CapabilityRef>,
    depth: Vec<CapabilityRef>,
    imu: Vec<CapabilityRef>,
    gnss: Vec<CapabilityRef>,
}

/// Resolve the localization backend from the robot's localization-tagged sensing roles.
/// BLUEPRINT: the backend is selected by resolved sensing roles, not hand-declared.
#[must_use]
pub fn resolve_localize_backend(
    model: &Robot,
    components: &BTreeMap<String, crate::model::component::v1::Component>,
) -> ResolvedLocalizeBackend {
    let mut candidates = InputCandidates::default();

    for (component_id, component_instance) in &model.components {
        for (capability_id, roles) in &component_instance.roles {
            if !roles.contains(&Role::Localization) {
                continue;
            }

            let Some(component) = components.get(&component_instance.component) else {
                continue;
            };
            let Some(capability) = component.capability(capability_id) else {
                continue;
            };

            let capability_ref = CapabilityRef::new(component_id, capability_id);
            match capability {
                Capability::Camera { .. } => candidates.camera.push(capability_ref),
                Capability::Depth { .. } => candidates.depth.push(capability_ref),
                Capability::Imu { .. } => candidates.imu.push(capability_ref),
                Capability::Gnss { .. } => candidates.gnss.push(capability_ref),
                _ => {}
            }
        }
    }

    if let ([camera], [depth], [imu]) = (
        candidates.camera.as_slice(),
        candidates.depth.as_slice(),
        candidates.imu.as_slice(),
    ) {
        return ResolvedLocalizeBackend::OrbSlam3RgbdInertial {
            camera: camera.clone(),
            depth: depth.clone(),
            imu: imu.clone(),
        };
    }

    if let ([camera], [depth], true) = (
        candidates.camera.as_slice(),
        candidates.depth.as_slice(),
        candidates.imu.is_empty(),
    ) {
        return ResolvedLocalizeBackend::OrbSlam3Rgbd {
            camera: camera.clone(),
            depth: depth.clone(),
        };
    }

    match candidates.gnss.as_slice() {
        [gnss] => ResolvedLocalizeBackend::GnssAnchored { gnss: gnss.clone() },
        _ => ResolvedLocalizeBackend::DeadReckoning,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use crate::model::component::v1::CapabilityRef;
    use crate::model::component::v1::capability::{Capability, Gnss, StructuralTarget};

    use super::{LocalizeBackendKind, ResolvedLocalizeBackend, Role, resolve_localize_backend};
    use crate::model::robot::v1::{Component, Robot};

    #[test]
    fn resolves_orb_slam3_from_rgbd_imu_roles() {
        let (model, components) = fixture_model_and_components();

        assert_eq!(
            resolve_localize_backend(&model, &components),
            ResolvedLocalizeBackend::OrbSlam3RgbdInertial {
                camera: CapabilityRef::new("front_camera", "rgb"),
                depth: CapabilityRef::new("front_camera", "depth"),
                imu: CapabilityRef::new("imu", "imu"),
            }
        );
    }

    #[test]
    fn resolves_dead_reckoning_when_depth_role_missing() {
        let (mut model, components) = fixture_model_and_components();
        component_roles_mut(&mut model, "front_camera").remove("depth");

        assert_eq!(
            resolve_localize_backend(&model, &components),
            ResolvedLocalizeBackend::DeadReckoning
        );
    }

    #[test]
    fn resolves_orb_slam3_rgbd_when_imu_role_missing() {
        let (mut model, components) = fixture_model_and_components();
        component_roles_mut(&mut model, "imu").remove("imu");

        assert_eq!(
            resolve_localize_backend(&model, &components),
            ResolvedLocalizeBackend::OrbSlam3Rgbd {
                camera: CapabilityRef::new("front_camera", "rgb"),
                depth: CapabilityRef::new("front_camera", "depth"),
            }
        );
    }

    #[test]
    fn resolves_dead_reckoning_with_two_localization_imus() {
        let (mut model, components) = fixture_model_and_components();
        let second_imu = match model.components.get("imu") {
            Some(component) => component.clone(),
            None => panic!("fixture missing imu component instance"),
        };
        model.components.insert("rear_imu".to_string(), second_imu);
        component_roles_mut(&mut model, "rear_imu")
            .insert("imu".to_string(), vec![Role::Localization]);

        assert_eq!(
            resolve_localize_backend(&model, &components),
            ResolvedLocalizeBackend::DeadReckoning
        );
    }

    #[test]
    fn resolves_gnss_anchored_from_single_localization_gnss() {
        let (mut model, mut components) = fixture_model_and_components();
        component_roles_mut(&mut model, "front_camera").clear();
        component_roles_mut(&mut model, "imu").clear();
        add_gnss_localization_component(&mut model, &mut components);

        assert_eq!(
            resolve_localize_backend(&model, &components),
            ResolvedLocalizeBackend::GnssAnchored {
                gnss: CapabilityRef::new("gnss", "gnss"),
            }
        );
    }

    #[test]
    fn resolves_orb_slam3_before_gnss_when_rgbd_imu_is_complete() {
        let (mut model, mut components) = fixture_model_and_components();
        add_gnss_localization_component(&mut model, &mut components);

        assert_eq!(
            resolve_localize_backend(&model, &components),
            ResolvedLocalizeBackend::OrbSlam3RgbdInertial {
                camera: CapabilityRef::new("front_camera", "rgb"),
                depth: CapabilityRef::new("front_camera", "depth"),
                imu: CapabilityRef::new("imu", "imu"),
            }
        );
    }

    #[test]
    fn kind_descriptor_strings_match_blueprint() {
        let orb = ResolvedLocalizeBackend::OrbSlam3RgbdInertial {
            camera: CapabilityRef::new("front_camera", "rgb"),
            depth: CapabilityRef::new("front_camera", "depth"),
            imu: CapabilityRef::new("imu", "imu"),
        }
        .kind();
        assert_eq!(orb, LocalizeBackendKind::OrbSlam3RgbdInertial);
        assert_eq!(orb.name(), "orb_slam3");
        assert_eq!(orb.family(), "vio_rgbd");

        let orb_rgbd = ResolvedLocalizeBackend::OrbSlam3Rgbd {
            camera: CapabilityRef::new("front_camera", "rgb"),
            depth: CapabilityRef::new("front_camera", "depth"),
        }
        .kind();
        assert_eq!(orb_rgbd, LocalizeBackendKind::OrbSlam3Rgbd);
        assert_eq!(orb_rgbd.name(), "orb_slam3_rgbd");
        assert_eq!(orb_rgbd.family(), "vslam_rgbd");
        assert_eq!(orb_rgbd.version(), "orb-slam3-rgbd-v1");

        let gnss = ResolvedLocalizeBackend::GnssAnchored {
            gnss: CapabilityRef::new("gnss", "gnss"),
        }
        .kind();
        assert_eq!(gnss, LocalizeBackendKind::GnssAnchored);
        assert_eq!(gnss.name(), "gnss_anchored");
        assert_eq!(gnss.family(), "gnss");
        assert_eq!(gnss.version(), "gnss-anchored-v1");

        let dead_reckoning = ResolvedLocalizeBackend::DeadReckoning.kind();
        assert_eq!(dead_reckoning, LocalizeBackendKind::DeadReckoning);
        assert_eq!(dead_reckoning.name(), "dead_reckoning");
        assert_eq!(dead_reckoning.family(), "proprioceptive");
    }

    fn fixture_model_and_components() -> (
        Robot,
        BTreeMap<String, crate::model::component::v1::Component>,
    ) {
        let bundle_root = fixture_bundle_root();
        let model = match Robot::read_from_dir(&bundle_root) {
            Ok(model) => model,
            Err(error) => panic!(
                "failed to read fixture robot from {}: {error:#}",
                bundle_root.display()
            ),
        };

        let mut components = BTreeMap::new();
        for component_type in model.used_component_types() {
            let component = read_fixture_component(&bundle_root, component_type);
            components.insert(component_type.to_string(), component);
        }

        (model, components)
    }

    fn fixture_bundle_root() -> PathBuf {
        let manifest_dir = match std::env::var("CARGO_MANIFEST_DIR") {
            Ok(value) => PathBuf::from(value),
            Err(error) => panic!("CARGO_MANIFEST_DIR is not set: {error}"),
        };
        // robot sits at core/robot/ — two levels below the workspace root.
        let workspace_root = match manifest_dir.parent().and_then(|path| path.parent()) {
            Some(path) => path,
            None => panic!(
                "robot CARGO_MANIFEST_DIR must live two levels below the workspace root: {}",
                manifest_dir.display()
            ),
        };
        workspace_root
            .join("fixture")
            .join("robot")
            .join("rgbd-imu-diff-drive")
    }

    fn read_fixture_component(
        bundle_root: &Path,
        component_type: &str,
    ) -> crate::model::component::v1::Component {
        let fixture_root = match bundle_root.parent().and_then(Path::parent) {
            Some(path) => path,
            None => panic!(
                "fixture bundle root must live under fixture/robot: {}",
                bundle_root.display()
            ),
        };
        let component_root = fixture_root.join("component").join(component_type);
        match crate::model::component::Component::read_from_dir(&component_root) {
            Ok(component) => match component.as_v1() {
                Some(component) => component.clone(),
                None => panic!("fixture component {component_type} is not v1"),
            },
            Err(error) => panic!(
                "failed to read fixture component from {}: {error:#}",
                component_root.display()
            ),
        }
    }

    fn component_roles_mut<'a>(
        model: &'a mut Robot,
        component_id: &str,
    ) -> &'a mut BTreeMap<String, Vec<Role>> {
        match model.components.get_mut(component_id) {
            Some(component) => &mut component.roles,
            None => panic!("fixture missing {component_id} component instance"),
        }
    }

    fn add_gnss_localization_component(
        model: &mut Robot,
        components: &mut BTreeMap<String, crate::model::component::v1::Component>,
    ) {
        components.insert(
            "zed_f9p".to_string(),
            crate::model::component::v1::Component::new(BTreeMap::from([(
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
        model.components.insert(
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
