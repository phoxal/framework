use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::model::component::v1::capability::{Capability, Encoder, Imu, Motor, StructuralTarget};
use crate::model::component::v1::{CapabilityRef, Component as ComponentSpec};
use crate::model::robot::v1::capability::Parameters;
use crate::model::robot::v1::{
    self as robot_v1, ArtifactPin, ResolvedFacts, SourceBundle, resolve_source_bundle,
};
use crate::model::structure::Structure;
use anyhow::{Context, Result, anyhow, bail};

const COMPONENTS_DIR: &str = "components";
const COMPONENT_FILE: &str = "component.yaml";

/// Complete authored robot bundle: manifest, used component specs, and structure.
#[derive(Debug, Clone)]
pub struct Robot {
    pub manifest: crate::model::robot::v1::Robot,
    pub components: BTreeMap<String, ComponentSpec>,
    pub structure: Structure,
}

struct ResolvedCapability<'a> {
    capability: &'a Capability,
    parameters: Option<&'a Parameters>,
}

pub struct DriverBinding<'a> {
    pub component_id: String,
    pub component: &'a ComponentSpec,
    pub component_instance: &'a robot_v1::Component,
    pub driver: &'a robot_v1::DriverConfig,
}

impl Robot {
    pub fn read_from_dir(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let manifest = Self::read_manifest(path)?;
        let components = Self::read_used_component_configs(path, &manifest)?;
        let structure = Self::read_structure(path, &manifest)?;
        Ok(Self {
            manifest,
            components,
            structure,
        })
    }

    pub fn resolve(&self) -> Result<ResolvedFacts> {
        resolve_source_bundle(SourceBundle::new(
            self.manifest.clone(),
            self.components.clone(),
        ))
    }

    fn read_manifest(path: impl AsRef<Path>) -> Result<crate::model::robot::v1::Robot> {
        crate::model::robot::v1::Robot::read_from_dir(path)
    }

    fn read_structure(
        path: impl AsRef<Path>,
        manifest: &crate::model::robot::v1::Robot,
    ) -> Result<Structure> {
        let path = path.as_ref();
        let structure_path = path.join(&manifest.robot.structure);
        let structure = Structure::read_from_file(&structure_path).with_context(|| {
            format!(
                "failed to read structure declared by robot.yaml structure: {}",
                manifest.robot.structure.display()
            )
        })?;
        structure.validate()?;
        Ok(structure)
    }

    fn read_component_config(
        path: impl AsRef<Path>,
        manifest: &crate::model::robot::v1::Robot,
        component_type: &str,
    ) -> Result<ComponentSpec> {
        let component_path = component_config_path(path.as_ref(), manifest, component_type);
        Ok(
            crate::model::component::Component::read_from_dir(&component_path)
                .with_context(|| {
                    format!(
                        "failed to read component configuration for '{}' from {}",
                        component_type,
                        component_path.display()
                    )
                })?
                .as_v1()
                .context("robot bundle only supports component.yaml version v1")?
                .clone(),
        )
    }

    fn read_used_component_configs(
        path: impl AsRef<Path>,
        manifest: &crate::model::robot::v1::Robot,
    ) -> Result<BTreeMap<String, ComponentSpec>> {
        manifest
            .used_component_types()
            .into_iter()
            .map(|component_type| {
                Ok((
                    component_type.to_string(),
                    Self::read_component_config(path.as_ref(), manifest, component_type)?,
                ))
            })
            .collect()
    }

    pub fn component_instance(&self, component_id: &str) -> Result<&robot_v1::Component> {
        self.manifest
            .component_instance(component_id)
            .ok_or_else(|| {
                anyhow!(
                    "component instance '{}' is not defined in robot.yaml",
                    component_id
                )
            })
    }

    pub fn component_for_instance(&self, component_id: &str) -> Result<&ComponentSpec> {
        let manifest_component = self.component_instance(component_id)?;
        self.components
            .get(&manifest_component.component)
            .ok_or_else(|| {
                anyhow!(
                    "component type '{}' for instance '{}' is not loaded",
                    manifest_component.component,
                    component_id
                )
            })
    }

