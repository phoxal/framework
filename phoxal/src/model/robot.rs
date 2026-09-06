//! Canonical immutable robot model.

use crate::model::compiler::RobotParts;
use crate::model::component::Component;
use crate::model::component::capability::{
    Capability, CapabilityKind, CapabilityRole, Encoder, Motor, StructuralKind, StructuralTarget,
};
use crate::model::connection::Connection;
use crate::model::error::{IdentifierKind, JointOwner, ModelError};
use crate::model::footprint::FootprintEnvelope;
use crate::model::identity::{
    CapabilityId, CapabilityRef, ComponentInstanceId, ComponentTypeId, LinkId,
    MODULE_INSTANCE_SEPARATOR, RobotId, ServiceId,
};
use crate::model::kinematics::{KinematicConfig, MotionLimits, MotionModel};
use crate::model::simulation::Simulation;
use crate::model::structure::{Joint, JointKind, Structure};
use std::collections::{BTreeMap, BTreeSet};

/// One service this robot runs.
///
/// Official and user services alike: presence in [`Robot::services`] is what
/// declares the service, and the config is the only thing that distinguishes
/// one entry from another. The config stays an opaque JSON value because its
/// shape belongs to the service binary, which validates it against the schema it
/// embeds; the model carries it without an opinion.
#[derive(phoxal_macros::DescribeWire, Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Service {
    config: Option<serde_json::Value>,
}

impl Service {
    pub(crate) const fn new(config: Option<serde_json::Value>) -> Self {
        Self { config }
    }

    /// The user-owned configuration this service is launched with.
    #[must_use]
    pub const fn config(&self) -> Option<&serde_json::Value> {
        self.config.as_ref()
    }
}

/// The hardware driver one mounted component instance is driven by.
///
/// Two slots, one owner each. `connection` is the framework's: it is a closed
/// vocabulary the compiler validates, an editor completes, and the runner hands
/// to the driver as a typed value. `config` is the driver binary's, and stays an
/// opaque JSON value for the same reason a [`Service`]'s does - its shape
/// belongs to the binary, which validates it against the schema it embeds.
///
/// Its presence on an instance is what declares that a component driver runs
/// for that instance, under the instance's own id.
#[derive(
    phoxal_macros::DescribeWire, Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct Driver {
    connection: Connection,
    config: Option<serde_json::Value>,
}

impl Driver {
    pub(crate) const fn new(connection: Connection, config: Option<serde_json::Value>) -> Self {
        Self { connection, config }
    }

    /// How this component is physically wired to the machine.
    #[must_use]
    pub const fn connection(&self) -> &Connection {
        &self.connection
    }

    /// The driver-owned configuration this driver is launched with, and the
    /// only thing deserialized into its declared `Config` type.
    #[must_use]
    pub const fn config(&self) -> Option<&serde_json::Value> {
        self.config.as_ref()
    }
}

/// One mounted component instance in the canonical robot.
///
/// The instance is keyed by its own id in [`Robot::components`], so it carries
/// no copy of that id: the map is the identity.
#[derive(phoxal_macros::DescribeWire, Debug, Clone, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentInstance {
    #[serde(rename = "type")]
    component_type: ComponentTypeId,
    mount_link: LinkId,
    /// The driver block, present exactly when this instance is driven by a
    /// component driver.
    driver: Option<Driver>,
    direction_signs: BTreeMap<CapabilityId, i8>,
    /// Authored purpose(s) for each capability. An absent key means that the
    /// capability was not selected for any role and creates no obligation.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    roles: BTreeMap<CapabilityId, BTreeSet<CapabilityRole>>,
}

impl<'de> serde::Deserialize<'de> for ComponentInstance {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            #[serde(rename = "type")]
            component_type: ComponentTypeId,
            mount_link: LinkId,
            driver: Option<Driver>,
            direction_signs: BTreeMap<CapabilityId, i8>,
            #[serde(default)]
            roles: BTreeMap<CapabilityId, Vec<CapabilityRole>>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let mut roles = BTreeMap::new();
        for (capability_id, authored) in wire.roles {
            if authored.is_empty() {
                return Err(serde::de::Error::custom(ModelError::EmptyCapabilityRoles {
                    capability_id,
                }));
            }
            let mut canonical = BTreeSet::new();
            for role in authored {
                if !canonical.insert(role) {
                    return Err(serde::de::Error::custom(
                        ModelError::DuplicateCapabilityRole {
                            capability_id,
                            role,
                        },
                    ));
                }
            }
            roles.insert(capability_id, canonical);
        }
        Ok(Self::new(
            wire.component_type,
            wire.mount_link,
            wire.direction_signs,
            roles,
            wire.driver,
        ))
    }
}

