//! Canonical normalized structure facts.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

use crate::{AssetId, ModelError};

const MIN_AXIS_NORM_SQUARED: f64 = 1.0e-16;
const PSD_RELATIVE_TOLERANCE: f64 = 1.0e-12;

/// A normalized robot structure without authored URDF or filesystem state.
#[derive(Clone, Debug)]
pub struct Structure {
    document: Value,
    links: Vec<Link>,
    joints: Vec<Joint>,
    materials: Vec<Material>,
}

/// A canonical structural link.
#[derive(Clone, Debug)]
pub struct Link {
    name: String,
    inertial: Inertial,
    visuals: Vec<Visual>,
    collisions: Vec<Collision>,
}

/// A canonical structural joint.
#[derive(Clone, Debug)]
pub struct Joint {
    name: String,
    kind: JointKind,
    origin: Pose,
    parent: String,
    child: String,
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
    joint: String,
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

    /// The validated single root link.
    #[must_use]
    pub fn root_link(&self) -> &Link {
        self.links
            .iter()
            .find(|link| !self.joints.iter().any(|joint| joint.child() == link.name()))
            .expect("validated structure has one root link")
    }

    #[must_use]
    pub fn parent_joint(&self, link: &str) -> Option<&Joint> {
        self.joints.iter().find(|joint| joint.child() == link)
    }

    pub fn child_joints<'a>(&'a self, link: &'a str) -> impl Iterator<Item = &'a Joint> {
        self.joints
            .iter()
            .filter(move |joint| joint.parent() == link)
    }

    pub(crate) fn validate(&self) -> Result<(), ModelError> {
        self.validate_leaf_values()?;
        validate_unique(self.links.iter().map(Link::name), "link")?;
        validate_unique(self.joints.iter().map(Joint::name), "joint")?;
        let link_names = self.links.iter().map(Link::name).collect::<HashSet<_>>();
        let mut children = HashSet::new();
        let mut parent_by_child = HashMap::new();
        for joint in &self.joints {
            if !link_names.contains(joint.parent()) {
                return Err(ModelError::Invalid(format!(
                    "joint '{}' references unknown parent link '{}'",
                    joint.name(),
                    joint.parent()
                )));
            }
            if !link_names.contains(joint.child()) {
                return Err(ModelError::Invalid(format!(
                    "joint '{}' references unknown child link '{}'",
                    joint.name(),
                    joint.child()
                )));
            }
            if !children.insert(joint.child()) {
                return Err(ModelError::Invalid(format!(
                    "link '{}' is the child of multiple joints",
                    joint.child()
                )));
            }
            if joint.parent() == joint.child() {
                return Err(ModelError::Invalid(format!(
                    "joint '{}' cannot use '{}' as both parent and child",
                    joint.name(),
                    joint.parent()
                )));
            }
            parent_by_child.insert(joint.child(), joint.parent());
        }
        let roots = self
            .links
            .iter()
            .map(Link::name)
            .filter(|link| !children.contains(link))
            .collect::<Vec<_>>();
        if roots.len() != 1 {
            return Err(ModelError::Invalid(format!(
                "structure must have exactly one root link, found {}",
                roots.len()
            )));
        }
        for link in &self.links {
            let mut seen = HashSet::new();
            let mut current = Some(link.name());
            while let Some(link_id) = current {
                if !seen.insert(link_id) {
                    return Err(ModelError::Invalid(format!(
                        "structure contains a joint cycle involving '{link_id}'"
                    )));
                }
                current = parent_by_child.get(link_id).copied();
            }
        }
        Ok(())
    }

