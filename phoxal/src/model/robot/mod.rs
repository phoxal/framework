//! Unversioned canonical robot model.

mod motion;

pub use motion::{KinematicConfig, MotionLimits};

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::model::component::capability::{Capability, Encoder, Motor, StructuralTarget};
use crate::model::component::{CapabilityRef, Component};
use crate::model::simulation::Simulation;
use crate::model::source;
use crate::model::structure::Structure;
use anyhow::{Context, Result, anyhow, bail};

const COMPONENTS_DIR: &str = "components";
const SIMULATION_FILE: &str = "simulation.yaml";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BehaviorConfig {
    pub root: String,
    pub autostart: bool,
}

impl From<source::robot::v0::BehaviorConfig> for BehaviorConfig {
    fn from(value: source::robot::v0::BehaviorConfig) -> Self {
        Self {
            root: value.root,
            autostart: value.autostart,
        }
    }
}

/// One resolved component instance in the canonical robot.
#[derive(Debug, Clone)]
pub struct ComponentInstance {
    pub component_type: String,
    pub mount_link: String,
    direction_signs: BTreeMap<String, i8>,
}

/// Fully loaded runtime-facing model.
///
/// This type contains normalized robot facts, resolved component documents,
/// and the validated structure. It deliberately does not retain a public
/// source-version wrapper.
#[derive(Debug, Clone)]
pub struct Robot {
    id: String,
    namespace: String,
    behavior: Option<BehaviorConfig>,
    kinematic: KinematicConfig,
    motion_limits: MotionLimits,
    component_instances: BTreeMap<String, ComponentInstance>,
    component_types: BTreeMap<String, Component>,
    simulation_types: BTreeMap<String, Simulation>,
    pub structure: Structure,
}

struct ResolvedCapability<'a> {
    capability: &'a Capability,
    direction_sign: i8,
}

#[derive(Debug)]
struct NormalizedRobotSpec {
    id: String,
    namespace: String,
    behavior: Option<BehaviorConfig>,
    kinematic: KinematicConfig,
    motion_limits: MotionLimits,
    components: BTreeMap<String, NormalizedComponentInstance>,
}

#[derive(Debug)]
struct NormalizedComponentInstance {
    component_type: String,
    mount_link: String,
    parameters: BTreeMap<String, NormalizedParameter>,
}

#[derive(Debug)]
struct NormalizedParameter {
    capability_kind: &'static str,
    direction_sign: i8,
}

impl From<source::robot::v0::Manifest> for NormalizedRobotSpec {
    fn from(value: source::robot::v0::Manifest) -> Self {
        let source::robot::v0::Manifest {
            robot, behavior, ..
        } = value;
        Self {
            id: robot.id,
            namespace: robot.namespace,
            behavior: behavior.map(Into::into),
            kinematic: robot.kinematic.into(),
            motion_limits: robot.motion_limits.into(),
            components: robot
                .components
                .into_iter()
                .map(|(component_id, component)| {
                    let parameters = component
                        .parameters
                        .into_iter()
                        .map(|(capability_id, parameter)| {
                            use source::robot::v0::capability::Parameters;
                            let capability_kind = parameter.kind_name();
                            let direction_sign = match parameter {
                                Parameters::Motor(parameter) => parameter.direction_sign,
                                Parameters::Encoder(parameter) => parameter.direction_sign,
                                _ => 1,
                            };
                            (
                                capability_id,
                                NormalizedParameter {
                                    capability_kind,
                                    direction_sign,
                                },
                            )
                        })
                        .collect();
                    (
                        component_id,
                        NormalizedComponentInstance {
                            component_type: component.component,
                            mount_link: component.mount_link,
                            parameters,
                        },
                    )
                })
                .collect(),
        }
    }
}

impl Robot {
    pub fn read_from_dir(path: impl AsRef<Path>) -> Result<Self> {
        let root = path.as_ref();
        let robot_source = source::robot::read_from_dir(root)?;
        let component_sources = Self::read_used_component_sources(root, &robot_source)?;
        let simulation_sources = Self::read_used_simulation_sources(root, &robot_source)?;
        let structure_path = root.join(&robot_source.robot.structure);
        let structure = Structure::read_from_file(&structure_path).with_context(|| {
            format!(
                "failed to read structure declared by robot/v0 document {} at {}",
                robot_source.robot.structure.display(),
                structure_path.display()
            )
        })?;

        Self::try_from_sources(
            robot_source,
            component_sources,
            simulation_sources,
            structure,
        )
    }