/// is read from here, so there is no second persisted document to agree with.
#[derive(Debug, Clone)]
pub struct Robot {
    id: RobotId,
    motion: MotionModel,
    services: BTreeMap<ServiceId, Service>,
    components: BTreeMap<ComponentInstanceId, ComponentInstance>,
    component_types: BTreeMap<ComponentTypeId, Component>,
    structure: Structure,
    /// Compiler-derived stock-safety facts. `None` is explicitly persisted
    /// when the authored robot has no collision geometry.
    footprint: Option<FootprintEnvelope>,
}

/// The canonical robot wire shape used by the persisted manifest.
///
/// This is intentionally private to the model crate: bundle layout belongs to
/// `crate::bundle`, while the model owns the exact fields and the validation
/// that turns them into a `Robot`. Keeping the wire helper here also means
/// deserialization can never construct an invalid robot by bypassing
/// [`Robot::new`].
#[derive(phoxal_macros::DescribeWire, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RobotWire {
    id: RobotId,
    kinematic: KinematicConfig,
    motion_limits: MotionLimits,
    services: BTreeMap<ServiceId, Service>,
    components: BTreeMap<ComponentInstanceId, ComponentInstance>,
    component_types: BTreeMap<ComponentTypeId, Component>,
    structure: Structure,
    footprint: PersistedFootprint,
}

/// A required wire field whose value may be `null`.
///
/// `Option<T>` is normally permissive in a derived serde struct: both a
/// missing key and `null` become `None`. The manifest needs to distinguish
/// them so every persisted robot says explicitly whether a footprint exists.
#[derive(phoxal_macros::DescribeWire, serde::Serialize, serde::Deserialize)]
struct PersistedFootprint(Option<FootprintEnvelope>);

impl serde::Serialize for Robot {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        RobotWire {
            id: self.id.clone(),
            kinematic: self.motion.kinematic().clone(),
            motion_limits: self.motion.limits(),
            services: self.services.clone(),
            components: self.components.clone(),
            component_types: self.component_types.clone(),
            structure: self.structure.clone(),
            footprint: PersistedFootprint(self.footprint),
        }
        .serialize(serializer)
    }
}

impl crate::__compat::wire::DescribeWire for Robot {
    // Invariant: the `Serialize` above builds a `RobotWire` and writes that, so
    // the wire helper's shape is the whole of what a persisted robot is.
    fn wire_schema() -> crate::__compat::wire::WireSchema {
        crate::__compat::wire::WireSchema::opaque(
            "Robot",
            <RobotWire as crate::__compat::wire::DescribeWire>::wire_schema(),
        )
    }
}

impl<'de> serde::Deserialize<'de> for Robot {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = RobotWire::deserialize(deserializer)?;
        Self::new(
            RobotParts {
                id: wire.id,
                kinematic: wire.kinematic,
                motion_limits: wire.motion_limits,
                services: wire.services,
                components: wire.components,
                component_types: wire.component_types,
                structure: wire.structure,
            },
            wire.footprint.0,
        )
        .map_err(serde::de::Error::custom)
    }
}

/// One mounted component, read as the single fact it is: the instance, the type
/// behind it, and how a simulated world models that type.
///
/// This exists so no consumer joins [`Robot::components`] and
/// [`Robot::component_types`] by hand. Joining them is the one lookup every
/// participant, the supervisor and the simulator all need, and doing it in three
/// places is three chances to disagree about what an absent type means.
#[derive(Clone, Copy, Debug)]
pub struct ComponentView<'a> {
    id: &'a ComponentInstanceId,
    instance: &'a ComponentInstance,
    component_type: &'a Component,
}

impl<'a> ComponentView<'a> {
    /// The mounted instance's identity.
    #[must_use]
    pub const fn id(&self) -> &'a ComponentInstanceId {
        self.id
    }

    /// The mounted instance: its type, mount, driver block and per-capability
    /// parameters.
    #[must_use]
    pub const fn instance(&self) -> &'a ComponentInstance {
        self.instance
    }

    /// The component type this instance mounts.
    #[must_use]
    pub const fn component_type(&self) -> &'a Component {
        self.component_type
    }

    /// How a simulated world models this component, when a document authored a
    /// simulation for its type.
    #[must_use]
    pub const fn simulation(&self) -> Option<&'a Simulation> {
        self.component_type.simulation()
    }
}