    pub fn capability(&self, capability_ref: &CapabilityRef) -> Result<&Capability> {
        self.component_for_instance(&capability_ref.component_id)?
            .capability(&capability_ref.capability_id)
            .ok_or_else(|| {
                anyhow!(
                    "capability '{}' is not defined in component.yaml",
                    capability_ref
                )
            })
    }

    #[must_use]
    pub fn camera_capabilities(&self) -> Vec<CapabilityRef> {
        let mut capabilities = self
            .manifest
            .components()
            .iter()
            .filter_map(|(component_id, component_instance)| {
                self.components
                    .get(&component_instance.component)
                    .map(|component| (component_id, component))
            })
            .flat_map(|(component_id, component)| {
                component
                    .capabilities
                    .iter()
                    .filter(|(_, capability)| matches!(capability, Capability::Camera(_)))
                    .map(move |(capability_id, _)| CapabilityRef::new(component_id, capability_id))
            })
            .collect::<Vec<_>>();
        capabilities.sort();
        capabilities
    }

    fn resolved_capability(&self, reference: &CapabilityRef) -> Result<ResolvedCapability<'_>> {
        Ok(ResolvedCapability {
            capability: self.capability(reference)?,
            parameters: self.manifest.parameter(reference),
        })
    }

    pub fn require_motor(&self, reference: &CapabilityRef) -> Result<(&Motor, i8)> {
        let resolved = self.resolved_capability(reference)?;
        let Capability::Motor(motor) = resolved.capability else {
            bail!(
                "capability '{}' must reference a motor, found {}",
                reference,
                resolved.capability.kind_name()
            );
        };
        let direction_sign = match resolved.parameters {
            Some(Parameters::Motor(parameters)) => parameters.direction_sign,
            Some(parameters) => bail!(
                "capability '{}' parameters must match motor kind, found {}",
                reference,
                parameters.kind_name()
            ),
            None => 1,
        };
        Ok((motor, direction_sign))
    }

    pub fn require_encoder(&self, reference: &CapabilityRef) -> Result<(&Encoder, i8)> {
        let resolved = self.resolved_capability(reference)?;
        let Capability::Encoder(encoder) = resolved.capability else {
            bail!(
                "capability '{}' must reference an encoder, found {}",
                reference,
                resolved.capability.kind_name()
            );
        };
        let direction_sign = match resolved.parameters {
            Some(Parameters::Encoder(parameters)) => parameters.direction_sign,
            Some(parameters) => bail!(
                "capability '{}' parameters must match encoder kind, found {}",
                reference,
                parameters.kind_name()
            ),
            None => 1,
        };
        Ok((encoder, direction_sign))
    }

    pub fn require_imu(&self, reference: &CapabilityRef) -> Result<&Imu> {
        let resolved = self.resolved_capability(reference)?;
        let Capability::Imu(imu) = resolved.capability else {
            bail!(
                "capability '{}' must reference an IMU, found {}",
                reference,
                resolved.capability.kind_name()
            );
        };
        if let Some(parameters) = resolved.parameters
            && !matches!(parameters, Parameters::Imu(_))
        {
            bail!(
                "capability '{}' parameters must match IMU kind, found {}",
                reference,
                parameters.kind_name()
            );
        }
        Ok(imu)
    }

    pub fn require_joint(&self, reference: &CapabilityRef) -> Result<&urdf_rs::Joint> {
        let target = self
            .capability(reference)?
            .target()
            .namespaced(&reference.component_id);
        let StructuralTarget::Joint { id } = target else {
            bail!("capability '{}' must target a joint", reference);
        };
        self.structure.joint(&id).ok_or_else(|| {
            anyhow!(
                "joint target '{}' for capability '{}' not found in structure",
                id,
                reference
            )
        })
    }

    pub fn require_link_target(&self, reference: &CapabilityRef) -> Result<String> {
        let target = self
            .capability(reference)?
            .target()
            .namespaced(&reference.component_id);
        let StructuralTarget::Link { id } = target else {
            bail!("capability '{}' must target a link", reference);
        };
        if self.structure.link(&id).is_some() {
            Ok(id)
        } else {
            bail!(
                "link target '{}' for capability '{}' not found in structure",
                id,
                reference
            )
        }
    }

    pub fn component_mount_link(&self, component_id: &str) -> Result<String> {
        let mount_link = self.component_instance(component_id)?.mount_link.clone();
        if self.structure.link(&mount_link).is_some() {
            Ok(mount_link)
        } else {
            bail!(
                "mount link '{}' for component '{}' not found in structure",
                mount_link,
                component_id
            )
        }
    }

    pub fn driver_binding(&self, component_id: &str) -> Result<DriverBinding<'_>> {
        let component_instance = self.component_instance(component_id)?;
        let driver = component_instance.driver.as_ref().ok_or_else(|| {
            anyhow!(
                "component '{}' has no driver config in robot.yaml",
                component_id
            )
        })?;
        Ok(DriverBinding {
            component_id: component_id.to_string(),
            component: self.component_for_instance(component_id)?,
            component_instance,
            driver,
        })
    }
}

