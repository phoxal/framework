//! Canonical immutable robot model.

mod motion;

pub use motion::{KinematicConfig, MotionLimits};

use std::collections::BTreeMap;

use crate::component::capability::{Capability, Encoder, Motor, StructuralTarget};
use crate::component::{CapabilityRef, Component};
use crate::simulation::Simulation;
use crate::structure::Structure;
use crate::{DecodeError, EncodeError, ModelError, strict_json};

/// Exact compiled-model wire schema. There is no legacy fallback.
pub const ROBOT_SCHEMA: &str = "phoxal/robot/v0";

/// Stable canonical robot identity.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RobotIdentity {
    id: String,
    namespace: String,
}

/// One resolved component instance in the canonical robot.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentInstance {
    id: String,
    component_type: String,
    mount_link: String,
    direction_signs: BTreeMap<String, i8>,
}

/// Canonical motion facts.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MotionModel {
    kinematic: KinematicConfig,
    limits: MotionLimits,
}

/// Compiler-only construction seam used by `phoxal-manifest`.
#[doc(hidden)]
pub struct RobotParts {
    pub id: String,
    pub namespace: String,
    pub kinematic: KinematicConfig,
    pub motion_limits: MotionLimits,
    pub component_instances: BTreeMap<String, ComponentInstance>,
    pub component_types: BTreeMap<String, Component>,
    pub simulation_types: BTreeMap<String, Simulation>,
    pub structure: Structure,
}

/// Fully normalized runtime-facing robot model.
#[derive(Debug, Clone)]
pub struct Robot {
    identity: RobotIdentity,
    motion: MotionModel,
    component_instances: BTreeMap<String, ComponentInstance>,
    component_types: BTreeMap<String, Component>,
    simulation_types: BTreeMap<String, Simulation>,
    structure: Structure,
}

impl RobotIdentity {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }
}

impl ComponentInstance {
    /// Compiler-only constructor after source validation and normalization.
    #[doc(hidden)]
    pub fn __new(
        id: String,
        component_type: String,
        mount_link: String,
        direction_signs: BTreeMap<String, i8>,
    ) -> Self {
        Self {
            id,
            component_type,
            mount_link,
            direction_signs,
        }
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn component_type(&self) -> &str {
        &self.component_type
    }

    #[must_use]
    pub fn mount_link(&self) -> &str {
        &self.mount_link
    }
}

impl MotionModel {
    #[must_use]
    pub fn kinematic(&self) -> &KinematicConfig {
        &self.kinematic
    }

    #[must_use]
    pub const fn limits(&self) -> MotionLimits {
        self.limits
    }
}

impl Robot {
    /// Build a canonical model from compiler-normalized parts.
    #[doc(hidden)]
    pub fn __from_compiler(parts: RobotParts) -> Result<Self, ModelError> {
        let robot = Self {
            identity: RobotIdentity {
                id: parts.id,
                namespace: parts.namespace,
            },
            motion: MotionModel {
                kinematic: parts.kinematic,
                limits: parts.motion_limits,
            },
            component_instances: parts.component_instances,
            component_types: parts.component_types,
            simulation_types: parts.simulation_types,
            structure: parts.structure,
        };
        robot.validate()?;
        Ok(robot)
    }

    /// Encode the canonical runtime wire document deterministically.
    pub fn encode(&self) -> Result<Vec<u8>, EncodeError> {
        serde_json::to_vec_pretty(&RobotWire {
            schema: ROBOT_SCHEMA,
            robot: RobotPayloadRef {
                identity: &self.identity,
                motion: &self.motion,
                component_instances: &self.component_instances,
                component_types: &self.component_types,
                simulation_types: &self.simulation_types,
                structure: &self.structure,
            },
        })
        .map_err(EncodeError::from)
    }

    /// Strictly decode and validate the canonical runtime wire document.
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let value = strict_json::parse(bytes)?;
        let wire: RobotWireOwned = serde_json::from_value(value)?;
        if wire.schema != ROBOT_SCHEMA {
            return Err(DecodeError::UnsupportedSchema(wire.schema));
        }
        Self::__from_compiler(RobotParts {
            id: wire.robot.identity.id,
            namespace: wire.robot.identity.namespace,
            kinematic: wire.robot.motion.kinematic,
            motion_limits: wire.robot.motion.limits,
            component_instances: wire.robot.component_instances,
            component_types: wire.robot.component_types,
            simulation_types: wire.robot.simulation_types,
            structure: wire.robot.structure,
        })
        .map_err(DecodeError::Model)
    }

    #[must_use]
    pub fn identity(&self) -> &RobotIdentity {
        &self.identity
    }

    #[must_use]
    pub fn robot_id(&self) -> &str {
        self.identity.id()
    }

    #[must_use]
    pub fn namespace(&self) -> &str {
        self.identity.namespace()
    }

    #[must_use]
    pub fn motion(&self) -> &MotionModel {
        &self.motion
    }

