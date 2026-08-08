//! The one construction seam for the canonical model.
//!
//! Nothing in this crate can build a [`Robot`], a [`Component`], a
//! [`Simulation`] or a [`Structure`] from raw values: they are only ever
//! produced by normalizing authored documents, and that normalizer lives in
//! `phoxal-manifest`, a separate crate. Rust has no visibility that means
//! "one other crate", so these entry points are `pub` and hidden rather than
//! `pub(crate)`.
//!
//! `phoxal-manifest` is the only permitted caller outside this crate. This is
//! not runtime API: a participant receives an already-built [`Robot`] and reads
//! it through the runtime modules. Every entry point here still runs the full
//! validation, so calling one cannot produce a model the runtime would reject -
//! which is also why the feature-gated `test_builder` assembles its in-memory
//! robots through these same entry points rather than around them.

use std::collections::BTreeMap;

use crate::component::Component;
use crate::component::capability::Capability;
use crate::error::ModelError;
use crate::identity::{CapabilityId, ComponentInstanceId, ComponentTypeId, LinkId, RobotId};
use crate::robot::{Clock, ComponentInstance, KinematicConfig, MotionLimits, Robot};
use crate::simulation::{self, Simulation};
use crate::structure::Structure;

/// The normalized inputs a canonical [`Robot`] is assembled from.
///
/// A plain field bag rather than a builder: the compiler produces all of it in
/// one pass, and [`robot`] validates the whole before any of it is observable.
pub struct RobotParts {
    pub id: RobotId,
    pub clock: Clock,
    pub kinematic: KinematicConfig,
    pub motion_limits: MotionLimits,
    pub component_instances: BTreeMap<ComponentInstanceId, ComponentInstance>,
    pub component_types: BTreeMap<ComponentTypeId, Component>,
    pub simulation_types: BTreeMap<ComponentTypeId, Simulation>,
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

/// Build one component type from its normalized capabilities and structure.
#[must_use]
pub fn component(
    capabilities: BTreeMap<CapabilityId, Capability>,
    structure: Structure,
) -> Component {
    Component::new(capabilities, structure)
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
#[must_use]
pub fn component_instance(
    id: ComponentInstanceId,
    component_type: ComponentTypeId,
    mount_link: LinkId,
    direction_signs: BTreeMap<CapabilityId, i8>,
) -> ComponentInstance {
    ComponentInstance::new(id, component_type, mount_link, direction_signs)
}

/// Assemble and validate the canonical robot.
///
/// # Errors
///
/// Returns the first [`ModelError`] the assembled model violates.
pub fn robot(parts: RobotParts) -> Result<Robot, ModelError> {
    let footprint = match crate::footprint::compile(
        &parts.structure,
        &parts.component_instances,
        &parts.component_types,
    ) {
        Ok(footprint) => footprint,
        // Generic model builders historically accept movable collision
        // geometry. Stock safety rejects it at the manifest boundary, while
        // retaining a `None` envelope here makes those non-safety model tests
        // and legacy runtime documents explicit fail-closed values.
        Err(ModelError::FootprintMovableJoint { .. } | ModelError::FootprintMesh { .. }) => None,
        Err(error) => return Err(error),
    };
    Robot::new(parts, footprint)
}
