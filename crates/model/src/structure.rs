//! Canonical normalized structure facts.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

use crate::asset::AssetId;
use crate::component::capability::StructuralKind;
use crate::error::{LinkRole, PoseOwner, StructureError};
use crate::identity::{JointId, LinkId};

const MIN_AXIS_NORM_SQUARED: f64 = 1.0e-16;
const PSD_RELATIVE_TOLERANCE: f64 = 1.0e-12;

/// The ground-projection frame every robot structure is rooted at.
pub(crate) const BASE_FOOTPRINT_LINK: &str = "base_footprint";
/// The body frame every robot structure attaches rigidly beneath the root.
pub(crate) const BASE_LINK: &str = "base_link";

/// A normalized robot structure without authored URDF or filesystem state.
#[derive(Clone, Debug)]
pub struct Structure {
    document: Value,
    links: Vec<Link>,
    joints: Vec<Joint>,
    materials: Vec<Material>,
    /// Resolved once, when the link graph is validated to be a single tree.
    /// Storing it here is what makes "exactly one root" an invariant of the
    /// type rather than something every later caller has to re-derive and then
    /// assert about.
    root: LinkId,
}

/// A canonical structural link.
#[derive(Clone, Debug)]
pub struct Link {
    name: LinkId,
    inertial: Inertial,
    visuals: Vec<Visual>,
    collisions: Vec<Collision>,
}

/// A canonical structural joint.
#[derive(Clone, Debug)]
pub struct Joint {
    name: JointId,
    kind: JointKind,
    origin: Pose,
    parent: LinkId,
    child: LinkId,
    axis: [f64; 3],
    limit: JointLimit,
    calibration: Option<Calibration>,
    dynamics: Option<Dynamics>,
    mimic: Option<Mimic>,
    safety: Option<Safety>,
}

/// A normalized rigid transform.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Pose {
    xyz: [f64; 3],
    rpy: [f64; 3],
}

/// Canonical mass properties for one link.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Inertial {
    origin: Pose,
    mass_kg: f64,
    inertia: Inertia,
}

/// Canonical symmetric inertia tensor.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Inertia {
    ixx: f64,
    ixy: f64,
    ixz: f64,
    iyy: f64,
    iyz: f64,
    izz: f64,
}

/// One canonical visual shape.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Visual {
    name: Option<String>,
    origin: Pose,
    geometry: Geometry,
    material: Option<Material>,
}

/// One canonical collision shape.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Collision {
    name: Option<String>,
    origin: Pose,
    geometry: Geometry,
}

/// Canonical render material.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Material {
    name: String,
    color: Option<[f64; 4]>,
    texture: Option<AssetId>,
}

/// Complete canonical geometry vocabulary.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Geometry {
    Box {
        size: [f64; 3],
    },
    Cylinder {
        radius: f64,
        length: f64,
    },
    Capsule {
        radius: f64,
        length: f64,
    },
    Sphere {
        radius: f64,
    },
    Mesh {
        #[serde(rename = "filename")]
        asset: AssetId,
        scale: Option<[f64; 3]>,
    },
}

/// Canonical joint limits.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JointLimit {
    lower: f64,
    upper: f64,
    effort: f64,
    velocity: f64,
}

/// Canonical joint calibration.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Calibration {
    rising: Option<f64>,
    falling: Option<f64>,
}

/// Canonical joint damping and friction.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Dynamics {
    damping: f64,
    friction: f64,
}

/// Canonical mimic relationship.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Mimic {
    joint: JointId,
    multiplier: Option<f64>,
    offset: Option<f64>,
}

/// Canonical software joint safety limits.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Safety {
    soft_lower_limit: f64,
    soft_upper_limit: f64,
    k_position: f64,
    k_velocity: f64,
}

/// Supported structural joint kinds.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JointKind {
    Revolute,
    Continuous,
    Prismatic,
    Fixed,
    Floating,
    Planar,
    Spherical,
}

impl Structure {
    /// Canonical links in deterministic document order.
    pub fn links(&self) -> impl ExactSizeIterator<Item = &Link> {
        self.links.iter()
    }

    /// Canonical joints in deterministic document order.
    pub fn joints(&self) -> impl ExactSizeIterator<Item = &Joint> {
        self.joints.iter()
    }

    /// Canonical materials in deterministic document order.
    pub fn materials(&self) -> impl ExactSizeIterator<Item = &Material> {
        self.materials.iter()
    }

