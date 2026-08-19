//! The one construction seam for the canonical model.
//!
//! Nothing in this crate can build a [`Robot`], a [`Component`], a
//! [`Simulation`] or a [`Structure`] from raw values: they are only ever
//! produced by normalizing authored documents, and that normalizer lives in
//! `crate::authoring`, a sibling module. Rust has no visibility that means
//! "one other crate", so these entry points are `pub` and hidden rather than
//! `pub(crate)`.
//!
//! `crate::authoring` is the only permitted caller outside this module. This is
//! not runtime API: a participant receives an already-built [`Robot`] and reads
//! it through the runtime modules. Every entry point here still runs the full
//! validation, so calling one cannot produce a model the runtime would reject -
//! which is also why the feature-gated `test_builder` assembles its in-memory
//! robots through these same entry points rather than around them.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::component::Component;
use crate::model::component::capability::{Capability, CapabilityRole};
use crate::model::error::ModelError;
use crate::model::identity::{
    CapabilityId, ComponentInstanceId, ComponentTypeId, LinkId, RobotId, ServiceId,
};
use crate::model::robot::{ComponentInstance, KinematicConfig, MotionLimits, Robot, Service};
use crate::model::simulation::{self, Simulation};
use crate::model::structure::Structure;

/// The normalized inputs a canonical [`Robot`] is assembled from.
///
/// A plain field bag rather than a builder: the compiler produces all of it in
/// one pass, and [`robot`] validates the whole before any of it is observable.
pub struct RobotParts {
    pub id: RobotId,
    pub kinematic: KinematicConfig,
    pub motion_limits: MotionLimits,
    pub services: BTreeMap<ServiceId, Service>,
    pub components: BTreeMap<ComponentInstanceId, ComponentInstance>,
    pub component_types: BTreeMap<ComponentTypeId, Component>,
    pub structure: Structure,
}

/// Build a validated structure from the compiler's normalized JSON document.
///
/// # Errors
///
/// Returns [`ModelError::Structure`] when the document is not a single valid
/// link tree.
pub fn structure(document: serde_json::Value) -> Result<Structure, ModelError> {
    Ok(Structure::from_compiler_value(document)?)
}

/// Build one component type from its normalized capabilities, structure, and
/// the simulation modelling it, when a document authored one.
#[must_use]
pub fn component(
    capabilities: BTreeMap<CapabilityId, Capability>,
    structure: Structure,
    simulation: Option<Simulation>,
) -> Component {
    Component::new(capabilities, structure, simulation)
}

/// Build one service entry from its user-owned configuration.
#[must_use]
pub const fn service(config: Option<serde_json::Value>) -> Service {
    Service::new(config)
}

/// Build one component type's simulation from its normalized capabilities and
/// per-link contact materials.
#[must_use]
pub fn simulation(
    capabilities: BTreeMap<CapabilityId, simulation::Capability>,
    links: BTreeMap<LinkId, Option<String>>,
) -> Simulation {
    Simulation::new(capabilities, links)
}

/// Build one mounted component instance.
///
/// The instance carries no id: it is filed under one in
/// [`RobotParts::components`], and that key is the identity.
#[must_use]
pub fn component_instance(
    component_type: ComponentTypeId,
    mount_link: LinkId,
    direction_signs: BTreeMap<CapabilityId, i8>,
    roles: BTreeMap<CapabilityId, BTreeSet<CapabilityRole>>,
    driver: Option<serde_json::Value>,
) -> ComponentInstance {
    ComponentInstance::new(component_type, mount_link, direction_signs, roles, driver)
}

/// Assemble and validate the canonical robot.
///
/// # Errors
///
/// Returns the first [`ModelError`] the assembled model violates.
pub fn robot(parts: RobotParts) -> Result<Robot, ModelError> {
    // The footprint is a source/build product. A persisted manifest carries
    // this value (or an explicit `null`) and never reconstructs it from
    // collision geometry.
    let footprint = crate::model::footprint::compile(
        &parts.structure,
        &parts.components,
        &parts.component_types,
    )?;
    Robot::new(parts, footprint)
}