/// Resolves the on-disk directory for a used component type.
///
/// The default is the staged `<bundle_root>/components/<type>` directory that
/// `phoxal-cli` populates for official/resolved components. A local/forked
/// component overrides this via an `artifacts.pins` path pin keyed by its
/// provider-qualified assets package id (`phoxal/component-<type>-assets`);
/// the override is only honored when it actually contains a `component.yaml`,
/// otherwise resolution falls back to the staged path.
fn component_config_path(
    bundle_root: &Path,
    manifest: &crate::model::robot::v1::Robot,
    component_type: &str,
) -> PathBuf {
    let staged_path = bundle_root.join(COMPONENTS_DIR).join(component_type);
    let assets_pin_key = format!("phoxal/component-{component_type}-assets");
    let Some(ArtifactPin::Path(path_pin)) = manifest.artifacts.pins.get(&assets_pin_key) else {
        return staged_path;
    };

    let source_path = bundle_root.join(&path_pin.path);
    if source_path.join(COMPONENT_FILE).is_file() {
        source_path
    } else {
        staged_path
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::model::component::v1::Component as ComponentSpec;
    use crate::model::component::v1::capability::{
        Camera, CameraMode, Capability, Depth, StructuralTarget,
    };
    use crate::model::structure::Structure;

    use super::*;

    #[test]
    fn camera_capabilities_lists_color_cameras_not_depth() {
        let manifest = Robot::read_manifest(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../fixture/robot/rgbd-imu-diff-drive"
        ))
        .expect("fixture manifest should load");
        let robot = Robot {
            manifest,
            components: BTreeMap::from([(
                "camera_rgbd_640x480".to_string(),
                ComponentSpec {
                    gtin: None,
                    capabilities: BTreeMap::from([
                        (
                            "rgb".to_string(),
                            Capability::Camera(Camera {
                                target: link_target(),
                                mode: CameraMode::Rgb,
                                publish_rate_hz: 30.0,
                                width_px: 640,
                                height_px: 480,
                                field_of_view_rad: None,
                            }),
                        ),
                        (
                            "depth".to_string(),
                            Capability::Depth(Depth {
                                target: link_target(),
                                publish_rate_hz: 30.0,
                                width_px: 640,
                                height_px: 480,
                                field_of_view_rad: None,
                                min_range_m: None,
                                max_range_m: None,
                            }),
                        ),
                    ]),
                },
            )]),
            structure: empty_structure(),
        };

        assert_eq!(
            robot.camera_capabilities(),
            vec![CapabilityRef::new("front_camera", "rgb")]
        );
    }

    #[test]
    fn read_from_dir_honors_manifest_structure_path() -> Result<()> {
        let robot = Robot::read_from_dir(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../fixture/robot/rgbd-imu-diff-drive"
        ))?;

        assert_eq!(robot.structure.root_link_name()?, "base_footprint");
        assert!(robot.components.contains_key("drive_motor"));
        Ok(())
    }

    fn link_target() -> StructuralTarget {
        StructuralTarget::Link {
            id: "sensor_link".to_string(),
        }
    }

    fn empty_structure() -> Structure {
        Structure::from_urdf_str(
            r#"<robot name="test-bot">
  <link name="base_footprint" />
  <link name="base_link" />
  <joint name="root" type="fixed">
    <parent link="base_footprint" />
    <child link="base_link" />
    <origin xyz="0 0 0" rpy="0 0 0" />
  </joint>
</robot>
"#,
        )
        .expect("test structure should parse")
    }
}