    /// Normalize exact source documents and validate their cross-file invariants.
    pub fn try_from_sources(
        robot_source: source::robot::v0::Manifest,
        component_sources: BTreeMap<String, source::component::v0::Manifest>,
        simulation_sources: BTreeMap<String, source::simulation::v0::Manifest>,
        structure: Structure,
    ) -> Result<Self> {
        robot_source.validate().map_err(source_validation_error)?;
        structure
            .validate()
            .context("canonical robot structure validation failed")?;

        let component_types = component_sources
            .into_iter()
            .map(|(component_type, document)| {
                document.validate_for_component(&component_type).with_context(|| {
                    format!(
                        "invalid component/v0 document components/{component_type}/component.yaml"
                    )
                })?;
                Ok((component_type, Component::from(document)))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let simulation_types = simulation_sources
            .into_iter()
            .map(|(component_type, document)| {
                document.validate().with_context(|| {
                    format!(
                        "invalid simulation/v0 document \
                         components/{component_type}/simulation.yaml"
                    )
                })?;
                Ok((component_type, Simulation::from(document)))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;

        Self::try_from_normalized(
            robot_source.into(),
            component_types,
            simulation_types,
            structure,
        )
    }

    fn try_from_normalized(
        robot: NormalizedRobotSpec,
        component_types: BTreeMap<String, Component>,
        simulation_types: BTreeMap<String, Simulation>,
        structure: Structure,
    ) -> Result<Self> {
        let component_instances = robot
            .components
            .into_iter()
            .map(|(component_id, source)| {
                let definition = component_types.get(&source.component_type).ok_or_else(|| {
                    anyhow!(
                        "robot.yaml components.{component_id} references component type '{}' \
                         without components/{}/component.yaml",
                        source.component_type,
                        source.component_type
                    )
                })?;
                let mut direction_signs = BTreeMap::new();
                for (capability_id, parameters) in source.parameters {
                    let capability = definition.capability(&capability_id).ok_or_else(|| {
                        anyhow!(
                            "robot.yaml components.{component_id}.parameters.{capability_id} \
                             references a capability missing from \
                             components/{}/component.yaml",
                            source.component_type
                        )
                    })?;
                    if capability.kind_name() != parameters.capability_kind {
                        bail!(
                            "robot.yaml components.{component_id}.parameters.{capability_id} \
                             has kind '{}', but components/{}/component.yaml defines '{}'",
                            parameters.capability_kind,
                            source.component_type,
                            capability.kind_name()
                        );
                    }
                    direction_signs.insert(capability_id, parameters.direction_sign);
                }
                Ok((
                    component_id,
                    ComponentInstance {
                        component_type: source.component_type,
                        mount_link: source.mount_link,
                        direction_signs,
                    },
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;

        let canonical = Self {
            id: robot.id,
            namespace: robot.namespace,
            behavior: robot.behavior,
            kinematic: robot.kinematic,
            motion_limits: robot.motion_limits,
            component_instances,
            component_types,
            simulation_types,
            structure,
        };
        canonical.validate_cross_document_invariants()?;
        Ok(canonical)
    }

    fn read_used_component_sources(
        root: &Path,
        robot: &source::robot::v0::Manifest,
    ) -> Result<BTreeMap<String, source::component::v0::Manifest>> {
        robot
            .used_component_types()
            .into_iter()
            .map(|component_type| {
                let component_root = component_config_path(root, component_type);
                let document = source::component::read_from_dir(&component_root).with_context(|| {
                    format!(
                        "failed to load component type '{component_type}' referenced by robot.yaml"
                    )
                })?;
                Ok((component_type.to_string(), document))
            })
            .collect()
    }

    fn read_used_simulation_sources(
        root: &Path,
        robot: &source::robot::v0::Manifest,
    ) -> Result<BTreeMap<String, source::simulation::v0::Manifest>> {
        robot
            .used_component_types()
            .into_iter()
            .filter_map(|component_type| {
                let component_root = component_config_path(root, component_type);
                component_root
                    .join(SIMULATION_FILE)
                    .is_file()
                    .then_some((component_type, component_root))
            })
            .map(|(component_type, component_root)| {
                let document =
                    source::simulation::read_from_dir(&component_root).with_context(|| {
                        format!(
                            "failed to load simulation/v0 document for component type \
                             '{component_type}' referenced by robot.yaml"
                        )
                    })?;
                Ok((component_type.to_string(), document))
            })
            .collect()
    }

    fn validate_cross_document_invariants(&self) -> Result<()> {
        for (component_id, instance) in &self.component_instances {
            if self.structure.link(&instance.mount_link).is_none() {
                bail!(
                    "robot.yaml components.{component_id}.mount_link '{}' is missing from the \
                     canonical structure",
                    instance.mount_link
                );
            }
        }

        for (component_type, simulation) in &self.simulation_types {
            let component = self.component_types.get(component_type).ok_or_else(|| {
                anyhow!(
                    "components/{component_type}/simulation.yaml has no matching \
                     component.yaml loaded by robot.yaml"
                )
            })?;
            for (capability_id, simulation_capability) in &simulation.capabilities {
                let capability = component.capability(capability_id).ok_or_else(|| {
                    anyhow!(
                        "components/{component_type}/simulation.yaml \
                         capabilities.{capability_id} has no matching capability in \
                         components/{component_type}/component.yaml"
                    )
                })?;
                if simulation_capability.kind_name() != capability.kind_name() {
                    bail!(
                        "components/{component_type}/simulation.yaml \
                         capabilities.{capability_id} has kind '{}', but \
                         components/{component_type}/component.yaml defines '{}'",
                        simulation_capability.kind_name(),
                        capability.kind_name()
                    );
                }
            }
        }

        match &self.kinematic {
            KinematicConfig::Differential {
                left_actuators,
                right_actuators,
                left_encoders,
                right_encoders,
                ..
            } => {
                for reference in left_actuators.iter().chain(right_actuators) {
                    self.require_motor(reference).with_context(|| {
                        format!("robot.yaml kinematic actuator '{reference}' is invalid")
                    })?;
                }
                for reference in left_encoders.iter().chain(right_encoders) {
                    self.require_encoder(reference).with_context(|| {
                        format!("robot.yaml kinematic encoder '{reference}' is invalid")
                    })?;
                }
            }
            KinematicConfig::Mecanum {
                front_left_actuator,
                front_right_actuator,
                rear_left_actuator,
                rear_right_actuator,
                ..
            } => {
                for reference in [
                    front_left_actuator,
                    front_right_actuator,
                    rear_left_actuator,
                    rear_right_actuator,
                ] {
                    self.require_motor(reference).with_context(|| {
                        format!("robot.yaml kinematic actuator '{reference}' is invalid")
                    })?;
                }
            }
            KinematicConfig::Ackermann {
                steering_actuator,
                drive_actuator,
                steering_encoder,
                drive_encoder,
                ..
            } => {
                for reference in [steering_actuator, drive_actuator] {
                    self.require_motor(reference).with_context(|| {
                        format!("robot.yaml kinematic actuator '{reference}' is invalid")
                    })?;
                }
                for reference in steering_encoder.iter().chain(drive_encoder) {
                    self.require_encoder(reference).with_context(|| {
                        format!("robot.yaml kinematic encoder '{reference}' is invalid")
                    })?;
                }
            }
            KinematicConfig::Omnidirectional {
                actuators,
                encoders,
            } => {
                for reference in actuators {
                    self.require_motor(reference).with_context(|| {
                        format!("robot.yaml kinematic actuator '{reference}' is invalid")
                    })?;
                }
                for reference in encoders {
                    self.require_encoder(reference).with_context(|| {
                        format!("robot.yaml kinematic encoder '{reference}' is invalid")
                    })?;
                }
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn robot_id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    #[must_use]
    pub fn behavior(&self) -> Option<&BehaviorConfig> {
        self.behavior.as_ref()
    }

    #[must_use]
    pub fn kinematic(&self) -> &KinematicConfig {
        &self.kinematic
    }

    #[must_use]
    pub const fn motion_limits(&self) -> MotionLimits {
        self.motion_limits
    }

    #[must_use]
    pub fn components(&self) -> &BTreeMap<String, ComponentInstance> {
        &self.component_instances
    }

    #[must_use]
    pub fn component_types(&self) -> BTreeSet<&str> {
        self.component_instances
            .values()
            .map(|component| component.component_type.as_str())
            .collect()
    }

    pub fn component_instance(&self, component_id: &str) -> Result<&ComponentInstance> {
        self.component_instances.get(component_id).ok_or_else(|| {
            anyhow!("component instance '{component_id}' is not defined in robot.yaml")
        })
    }

    pub fn component_for_instance(&self, component_id: &str) -> Result<&Component> {
        let instance = self.component_instance(component_id)?;
        self.component_types
            .get(&instance.component_type)
            .ok_or_else(|| {
                anyhow!(
                    "component type '{}' for instance '{component_id}' is not loaded",
                    instance.component_type
                )
            })
    }

    #[must_use]
    pub fn simulation_for_component_type(&self, component_type: &str) -> Option<&Simulation> {
        self.simulation_types.get(component_type)
    }

    pub fn simulation_for_instance(&self, component_id: &str) -> Result<Option<&Simulation>> {
        let instance = self.component_instance(component_id)?;
        Ok(self.simulation_for_component_type(&instance.component_type))
    }

    pub fn capability(&self, reference: &CapabilityRef) -> Result<&Capability> {
        self.component_for_instance(&reference.component_id)?
            .capability(&reference.capability_id)
            .ok_or_else(|| {
                anyhow!("capability '{reference}' is not defined in its component.yaml document")
            })
    }

    #[must_use]
    pub fn camera_capabilities(&self) -> Vec<CapabilityRef> {
        let mut capabilities = self
            .component_instances
            .keys()
            .flat_map(|component_id| {
                self.component_for_instance(component_id)
                    .into_iter()
                    .flat_map(move |component| {
                        component
                            .capabilities
                            .iter()
                            .filter(|(_, capability)| matches!(capability, Capability::Camera(_)))
                            .map(move |(capability_id, _)| {
                                CapabilityRef::new(component_id, capability_id)
                            })
                    })
            })
            .collect::<Vec<_>>();
        capabilities.sort();
        capabilities
    }

    fn resolved_capability(&self, reference: &CapabilityRef) -> Result<ResolvedCapability<'_>> {
        Ok(ResolvedCapability {
            capability: self.capability(reference)?,
            direction_sign: self
                .component_instance(&reference.component_id)?
                .direction_signs
                .get(&reference.capability_id)
                .copied()
                .unwrap_or(1),
        })
    }

    pub fn require_motor(&self, reference: &CapabilityRef) -> Result<(&Motor, i8)> {
        let resolved = self.resolved_capability(reference)?;
        let Capability::Motor(motor) = resolved.capability else {
            bail!(
                "capability '{reference}' must reference a motor, found {}",
                resolved.capability.kind_name()
            );
        };
        Ok((motor, resolved.direction_sign))
    }

    pub fn require_encoder(&self, reference: &CapabilityRef) -> Result<(&Encoder, i8)> {
        let resolved = self.resolved_capability(reference)?;
        let Capability::Encoder(encoder) = resolved.capability else {
            bail!(
                "capability '{reference}' must reference an encoder, found {}",
                resolved.capability.kind_name()
            );
        };
        Ok((encoder, resolved.direction_sign))
    }

    pub fn require_link_target(&self, reference: &CapabilityRef) -> Result<String> {
        let target = self
            .capability(reference)?
            .target()
            .namespaced(&reference.component_id);
        let StructuralTarget::Link { id } = target else {
            bail!("capability '{reference}' must target a link");
        };
        if self.structure.link(&id).is_some() {
            Ok(id)
        } else {
            bail!("link target '{id}' for capability '{reference}' not found in structure")
        }
    }

    pub fn component_mount_link(&self, component_id: &str) -> Result<String> {
        Ok(self.component_instance(component_id)?.mount_link.clone())
    }
}

fn source_validation_error(errors: Vec<source::robot::v0::ValidationError>) -> anyhow::Error {
    anyhow!(
        "Robot errors:\n{}",
        errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    )
}

fn component_config_path(bundle_root: &Path, component_type: &str) -> PathBuf {
    bundle_root.join(COMPONENTS_DIR).join(component_type)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use anyhow::Context as _;

    use crate::model::component::Component;
    use crate::model::structure::Structure;

    use super::{
        KinematicConfig, MotionLimits, NormalizedComponentInstance, NormalizedRobotSpec, Robot,
    };

    #[test]
    fn v0_sources_build_the_canonical_robot() -> anyhow::Result<()> {
        let robot = Robot::read_from_dir(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../fixture/robot/rgbd-imu-diff-drive"
        ))?;
        assert_eq!(robot.robot_id(), "rgbd-imu-diff-drive");
        assert!(!robot.components().is_empty());
        assert!(
            robot
                .simulation_for_component_type("drive_motor")
                .is_some_and(|simulation| simulation.capabilities.contains_key("encoder"))
        );
        Ok(())
    }

    #[test]
    fn every_repository_robot_document_builds_the_canonical_model() -> anyhow::Result<()> {
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let roots = [
            "../fixture/robot/rgbd-diff-drive",
            "../fixture/robot/rgbd-imu-diff-drive",
            "../fixture/robot/rgbd-imu-gnss-outdoor",
            "../fixture/robot/rgbd-imu-orb-lowres",
            "../examples/hello-rover",
        ];

        for root in roots {
            let root = manifest_dir.join(root);
            Robot::read_from_dir(&root)
                .with_context(|| format!("failed to load repository robot {}", root.display()))?;
        }
        Ok(())
    }

    #[test]
    fn cross_file_errors_retain_authored_context() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        std::fs::write(
            temp.path().join("robot.yaml"),
            r#"
schema: robot/v0
robot:
  id: broken
  namespace: test
  kinematic: { kind: omnidirectional, actuators: [drive.motor], encoders: [] }
  motion_limits: { max_linear_speed_mps: 1.0, max_angular_speed_radps: 1.0 }
  components:
    drive:
      component: missing
      mount_link: base_link
"#,
        )?;
        let error = Robot::read_from_dir(temp.path()).expect_err("missing component must fail");
        let message = format!("{error:#}");
        assert!(message.contains("robot.yaml"));
        assert!(message.contains("components/missing/component.yaml"));
        Ok(())
    }

    #[test]
    fn future_robot_version_can_normalize_with_component_v0() -> anyhow::Result<()> {
        struct FutureRobotV1 {
            id: String,
        }

        impl From<FutureRobotV1> for NormalizedRobotSpec {
            fn from(value: FutureRobotV1) -> Self {
                Self {
                    id: value.id,
                    namespace: "future".to_string(),
                    behavior: None,
                    kinematic: KinematicConfig::Omnidirectional {
                        actuators: Vec::new(),
                        encoders: Vec::new(),
                    },
                    motion_limits: MotionLimits {
                        max_linear_speed_mps: 1.0,
                        max_angular_speed_radps: 1.0,
                    },
                    components: BTreeMap::from([(
                        "sensor".to_string(),
                        NormalizedComponentInstance {
                            component_type: "sensor_v0".to_string(),
                            mount_link: "base_link".to_string(),
                            parameters: BTreeMap::new(),
                        },
                    )]),
                }
            }
        }

        let component_v0 = crate::model::source::component::read_from_string(
            "schema: component/v0\ncapabilities: {}\n",
        )?;
        let structure = Structure::from_urdf_str(
            r#"<robot name="future">
  <link name="base_footprint" />
  <link name="base_link" />
  <joint name="base" type="fixed">
    <parent link="base_footprint" />
    <child link="base_link" />
  </joint>
</robot>"#,
        )?;

        let robot = Robot::try_from_normalized(
            FutureRobotV1 {
                id: "future-v1".to_string(),
            }
            .into(),
            BTreeMap::from([("sensor_v0".to_string(), Component::from(component_v0))]),
            BTreeMap::new(),
            structure,
        )?;
        assert_eq!(robot.robot_id(), "future-v1");
        assert_eq!(
            robot.component_instance("sensor")?.component_type,
            "sensor_v0"
        );
        Ok(())
    }
}
