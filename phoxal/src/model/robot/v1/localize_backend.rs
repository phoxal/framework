use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::Robot;

/// Resolved-fact identity of the shipped localization backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalizeBackendKind {
    DeadReckoning,
}

impl LocalizeBackendKind {
    /// Backend family.
    #[must_use]
    pub const fn family(self) -> &'static str {
        match self {
            Self::DeadReckoning => "proprioceptive",
        }
    }

    /// Backend name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::DeadReckoning => "dead_reckoning",
        }
    }

    /// Pinned backend version, part of the resolved localize-backend identity.
    #[must_use]
    pub const fn version(self) -> &'static str {
        match self {
            Self::DeadReckoning => "builtin-v1",
        }
    }
}

/// Backend resolved from the robot model.
///
/// The shipped `localize` runtime currently republishes odometry as map-frame
/// localization with confidence decay. ORB-SLAM3 and GNSS anchoring are not
/// implemented by this workspace, so they must not appear as resolved backend
/// facts until corresponding runtimes and validation exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedLocalizeBackend {
    DeadReckoning,
}

impl ResolvedLocalizeBackend {
    #[must_use]
    pub const fn kind(&self) -> LocalizeBackendKind {
        match self {
            Self::DeadReckoning => LocalizeBackendKind::DeadReckoning,
        }
    }
}

/// Resolve the localization backend implemented by this workspace.
#[must_use]
pub fn resolve_localize_backend(
    _model: &Robot,
    _components: &BTreeMap<String, crate::model::component::v1::Component>,
) -> ResolvedLocalizeBackend {
    ResolvedLocalizeBackend::DeadReckoning
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use crate::model::component::v1::capability::{Capability, Gnss, StructuralTarget};

    use super::{LocalizeBackendKind, ResolvedLocalizeBackend, resolve_localize_backend};
    use crate::model::robot::v1::{Component, Robot, Role};

    #[test]
    fn resolves_dead_reckoning_for_rgbd_imu_roles() {
        let (model, components) = fixture_model_and_components();

        assert_eq!(
            resolve_localize_backend(&model, &components),
            ResolvedLocalizeBackend::DeadReckoning
        );
    }

    #[test]
    fn resolves_dead_reckoning_for_gnss_roles_until_backend_ships() {
        let (mut model, mut components) = fixture_model_and_components();
        add_gnss_localization_component(&mut model, &mut components);

        assert_eq!(
            resolve_localize_backend(&model, &components),
            ResolvedLocalizeBackend::DeadReckoning
        );
    }

    #[test]
    fn kind_descriptor_strings_match_shipped_backend() {
        let dead_reckoning = ResolvedLocalizeBackend::DeadReckoning.kind();
        assert_eq!(dead_reckoning, LocalizeBackendKind::DeadReckoning);
        assert_eq!(dead_reckoning.name(), "dead_reckoning");
        assert_eq!(dead_reckoning.family(), "proprioceptive");
        assert_eq!(dead_reckoning.version(), "builtin-v1");
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
        let workspace_root = match manifest_dir.parent() {
            Some(path) => path,
            None => panic!(
                "phoxal CARGO_MANIFEST_DIR must live one level below the workspace root: {}",
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