    /// Distinct logical asset identities referenced by this structure.
    pub fn asset_ids(&self) -> impl Iterator<Item = &AssetId> {
        let mut seen = HashSet::new();
        self.links
            .iter()
            .flat_map(|link| {
                link.visuals()
                    .filter_map(|visual| visual.geometry().asset_id())
                    .chain(
                        link.collisions()
                            .filter_map(|collision| collision.geometry().asset_id()),
                    )
                    .chain(
                        link.visuals()
                            .filter_map(|visual| visual.material()?.texture()),
                    )
            })
            .chain(self.materials.iter().filter_map(Material::texture))
            .filter(move |id| seen.insert(id.as_str()))
    }

    /// Find a canonical link by identity.
    #[must_use]
    pub fn link(&self, id: &str) -> Option<&Link> {
        self.links.iter().find(|link| link.name == id)
    }

    /// Find a canonical joint by identity.
    #[must_use]
    pub fn joint(&self, id: &str) -> Option<&Joint> {
        self.joints.iter().find(|joint| joint.name == id)
    }

    /// The single root link, resolved when the structure was validated.
    #[must_use]
    pub const fn root_link(&self) -> &LinkId {
        &self.root
    }

    /// The joint that attaches `link` to its parent, absent only for the root.
    #[must_use]
    pub fn parent_joint(&self, link: &str) -> Option<&Joint> {
        self.joints.iter().find(|joint| joint.child() == link)
    }

    /// Every joint that attaches a child directly beneath `link`.
    pub fn child_joints<'a>(&'a self, link: &'a str) -> impl Iterator<Item = &'a Joint> {
        self.joints
            .iter()
            .filter(move |joint| joint.parent() == link)
    }

    /// Check the link graph is a single tree, and return its root.
    fn validate(links: &[Link], joints: &[Joint]) -> Result<LinkId, StructureError> {
        Self::validate_unique(
            links.iter().map(|link| link.name.as_str()),
            StructuralKind::Link,
        )?;
        Self::validate_unique(
            joints.iter().map(|joint| joint.name.as_str()),
            StructuralKind::Joint,
        )?;
        for link in links {
            link.validate()?;
        }
        let joint_names = joints
            .iter()
            .map(|joint| joint.name.as_str())
            .collect::<HashSet<_>>();
        for joint in joints {
            joint.validate(&joint_names)?;
        }
        let link_names = links.iter().map(Link::name).collect::<HashSet<_>>();
        let mut children = HashSet::new();
        let mut parent_by_child = HashMap::new();
        for joint in joints {
            for (role, link) in [
                (LinkRole::Parent, joint.parent()),
                (LinkRole::Child, joint.child()),
            ] {
                if !link_names.contains(link) {
                    return Err(StructureError::UnknownJointLink {
                        joint: joint.name().clone(),
                        role,
                        link: link.clone(),
                    });
                }
            }
            if !children.insert(joint.child()) {
                return Err(StructureError::MultipleParentJoints {
                    link: joint.child().clone(),
                });
            }
            if joint.parent() == joint.child() {
                return Err(StructureError::SelfReferentialJoint {
                    joint: joint.name().clone(),
                    link: joint.parent().clone(),
                });
            }
            parent_by_child.insert(joint.child(), joint.parent());
        }
        let roots = links
            .iter()
            .map(Link::name)
            .filter(|link| !children.contains(link))
            .collect::<Vec<_>>();
        let [root] = roots.as_slice() else {
            return Err(StructureError::RootLinkCount { found: roots.len() });
        };
        for link in links {
            let mut seen = HashSet::new();
            let mut current = Some(link.name());
            while let Some(link_id) = current {
                if !seen.insert(link_id) {
                    return Err(StructureError::JointCycle {
                        link: link_id.clone(),
                    });
                }
                current = parent_by_child.get(link_id).copied();
            }
        }
        Ok((*root).clone())
    }