    #[must_use]
    pub fn kinematic(&self) -> &KinematicConfig {
        self.motion.kinematic()
    }

    #[must_use]
    pub const fn motion_limits(&self) -> MotionLimits {
        self.motion.limits()
    }

    pub fn components(&self) -> impl ExactSizeIterator<Item = &ComponentInstance> {
        self.component_instances.values()
    }

    pub fn component_ids(&self) -> impl ExactSizeIterator<Item = &str> {
        self.component_instances.keys().map(String::as_str)
    }

    #[must_use]
    pub fn component(&self, id: &str) -> Option<&ComponentInstance> {
        self.component_instances.get(id)
    }

    pub fn component_instance(&self, id: &str) -> Result<&ComponentInstance, ModelError> {
        self.component(id)
            .ok_or_else(|| ModelError::Invalid(format!("unknown component instance '{id}'")))
    }

    pub fn component_for_instance(&self, id: &str) -> Result<&Component, ModelError> {
        let instance = self.component_instance(id)?;
        self.component_types
            .get(instance.component_type())
            .ok_or_else(|| {
                ModelError::Invalid(format!(
                    "component type '{}' for instance '{id}' is not loaded",
                    instance.component_type()
                ))
            })
    }

    #[must_use]
    pub fn simulation_for_component_type(&self, component_type: &str) -> Option<&Simulation> {
        self.simulation_types.get(component_type)
    }

    pub fn simulation_for_instance(
        &self,
        component_id: &str,
    ) -> Result<Option<&Simulation>, ModelError> {
        let instance = self.component_instance(component_id)?;
        Ok(self.simulation_for_component_type(instance.component_type()))
    }

    #[must_use]
    pub fn structure(&self) -> &Structure {
        &self.structure
    }

    pub fn capability(&self, reference: &CapabilityRef) -> Result<&Capability, ModelError> {
        self.component_for_instance(&reference.component_id)?
            .capability(&reference.capability_id)
            .ok_or_else(|| ModelError::Invalid(format!("unknown capability '{reference}'")))
    }

    pub fn camera_capabilities(&self) -> Result<Vec<CapabilityRef>, ModelError> {
        let mut capabilities = Vec::new();
        for component_id in self.component_ids() {
            let component = self.component_for_instance(component_id)?;
            capabilities.extend(
                component
                    .capabilities()
                    .filter(|(_, capability)| matches!(capability, Capability::Camera(_)))
                    .map(|(capability_id, _)| CapabilityRef::new(component_id, capability_id)),
            );
        }
        capabilities.sort();
        Ok(capabilities)
    }

    pub fn require_motor(&self, reference: &CapabilityRef) -> Result<(&Motor, i8), ModelError> {
        let capability = self.capability(reference)?;
        let Capability::Motor(motor) = capability else {
            return Err(ModelError::Invalid(format!(
                "capability '{reference}' must reference a motor, found {}",
                capability.kind_name()
            )));
        };
        Ok((motor, self.direction_sign(reference)?))
    }

    pub fn require_encoder(&self, reference: &CapabilityRef) -> Result<(&Encoder, i8), ModelError> {
        let capability = self.capability(reference)?;
        let Capability::Encoder(encoder) = capability else {
            return Err(ModelError::Invalid(format!(
                "capability '{reference}' must reference an encoder, found {}",
                capability.kind_name()
            )));
        };
        Ok((encoder, self.direction_sign(reference)?))
    }

    pub fn require_link_target(&self, reference: &CapabilityRef) -> Result<String, ModelError> {
        let StructuralTarget::Link { id } = self
            .capability(reference)?
            .target()
            .namespaced(&reference.component_id)
        else {
            return Err(ModelError::Invalid(format!(
                "capability '{reference}' must target a link"
            )));
        };
        self.structure.link(&id).map(|_| id.clone()).ok_or_else(|| {
            ModelError::Invalid(format!(
                "link target '{id}' for capability '{reference}' not found"
            ))
        })
    }

    pub fn component_mount_link(&self, id: &str) -> Result<String, ModelError> {
        Ok(self.component_instance(id)?.mount_link().to_string())
    }

    fn direction_sign(&self, reference: &CapabilityRef) -> Result<i8, ModelError> {
        Ok(self
            .component_instance(&reference.component_id)?
            .direction_signs
            .get(&reference.capability_id)
            .copied()
            .unwrap_or(1))
    }

