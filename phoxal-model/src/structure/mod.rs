//! Canonical normalized structure facts.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};

use crate::ModelError;

/// A normalized robot structure without authored URDF or filesystem state.
#[derive(Clone, Debug)]
pub struct Structure {
    document: Value,
    links: Vec<Link>,
    joints: Vec<Joint>,
}

/// A canonical structural link.
#[derive(Clone, Debug)]
pub struct Link {
    name: String,
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
}

/// A normalized rigid transform.
#[derive(Clone, Copy, Debug)]
pub struct Pose {
    xyz: [f64; 3],
    rpy: [f64; 3],
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

    pub(crate) fn validate(&self) -> Result<(), ModelError> {
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
        if roots[0] != "base_footprint" {
            return Err(ModelError::Invalid(format!(
                "structure root link must be 'base_footprint', found '{}'",
                roots[0]
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
        validate_document(&document)?;
        let summary: Summary = serde_json::from_value(document.clone())
            .map_err(|error| ModelError::Invalid(error.to_string()))?;
        let structure = Self {
            document,
            links: summary
                .links
                .into_iter()
                .map(|link| Link { name: link.name })
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
                })
                .collect(),
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

impl Serialize for Structure {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.document.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Structure {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let document = Value::deserialize(deserializer)?;
        Self::from_compiler_value(document).map_err(serde::de::Error::custom)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Summary {
    #[serde(rename = "name")]
    _name: String,
    links: Vec<LinkSummary>,
    joints: Vec<JointSummary>,
    #[serde(rename = "materials")]
    _materials: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkSummary {
    name: String,
    #[serde(rename = "inertial")]
    _inertial: Value,
    #[serde(rename = "visuals")]
    _visuals: Vec<Value>,
    #[serde(rename = "collisions")]
    _collisions: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JointSummary {
    name: String,
    kind: JointKind,
    origin: PoseSummary,
    parent: String,
    child: String,
    axis: [f64; 3],
    #[serde(rename = "limit")]
    _limit: Value,
    #[serde(rename = "calibration")]
    _calibration: Option<Value>,
    #[serde(rename = "dynamics")]
    _dynamics: Option<Value>,
    #[serde(rename = "mimic")]
    _mimic: Option<Value>,
    #[serde(rename = "safety")]
    _safety: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PoseSummary {
    xyz: [f64; 3],
    rpy: [f64; 3],
}

fn validate_document(value: &Value) -> Result<(), ModelError> {
    let root = object(value, "structure")?;
    exact(root, &["name", "links", "joints", "materials"], "structure")?;
    for link in array(root, "links")? {
        validate_link(link)?;
    }
    for joint in array(root, "joints")? {
        validate_joint(joint)?;
    }
    for material in array(root, "materials")? {
        validate_material(material)?;
    }
    Ok(())
}

fn validate_link(value: &Value) -> Result<(), ModelError> {
    let map = object(value, "link")?;
    exact(map, &["name", "inertial", "visuals", "collisions"], "link")?;
    validate_inertial(required(map, "inertial")?)?;
    for visual in array(map, "visuals")? {
        let visual = object(visual, "visual")?;
        exact(
            visual,
            &["name", "origin", "geometry", "material"],
            "visual",
        )?;
        validate_pose(required(visual, "origin")?)?;
        validate_geometry(required(visual, "geometry")?)?;
        if let Some(material) = visual.get("material").filter(|value| !value.is_null()) {
            validate_material(material)?;
        }
    }
    for collision in array(map, "collisions")? {
        let collision = object(collision, "collision")?;
        exact(collision, &["name", "origin", "geometry"], "collision")?;
        validate_pose(required(collision, "origin")?)?;
        validate_geometry(required(collision, "geometry")?)?;
    }
    Ok(())
}

fn validate_inertial(value: &Value) -> Result<(), ModelError> {
    let map = object(value, "inertial")?;
    exact(map, &["origin", "mass_kg", "inertia"], "inertial")?;
    validate_pose(required(map, "origin")?)?;
    exact(
        object(required(map, "inertia")?, "inertia")?,
        &["ixx", "ixy", "ixz", "iyy", "iyz", "izz"],
        "inertia",
    )
}

fn validate_joint(value: &Value) -> Result<(), ModelError> {
    let map = object(value, "joint")?;
    exact(
        map,
        &[
            "name",
            "kind",
            "origin",
            "parent",
            "child",
            "axis",
            "limit",
            "calibration",
            "dynamics",
            "mimic",
            "safety",
        ],
        "joint",
    )?;
    validate_pose(required(map, "origin")?)?;
    exact(
        object(required(map, "limit")?, "joint limit")?,
        &["lower", "upper", "effort", "velocity"],
        "joint limit",
    )?;
    optional_exact(map, "calibration", &["rising", "falling"])?;
    optional_exact(map, "dynamics", &["damping", "friction"])?;
    optional_exact(map, "mimic", &["joint", "multiplier", "offset"])?;
    optional_exact(
        map,
        "safety",
        &[
            "soft_lower_limit",
            "soft_upper_limit",
            "k_position",
            "k_velocity",
        ],
    )
}

fn validate_material(value: &Value) -> Result<(), ModelError> {
    exact(
        object(value, "material")?,
        &["name", "color", "texture"],
        "material",
    )
}

fn validate_pose(value: &Value) -> Result<(), ModelError> {
    exact(object(value, "pose")?, &["xyz", "rpy"], "pose")
}

fn validate_geometry(value: &Value) -> Result<(), ModelError> {
    let map = object(value, "geometry")?;
    let kind = map
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| ModelError::Invalid("geometry.kind must be a string".into()))?;
    let fields: &[&str] = match kind {
        "box" => &["kind", "size"],
        "cylinder" | "capsule" => &["kind", "radius", "length"],
        "sphere" => &["kind", "radius"],
        "mesh" => &["kind", "filename", "scale"],
        _ => {
            return Err(ModelError::Invalid(format!(
                "unsupported geometry kind '{kind}'"
            )));
        }
    };
    exact(map, fields, "geometry")
}

fn optional_exact(map: &Map<String, Value>, key: &str, fields: &[&str]) -> Result<(), ModelError> {
    if let Some(value) = map.get(key).filter(|value| !value.is_null()) {
        exact(object(value, key)?, fields, key)?;
    }
    Ok(())
}

fn object<'a>(value: &'a Value, label: &str) -> Result<&'a Map<String, Value>, ModelError> {
    value
        .as_object()
        .ok_or_else(|| ModelError::Invalid(format!("{label} must be an object")))
}

fn required<'a>(map: &'a Map<String, Value>, key: &str) -> Result<&'a Value, ModelError> {
    map.get(key)
        .ok_or_else(|| ModelError::Invalid(format!("missing required field '{key}'")))
}

fn array<'a>(map: &'a Map<String, Value>, key: &str) -> Result<&'a [Value], ModelError> {
    required(map, key)?
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| ModelError::Invalid(format!("'{key}' must be an array")))
}

fn exact(map: &Map<String, Value>, allowed: &[&str], label: &str) -> Result<(), ModelError> {
    if let Some(key) = map.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(ModelError::Invalid(format!(
            "{label} contains unknown field '{key}'"
        )));
    }
    Ok(())
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