    fn validate_leaf_values(&self) -> Result<(), ModelError> {
        for link in &self.links {
            let inertial = link.inertial();
            validate_pose_values(
                inertial.origin(),
                &format!("link '{}' inertial", link.name()),
            )?;
            validate_nonnegative(inertial.mass_kg(), &format!("link '{}' mass", link.name()))?;
            validate_inertia(inertial.inertia(), link.name())?;
            for visual in link.visuals() {
                validate_pose_values(visual.origin(), &format!("link '{}' visual", link.name()))?;
                validate_geometry_values(visual.geometry(), link.name())?;
            }
            for collision in link.collisions() {
                validate_pose_values(
                    collision.origin(),
                    &format!("link '{}' collision", link.name()),
                )?;
                validate_geometry_values(collision.geometry(), link.name())?;
            }
        }
        let joint_names = self.joints.iter().map(Joint::name).collect::<HashSet<_>>();
        for joint in &self.joints {
            validate_pose_values(joint.origin(), &format!("joint '{}'", joint.name()))?;
            let axis = joint.axis();
            if !axis.iter().all(|value| value.is_finite()) {
                return Err(ModelError::Invalid(format!(
                    "joint '{}' axis must be finite",
                    joint.name()
                )));
            }
            if joint.kind() != JointKind::Fixed
                && axis.iter().map(|value| value * value).sum::<f64>() <= MIN_AXIS_NORM_SQUARED
            {
                return Err(ModelError::Invalid(format!(
                    "joint '{}' axis must be non-zero",
                    joint.name()
                )));
            }
            let limit = joint.limit();
            let limits = [
                limit.lower(),
                limit.upper(),
                limit.effort(),
                limit.velocity(),
            ];
            if !limits.iter().all(|value| value.is_finite()) || limit.lower() > limit.upper() {
                return Err(ModelError::Invalid(format!(
                    "joint '{}' limits must be finite with lower <= upper",
                    joint.name()
                )));
            }
            if let Some(dynamics) = joint.dynamics()
                && (!dynamics.damping().is_finite()
                    || dynamics.damping() < 0.0
                    || !dynamics.friction().is_finite()
                    || dynamics.friction() < 0.0)
            {
                return Err(ModelError::Invalid(format!(
                    "joint '{}' dynamics must be finite and non-negative",
                    joint.name()
                )));
            }
            if let Some(mimic) = joint.mimic()
                && !joint_names.contains(mimic.joint())
            {
                return Err(ModelError::Invalid(format!(
                    "joint '{}' mimics unknown joint '{}'",
                    joint.name(),
                    mimic.joint()
                )));
            }
            if let Some(safety) = joint.safety()
                && (![
                    safety.soft_lower_limit(),
                    safety.soft_upper_limit(),
                    safety.k_position(),
                    safety.k_velocity(),
                ]
                .iter()
                .all(|value| value.is_finite())
                    || safety.soft_lower_limit() > safety.soft_upper_limit())
            {
                return Err(ModelError::Invalid(format!(
                    "joint '{}' safety limits must be finite with lower <= upper",
                    joint.name()
                )));
            }
        }
        Ok(())
    }

    pub(crate) fn validate_robot_frames(&self) -> Result<(), ModelError> {
        if self.root_link().name() != "base_footprint" {
            return Err(ModelError::Invalid(format!(
                "structure root link must be 'base_footprint', found '{}'",
                self.root_link().name()
            )));
        }
        let base_joint = self
            .joints
            .iter()
            .find(|joint| joint.child() == "base_link")
            .ok_or_else(|| {
                ModelError::Invalid(
                    "structure must attach 'base_link' under 'base_footprint' with a fixed joint"
                        .to_string(),
                )
            })?;
        if base_joint.parent() != "base_footprint" || base_joint.kind() != JointKind::Fixed {
            return Err(ModelError::Invalid(
                "structure must attach 'base_link' directly under 'base_footprint' with a fixed joint"
                    .to_string(),
            ));
        }
        Ok(())
    }

    pub(crate) fn from_compiler_value(document: Value) -> Result<Self, ModelError> {
        let summary: Summary = serde_json::from_value(document)
            .map_err(|error| ModelError::Invalid(error.to_string()))?;
        Self::from_summary(summary)
    }