    fn validate(&self) -> Result<(), ModelError> {
        validate_token(self.identity.id(), "robot id")?;
        validate_token(self.identity.namespace(), "robot namespace")?;
        self.motion.limits.validate()?;
        self.structure.validate()?;
        for (id, instance) in &self.component_instances {
            if id != instance.id() {
                return Err(ModelError::Invalid(format!(
                    "component map identity '{id}' does not match embedded id '{}'",
                    instance.id()
                )));
            }
            if !self.component_types.contains_key(instance.component_type()) {
                return Err(ModelError::Invalid(format!(
                    "component '{id}' references unknown component type '{}'",
                    instance.component_type()
                )));
            }
            if self.structure.link(instance.mount_link()).is_none() {
                return Err(ModelError::Invalid(format!(
                    "component '{id}' references unknown mount link '{}'",
                    instance.mount_link()
                )));
            }
        }
        for (component_type, simulation) in &self.simulation_types {
            let component = self.component_types.get(component_type).ok_or_else(|| {
                ModelError::Invalid(format!(
                    "simulation type '{component_type}' has no matching component type"
                ))
            })?;
            for (id, simulated) in simulation.capabilities() {
                let capability = component.capability(id).ok_or_else(|| {
                    ModelError::Invalid(format!(
                        "simulation capability '{component_type}.{id}' has no component capability"
                    ))
                })?;
                if simulated.kind_name() != capability.kind_name() {
                    return Err(ModelError::Invalid(format!(
                        "simulation capability '{component_type}.{id}' kind does not match component"
                    )));
                }
            }
        }
        match self.kinematic() {
            KinematicConfig::Differential {
                left_actuators,
                right_actuators,
                left_encoders,
                right_encoders,
                wheel_radius_m,
                wheel_base_m,
            } => {
                validate_positive(*wheel_radius_m, "differential wheel_radius_m")?;
                validate_positive(*wheel_base_m, "differential wheel_base_m")?;
                for reference in left_actuators.iter().chain(right_actuators) {
                    self.require_motor(reference)?;
                }
                for reference in left_encoders.iter().chain(right_encoders) {
                    self.require_encoder(reference)?;
                }
            }
            KinematicConfig::Mecanum {
                front_left_actuator,
                front_right_actuator,
                rear_left_actuator,
                rear_right_actuator,
                wheel_radius_m,
                wheel_base_m,
                track_m,
            } => {
                validate_positive(*wheel_radius_m, "mecanum wheel_radius_m")?;
                validate_positive(*wheel_base_m, "mecanum wheel_base_m")?;
                validate_positive(*track_m, "mecanum track_m")?;
                for reference in [
                    front_left_actuator,
                    front_right_actuator,
                    rear_left_actuator,
                    rear_right_actuator,
                ] {
                    self.require_motor(reference)?;
                }
            }
            KinematicConfig::Ackermann {
                steering_actuator,
                drive_actuator,
                steering_encoder,
                drive_encoder,
                wheel_base_m,
                track_m,
                max_steering_angle_rad,
            } => {
                validate_positive(*wheel_base_m, "ackermann wheel_base_m")?;
                validate_positive(*track_m, "ackermann track_m")?;
                validate_positive(*max_steering_angle_rad, "ackermann max_steering_angle_rad")?;
                self.require_motor(steering_actuator)?;
                self.require_motor(drive_actuator)?;
                if let Some(reference) = steering_encoder {
                    self.require_encoder(reference)?;
                }
                if let Some(reference) = drive_encoder {
                    self.require_encoder(reference)?;
                }
            }
            KinematicConfig::Omnidirectional {
                actuators,
                encoders,
            } => {
                for reference in actuators {
                    self.require_motor(reference)?;
                }
                for reference in encoders {
                    self.require_encoder(reference)?;
                }
            }
        }
        Ok(())
    }
}

fn validate_token(value: &str, label: &str) -> Result<(), ModelError> {
    if value.is_empty()
        || !value
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '_' | '-'))
    {
        return Err(ModelError::Invalid(format!(
            "{label} '{value}' is not normalized"
        )));
    }
    Ok(())
}

fn validate_positive(value: f64, label: &str) -> Result<(), ModelError> {
    if !value.is_finite() || value <= 0.0 {
        return Err(ModelError::Invalid(format!(
            "{label} must be finite and positive"
        )));
    }
    Ok(())
}

#[derive(serde::Serialize)]
#[serde(deny_unknown_fields)]
struct RobotWire<'a> {
    schema: &'static str,
    robot: RobotPayloadRef<'a>,
}

#[derive(serde::Serialize)]
#[serde(deny_unknown_fields)]
struct RobotPayloadRef<'a> {
    identity: &'a RobotIdentity,
    motion: &'a MotionModel,
    component_instances: &'a BTreeMap<String, ComponentInstance>,
    component_types: &'a BTreeMap<String, Component>,
    simulation_types: &'a BTreeMap<String, Simulation>,
    structure: &'a Structure,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RobotWireOwned {
    schema: String,
    robot: RobotPayloadOwned,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RobotPayloadOwned {
    identity: RobotIdentity,
    motion: MotionModel,
    component_instances: BTreeMap<String, ComponentInstance>,
    component_types: BTreeMap<String, Component>,
    simulation_types: BTreeMap<String, Simulation>,
    structure: Structure,
}