    fn validate_unique<'a>(
        names: impl Iterator<Item = &'a str>,
        kind: StructuralKind,
    ) -> Result<(), StructureError> {
        let mut seen = HashSet::new();
        for name in names {
            if !seen.insert(name) {
                return Err(StructureError::DuplicateIdentity {
                    kind,
                    name: name.to_string(),
                });
            }
        }
        Ok(())
    }

    /// Check the conventions every *robot* structure must follow: a
    /// ground-projection root with the body frame rigidly beneath it.
    pub(crate) fn validate_robot_frames(&self) -> Result<(), StructureError> {
        if self.root != BASE_FOOTPRINT_LINK {
            return Err(StructureError::RootLinkName {
                expected: LinkId::new(BASE_FOOTPRINT_LINK),
                found: self.root.clone(),
            });
        }
        let base_joint = self
            .joints
            .iter()
            .find(|joint| joint.child() == BASE_LINK)
            .ok_or(StructureError::MissingBaseLink)?;
        if base_joint.parent() != BASE_FOOTPRINT_LINK || base_joint.kind() != JointKind::Fixed {
            return Err(StructureError::MisattachedBaseLink);
        }
        Ok(())
    }

    pub(crate) fn from_compiler_value(document: Value) -> Result<Self, StructureError> {
        Self::from_summary(serde_json::from_value(document)?)
    }

    fn from_summary(summary: Summary) -> Result<Self, StructureError> {
        let document = serde_json::to_value(&summary)?;
        let links = summary
            .links
            .into_iter()
            .map(|link| Link {
                name: link.name,
                inertial: link.inertial,
                visuals: link.visuals,
                collisions: link.collisions,
            })
            .collect::<Vec<_>>();
        let joints = summary
            .joints
            .into_iter()
            .map(|joint| Joint {
                name: joint.name,
                kind: joint.kind,
                origin: Pose {
                    xyz: joint.origin.xyz,
                    rpy: joint.origin.rpy,
                },
                parent: joint.parent,
                child: joint.child,
                axis: joint.axis,
                limit: joint.limit,
                calibration: joint.calibration,
                dynamics: joint.dynamics,
                mimic: joint.mimic,
                safety: joint.safety,
            })
            .collect::<Vec<_>>();
        let root = Self::validate(&links, &joints)?;
        Ok(Self {
            document,
            links,
            joints,
            materials: summary.materials,
            root,
        })
    }
}

impl Link {
    #[must_use]
    pub const fn name(&self) -> &LinkId {
        &self.name
    }

    #[must_use]
    pub const fn inertial(&self) -> Inertial {
        self.inertial
    }

    pub fn visuals(&self) -> impl ExactSizeIterator<Item = &Visual> {
        self.visuals.iter()
    }

    pub fn collisions(&self) -> impl ExactSizeIterator<Item = &Collision> {
        self.collisions.iter()
    }

    fn validate(&self) -> Result<(), StructureError> {
        self.inertial.validate(&self.name)?;
        for visual in &self.visuals {
            visual
                .origin
                .validate(PoseOwner::LinkVisual(self.name.clone()))?;
            visual.geometry.validate(&self.name)?;
        }
        for collision in &self.collisions {
            collision
                .origin
                .validate(PoseOwner::LinkCollision(self.name.clone()))?;
            collision.geometry.validate(&self.name)?;
        }
        Ok(())
    }
}

impl Joint {
    #[must_use]
    pub const fn name(&self) -> &JointId {
        &self.name
    }
    #[must_use]
    pub const fn kind(&self) -> JointKind {
        self.kind
    }
    #[must_use]
    pub const fn origin(&self) -> Pose {
        self.origin
    }
    #[must_use]
    pub const fn parent(&self) -> &LinkId {
        &self.parent
    }
    #[must_use]
    pub const fn child(&self) -> &LinkId {
        &self.child
    }
    #[must_use]
    pub const fn axis(&self) -> [f64; 3] {
        self.axis
    }

    #[must_use]
    pub const fn limit(&self) -> JointLimit {
        self.limit
    }

    #[must_use]
    pub const fn calibration(&self) -> Option<Calibration> {
        self.calibration
    }

    #[must_use]
    pub const fn dynamics(&self) -> Option<Dynamics> {
        self.dynamics
    }

    #[must_use]
    pub fn mimic(&self) -> Option<&Mimic> {
        self.mimic.as_ref()
    }

    #[must_use]
    pub const fn safety(&self) -> Option<Safety> {
        self.safety
    }

    fn validate(&self, joint_names: &HashSet<&str>) -> Result<(), StructureError> {
        self.origin.validate(PoseOwner::Joint(self.name.clone()))?;
        if !self.axis.iter().all(|value| value.is_finite()) {
            return Err(StructureError::AxisNotFinite {
                joint: self.name.clone(),
            });
        }
        // A movable joint with a degenerate axis has no defined direction of
        // motion, so it cannot be commanded or measured.
        if self.kind != JointKind::Fixed
            && self.axis.iter().map(|value| value * value).sum::<f64>() <= MIN_AXIS_NORM_SQUARED
        {
            return Err(StructureError::AxisNotOriented {
                joint: self.name.clone(),
            });
        }
        self.limit.validate(&self.name)?;
        if let Some(dynamics) = self.dynamics {
            dynamics.validate(&self.name)?;
        }
        if let Some(mimic) = &self.mimic
            && !joint_names.contains(mimic.joint().as_str())
        {
            return Err(StructureError::UnknownMimicJoint {
                joint: self.name.clone(),
                mimicked: mimic.joint().clone(),
            });
        }
        if let Some(safety) = self.safety {
            safety.validate(&self.name)?;
        }
        Ok(())
    }
}