impl ComponentInstance {
    pub(crate) const fn new(
        component_type: ComponentTypeId,
        mount_link: LinkId,
        direction_signs: BTreeMap<CapabilityId, i8>,
        roles: BTreeMap<CapabilityId, BTreeSet<CapabilityRole>>,
        driver: Option<Driver>,
    ) -> Self {
        Self {
            component_type,
            mount_link,
            driver,
            direction_signs,
            roles,
        }
    }

    #[must_use]
    pub const fn component_type(&self) -> &ComponentTypeId {
        &self.component_type
    }

    /// The robot link this instance is rigidly mounted on.
    #[must_use]
    pub const fn mount_link(&self) -> &LinkId {
        &self.mount_link
    }

    /// The driver block, present exactly when a component driver runs for this
    /// instance. The driver's participant id is this instance's id.
    #[must_use]
    pub const fn driver(&self) -> Option<&Driver> {
        self.driver.as_ref()
    }

    /// The authored direction sign for each capability this instance overrides.
    /// An absent capability turns forward.
    #[must_use]
    pub const fn direction_signs(&self) -> &BTreeMap<CapabilityId, i8> {
        &self.direction_signs
    }

    /// Authored role assignments, ordered by capability and role.
    #[must_use]
    pub const fn roles(&self) -> &BTreeMap<CapabilityId, BTreeSet<CapabilityRole>> {
        &self.roles
    }

    /// Whether this instance assigns `role` to the named capability.
    #[must_use]
    pub fn has_role(&self, capability: &CapabilityId, role: CapabilityRole) -> bool {
        self.roles
            .get(capability)
            .is_some_and(|roles| roles.contains(&role))
    }
}

impl Robot {
    pub(crate) fn new(
        parts: RobotParts,
        footprint: Option<FootprintEnvelope>,
    ) -> Result<Self, ModelError> {
        let robot = Self {
            id: parts.id,
            motion: MotionModel::new(parts.kinematic, parts.motion_limits),
            services: parts.services,
            components: parts.components,
            component_types: parts.component_types,
            structure: parts.structure,
            footprint,
        };
        robot.validate()?;
        Ok(robot)
    }

    #[must_use]
    pub const fn id(&self) -> &RobotId {
        &self.id
    }

    #[must_use]
    pub const fn motion(&self) -> &MotionModel {
        &self.motion
    }

    /// Every service this robot runs, ordered by service id.
    pub fn services(&self) -> impl ExactSizeIterator<Item = (&ServiceId, &Service)> {
        self.services.iter()
    }

    /// The named service, if this robot runs one.
    #[must_use]
    pub fn service(&self, id: &str) -> Option<&Service> {
        self.services.get(id)
    }

    /// The configuration the named service is launched with.
    ///
    /// `None` covers both "this robot does not run that service" and "it runs
    /// with no configuration", which is the same answer to the one question a
    /// service asks about itself at startup.
    #[must_use]
    pub fn service_config(&self, id: &str) -> Option<&serde_json::Value> {
        self.service(id)?.config()
    }

