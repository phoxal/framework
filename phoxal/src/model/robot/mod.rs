//! Unversioned canonical robot model.

mod motion;

pub use motion::{KinematicConfig, MotionLimits};

use std::collections::BTreeMap;
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
/// source-version wrapper. Construction is intentionally fail-fast across the
/// complete declared bundle: every present `simulation.yaml` is part of the
/// fully loaded model and is validated even when the current participant runs
/// against physical hardware.
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
    structure: Structure,
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

/// Exact documents and structure accepted by [`Robot::try_from_sources`].
///
/// The source versions are named independently in the tuple; this is a
/// version-sensitive tooling seam, not a version of the canonical graph.
pub type SourceInputs = (
    source::robot::v0::Manifest,
    BTreeMap<String, source::component::v0::Manifest>,
    BTreeMap<String, source::simulation::v0::Manifest>,
    Structure,
);

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
        let (robot_source, component_sources, simulation_sources, structure) =
            Self::read_sources_from_dir(path)?;
        Self::try_from_sources(
            robot_source,
            component_sources,
            simulation_sources,
            structure,
        )
    }

    /// Load the exact source documents and structure used to build a robot.
    ///
    /// This version-sensitive seam is intended for fixtures and migration
    /// tooling that must adjust exact documents before normalizing them
    /// explicitly through [`Self::try_from_sources`]. Ordinary runtime
    /// consumers should call [`Self::read_from_dir`].
    pub fn read_sources_from_dir(path: impl AsRef<Path>) -> Result<SourceInputs> {
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

        Ok((
            robot_source,
            component_sources,
            simulation_sources,
            structure,
        ))
    }

    /// Normalize exact source documents and validate their cross-file invariants.
    ///
    /// Source readers already perform file-local validation. This method
    /// deliberately repeats it because callers may construct or modify exact
    /// DTOs directly before crossing the canonicalization boundary.
    pub fn try_from_sources(
        robot_source: source::robot::v0::Manifest,
        component_sources: BTreeMap<String, source::component::v0::Manifest>,
        simulation_sources: BTreeMap<String, source::simulation::v0::Manifest>,
        structure: Structure,
    ) -> Result<Self> {
        robot_source.validate().map_err(|errors| {
            source::robot::validation_error("robot.yaml passed to Robot::try_from_sources", errors)
        })?;
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

    #[must_use]
    pub fn structure(&self) -> &Structure {
        &self.structure
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
        SourceInputs,
    };

    fn fixture_root() -> &'static str {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../fixture/robot/rgbd-imu-diff-drive"
        )
    }

    fn fixture_inputs() -> anyhow::Result<SourceInputs> {
        Robot::read_sources_from_dir(fixture_root())
    }

    fn canonicalization_error(inputs: SourceInputs) -> String {
        let (robot, components, simulations, structure) = inputs;
        format!(
            "{:#}",
            Robot::try_from_sources(robot, components, simulations, structure)
                .expect_err("mutated sources must fail")
        )
    }

    #[test]
    fn v0_sources_build_the_canonical_robot() -> anyhow::Result<()> {
        let robot = Robot::read_from_dir(fixture_root())?;
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
    fn parameter_capability_must_exist_in_component_source() -> anyhow::Result<()> {
        let (mut robot, components, simulations, structure) = fixture_inputs()?;
        let drive = robot
            .robot
            .components
            .get_mut("front_left_drive")
            .expect("fixture drive instance");
        let parameter = drive.parameters["encoder"].clone();
        drive.parameters.insert("missing".to_string(), parameter);
        let error = canonicalization_error((robot, components, simulations, structure));
        assert!(error.contains("robot.yaml components.front_left_drive.parameters.missing"));
        assert!(error.contains("components/drive_motor/component.yaml"));
        Ok(())
    }

    #[test]
    fn parameter_kind_must_match_component_source() -> anyhow::Result<()> {
        let (mut robot, components, simulations, structure) = fixture_inputs()?;
        let drive = robot
            .robot
            .components
            .get_mut("front_left_drive")
            .expect("fixture drive instance");
        drive
            .parameters
            .insert("motor".to_string(), drive.parameters["encoder"].clone());
        let error = canonicalization_error((robot, components, simulations, structure));
        assert!(error.contains("robot.yaml components.front_left_drive.parameters.motor"));
        assert!(error.contains("components/drive_motor/component.yaml defines 'motor'"));
        Ok(())
    }

    #[test]
    fn component_mount_link_must_exist_in_structure() -> anyhow::Result<()> {
        let (mut robot, components, simulations, structure) = fixture_inputs()?;
        robot
            .robot
            .components
            .get_mut("front_left_drive")
            .expect("fixture drive instance")
            .mount_link = "missing_link".to_string();
        let error = canonicalization_error((robot, components, simulations, structure));
        assert!(error.contains("robot.yaml components.front_left_drive.mount_link"));
        assert!(error.contains("missing_link"));
        Ok(())
    }

    #[test]
    fn simulation_source_requires_a_matching_component_source() -> anyhow::Result<()> {
        let (robot, components, mut simulations, structure) = fixture_inputs()?;
        simulations.insert(
            "orphan".to_string(),
            crate::model::source::simulation::read_from_string(
                "schema: simulation/v0\ncapabilities: {}\n",
            )?,
        );
        let error = canonicalization_error((robot, components, simulations, structure));
        assert!(error.contains("components/orphan/simulation.yaml"));
        assert!(error.contains("component.yaml"));
        Ok(())
    }

    #[test]
    fn simulation_capability_requires_a_matching_component_capability() -> anyhow::Result<()> {
        let (robot, components, mut simulations, structure) = fixture_inputs()?;
        let simulation = simulations
            .get_mut("drive_motor")
            .expect("fixture simulation");
        simulation.capabilities.insert(
            "missing".to_string(),
            simulation.capabilities["encoder"].clone(),
        );
        let error = canonicalization_error((robot, components, simulations, structure));
        assert!(error.contains("components/drive_motor/simulation.yaml capabilities.missing"));
        assert!(error.contains("components/drive_motor/component.yaml"));
        Ok(())
    }

    #[test]
    fn simulation_capability_kind_must_match_component_source() -> anyhow::Result<()> {
        let (robot, components, mut simulations, structure) = fixture_inputs()?;
        let simulation = simulations
            .get_mut("drive_motor")
            .expect("fixture simulation");
        simulation.capabilities.insert(
            "motor".to_string(),
            simulation.capabilities["encoder"].clone(),
        );
        let error = canonicalization_error((robot, components, simulations, structure));
        assert!(error.contains("components/drive_motor/simulation.yaml capabilities.motor"));
        assert!(error.contains("components/drive_motor/component.yaml defines 'motor'"));
        Ok(())
    }

    #[test]
    fn malformed_simulation_source_rejects_the_fully_loaded_canonical_robot() -> anyhow::Result<()>
    {
        let root = tempfile::tempdir()?;
        std::fs::create_dir_all(root.path().join("components/sensor"))?;
        std::fs::write(
            root.path().join("robot.yaml"),
            r#"
schema: robot/v0
robot:
  id: simulation-validation
  namespace: test
  structure: structure.urdf
  kinematic: { kind: omnidirectional, actuators: [sensor.motor], encoders: [] }
  motion_limits: { max_linear_speed_mps: 1.0, max_angular_speed_radps: 1.0 }
  components:
    sensor:
      component: sensor
      mount_link: base_link
"#,
        )?;
        std::fs::write(
            root.path().join("structure.urdf"),
            r#"<robot name="test">
  <link name="base_footprint" />
  <link name="base_link" />
  <joint name="base" type="fixed">
    <parent link="base_footprint" />
    <child link="base_link" />
  </joint>
</robot>"#,
        )?;
        std::fs::write(
            root.path().join("components/sensor/component.yaml"),
            r#"schema: component/v0
capabilities:
  motor:
    kind: motor
    command: velocity
    target: { kind: joint, id: base }
"#,
        )?;
        std::fs::write(
            root.path().join("components/sensor/simulation.yaml"),
            "schema: simulation/v0\ncapabilities:\n  Bad Id: { kind: range }\n",
        )?;

        let error =
            Robot::read_from_dir(root.path()).expect_err("invalid simulation source must fail");
        let message = format!("{error:#}");
        assert!(message.contains("simulation/v0"));
        assert!(message.contains("components/sensor/simulation.yaml"));
        Ok(())
    }

    #[test]
    fn camera_capabilities_include_color_and_exclude_depth() -> anyhow::Result<()> {
        let robot = Robot::read_from_dir(fixture_root())?;
        let cameras = robot
            .camera_capabilities()
            .into_iter()
            .map(|reference| reference.to_string())
            .collect::<Vec<_>>();
        assert!(cameras.contains(&"front_camera.rgb".to_string()));
        assert!(!cameras.contains(&"front_camera.depth".to_string()));
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
