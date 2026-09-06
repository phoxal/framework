//! The version-independent form every authored document is compiled from.
//!
//! An authored document's schema tag names the *source language* it is written
//! in, and each generation of that language owns its own syntax, its own
//! defaults and its own rules. Normalization is where those end: a versioned
//! DTO resolves its spellings and defaults, and produces the values in this
//! module. Everything below this line - the canonical model assembly, the
//! source-owned service and driver facts, the asset staging - reads only these
//! types and therefore has no opinion about which generation authored the
//! document.
//!
//! That is the whole point of the boundary: a new source generation is a new
//! DTO plus a new `normalize`, never a second copy of the compiler.
//!
//! These types are deliberately not a second canonical model. They carry the
//! authored facts that survive normalization and nothing else: identifiers stay
//! plain strings, because turning one into a canonical identity is the
//! canonical model's rejection to make, and paths stay relative, because
//! resolving one against a root is the compiler's job.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crate::model::CapabilityRole;
use crate::model::component::capability::CapabilityKind;
use crate::model::identity::{CapabilityId, LinkId};
use crate::model::kinematics::{KinematicConfig, MotionLimits};

use crate::authoring::source::robot::driver::DriverConfig;

/// One authored robot, in the form the compiler consumes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Robot {
    /// The authored robot identifier, still unvalidated as an identity.
    pub id: String,
    /// The URDF structure path, relative to the resolved robot root.
    pub structure: PathBuf,
    pub kinematic: KinematicConfig,
    pub motion_limits: MotionLimits,
    /// Mounted component instances, keyed by authored instance id.
    pub instances: BTreeMap<String, ComponentInstance>,
    /// The authored service entries, keyed by service id, each with its own
    /// configuration. This is the authored half alone; the official set the
    /// compiler merges in is not here.
    pub services: BTreeMap<String, Option<serde_json::Value>>,
}

/// One mounted component instance.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ComponentInstance {
    pub component_type: String,
    pub mount_link: String,
    /// Present exactly when this instance declares a component driver: how it
    /// is wired to the machine, and the driver binary's own configuration.
    pub driver: Option<DriverConfig>,
    /// What each capability on this instance is declared to be for. The
    /// authored order is gone: a role set is a set, and the document grammar is
    /// what rejects a repeat.
    pub roles: BTreeMap<String, BTreeSet<CapabilityRole>>,
    /// Per-capability instance overrides, with every authored spelling and
    /// default already resolved.
    pub parameters: BTreeMap<String, CapabilityParameters>,
}

/// The resolved per-instance overrides for one capability.
///
/// The authored grammar carries one parameter block per capability kind; what
/// survives normalization is the kind it claims and the values the compiler
/// acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CapabilityParameters {
    pub kind: CapabilityKind,
    pub direction_sign: i8,
}

/// One authored component type, in the form the compiler consumes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Component {
    pub capabilities: BTreeMap<CapabilityId, crate::model::component::capability::Capability>,
}

/// One authored component type's simulated behaviour.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Simulation {
    pub capabilities: BTreeMap<CapabilityId, crate::model::simulation::Capability>,
    pub links: BTreeMap<LinkId, Option<String>>,
}

/// World facts after source defaults and spellings have been resolved.
#[derive(Debug, Clone)]
pub(crate) struct World {
    pub id: String,
    pub time_step_ms: u64,
    pub gravity_mps2: [f64; 3],
    pub assets: BTreeMap<String, WorldAsset>,
    pub spawn_points: BTreeMap<String, crate::model::structure::Pose>,
    pub entities: BTreeMap<String, WorldEntity>,
}

#[derive(Debug, Clone)]
pub(crate) struct WorldAsset {
    pub geometry: WorldGeometry,
    pub collision: WorldGeometry,
}

#[derive(Debug, Clone)]
pub(crate) struct WorldEntity {
    pub asset: String,
    pub instances: Vec<crate::model::structure::Pose>,
}

/// Primitives are already canonical; only mesh source paths need compilation.
#[derive(Debug, Clone)]
pub(crate) enum WorldGeometry {
    Primitive(crate::model::geometry::Geometry),
    Mesh {
        path: PathBuf,
        scale: Option<[f64; 3]>,
    },
}

impl Robot {
    /// Every component type this robot mounts at least one instance of.
    pub(crate) fn used_component_types(&self) -> BTreeSet<&str> {
        self.instances
            .values()
            .map(|instance| instance.component_type.as_str())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use crate::authoring::source::robot::Manifest;

    /// Two spellings of one robot that differ only in the order an author
    /// happened to write things down.
    fn document(components: &str, services: &str, roles: &str) -> String {
        format!(
            r#"
schema: phoxal/robot/v0
robot:
  id: order-bot
  motion_limits:
    max_linear_speed_mps: 0.6
    max_angular_speed_radps: 2.0
  kinematic:
    kind: omnidirectional
    actuators: [alpha.motor]
    encoders: []
  components:
{components}
services:
{services}
"#
        )
        .replace("<ROLES>", roles)
    }

    const ALPHA: &str = "    alpha:\n      component: drive\n      mount_link: alpha_mount\n      \
                         roles:\n        range: <ROLES>\n";
    const BETA: &str = "    beta:\n      component: sensor\n      mount_link: beta_mount\n";

    /// Normalization is a pure restatement of the document, so the same
    /// document always produces the same value, and two documents that differ
    /// only in authored order produce the same value too. Everything the
    /// compiler reads afterwards - the canonical model, the compiled assets -
    /// inherits that determinism from here.
    #[test]
    fn normalization_is_deterministic_and_independent_of_authored_order() -> anyhow::Result<()> {
        let first = Manifest::parse(&document(
            &[ALPHA, BETA].concat(),
            "  localize: {}\n  map: {}\n",
            "[mapping, safety]",
        ))?
        .normalize()?;
        let again = Manifest::parse(&document(
            &[ALPHA, BETA].concat(),
            "  localize: {}\n  map: {}\n",
            "[mapping, safety]",
        ))?
        .normalize()?;
        assert_eq!(first, again, "the same document normalizes to one value");

        let reordered = Manifest::parse(&document(
            &[BETA, ALPHA].concat(),
            "  map: {}\n  localize: {}\n",
            "[safety, mapping]",
        ))?
        .normalize()?;
        assert_eq!(
            reordered, first,
            "authored order is not a fact that survives normalization"
        );

        assert_eq!(
            first.instances.keys().collect::<Vec<_>>(),
            ["alpha", "beta"]
        );
        assert_eq!(
            first.services.keys().collect::<Vec<_>>(),
            ["localize", "map"]
        );
        Ok(())
    }
}