impl Pose {
    #[must_use]
    pub const fn xyz(self) -> [f64; 3] {
        self.xyz
    }
    #[must_use]
    pub const fn rpy(self) -> [f64; 3] {
        self.rpy
    }

    fn validate(self, owner: PoseOwner) -> Result<(), StructureError> {
        if self.xyz.into_iter().chain(self.rpy).all(f64::is_finite) {
            Ok(())
        } else {
            Err(StructureError::Pose { owner })
        }
    }
}

impl Inertial {
    #[must_use]
    pub const fn origin(self) -> Pose {
        self.origin
    }
    #[must_use]
    pub const fn mass_kg(self) -> f64 {
        self.mass_kg
    }
    #[must_use]
    pub const fn inertia(self) -> Inertia {
        self.inertia
    }

    fn validate(self, link: &LinkId) -> Result<(), StructureError> {
        self.origin
            .validate(PoseOwner::LinkInertial(link.clone()))?;
        if !(self.mass_kg.is_finite() && self.mass_kg >= 0.0) {
            return Err(StructureError::Mass { link: link.clone() });
        }
        self.inertia.validate(link)
    }
}

impl Inertia {
    #[must_use]
    pub const fn values(self) -> [f64; 6] {
        [self.ixx, self.ixy, self.ixz, self.iyy, self.iyz, self.izz]
    }

    /// Check the tensor describes a physically realizable body.
    ///
    /// Positive semidefiniteness is tested through the leading principal
    /// minors, with tolerances scaled by the largest entry so that a tensor
    /// authored in kg*m^2 and the same tensor authored in g*mm^2 are judged
    /// alike rather than by an absolute epsilon.
    fn validate(self, link: &LinkId) -> Result<(), StructureError> {
        let [ixx, ixy, ixz, iyy, iyz, izz] = self.values();
        let finite = self.values().into_iter().all(f64::is_finite);
        let scale = self
            .values()
            .into_iter()
            .map(f64::abs)
            .fold(0.0, f64::max)
            .max(f64::MIN_POSITIVE);
        let diagonal_tolerance = PSD_RELATIVE_TOLERANCE * scale;
        let minor_tolerance = PSD_RELATIVE_TOLERANCE * scale.powi(2);
        let determinant_tolerance = PSD_RELATIVE_TOLERANCE * scale.powi(3);
        let principal_xy = ixx * iyy - ixy * ixy;
        let principal_xz = ixx * izz - ixz * ixz;
        let principal_yz = iyy * izz - iyz * iyz;
        let determinant = ixx * (iyy * izz - iyz * iyz) - ixy * (ixy * izz - iyz * ixz)
            + ixz * (ixy * iyz - iyy * ixz);
        if finite
            && ixx >= -diagonal_tolerance
            && iyy >= -diagonal_tolerance
            && izz >= -diagonal_tolerance
            && principal_xy >= -minor_tolerance
            && principal_xz >= -minor_tolerance
            && principal_yz >= -minor_tolerance
            && determinant >= -determinant_tolerance
        {
            Ok(())
        } else {
            Err(StructureError::Inertia { link: link.clone() })
        }
    }
}

impl Visual {
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
    #[must_use]
    pub const fn origin(&self) -> Pose {
        self.origin
    }
    #[must_use]
    pub fn geometry(&self) -> &Geometry {
        &self.geometry
    }
    #[must_use]
    pub fn material(&self) -> Option<&Material> {
        self.material.as_ref()
    }
}

impl Collision {
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
    #[must_use]
    pub const fn origin(&self) -> Pose {
        self.origin
    }
    #[must_use]
    pub fn geometry(&self) -> &Geometry {
        &self.geometry
    }
}

impl Material {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
    #[must_use]
    pub const fn color(&self) -> Option<[f64; 4]> {
        self.color
    }
    #[must_use]
    pub fn texture(&self) -> Option<&AssetId> {
        self.texture.as_ref()
    }
}

impl Geometry {
    #[must_use]
    pub fn asset_id(&self) -> Option<&AssetId> {
        match self {
            Self::Mesh { asset, .. } => Some(asset),
            _ => None,
        }
    }

