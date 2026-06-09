use std::collections::BTreeMap;
use std::path::Path;

use crate::model::component::v1::CapabilityRef;
use crate::model::component::v1::capability::{Capability, Encoder, Motor, StructuralTarget};
use crate::model::robot::v1::Robot as RobotManifest;
use crate::model::robot::v1::capability::Parameters;
use crate::model::robot::v1::{
    self as model_v1, ResolvedFacts, SourceBundle, resolve_source_bundle,
};
use crate::model::structure::Structure;
use crate::runtime::conventions::COMPONENTS_DIR;
use anyhow::{Context, Result, anyhow, bail};

#[derive(Debug, Clone)]
pub struct Robot {
    pub model: RobotManifest,
    pub components: BTreeMap<String, crate::model::component::v1::Component>,
}

struct ResolvedCapability<'a> {
    reference: CapabilityRef,
    capability: &'a Capability,
    parameters: Option<&'a Parameters>,
}

pub struct ResolvedMotor<'a> {
    pub reference: CapabilityRef,
    pub motor: &'a Motor,
    pub direction_sign: i8,
    pub gear_ratio: f64,
}

pub struct ResolvedEncoder<'a> {
    pub reference: CapabilityRef,
    pub encoder: &'a Encoder,
    pub direction_sign: i8,
    pub gear_ratio: f64,
    pub counts_per_revolution: u32,
}

pub struct ResolvedImu {
    pub reference: CapabilityRef,
}

pub struct DriverBinding<'a> {
    pub component_id: String,
    pub component: &'a crate::model::component::v1::Component,
    pub component_instance: &'a model_v1::Component,
    pub driver: &'a model_v1::DriverConfig,
}

impl Robot {
    pub fn read_from_dir(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let model = Self::read_model_config(path)?;
        let components = Self::read_used_component_configs(path, &model)?;
        Ok(Self { model, components })
    }

    pub fn resolve(&self) -> Result<ResolvedFacts> {
        resolve_source_bundle(SourceBundle::new(
            self.model.clone(),
            self.components.clone(),
        ))
    }

    fn read_model_config(path: impl AsRef<Path>) -> Result<RobotManifest> {
        crate::model::robot::v1::Robot::read_from_dir(path)
    }