    fn from_summary(summary: Summary) -> Result<Self, ModelError> {
        let document = serde_json::to_value(&summary)
            .map_err(|error| ModelError::Invalid(error.to_string()))?;
        let structure = Self {
            document,
            links: summary
                .links
                .into_iter()
                .map(|link| Link {
                    name: link.name,
                    inertial: link.inertial,
                    visuals: link.visuals,
                    collisions: link.collisions,
                })
                .collect(),
            joints: summary
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
                .collect(),
            materials: summary.materials,
        };
        structure.validate()?;
        Ok(structure)
    }
}

impl Link {
    #[must_use]
    pub fn name(&self) -> &str {
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
}

impl Joint {
    #[must_use]
    pub fn name(&self) -> &str {
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
    pub fn parent(&self) -> &str {
        &self.parent
    }
    #[must_use]
    pub fn child(&self) -> &str {
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
}

impl Inertia {
    #[must_use]
    pub const fn values(self) -> [f64; 6] {
        [self.ixx, self.ixy, self.ixz, self.iyy, self.iyz, self.izz]
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
}

impl Mimic {
    #[must_use]
    pub fn joint(&self) -> &str {
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

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Summary {
    #[serde(rename = "name")]
    _name: String,
    links: Vec<LinkSummary>,
    joints: Vec<JointSummary>,
    materials: Vec<Material>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LinkSummary {
    name: String,
    inertial: Inertial,
    visuals: Vec<Visual>,
    collisions: Vec<Collision>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct JointSummary {
    name: String,
    kind: JointKind,
    origin: PoseSummary,
    parent: String,
    child: String,
    axis: [f64; 3],
    #[serde(rename = "limit")]
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

fn validate_pose_values(pose: Pose, label: &str) -> Result<(), ModelError> {
    if pose.xyz().into_iter().chain(pose.rpy()).all(f64::is_finite) {
        Ok(())
    } else {
        Err(ModelError::Invalid(format!("{label} pose must be finite")))
    }
}

fn validate_nonnegative(value: f64, label: &str) -> Result<(), ModelError> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        Err(ModelError::Invalid(format!(
            "{label} must be finite and non-negative"
        )))
    }
}

fn validate_inertia(inertia: Inertia, link: &str) -> Result<(), ModelError> {
    let [ixx, ixy, ixz, iyy, iyz, izz] = inertia.values();
    let finite = [ixx, ixy, ixz, iyy, iyz, izz]
        .into_iter()
        .all(f64::is_finite);
    let scale = [ixx, ixy, ixz, iyy, iyz, izz]
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
        Err(ModelError::Invalid(format!(
            "link '{link}' inertia must be finite and positive semidefinite"
        )))
    }
}

fn validate_geometry_values(geometry: &Geometry, link: &str) -> Result<(), ModelError> {
    let values = match geometry {
        Geometry::Box { size } => size.to_vec(),
        Geometry::Cylinder { radius, length } | Geometry::Capsule { radius, length } => {
            vec![*radius, *length]
        }
        Geometry::Sphere { radius } => vec![*radius],
        Geometry::Mesh { scale, .. } => scale.map_or_else(Vec::new, |values| values.to_vec()),
    };
    if values.iter().all(|value| value.is_finite() && *value > 0.0) {
        Ok(())
    } else {
        Err(ModelError::Invalid(format!(
            "link '{link}' geometry dimensions must be finite and positive"
        )))
    }
}

fn validate_unique<'a>(
    names: impl Iterator<Item = &'a str>,
    label: &str,
) -> Result<(), ModelError> {
    let mut seen = HashSet::new();
    for name in names {
        if !seen.insert(name) {
            return Err(ModelError::Invalid(format!(
                "duplicate {label} identity '{name}'"
            )));
        }
    }
    Ok(())
}