    fn validate(&self, link: &LinkId) -> Result<(), StructureError> {
        let dimensions: &[f64] = match self {
            Self::Box { size } => size,
            Self::Cylinder { radius, length } | Self::Capsule { radius, length } => {
                &[*radius, *length]
            }
            Self::Sphere { radius } => &[*radius],
            Self::Mesh { scale, .. } => scale.as_ref().map_or(&[], |values| values.as_slice()),
        };
        if dimensions
            .iter()
            .all(|value| value.is_finite() && *value > 0.0)
        {
            Ok(())
        } else {
            Err(StructureError::Geometry { link: link.clone() })
        }
    }
}

impl JointLimit {
    #[must_use]
    pub const fn lower(self) -> f64 {
        self.lower
    }
    #[must_use]
    pub const fn upper(self) -> f64 {
        self.upper
    }
    #[must_use]
    pub const fn effort(self) -> f64 {
        self.effort
    }
    #[must_use]
    pub const fn velocity(self) -> f64 {
        self.velocity
    }

    fn validate(self, joint: &JointId) -> Result<(), StructureError> {
        if [self.lower, self.upper, self.effort, self.velocity]
            .iter()
            .all(|value| value.is_finite())
            && self.lower <= self.upper
        {
            Ok(())
        } else {
            Err(StructureError::JointLimits {
                joint: joint.clone(),
            })
        }
    }
}

impl Calibration {
    #[must_use]
    pub const fn rising(self) -> Option<f64> {
        self.rising
    }
    #[must_use]
    pub const fn falling(self) -> Option<f64> {
        self.falling
    }
}

impl Dynamics {
    #[must_use]
    pub const fn damping(self) -> f64 {
        self.damping
    }
    #[must_use]
    pub const fn friction(self) -> f64 {
        self.friction
    }

    fn validate(self, joint: &JointId) -> Result<(), StructureError> {
        if [self.damping, self.friction]
            .iter()
            .all(|value| value.is_finite() && *value >= 0.0)
        {
            Ok(())
        } else {
            Err(StructureError::JointDynamics {
                joint: joint.clone(),
            })
        }
    }
}

impl Mimic {
    #[must_use]
    pub const fn joint(&self) -> &JointId {
        &self.joint
    }
    #[must_use]
    pub const fn multiplier(&self) -> Option<f64> {
        self.multiplier
    }
    #[must_use]
    pub const fn offset(&self) -> Option<f64> {
        self.offset
    }
}

impl Safety {
    #[must_use]
    pub const fn soft_lower_limit(self) -> f64 {
        self.soft_lower_limit
    }
    #[must_use]
    pub const fn soft_upper_limit(self) -> f64 {
        self.soft_upper_limit
    }
    #[must_use]
    pub const fn k_position(self) -> f64 {
        self.k_position
    }
    #[must_use]
    pub const fn k_velocity(self) -> f64 {
        self.k_velocity
    }

    fn validate(self, joint: &JointId) -> Result<(), StructureError> {
        if [
            self.soft_lower_limit,
            self.soft_upper_limit,
            self.k_position,
            self.k_velocity,
        ]
        .iter()
        .all(|value| value.is_finite())
            && self.soft_lower_limit <= self.soft_upper_limit
        {
            Ok(())
        } else {
            Err(StructureError::JointSafety {
                joint: joint.clone(),
            })
        }
    }
}

impl Serialize for Structure {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.document.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Structure {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let summary = Summary::deserialize(deserializer)?;
        Self::from_summary(summary).map_err(serde::de::Error::custom)
    }
}