    fn read_component_config(
        path: impl AsRef<Path>,
        component_type: &str,
    ) -> Result<crate::model::component::v1::Component> {
        let component_path = path.as_ref().join(COMPONENTS_DIR).join(component_type);
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
                .context("staged robot only supports component.yaml version v1")?
                .clone(),
        )
    }

    fn read_used_component_configs(
        path: impl AsRef<Path>,
        model: &RobotManifest,
    ) -> Result<BTreeMap<String, crate::model::component::v1::Component>> {
        model
            .used_component_types()
            .into_iter()
            .map(|component_type| {
                Ok((
                    component_type.to_string(),
                    Self::read_component_config(path.as_ref(), component_type)?,
                ))
            })
            .collect()
    }

    pub fn component_instance(&self, component_id: &str) -> Result<&model_v1::Component> {
        self.model.component_instance(component_id).ok_or_else(|| {
            anyhow!(
                "component instance '{}' is not defined in robot.yaml",
                component_id
            )
        })
    }

    pub fn component_for_instance(
        &self,
        component_id: &str,
    ) -> Result<&crate::model::component::v1::Component> {
        let model_component = self.component_instance(component_id)?;
        self.components
            .get(&model_component.component)
            .ok_or_else(|| {
                anyhow!(
                    "component type '{}' for instance '{}' is not staged",
                    model_component.component,
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
            .model
            .components
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

    pub fn parameters(
        &self,
        capability_ref: &CapabilityRef,
    ) -> Option<&crate::model::robot::v1::capability::Parameters> {
        self.model.parameter(capability_ref)
    }

    fn resolved_capability(&self, reference: &CapabilityRef) -> Result<ResolvedCapability<'_>> {
        Ok(ResolvedCapability {
            reference: reference.clone(),
            capability: self.capability(reference)?,
            parameters: self.parameters(reference),
        })
    }

    pub fn require_motor(&self, reference: &CapabilityRef) -> Result<ResolvedMotor<'_>> {
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
        validate_direction_sign(direction_sign, reference)?;
        validate_positive_f64(motor.gear_ratio, "gear_ratio", reference)?;

        Ok(ResolvedMotor {
            reference: resolved.reference,
            motor,
            direction_sign,
            gear_ratio: motor.gear_ratio,
        })
    }

    pub fn require_encoder(&self, reference: &CapabilityRef) -> Result<ResolvedEncoder<'_>> {
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
        validate_direction_sign(direction_sign, reference)?;
        validate_positive_f64(encoder.gear_ratio, "gear_ratio", reference)?;
        if encoder.counts_per_revolution == 0 {
            bail!(
                "capability '{}' counts_per_revolution must be > 0",
                reference
            );
        }

        Ok(ResolvedEncoder {
            reference: resolved.reference,
            encoder,
            direction_sign,
            gear_ratio: encoder.gear_ratio,
            counts_per_revolution: encoder.counts_per_revolution,
        })
    }

    pub fn require_imu(&self, reference: &CapabilityRef) -> Result<ResolvedImu> {
        let resolved = self.resolved_capability(reference)?;
        let Capability::Imu(_) = resolved.capability else {
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
        Ok(ResolvedImu {
            reference: resolved.reference,
        })
    }

    pub fn require_joint_target(
        &self,
        reference: &CapabilityRef,
        structure: &Structure,
    ) -> Result<String> {
        Ok(self.require_joint(reference, structure)?.name.clone())
    }

    pub fn require_joint<'a>(
        &self,
        reference: &CapabilityRef,
        structure: &'a Structure,
    ) -> Result<&'a urdf_rs::Joint> {
        let target = self
            .capability(reference)?
            .target()
            .namespaced(&reference.component_id);
        let StructuralTarget::Joint { id } = target else {
            bail!("capability '{}' must target a joint", reference);
        };
        structure.joint(&id).ok_or_else(|| {
            anyhow!(
                "joint target '{}' for capability '{}' not found in structure.urdf",
                id,
                reference
            )
        })
    }

    pub fn require_link_target(
        &self,
        reference: &CapabilityRef,
        structure: &Structure,
    ) -> Result<String> {
        let target = self
            .capability(reference)?
            .target()
            .namespaced(&reference.component_id);
        let StructuralTarget::Link { id } = target else {
            bail!("capability '{}' must target a link", reference);
        };
        if structure.link(&id).is_some() {
            Ok(id)
        } else {
            bail!(
                "link target '{}' for capability '{}' not found in structure.urdf",
                id,
                reference
            )
        }
    }

    pub fn component_mount_link(
        &self,
        component_id: &str,
        structure: &Structure,
    ) -> Result<String> {
        let mount_link = self.component_instance(component_id)?.mount_link.clone();
        if structure.link(&mount_link).is_some() {
            Ok(mount_link)
        } else {
            bail!(
                "mount link '{}' for component '{}' not found in structure.urdf",
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

fn validate_direction_sign(direction_sign: i8, reference: &CapabilityRef) -> Result<()> {
    if direction_sign == -1 || direction_sign == 1 {
        Ok(())
    } else {
        bail!(
            "capability '{}' direction_sign must be either -1 or 1",
            reference
        )
    }
}

fn validate_positive_f64(value: f64, field: &str, reference: &CapabilityRef) -> Result<()> {
    if value.is_finite() && value > f64::EPSILON {
        Ok(())
    } else {
        bail!("capability '{}' {field} must be > 0", reference)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::model::component::v1::Component as ComponentSpec;
    use crate::model::component::v1::capability::{Camera, CameraMode, Depth};

    use super::*;

    #[test]
    fn camera_capabilities_lists_color_cameras_not_depth() {
        let model = Robot::read_model_config(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../fixture/robot/rgbd-imu-diff-drive"
        ))
        .expect("fixture model should load");
        let robot = Robot {
            model,
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
        };

        assert_eq!(
            robot.camera_capabilities(),
            vec![CapabilityRef::new("front_camera", "rgb")]
        );
    }

    fn link_target() -> StructuralTarget {
        StructuralTarget::Link {
            id: "sensor_link".to_string(),
        }
    }
}