    /// Every mounted component, ordered by instance id.
    pub fn components(&self) -> impl Iterator<Item = ComponentView<'_>> {
        self.components
            .iter()
            .filter_map(|(id, instance)| self.view(id, instance))
    }

    /// Every mounted instance's identity, ordered.
    pub fn component_ids(&self) -> impl ExactSizeIterator<Item = &ComponentInstanceId> {
        self.components.keys()
    }

    /// The named component: its instance, its type, and its simulation.
    #[must_use]
    pub fn component(&self, id: &str) -> Option<ComponentView<'_>> {
        let (id, instance) = self.components.get_key_value(id)?;
        self.view(id, instance)
    }

    /// Every declared component type, ordered by type id.
    pub fn component_types(&self) -> impl ExactSizeIterator<Item = (&ComponentTypeId, &Component)> {
        self.component_types.iter()
    }

    /// Join one instance with the type behind it.
    ///
    /// A validated robot never mounts an instance of a type it did not load, so
    /// the `None` arm is unreachable rather than a component being dropped. It
    /// stays a `filter_map`/`?` rather than a panic so the impossible case
    /// degrades into a smaller answer instead of taking the process down.
    fn view<'a>(
        &'a self,
        id: &'a ComponentInstanceId,
        instance: &'a ComponentInstance,
    ) -> Option<ComponentView<'a>> {
        Some(ComponentView {
            id,
            instance,
            component_type: self.component_types.get(instance.component_type())?,
        })
    }

    /// The robot's own structure, in flattened runtime identities.
    #[must_use]
    pub const fn structure(&self) -> &Structure {
        &self.structure
    }

    /// The persisted compiler-derived stock-safety envelope, if available.
    #[must_use]
    pub const fn footprint_envelope(&self) -> Option<FootprintEnvelope> {
        self.footprint
    }

    /// The referenced capability, if the robot declares it.
    #[must_use]
    pub fn capability(&self, reference: &CapabilityRef) -> Option<&Capability> {
        self.resolve(reference).map(|(_, capability)| capability)
    }

    /// Every declared capability matching `selects`.
    ///
    /// The result is ordered by `(component id, capability id)`. That order is
    /// an invariant, not an accident of iteration: participants derive bus
    /// identities and arbitration priorities from these positions, so two runs
    /// over the same robot must produce the same sequence.
    ///
    /// This is infallible because a `Robot` value cannot exist with an instance
    /// whose component type is absent: validation rejects that with
    /// [`ModelError::UnknownComponentType`] before the value is constructed. The
    /// unresolved arm below is therefore unreachable rather than a capability
    /// being quietly dropped, and it stays a skip rather than a panic so that
    /// the impossible case degrades into a smaller result set instead of taking
    /// the process down.
    pub fn capability_refs(&self, selects: impl Fn(&Capability) -> bool) -> Vec<CapabilityRef> {
        let mut references = self
            .components()
            .flat_map(|component| {
                component
                    .component_type()
                    .capabilities()
                    .filter(|(_, capability)| selects(capability))
                    .map(move |(capability_id, _)| {
                        CapabilityRef::new(component.id().clone(), capability_id.clone())
                    })
            })
            .collect::<Vec<_>>();
        references.sort();
        references
    }

    /// Every capability assigned the given authored role, ordered by
    /// `(component id, capability id)`.
    #[must_use]
    pub fn capabilities_with_role(&self, role: CapabilityRole) -> Vec<CapabilityRef> {
        self.components()
            .flat_map(|component| {
                component
                    .instance()
                    .roles()
                    .iter()
                    .filter(move |(capability_id, roles)| {
                        roles.contains(&role)
                            && component
                                .component_type()
                                .capability(capability_id.as_str())
                                .is_some()
                    })
                    .map(move |(capability_id, _)| {
                        CapabilityRef::new(component.id().clone(), capability_id.clone())
                    })
            })
            .collect()
    }

    /// The referenced motor and the direction sign to apply to it.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::UnknownCapability`] when the robot does not
    /// declare the capability, and [`ModelError::CapabilityKindMismatch`] when
    /// it declares something other than a motor.
    pub fn require_motor(&self, reference: &CapabilityRef) -> Result<(&Motor, i8), ModelError> {
        let capability = self.require_capability(reference)?;
        let Capability::Motor(motor) = capability else {
            return Err(ModelError::CapabilityKindMismatch {
                reference: reference.clone(),
                expected: CapabilityKind::Motor,
                actual: capability.kind(),
            });
        };
        Ok((motor, self.direction_sign(reference)))
    }

    /// The referenced encoder and the direction sign to apply to it.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::UnknownCapability`] when the robot does not
    /// declare the capability, and [`ModelError::CapabilityKindMismatch`] when
    /// it declares something other than an encoder.
    pub fn require_encoder(&self, reference: &CapabilityRef) -> Result<(&Encoder, i8), ModelError> {
        let capability = self.require_capability(reference)?;
        let Capability::Encoder(encoder) = capability else {
            return Err(ModelError::CapabilityKindMismatch {
                reference: reference.clone(),
                expected: CapabilityKind::Encoder,
                actual: capability.kind(),
            });
        };
        Ok((encoder, self.direction_sign(reference)))
    }

    /// The namespaced runtime frame for a capability's component-local link.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::CapabilityTargetKind`] when the capability is
    /// attached to a joint rather than a link, and
    /// [`ModelError::UnknownBoundTarget`] when its component type has no such
    /// link.
    pub fn link_target_frame(&self, reference: &CapabilityRef) -> Result<LinkId, ModelError> {
        let (component, capability) =
            self.resolve(reference)
                .ok_or_else(|| ModelError::UnknownCapability {
                    reference: reference.clone(),
                })?;
        let StructuralTarget::Link { id } = capability.target() else {
            return Err(ModelError::CapabilityTargetKind {
                reference: reference.clone(),
                expected: StructuralKind::Link,
            });
        };
        if component.structure().link(id.as_str()).is_none() {
            return Err(ModelError::UnknownBoundTarget {
                reference: reference.clone(),
                kind: StructuralKind::Link,
                id: id.as_str().to_string(),
            });
        }
        Ok(id.namespaced(&reference.component_id))
    }

    fn resolve(&self, reference: &CapabilityRef) -> Option<(&Component, &Capability)> {
        let component = self
            .component(reference.component_id.as_str())?
            .component_type;
        let capability = component.capability(reference.capability_id.as_str())?;
        Some((component, capability))
    }

    fn require_capability(&self, reference: &CapabilityRef) -> Result<&Capability, ModelError> {
        self.capability(reference)
            .ok_or_else(|| ModelError::UnknownCapability {
                reference: reference.clone(),
            })
    }

    /// The authored direction sign, defaulting to `1` when none was authored.
    fn direction_sign(&self, reference: &CapabilityRef) -> i8 {
        self.components
            .get(reference.component_id.as_str())
            .and_then(|instance| {
                instance
                    .direction_signs
                    .get(reference.capability_id.as_str())
            })
            .copied()
            .unwrap_or(1)
    }

    fn validate(&self) -> Result<(), ModelError> {
        self.motion.limits().validate()?;
        self.validate_robot_structure()?;
        self.validate_component_types()?;
        self.validate_components()?;
        self.validate_kinematic()?;
        self.validate_footprint()
    }

    /// The robot's own structure carries flattened identities, so no authored
    /// robot link or joint may already contain the namespacing separator.
    fn validate_robot_structure(&self) -> Result<(), ModelError> {
        for link in self.structure.links() {
            Self::reject_reserved_separator(IdentifierKind::RobotLink, link.name().as_str())?;
        }
        for joint in self.structure.joints() {
            Self::reject_reserved_separator(IdentifierKind::RobotJoint, joint.name().as_str())?;
            Self::validate_runtime_joint_kind(joint, &JointOwner::Robot)?;
        }
        Ok(self.structure.validate_robot_frames()?)
    }

    fn validate_component_types(&self) -> Result<(), ModelError> {
        for (component_type, component) in &self.component_types {
            for joint in component.structure().joints() {
                Self::validate_runtime_joint_kind(
                    joint,
                    &JointOwner::ComponentType(component_type.clone()),
                )?;
            }
            for (capability_id, capability) in component.capabilities() {
                let target = capability.target();
                let present = match target {
                    StructuralTarget::Link { id } => {
                        component.structure().link(id.as_str()).is_some()
                    }
                    StructuralTarget::Joint { id } => {
                        component.structure().joint(id.as_str()).is_some()
                    }
                };
                if !present {
                    let id = match target {
                        StructuralTarget::Link { id } => id.as_str().to_string(),
                        StructuralTarget::Joint { id } => id.as_str().to_string(),
                    };
                    return Err(ModelError::UnknownDeclaredTarget {
                        component_type: component_type.clone(),
                        capability_id: capability_id.clone(),
                        kind: target.kind(),
                        id,
                    });
                }
            }
            Self::validate_simulation(component_type, component)?;
        }
        Ok(())
    }

    /// A simulation never introduces a capability of its own: every entry
    /// models one the type already declares, of the same kind.
    fn validate_simulation(
        component_type: &ComponentTypeId,
        component: &Component,
    ) -> Result<(), ModelError> {
        let Some(simulation) = component.simulation() else {
            return Ok(());
        };
        for (link, _) in simulation.links() {
            if component.structure().link(link.as_str()).is_none() {
                return Err(ModelError::SimulationWithoutLink {
                    component_type: component_type.clone(),
                    link: link.clone(),
                });
            }
        }
        for (capability_id, simulated) in simulation.capabilities() {
            let capability = component
                .capability(capability_id.as_str())
                .ok_or_else(|| ModelError::SimulationWithoutCapability {
                    component_type: component_type.clone(),
                    capability_id: capability_id.clone(),
                })?;
            if simulated.kind() != capability.kind() {
                return Err(ModelError::SimulationCapabilityKindMismatch {
                    component_type: component_type.clone(),
                    capability_id: capability_id.clone(),
                    simulated: simulated.kind(),
                    declared: capability.kind(),
                });
            }
        }
        Ok(())
    }

    /// Validate only the envelope's universal scalar invariant.
    ///
    /// Collision geometry is authored source and is deliberately unavailable
    /// to the persisted manifest. Its conservative envelope is derived once by
    /// the source compiler, then persisted as a value or explicit `null`.
    fn validate_footprint(&self) -> Result<(), ModelError> {
        if let Some(footprint) = self.footprint {
            FootprintEnvelope::new(footprint.radius_m)?;
        }
        Ok(())
    }

    fn validate_components(&self) -> Result<(), ModelError> {
        for (id, instance) in &self.components {
            Self::reject_reserved_separator(IdentifierKind::ComponentInstance, id.as_str())?;
            let component = self
                .component_types
                .get(instance.component_type())
                .ok_or_else(|| ModelError::UnknownComponentType {
                    instance: id.clone(),
                    component_type: instance.component_type().clone(),
                })?;
            if self
                .structure
                .link(instance.mount_link().as_str())
                .is_none()
            {
                return Err(ModelError::UnknownMountLink {
                    instance: id.clone(),
                    link: instance.mount_link().clone(),
                });
            }
            for (capability_id, sign) in &instance.direction_signs {
                if !matches!(sign, -1 | 1) {
                    return Err(ModelError::DirectionSign {
                        instance: id.clone(),
                        capability_id: capability_id.clone(),
                        value: *sign,
                    });
                }
                if component.capability(capability_id.as_str()).is_none() {
                    return Err(ModelError::UnknownDirectionSignCapability {
                        instance: id.clone(),
                        capability_id: capability_id.clone(),
                    });
                }
            }
            for capability_id in instance.roles.keys() {
                if component.capability(capability_id.as_str()).is_none() {
                    return Err(ModelError::UnknownRoleCapability {
                        instance: id.clone(),
                        capability_id: capability_id.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    fn validate_kinematic(&self) -> Result<(), ModelError> {
        // The geometry scalars are checked by the one reader that turns them
        // into geometry, so this cannot drift from what consumers actually get.
        self.motion.kinematic().drive_kinematics()?;
        match self.motion.kinematic() {
            KinematicConfig::Differential {
                left_actuators,
                right_actuators,
                left_encoders,
                right_encoders,
                ..
            } => {
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
                ..
            } => {
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
                ..
            } => {
                self.require_motor(steering_actuator)?;
                self.require_motor(drive_actuator)?;
                for reference in steering_encoder.iter().chain(drive_encoder) {
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

    /// Reject an identifier that already carries the namespacing separator,
    /// which would make the flattened runtime identity ambiguous.
    fn reject_reserved_separator(kind: IdentifierKind, value: &str) -> Result<(), ModelError> {
        if value.contains(MODULE_INSTANCE_SEPARATOR) {
            return Err(ModelError::ReservedSeparator {
                kind,
                value: value.to_string(),
            });
        }
        Ok(())
    }

    /// Reject a joint whose kind the runtime has no controller for.
    fn validate_runtime_joint_kind(joint: &Joint, owner: &JointOwner) -> Result<(), ModelError> {
        if matches!(
            joint.kind(),
            JointKind::Fixed | JointKind::Revolute | JointKind::Continuous | JointKind::Prismatic
        ) {
            Ok(())
        } else {
            Err(ModelError::UnsupportedJointKind {
                owner: owner.clone(),
                joint: joint.name().clone(),
                kind: joint.kind(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::compiler::{self, RobotParts};
    use serde_json::{Value, json};

    const INERTIAL: &str = r#"{
        "origin": { "xyz": [0.0, 0.0, 0.0], "rpy": [0.0, 0.0, 0.0] },
        "mass_kg": 1.0,
        "inertia": { "ixx": 1.0, "ixy": 0.0, "ixz": 0.0, "iyy": 1.0, "iyz": 0.0, "izz": 1.0 }
    }"#;

    fn inertial() -> Value {
        serde_json::from_str(INERTIAL).expect("a well-formed inertial fixture")
    }

    fn link(name: &str) -> Value {
        json!({ "name": name, "inertial": inertial(), "visuals": [], "collisions": [] })
    }

    fn robot_structure() -> Structure {
        compiler::structure(json!({
            "name": "rover",
            "links": [link("base_footprint"), link("base_link")],
            "joints": [{
                "name": "base_joint",
                "kind": "fixed",
                "origin": { "xyz": [0.0, 0.0, 0.0], "rpy": [0.0, 0.0, 0.0] },
                "parent": "base_footprint",
                "child": "base_link",
                "axis": [0.0, 0.0, 1.0],
                "limit": { "lower": 0.0, "upper": 0.0, "effort": 0.0, "velocity": 0.0 }
            }],
            "materials": []
        }))
        .expect("a well-formed robot structure fixture")
    }

    fn robot_structure_with_collision() -> Structure {
        compiler::structure(json!({
            "name": "rover",
            "links": [
                {
                    "name": "base_footprint",
                    "inertial": inertial(),
                    "visuals": [],
                    "collisions": [{
                        "name": "hull",
                        "origin": { "xyz": [0.0, 0.0, 0.0], "rpy": [0.0, 0.0, 0.0] },
                        "geometry": { "kind": "sphere", "radius": 0.5 }
                    }]
                },
                link("base_link")
            ],
            "joints": [{
                "name": "base_joint",
                "kind": "fixed",
                "origin": { "xyz": [0.0, 0.0, 0.0], "rpy": [0.0, 0.0, 0.0] },
                "parent": "base_footprint",
                "child": "base_link",
                "axis": [0.0, 0.0, 1.0],
                "limit": { "lower": 0.0, "upper": 0.0, "effort": 0.0, "velocity": 0.0 }
            }],
            "materials": []
        }))
        .expect("a well-formed colliding robot structure fixture")
    }

    fn component_structure() -> Structure {
        compiler::structure(json!({
            "name": "drive",
            "links": [link("body")],
            "joints": [],
            "materials": []
        }))
        .expect("a well-formed component structure fixture")
    }

    /// A component type declaring one motor and one camera, both on `body`.
    fn drive_component() -> Component {
        let capabilities = serde_json::from_value(json!({
            "spin": {
                "kind": "motor",
                "target": { "kind": "link", "id": "body" },
                "command": "velocity",
                "gear_ratio": 1.0
            },
            "eye": {
                "kind": "camera",
                "target": { "kind": "link", "id": "body" },
                "mode": "rgb",
                "publish_rate_hz": 30.0,
                "width_px": 640,
                "height_px": 480
            }
        }))
        .expect("a well-formed capability fixture");
        compiler::component(capabilities, component_structure(), None)
    }

    fn instance() -> ComponentInstance {
        compiler::component_instance(
            ComponentTypeId::new("drive").expect("a normalized type id"),
            LinkId::new("base_link"),
            BTreeMap::new(),
            BTreeMap::new(),
            None,
        )
    }

    fn robot_with_structure(structure: Structure, instance_ids: &[&str]) -> Robot {
        compiler::robot(RobotParts {
            id: RobotId::new("rover").expect("a normalized robot id"),
            kinematic: KinematicConfig::Omnidirectional {
                actuators: Vec::new(),
                encoders: Vec::new(),
            },
            motion_limits: MotionLimits {
                max_linear_speed_mps: 1.0,
                max_angular_speed_radps: 1.0,
            },
            services: BTreeMap::new(),
            components: instance_ids
                .iter()
                .map(|id| {
                    (
                        ComponentInstanceId::new(*id).expect("a normalized instance id"),
                        instance(),
                    )
                })
                .collect(),
            component_types: [(
                ComponentTypeId::new("drive").expect("a normalized type id"),
                drive_component(),
            )]
            .into_iter()
            .collect(),
            structure,
        })
        .expect("a valid canonical robot")
    }

    fn robot_with(instance_ids: &[&str]) -> Robot {
        robot_with_structure(robot_structure(), instance_ids)
    }

    /// `Robot` and `Structure` both write through a private wire helper their
    /// own declarations do not predict, so the declared shape is checked
    /// against a real serialized robot rather than asserted. This is the whole
    /// canonical model - components, capabilities, structure, footprint - so a
    /// type anywhere below it whose shape drifted fails here.
    #[test]
    fn the_declared_robot_shape_is_the_shape_serde_writes() {
        use crate::__compat::wire::DescribeWire;

        for robot in [
            robot_with(&["left"]),
            robot_with_structure(robot_structure_with_collision(), &[]),
        ] {
            let json = serde_json::to_value(&robot).expect("a canonical robot serializes");
            assert_eq!(Robot::wire_schema().conforms(&json), Ok(()));
        }
    }

    #[test]
    fn robot_wire_requires_an_explicit_footprint_value_or_null() {
        let robot = robot_with(&[]);
        let mut value = serde_json::to_value(&robot).expect("robot serializes");
        assert!(value["footprint"].is_null());
        value
            .as_object_mut()
            .expect("robot wire is an object")
            .remove("footprint");
        assert!(serde_json::from_value::<Robot>(value).is_err());
    }

    #[test]
    fn runtime_deserialize_checks_envelope_invariants_without_rederiving_geometry() {
        let robot = robot_with_structure(robot_structure_with_collision(), &[]);
        assert_eq!(robot.footprint_envelope().unwrap().radius_m, 0.5);
        let mut value = serde_json::to_value(&robot).expect("robot serializes");
        value["footprint"]["radius_m"] = json!(0.1);
        let decoded: Robot = serde_json::from_value(value).expect("finite stored radius is valid");
        assert_eq!(decoded.footprint_envelope().unwrap().radius_m, 0.1);
    }

    #[test]
    fn runtime_role_lists_reject_empty_and_duplicate_assignments() {
        let robot = robot_with(&["front"]);
        let value = serde_json::to_value(&robot).expect("robot serializes");

        let mut empty = value.clone();
        empty["components"]["front"]["roles"] = json!({"eye": []});
        assert!(serde_json::from_value::<Robot>(empty).is_err());

        let mut duplicate = value;
        duplicate["components"]["front"]["roles"] = json!({"eye": ["perception", "perception"]});
        assert!(serde_json::from_value::<Robot>(duplicate).is_err());
    }

    fn reference(component: &str, capability: &str) -> CapabilityRef {
        CapabilityRef::new(
            ComponentInstanceId::new(component).expect("a normalized instance id"),
            CapabilityId::new(capability).expect("a normalized capability id"),
        )
    }

    #[test]
    fn selecting_no_capability_yields_nothing() {
        let robot = robot_with(&["front", "rear"]);
        assert!(
            robot
                .capability_refs(|capability| matches!(capability, Capability::Lidar(_)))
                .is_empty()
        );
    }

    #[test]
    fn selection_spans_every_instance_that_declares_the_capability() {
        let robot = robot_with(&["front", "rear"]);
        let cameras =
            robot.capability_refs(|capability| matches!(capability, Capability::Camera(_)));
        assert_eq!(
            cameras.iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["front.eye", "rear.eye"]
        );
    }

    #[test]
    fn selection_is_ordered_by_component_then_capability() {
        // Instances are supplied in reverse order on purpose: the ordering is
        // an invariant of the result, not of the input.
        let robot = robot_with(&["rear", "front"]);
        let all = robot.capability_refs(|_| true);
        assert_eq!(
            all.iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["front.eye", "front.spin", "rear.eye", "rear.spin"]
        );
        let mut sorted = all.clone();
        sorted.sort();
        assert_eq!(all, sorted);
    }

    #[test]
    fn a_routine_lookup_miss_is_absence_not_failure() {
        let robot = robot_with(&["front"]);
        let front = robot.component("front").expect("the instance is mounted");
        assert_eq!(front.id().as_str(), "front");
        assert_eq!(front.instance().mount_link(), &LinkId::new("base_link"));
        assert!(front.simulation().is_none());
        assert!(robot.component("nope").is_none());
        assert!(robot.capability(&reference("front", "spin")).is_some());
        assert!(robot.capability(&reference("front", "nope")).is_none());
        assert!(robot.capability(&reference("nope", "spin")).is_none());
    }

    #[test]
    fn requiring_the_wrong_kind_names_both_kinds() {
        let robot = robot_with(&["front"]);
        let error = robot
            .require_motor(&reference("front", "eye"))
            .expect_err("a camera is not a motor");
        assert!(matches!(
            error,
            ModelError::CapabilityKindMismatch {
                expected: CapabilityKind::Motor,
                actual: CapabilityKind::Camera,
                ..
            }
        ));
        assert_eq!(
            error.to_string(),
            "capability 'front.eye' must reference a motor, found camera"
        );

        let error = robot
            .require_encoder(&reference("front", "nope"))
            .expect_err("an undeclared capability cannot be required");
        assert!(matches!(error, ModelError::UnknownCapability { .. }));
    }

    #[test]
    fn a_link_target_resolves_to_the_namespaced_runtime_frame() {
        let robot = robot_with(&["front"]);
        assert_eq!(
            robot
                .link_target_frame(&reference("front", "eye"))
                .expect("the camera targets a link"),
            LinkId::new("front__body")
        );
    }

    #[test]
    fn an_unauthored_direction_sign_defaults_to_forward() {
        let robot = robot_with(&["front"]);
        let (_, sign) = robot
            .require_motor(&reference("front", "spin"))
            .expect("the motor resolves");
        assert_eq!(sign, 1);
    }

    #[test]
    fn a_motion_limit_must_survive_the_narrowing_to_f32() {
        for limits in [
            MotionLimits {
                max_linear_speed_mps: 0.0,
                max_angular_speed_radps: 1.0,
            },
            MotionLimits {
                max_linear_speed_mps: 1.0,
                max_angular_speed_radps: f64::MAX,
            },
            MotionLimits {
                max_linear_speed_mps: f64::NAN,
                max_angular_speed_radps: 1.0,
            },
        ] {
            assert!(matches!(
                limits.validate(),
                Err(ModelError::MotionLimit { .. })
            ));
        }
        assert!(
            MotionLimits {
                max_linear_speed_mps: 1.5,
                max_angular_speed_radps: 2.5,
            }
            .validate()
            .is_ok()
        );
    }
}