/// The canonical structure document, exactly as the compiler emits it.
///
/// `Structure` re-serializes this document verbatim, so every authored field
/// has to survive the round trip - including `name`, which the runtime model
/// itself never consults but must not drop from the document it forwards.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Summary {
    name: String,
    links: Vec<LinkSummary>,
    joints: Vec<JointSummary>,
    materials: Vec<Material>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LinkSummary {
    name: LinkId,
    inertial: Inertial,
    visuals: Vec<Visual>,
    collisions: Vec<Collision>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct JointSummary {
    name: JointId,
    kind: JointKind,
    origin: PoseSummary,
    parent: LinkId,
    child: LinkId,
    axis: [f64; 3],
    limit: JointLimit,
    calibration: Option<Calibration>,
    dynamics: Option<Dynamics>,
    mimic: Option<Mimic>,
    safety: Option<Safety>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PoseSummary {
    xyz: [f64; 3],
    rpy: [f64; 3],
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn inertial() -> Value {
        json!({
            "origin": { "xyz": [0.0, 0.0, 0.0], "rpy": [0.0, 0.0, 0.0] },
            "mass_kg": 1.0,
            "inertia": { "ixx": 1.0, "ixy": 0.0, "ixz": 0.0, "iyy": 1.0, "iyz": 0.0, "izz": 1.0 }
        })
    }

    fn link(name: &str) -> Value {
        json!({ "name": name, "inertial": inertial(), "visuals": [], "collisions": [] })
    }

    fn joint(name: &str, parent: &str, child: &str) -> Value {
        json!({
            "name": name,
            "kind": "fixed",
            "origin": { "xyz": [0.0, 0.0, 0.0], "rpy": [0.0, 0.0, 0.0] },
            "parent": parent,
            "child": child,
            "axis": [0.0, 0.0, 1.0],
            "limit": { "lower": 0.0, "upper": 0.0, "effort": 0.0, "velocity": 0.0 },
            "calibration": null,
            "dynamics": null,
            "mimic": null,
            "safety": null
        })
    }

    fn document(links: Vec<Value>, joints: Vec<Value>) -> Value {
        json!({ "name": "rover", "links": links, "joints": joints, "materials": [] })
    }

    fn tree() -> Value {
        document(
            vec![link("base_footprint"), link("base_link")],
            vec![joint("base_joint", "base_footprint", "base_link")],
        )
    }

    #[test]
    fn the_document_survives_the_round_trip_unchanged() {
        // The canonical structure is forwarded verbatim: typing the link and
        // joint identities must not alter a single field of the wire form,
        // including `name`, which nothing in the runtime model reads.
        let source = tree();
        let structure =
            Structure::from_compiler_value(source.clone()).expect("a valid structure document");
        assert_eq!(
            serde_json::to_value(&structure).expect("the structure re-serializes"),
            source
        );
    }

    #[test]
    fn the_root_is_resolved_once_and_stored() {
        let structure = Structure::from_compiler_value(tree()).expect("a valid structure document");
        assert_eq!(structure.root_link(), &LinkId::new("base_footprint"));
        structure
            .validate_robot_frames()
            .expect("the conventional robot frames");
    }

    #[test]
    fn a_structure_without_exactly_one_root_is_refused() {
        let two_roots = document(vec![link("a"), link("b")], Vec::new());
        assert!(matches!(
            Structure::from_compiler_value(two_roots),
            Err(StructureError::RootLinkCount { found: 2 })
        ));

        let no_root = document(
            vec![link("a"), link("b")],
            vec![joint("ab", "a", "b"), joint("ba", "b", "a")],
        );
        // Every link is somebody's child, so the graph is a cycle, not a tree.
        assert!(matches!(
            Structure::from_compiler_value(no_root),
            Err(StructureError::RootLinkCount { found: 0 })
        ));
    }

    #[test]
    fn a_dangling_joint_names_the_end_that_dangles() {
        let dangling = document(
            vec![link("base_footprint")],
            vec![joint("base_joint", "base_footprint", "base_link")],
        );
        let error =
            Structure::from_compiler_value(dangling).expect_err("the child link does not exist");
        assert!(matches!(
            error,
            StructureError::UnknownJointLink {
                role: LinkRole::Child,
                ..
            }
        ));
        assert_eq!(
            error.to_string(),
            "joint 'base_joint' references unknown child link 'base_link'"
        );
    }

    /// Every structural fact the canonical model carries must be statable
    /// through [`crate::builder::RobotBuilder`]. A field the builder cannot
    /// state is a robot that can only be described by authored documents, and
    /// the builder silently substituting a default for it is worse still.
    ///
    /// This test lives beside the definitions rather than beside the builder
    /// because only this module may destructure [`Link`] and [`Joint`], whose
    /// fields are private. That is what makes the guarantee a compile-time one:
    /// adding a field to either stops this test compiling until the builder can
    /// state it and the assertions below read it back. The [`Geometry`] match
    /// is exhaustive for the same reason.
    #[test]
    fn every_structural_field_is_statable_through_the_builder() {
        use crate::asset::AssetId;
        use crate::builder;

        let mesh = AssetId::new("meshes/mast.stl").expect("a normalized asset id");
        let texture = AssetId::new("textures/paint.png").expect("a normalized asset id");
        let robot = builder::RobotBuilder::new("rover")
            // Spelled field by field rather than with `..Joint::default()`, so
            // that a field added to the builder's own `Joint` fails here too.
            .joint(builder::Joint {
                name: "mast_joint",
                kind: JointKind::Revolute,
                parent: "base_link",
                child: "mast",
                xyz: [0.1, 0.2, 0.3],
                rpy: [0.4, 0.5, 0.6],
                axis: [0.0, 1.0, 0.0],
                limit: builder::JointLimit {
                    lower: -1.5,
                    upper: 1.5,
                    effort: 8.0,
                    velocity: 2.0,
                },
                calibration: Some(builder::Calibration {
                    rising: Some(0.1),
                    falling: Some(-0.1),
                }),
                dynamics: Some(builder::Dynamics {
                    damping: 0.7,
                    friction: 0.2,
                }),
                mimic: Some(builder::Mimic {
                    joint: "base_joint",
                    multiplier: Some(2.0),
                    offset: Some(0.25),
                }),
                safety: Some(builder::Safety {
                    soft_lower_limit: -1.4,
                    soft_upper_limit: 1.4,
                    k_position: 12.0,
                    k_velocity: 3.0,
                }),
            })
            .link(builder::Link {
                name: "mast",
                inertial: builder::Inertial {
                    xyz: [0.01, 0.02, 0.03],
                    rpy: [0.04, 0.05, 0.06],
                    mass_kg: 2.5,
                    inertia: builder::Inertia {
                        ixx: 2.0,
                        ixy: 0.1,
                        ixz: 0.2,
                        iyy: 3.0,
                        iyz: 0.3,
                        izz: 4.0,
                    },
                },
                // One visual per geometry variant, in the order the match
                // below expects them back.
                visuals: vec![
                    builder::Visual {
                        name: Some("hull"),
                        xyz: [1.0, 2.0, 3.0],
                        rpy: [0.7, 0.8, 0.9],
                        geometry: Geometry::Box {
                            size: [0.4, 0.3, 0.2],
                        },
                        material: Some(builder::Material {
                            name: "grey",
                            color: Some([0.5, 0.5, 0.5, 1.0]),
                            texture: Some(texture.clone()),
                        }),
                    },
                    builder::Visual::new(Geometry::Cylinder {
                        radius: 0.1,
                        length: 0.5,
                    }),
                    builder::Visual::new(Geometry::Capsule {
                        radius: 0.2,
                        length: 0.6,
                    }),
                    builder::Visual::new(Geometry::Sphere { radius: 0.3 }),
                    builder::Visual::new(Geometry::Mesh {
                        asset: mesh.clone(),
                        scale: Some([1.0, 2.0, 3.0]),
                    }),
                ],
                // A collision beneath the revolute mast is intentionally not
                // buildable: the source compiler must refuse an envelope it
                // cannot conservatively derive. The empty collection still
                // states the builder field without weakening that invariant.
                collisions: Vec::new(),
            })
            // Keep the full collision wire shape covered on a fixed link: it
            // contributes to the compiled envelope without depending on the
            // mast's runtime pose.
            .link(builder::Link {
                name: "base_link",
                collisions: vec![builder::Collision {
                    name: Some("hull_bounds"),
                    xyz: [4.0, 5.0, 6.0],
                    rpy: [0.11, 0.12, 0.13],
                    geometry: Geometry::Sphere { radius: 0.35 },
                }],
                ..builder::Link::default()
            })
            .material(builder::Material {
                name: "grey",
                color: Some([0.5, 0.5, 0.5, 1.0]),
                texture: Some(texture.clone()),
            })
            .build()
            .expect("every structural fact composes a valid robot");

        let structure = robot.structure();
        let link = structure.link("mast").expect("the stated link");
        let Link {
            name,
            inertial,
            visuals,
            collisions,
        } = link;
        assert_eq!(name, &LinkId::new("mast"));
        assert_eq!(inertial.origin().xyz(), [0.01, 0.02, 0.03]);
        assert_eq!(inertial.origin().rpy(), [0.04, 0.05, 0.06]);
        assert_eq!(inertial.mass_kg(), 2.5);
        assert_eq!(inertial.inertia().values(), [2.0, 0.1, 0.2, 3.0, 0.3, 4.0]);
        assert_eq!(visuals.len(), 5);
        assert_eq!(collisions.len(), 0);

        let mut shapes = Vec::new();
        for visual in link.visuals() {
            let Visual {
                name,
                origin,
                geometry,
                material,
            } = visual;
            shapes.push(match geometry {
                Geometry::Box { size } => {
                    assert_eq!(*size, [0.4, 0.3, 0.2]);
                    // Only the first visual states the rest; the others prove
                    // the shape alone is enough.
                    assert_eq!(name.as_deref(), Some("hull"));
                    assert_eq!(origin.xyz(), [1.0, 2.0, 3.0]);
                    assert_eq!(origin.rpy(), [0.7, 0.8, 0.9]);
                    let Material {
                        name,
                        color,
                        texture: painted,
                    } = material.as_ref().expect("the stated material");
                    assert_eq!(name, "grey");
                    assert_eq!(*color, Some([0.5, 0.5, 0.5, 1.0]));
                    assert_eq!(painted.as_ref(), Some(&texture));
                    "box"
                }
                Geometry::Cylinder { radius, length } => {
                    assert_eq!((*radius, *length), (0.1, 0.5));
                    "cylinder"
                }
                Geometry::Capsule { radius, length } => {
                    assert_eq!((*radius, *length), (0.2, 0.6));
                    "capsule"
                }
                Geometry::Sphere { radius } => {
                    assert_eq!(*radius, 0.3);
                    "sphere"
                }
                Geometry::Mesh { asset, scale } => {
                    assert_eq!(asset, &mesh);
                    assert_eq!(*scale, Some([1.0, 2.0, 3.0]));
                    "mesh"
                }
            });
        }
        assert_eq!(shapes, ["box", "cylinder", "capsule", "sphere", "mesh"]);

        let collision = structure
            .link("base_link")
            .expect("the base link")
            .collisions()
            .next()
            .expect("the stated collision");
        let Collision {
            name,
            origin,
            geometry,
        } = collision;
        assert_eq!(name.as_deref(), Some("hull_bounds"));
        assert_eq!(origin.xyz(), [4.0, 5.0, 6.0]);
        assert_eq!(origin.rpy(), [0.11, 0.12, 0.13]);
        assert!(matches!(geometry, Geometry::Sphere { radius } if *radius == 0.35));

        let joint = structure.joint("mast_joint").expect("the stated joint");
        let Joint {
            name,
            kind,
            origin,
            parent,
            child,
            axis,
            limit,
            calibration,
            dynamics,
            mimic,
            safety,
        } = joint;
        assert_eq!(name, &JointId::new("mast_joint"));
        assert_eq!(*kind, JointKind::Revolute);
        assert_eq!(origin.xyz(), [0.1, 0.2, 0.3]);
        assert_eq!(origin.rpy(), [0.4, 0.5, 0.6]);
        assert_eq!(parent, &LinkId::new("base_link"));
        assert_eq!(child, &LinkId::new("mast"));
        assert_eq!(*axis, [0.0, 1.0, 0.0]);
        assert_eq!(
            (
                limit.lower(),
                limit.upper(),
                limit.effort(),
                limit.velocity()
            ),
            (-1.5, 1.5, 8.0, 2.0)
        );
        let calibration = calibration.expect("the stated calibration");
        assert_eq!(
            (calibration.rising(), calibration.falling()),
            (Some(0.1), Some(-0.1))
        );
        let dynamics = dynamics.expect("the stated dynamics");
        assert_eq!((dynamics.damping(), dynamics.friction()), (0.7, 0.2));
        let mimic = mimic.as_ref().expect("the stated mimic");
        assert_eq!(mimic.joint(), &JointId::new("base_joint"));
        assert_eq!(
            (mimic.multiplier(), mimic.offset()),
            (Some(2.0), Some(0.25))
        );
        let safety = safety.expect("the stated safety");
        assert_eq!(
            (
                safety.soft_lower_limit(),
                safety.soft_upper_limit(),
                safety.k_position(),
                safety.k_velocity()
            ),
            (-1.4, 1.4, 12.0, 3.0)
        );

        // The structure's own material catalogue, and the assets everything
        // above declares, come back too.
        let catalogue = structure.materials().collect::<Vec<_>>();
        assert_eq!(catalogue.len(), 1);
        assert_eq!(catalogue[0].name(), "grey");
        assert_eq!(catalogue[0].texture(), Some(&texture));
        let mut assets = structure
            .asset_ids()
            .map(AssetId::as_str)
            .collect::<Vec<_>>();
        assets.sort_unstable();
        assert_eq!(assets, ["meshes/mast.stl", "textures/paint.png"]);
    }

    #[test]
    fn a_robot_structure_must_carry_the_conventional_base_frames() {
        let wrong_root = document(vec![link("chassis")], Vec::new());
        let structure =
            Structure::from_compiler_value(wrong_root).expect("a single-link structure is a tree");
        assert!(matches!(
            structure.validate_robot_frames(),
            Err(StructureError::RootLinkName { .. })
        ));

        let no_base_link = document(
            vec![link("base_footprint"), link("mast")],
            vec![joint("mast_joint", "base_footprint", "mast")],
        );
        let structure =
            Structure::from_compiler_value(no_base_link).expect("a valid structure document");
        assert!(matches!(
            structure.validate_robot_frames(),
            Err(StructureError::MissingBaseLink)
        ));
    }
}
