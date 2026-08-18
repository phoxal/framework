//! The authored URDF document, and its normalization into the canonical
//! structure.
//!
//! This is a DTO layer: `urdf_rs` types are the exact shape of the file on
//! disk, and they stay private to this module. [`crate::model`] owns the
//! normalized runtime structure, which is what leaves here. The two carry
//! parallel `Link`/`Joint`/`Visual`/`Collision` hierarchies on purpose - one is
//! what an author wrote, the other is what the runtime reads - and the module
//! name says which one this is.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use urdf_rs::{JointType, Robot};

const BASE_FOOTPRINT_LINK: &str = "base_footprint";
const BASE_LINK: &str = "base_link";
const MODEL_URI_PREFIX: &str = "model://";
const PACKAGE_URI_PREFIX: &str = "package://";
const STRUCTURE_FILE: &str = "structure.urdf";

/// One authored URDF document.
#[derive(Clone, Debug)]
pub struct Structure {
    robot: Robot,
}

/// A URDF document that is not a usable robot structure.
#[derive(Debug, thiserror::Error)]
pub enum UrdfError {
    #[error("failed to read structure file {}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse structure file {}: {source}", path.display())]
    ParseFile {
        path: PathBuf,
        #[source]
        source: urdf_rs::UrdfError,
    },

    #[error("structure.urdf contains duplicate {kind} name '{name}'")]
    DuplicateName { kind: StructuralKind, name: String },

    #[error("link '{link}' is the child of multiple joints: '{first}' and '{second}'")]
    SharedChildLink {
        link: String,
        first: String,
        second: String,
    },

    #[error("joint '{joint}' references unknown {kind} link '{link}'")]
    UnknownJointLink {
        joint: String,
        kind: JointEnd,
        link: String,
    },

    #[error("joint '{joint}' cannot use '{link}' as both parent and child")]
    SelfJoint { joint: String, link: String },

    #[error("structure.urdf contains a joint cycle involving '{link}'")]
    JointCycle { link: String },

    #[error("structure.urdf does not define a root link")]
    NoRootLink,

    #[error("structure.urdf defines multiple root links: {}", roots.join(", "))]
    MultipleRootLinks { roots: Vec<String> },

    #[error("robot structure.urdf root link must be '{BASE_FOOTPRINT_LINK}', found '{found}'")]
    NonCanonicalRoot { found: String },

    #[error("robot structure.urdf must define link '{BASE_LINK}'")]
    MissingBaseLink,

    #[error(
        "robot structure.urdf must attach '{BASE_LINK}' directly under \
         '{BASE_FOOTPRINT_LINK}' with a fixed joint"
    )]
    MisattachedBaseLink,

    #[error(
        "robot structure.urdf joint '{joint}' from '{BASE_FOOTPRINT_LINK}' to '{BASE_LINK}' \
         must be fixed"
    )]
    NonFixedBaseJoint { joint: String },

    #[error("structure mesh '{reference}' must use a package:// or model:// URI")]
    UnqualifiedMesh { reference: String },

    #[error("asset URI '{reference}' must include a local package/model name and relative path")]
    MalformedAssetUri { reference: String },

    #[error(
        "asset URI '{reference}' names package/model '{found}', expected '{expected}' or 'meshes'"
    )]
    ForeignAssetPackage {
        reference: String,
        found: String,
        expected: String,
    },

    #[error("canonical mesh geometry is missing filename")]
    MeshWithoutFilename,

    #[error("failed to normalize URDF structure: {source}")]
    Normalize {
        #[source]
        source: serde_json::Error,
    },

    #[error(transparent)]
    Canonical(#[from] crate::model::ModelError),
}

/// Which structural item a name belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuralKind {
    Link,
    Joint,
}

/// Which end of a joint names a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JointEnd {
    Parent,
    Child,
}

impl std::fmt::Display for StructuralKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Link => "link",
            Self::Joint => "joint",
        })
    }
}

impl std::fmt::Display for JointEnd {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Parent => "parent",
            Self::Child => "child",
        })
    }
}

impl Serialize for Structure {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        wire::Structure::from(&self.robot).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Structure {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let wire = wire::Structure::deserialize(deserializer)?;
        Ok(Self { robot: wire.into() })
    }
}

impl Structure {
    /// Parse one authored URDF document from its exact text.
    fn parse(urdf: &str) -> Result<Self, urdf_rs::UrdfError> {
        urdf_rs::read_from_string(urdf).map(|robot| Self { robot })
    }

    /// Read one authored URDF document from `path`, which is either the
    /// document file itself or the directory that holds it.
    ///
    /// # Errors
    ///
    /// Returns [`UrdfError::Read`] when the file cannot be read and
    /// [`UrdfError::ParseFile`] when it is not URDF.
    pub(crate) fn load(path: impl AsRef<Path>) -> Result<Self, UrdfError> {
        let path = structure_path(path.as_ref());
        let urdf = std::fs::read_to_string(&path).map_err(|source| UrdfError::Read {
            path: path.clone(),
            source,
        })?;
        Self::parse(&urdf).map_err(|source| UrdfError::ParseFile { path, source })
    }

    /// Normalize a whole robot structure into the canonical value.
    ///
    /// # Errors
    ///
    /// Returns the first [`UrdfError`] the document violates, including the
    /// robot frame conventions a component fragment is not held to.
    pub(crate) fn into_canonical(
        self,
        component_type: Option<&str>,
    ) -> Result<crate::model::structure::Structure, UrdfError> {
        let root_link = self.validate_tree()?.to_string();
        validate_robot_frame_conventions(&self.robot, &root_link)?;
        self.normalize(component_type)
    }

    /// Normalize one component type's structure fragment, which is a tree but
    /// not a robot, so the robot frame conventions do not apply.
    ///
    /// # Errors
    ///
    /// Returns the first [`UrdfError`] the fragment violates.
    pub(crate) fn into_canonical_fragment(
        self,
        component_type: &str,
    ) -> Result<crate::model::structure::Structure, UrdfError> {
        self.validate_tree()?;
        self.normalize(Some(component_type))
    }

    fn normalize(
        &self,
        component_type: Option<&str>,
    ) -> Result<crate::model::structure::Structure, UrdfError> {
        let mut value = serde_json::to_value(wire::Structure::from(&self.robot))
            .map_err(|source| UrdfError::Normalize { source })?;
        normalize_asset_references(&mut value, component_type)?;
        Ok(crate::model::compiler::structure(value)?)
    }

    /// Check the document is a single acyclic link tree with unique names.
    ///
    /// # Errors
    ///
    /// Returns the first structural rule the document violates.
    fn validate_tree(&self) -> Result<&str, UrdfError> {
        validate_links_and_joints(&self.robot)?;
        self.root_link_name()
    }

    /// The one link no joint has as a child.
    fn root_link_name(&self) -> Result<&str, UrdfError> {
        let child_links = self
            .robot
            .joints
            .iter()
            .map(|joint| joint.child.link.as_str())
            .collect::<HashSet<_>>();

        let roots = self
            .robot
            .links
            .iter()
            .map(|link| link.name.as_str())
            .filter(|link_id| !child_links.contains(link_id))
            .collect::<Vec<_>>();

        match roots.as_slice() {
            [root] => Ok(root),
            [] => Err(UrdfError::NoRootLink),
            _ => Err(UrdfError::MultipleRootLinks {
                roots: roots.into_iter().map(str::to_string).collect(),
            }),
        }
    }
}

/// The structure file a caller meant, whether they named the file or the
/// directory that holds it.
fn structure_path(path: &Path) -> PathBuf {
    if path.is_dir() {
        path.join(STRUCTURE_FILE)
    } else {
        path.to_path_buf()
    }
}

fn normalize_asset_references(
    value: &mut serde_json::Value,
    component_type: Option<&str>,
) -> Result<(), UrdfError> {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                normalize_asset_references(value, component_type)?;
            }
        }
        serde_json::Value::Object(map) => {
            if map.get("kind").and_then(serde_json::Value::as_str) == Some("mesh") {
                let filename = map
                    .get("filename")
                    .and_then(serde_json::Value::as_str)
                    .ok_or(UrdfError::MeshWithoutFilename)?;
                let normalized = normalize_asset_id(filename, component_type, false)?;
                map.insert(
                    "filename".to_string(),
                    serde_json::Value::String(normalized),
                );
            }
            if let Some(texture) = map
                .get("texture")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
            {
                map.insert(
                    "texture".to_string(),
                    serde_json::Value::String(normalize_asset_id(&texture, component_type, true)?),
                );
            }
            for value in map.values_mut() {
                normalize_asset_references(value, component_type)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn normalize_asset_id(
    reference: &str,
    component_type: Option<&str>,
    allow_relative: bool,
) -> Result<String, UrdfError> {
    let uri = reference
        .strip_prefix(PACKAGE_URI_PREFIX)
        .or_else(|| reference.strip_prefix(MODEL_URI_PREFIX));
    let (package, relative) = match uri {
        Some(uri) => uri
            .split_once('/')
            .ok_or_else(|| UrdfError::MalformedAssetUri {
                reference: reference.to_string(),
            })?,
        None if allow_relative => ("meshes", reference),
        None => {
            return Err(UrdfError::UnqualifiedMesh {
                reference: reference.to_string(),
            });
        }
    };
    let expected_package = component_type.unwrap_or("robot");
    if package != "meshes" && package != expected_package {
        return Err(UrdfError::ForeignAssetPackage {
            reference: reference.to_string(),
            found: package.to_string(),
            expected: expected_package.to_string(),
        });
    }
    let relative = relative.strip_prefix("meshes/").unwrap_or(relative);
    // A logical asset id is exactly the path below `<bundle>/assets`, so the
    // finalized bundle needs no second mapping table to serve it.
    let logical = match component_type {
        None => format!("robot/meshes/{relative}"),
        Some(component_type) => format!("components/{component_type}/meshes/{relative}"),
    };
    Ok(crate::model::AssetId::new(logical)?.as_str().to_string())
}

fn validate_links_and_joints(robot: &Robot) -> Result<(), UrdfError> {
    validate_unique_names(
        robot.links.iter().map(|link| link.name.as_str()),
        StructuralKind::Link,
    )?;
    validate_unique_names(
        robot.joints.iter().map(|joint| joint.name.as_str()),
        StructuralKind::Joint,
    )?;
    validate_unique_joint_children(robot)?;

    let link_ids = robot
        .links
        .iter()
        .map(|link| link.name.as_str())
        .collect::<HashSet<_>>();
    for joint in &robot.joints {
        for (end, link) in [
            (JointEnd::Parent, &joint.parent.link),
            (JointEnd::Child, &joint.child.link),
        ] {
            if !link_ids.contains(link.as_str()) {
                return Err(UrdfError::UnknownJointLink {
                    joint: joint.name.clone(),
                    kind: end,
                    link: link.clone(),
                });
            }
        }
        if joint.parent.link == joint.child.link {
            return Err(UrdfError::SelfJoint {
                joint: joint.name.clone(),
                link: joint.parent.link.clone(),
            });
        }
    }
    validate_acyclic_link_graph(robot)
}

fn validate_unique_joint_children(robot: &Robot) -> Result<(), UrdfError> {
    let mut child_to_joint = HashMap::new();
    for joint in &robot.joints {
        if let Some(first) = child_to_joint.insert(joint.child.link.as_str(), &joint.name) {
            return Err(UrdfError::SharedChildLink {
                link: joint.child.link.clone(),
                first: first.clone(),
                second: joint.name.clone(),
            });
        }
    }
    Ok(())
}

/// The whole-robot frame conventions every runtime consumer assumes: the tree
/// is rooted at `base_footprint`, and `base_link` hangs rigidly beneath it.
fn validate_robot_frame_conventions(robot: &Robot, root_link: &str) -> Result<(), UrdfError> {
    if root_link != BASE_FOOTPRINT_LINK {
        return Err(UrdfError::NonCanonicalRoot {
            found: root_link.to_string(),
        });
    }
    if !robot.links.iter().any(|link| link.name == BASE_LINK) {
        return Err(UrdfError::MissingBaseLink);
    }

    let Some(base_joint) = robot
        .joints
        .iter()
        .find(|joint| joint.child.link == BASE_LINK)
    else {
        return Err(UrdfError::MisattachedBaseLink);
    };
    if base_joint.parent.link != BASE_FOOTPRINT_LINK {
        return Err(UrdfError::MisattachedBaseLink);
    }
    if base_joint.joint_type != JointType::Fixed {
        return Err(UrdfError::NonFixedBaseJoint {
            joint: base_joint.name.clone(),
        });
    }

    Ok(())
}

fn validate_acyclic_link_graph(robot: &Robot) -> Result<(), UrdfError> {
    let parent_by_child = robot
        .joints
        .iter()
        .map(|joint| (joint.child.link.as_str(), joint.parent.link.as_str()))
        .collect::<HashMap<_, _>>();

    for link in &robot.links {
        let mut seen = HashSet::new();
        let mut current = Some(link.name.as_str());
        while let Some(link_id) = current {
            if !seen.insert(link_id) {
                return Err(UrdfError::JointCycle {
                    link: link_id.to_string(),
                });
            }
            current = parent_by_child.get(link_id).copied();
        }
    }
    Ok(())
}

fn validate_unique_names<'a>(
    names: impl Iterator<Item = &'a str>,
    kind: StructuralKind,
) -> Result<(), UrdfError> {
    let mut seen = HashSet::new();
    for name in names {
        if !seen.insert(name) {
            return Err(UrdfError::DuplicateName {
                kind,
                name: name.to_string(),
            });
        }
    }
    Ok(())
}

mod wire {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Structure {
        name: String,
        links: Vec<Link>,
        joints: Vec<Joint>,
        materials: Vec<Material>,
    }

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Link {
        name: String,
        inertial: Inertial,
        visuals: Vec<Visual>,
        collisions: Vec<Collision>,
    }

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Inertial {
        origin: Pose,
        mass_kg: f64,
        inertia: Inertia,
    }

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Inertia {
        ixx: f64,
        ixy: f64,
        ixz: f64,
        iyy: f64,
        iyz: f64,
        izz: f64,
    }

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Visual {
        name: Option<String>,
        origin: Pose,
        geometry: Geometry,
        material: Option<Material>,
    }

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Collision {
        name: Option<String>,
        origin: Pose,
        geometry: Geometry,
    }

    #[derive(Clone, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Material {
        name: String,
        color: Option<[f64; 4]>,
        texture: Option<String>,
    }

    #[derive(Serialize, Deserialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    enum Geometry {
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
            filename: String,
            scale: Option<[f64; 3]>,
        },
    }

    #[derive(Clone, Copy, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Pose {
        xyz: [f64; 3],
        rpy: [f64; 3],
    }

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Joint {
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

    #[derive(Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum JointKind {
        Revolute,
        Continuous,
        Prismatic,
        Fixed,
        Floating,
        Planar,
        Spherical,
    }

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct JointLimit {
        lower: f64,
        upper: f64,
        effort: f64,
        velocity: f64,
    }

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Calibration {
        rising: Option<f64>,
        falling: Option<f64>,
    }

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Dynamics {
        damping: f64,
        friction: f64,
    }

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Mimic {
        joint: String,
        multiplier: Option<f64>,
        offset: Option<f64>,
    }

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Safety {
        soft_lower_limit: f64,
        soft_upper_limit: f64,
        k_position: f64,
        k_velocity: f64,
    }

    impl From<&urdf_rs::Robot> for Structure {
        fn from(robot: &urdf_rs::Robot) -> Self {
            Self {
                name: robot.name.clone(),
                links: robot.links.iter().map(Link::from).collect(),
                joints: robot.joints.iter().map(Joint::from).collect(),
                materials: robot.materials.iter().map(Material::from).collect(),
            }
        }
    }

    impl From<Structure> for urdf_rs::Robot {
        fn from(value: Structure) -> Self {
            Self {
                name: value.name,
                links: value.links.into_iter().map(Into::into).collect(),
                joints: value.joints.into_iter().map(Into::into).collect(),
                materials: value.materials.into_iter().map(Into::into).collect(),
            }
        }
    }

    impl From<&urdf_rs::Link> for Link {
        fn from(value: &urdf_rs::Link) -> Self {
            Self {
                name: value.name.clone(),
                inertial: Inertial::from(&value.inertial),
                visuals: value.visual.iter().map(Visual::from).collect(),
                collisions: value.collision.iter().map(Collision::from).collect(),
            }
        }
    }

    impl From<Link> for urdf_rs::Link {
        fn from(value: Link) -> Self {
            Self {
                name: value.name,
                inertial: value.inertial.into(),
                visual: value.visuals.into_iter().map(Into::into).collect(),
                collision: value.collisions.into_iter().map(Into::into).collect(),
            }
        }
    }

    impl From<&urdf_rs::Inertial> for Inertial {
        fn from(value: &urdf_rs::Inertial) -> Self {
            Self {
                origin: Pose::from(&value.origin),
                mass_kg: value.mass.value,
                inertia: Inertia::from(&value.inertia),
            }
        }
    }

    impl From<Inertial> for urdf_rs::Inertial {
        fn from(value: Inertial) -> Self {
            Self {
                origin: value.origin.into(),
                mass: urdf_rs::Mass {
                    value: value.mass_kg,
                },
                inertia: value.inertia.into(),
            }
        }
    }

    impl From<&urdf_rs::Inertia> for Inertia {
        fn from(value: &urdf_rs::Inertia) -> Self {
            Self {
                ixx: value.ixx,
                ixy: value.ixy,
                ixz: value.ixz,
                iyy: value.iyy,
                iyz: value.iyz,
                izz: value.izz,
            }
        }
    }

    impl From<Inertia> for urdf_rs::Inertia {
        fn from(value: Inertia) -> Self {
            Self {
                ixx: value.ixx,
                ixy: value.ixy,
                ixz: value.ixz,
                iyy: value.iyy,
                iyz: value.iyz,
                izz: value.izz,
            }
        }
    }

    impl From<&urdf_rs::Visual> for Visual {
        fn from(value: &urdf_rs::Visual) -> Self {
            Self {
                name: value.name.clone(),
                origin: Pose::from(&value.origin),
                geometry: Geometry::from(&value.geometry),
                material: value.material.as_ref().map(Material::from),
            }
        }
    }

    impl From<Visual> for urdf_rs::Visual {
        fn from(value: Visual) -> Self {
            Self {
                name: value.name,
                origin: value.origin.into(),
                geometry: value.geometry.into(),
                material: value.material.map(Into::into),
            }
        }
    }

    impl From<&urdf_rs::Collision> for Collision {
        fn from(value: &urdf_rs::Collision) -> Self {
            Self {
                name: value.name.clone(),
                origin: Pose::from(&value.origin),
                geometry: Geometry::from(&value.geometry),
            }
        }
    }

    impl From<Collision> for urdf_rs::Collision {
        fn from(value: Collision) -> Self {
            Self {
                name: value.name,
                origin: value.origin.into(),
                geometry: value.geometry.into(),
            }
        }
    }

    impl From<&urdf_rs::Material> for Material {
        fn from(value: &urdf_rs::Material) -> Self {
            Self {
                name: value.name.clone(),
                color: value.color.as_ref().map(|color| *color.rgba),
                texture: value
                    .texture
                    .as_ref()
                    .map(|texture| texture.filename.clone()),
            }
        }
    }

    impl From<Material> for urdf_rs::Material {
        fn from(value: Material) -> Self {
            Self {
                name: value.name,
                color: value.color.map(|rgba| urdf_rs::Color {
                    rgba: urdf_rs::Vec4(rgba),
                }),
                texture: value.texture.map(|filename| urdf_rs::Texture { filename }),
            }
        }
    }

    impl From<&urdf_rs::Geometry> for Geometry {
        fn from(value: &urdf_rs::Geometry) -> Self {
            match value {
                urdf_rs::Geometry::Box { size } => Self::Box { size: **size },
                urdf_rs::Geometry::Cylinder { radius, length } => Self::Cylinder {
                    radius: *radius,
                    length: *length,
                },
                urdf_rs::Geometry::Capsule { radius, length } => Self::Capsule {
                    radius: *radius,
                    length: *length,
                },
                urdf_rs::Geometry::Sphere { radius } => Self::Sphere { radius: *radius },
                urdf_rs::Geometry::Mesh { filename, scale } => Self::Mesh {
                    filename: filename.clone(),
                    scale: scale.as_ref().map(|value| **value),
                },
            }
        }
    }

    impl From<Geometry> for urdf_rs::Geometry {
        fn from(value: Geometry) -> Self {
            match value {
                Geometry::Box { size } => Self::Box {
                    size: urdf_rs::Vec3(size),
                },
                Geometry::Cylinder { radius, length } => Self::Cylinder { radius, length },
                Geometry::Capsule { radius, length } => Self::Capsule { radius, length },
                Geometry::Sphere { radius } => Self::Sphere { radius },
                Geometry::Mesh { filename, scale } => Self::Mesh {
                    filename,
                    scale: scale.map(urdf_rs::Vec3),
                },
            }
        }
    }

    impl From<&urdf_rs::Pose> for Pose {
        fn from(value: &urdf_rs::Pose) -> Self {
            Self {
                xyz: *value.xyz,
                rpy: *value.rpy,
            }
        }
    }

    impl From<Pose> for urdf_rs::Pose {
        fn from(value: Pose) -> Self {
            Self {
                xyz: urdf_rs::Vec3(value.xyz),
                rpy: urdf_rs::Vec3(value.rpy),
            }
        }
    }

    impl From<&urdf_rs::Joint> for Joint {
        fn from(value: &urdf_rs::Joint) -> Self {
            Self {
                name: value.name.clone(),
                kind: JointKind::from(&value.joint_type),
                origin: Pose::from(&value.origin),
                parent: value.parent.link.clone(),
                child: value.child.link.clone(),
                axis: *value.axis.xyz,
                limit: JointLimit::from(&value.limit),
                calibration: value.calibration.as_ref().map(Calibration::from),
                dynamics: value.dynamics.as_ref().map(Dynamics::from),
                mimic: value.mimic.as_ref().map(Mimic::from),
                safety: value.safety_controller.as_ref().map(Safety::from),
            }
        }
    }

    impl From<Joint> for urdf_rs::Joint {
        fn from(value: Joint) -> Self {
            Self {
                name: value.name,
                joint_type: value.kind.into(),
                origin: value.origin.into(),
                parent: urdf_rs::LinkName { link: value.parent },
                child: urdf_rs::LinkName { link: value.child },
                axis: urdf_rs::Axis {
                    xyz: urdf_rs::Vec3(value.axis),
                },
                limit: value.limit.into(),
                calibration: value.calibration.map(Into::into),
                dynamics: value.dynamics.map(Into::into),
                mimic: value.mimic.map(Into::into),
                safety_controller: value.safety.map(Into::into),
            }
        }
    }

    impl From<&urdf_rs::JointType> for JointKind {
        fn from(value: &urdf_rs::JointType) -> Self {
            match value {
                urdf_rs::JointType::Revolute => Self::Revolute,
                urdf_rs::JointType::Continuous => Self::Continuous,
                urdf_rs::JointType::Prismatic => Self::Prismatic,
                urdf_rs::JointType::Fixed => Self::Fixed,
                urdf_rs::JointType::Floating => Self::Floating,
                urdf_rs::JointType::Planar => Self::Planar,
                urdf_rs::JointType::Spherical => Self::Spherical,
            }
        }
    }

    impl From<JointKind> for urdf_rs::JointType {
        fn from(value: JointKind) -> Self {
            match value {
                JointKind::Revolute => Self::Revolute,
                JointKind::Continuous => Self::Continuous,
                JointKind::Prismatic => Self::Prismatic,
                JointKind::Fixed => Self::Fixed,
                JointKind::Floating => Self::Floating,
                JointKind::Planar => Self::Planar,
                JointKind::Spherical => Self::Spherical,
            }
        }
    }

    impl From<&urdf_rs::JointLimit> for JointLimit {
        fn from(value: &urdf_rs::JointLimit) -> Self {
            Self {
                lower: value.lower,
                upper: value.upper,
                effort: value.effort,
                velocity: value.velocity,
            }
        }
    }

    impl From<JointLimit> for urdf_rs::JointLimit {
        fn from(value: JointLimit) -> Self {
            Self {
                lower: value.lower,
                upper: value.upper,
                effort: value.effort,
                velocity: value.velocity,
            }
        }
    }

    macro_rules! option_wire {
        ($wire:ty, $urdf:ty, {$($field:ident),+ $(,)?}) => {
            impl From<&$urdf> for $wire {
                fn from(value: &$urdf) -> Self {
                    Self { $($field: value.$field.clone()),+ }
                }
            }
            impl From<$wire> for $urdf {
                fn from(value: $wire) -> Self {
                    Self { $($field: value.$field),+ }
                }
            }
        };
    }

    option_wire!(Calibration, urdf_rs::Calibration, { rising, falling });
    option_wire!(Dynamics, urdf_rs::Dynamics, { damping, friction });
    option_wire!(Mimic, urdf_rs::Mimic, { joint, multiplier, offset });
    option_wire!(Safety, urdf_rs::SafetyController, {
        soft_lower_limit,
        soft_upper_limit,
        k_position,
        k_velocity,
    });
}

#[cfg(test)]
mod tests {
    use super::{
        BASE_FOOTPRINT_LINK, BASE_LINK, STRUCTURE_FILE, Structure, UrdfError, normalize_asset_id,
    };
    use tempfile::tempdir;

    const CANONICAL_ROBOT: &str = r#"<robot name="test-bot">
  <link name="base_footprint" />
  <link name="base_link" />
  <joint name="root" type="fixed">
    <parent link="base_footprint" />
    <child link="base_link" />
    <origin xyz="0 0 0.2" rpy="0 0 0" />
  </joint>
</robot>
"#;

    #[test]
    fn normalizes_only_local_asset_references() -> anyhow::Result<()> {
        assert_eq!(
            normalize_asset_id("package://robot/body.stl", None, false)?,
            "robot/meshes/body.stl"
        );
        assert_eq!(
            normalize_asset_id(
                "package://drive_motor/meshes/rotor.stl",
                Some("drive_motor"),
                false,
            )?,
            "components/drive_motor/meshes/rotor.stl"
        );
        assert_eq!(
            normalize_asset_id("model://meshes/sensor.obj", Some("camera"), false)?,
            "components/camera/meshes/sensor.obj"
        );
        assert_eq!(
            normalize_asset_id("wood.png", Some("camera"), true)?,
            "components/camera/meshes/wood.png"
        );
        assert!(matches!(
            normalize_asset_id("body.stl", None, false),
            Err(UrdfError::UnqualifiedMesh { .. })
        ));
        assert!(matches!(
            normalize_asset_id("package://other/body.stl", Some("drive_motor"), false),
            Err(UrdfError::ForeignAssetPackage { .. })
        ));
        Ok(())
    }

    #[test]
    fn loading_a_directory_names_the_structure_file() {
        let temp_dir = tempdir().expect("a temp directory");
        let structure_dir = temp_dir.path().join("robot");
        std::fs::create_dir_all(&structure_dir).expect("a structure directory");

        let error = Structure::load(&structure_dir).expect_err("no structure file exists yet");
        assert!(matches!(error, UrdfError::Read { .. }), "{error}");
        assert!(error.to_string().contains(STRUCTURE_FILE), "{error}");

        std::fs::write(structure_dir.join(STRUCTURE_FILE), "invalid urdf")
            .expect("a writable structure file");
        let error = Structure::load(&structure_dir).expect_err("the file is not URDF");
        assert!(matches!(error, UrdfError::ParseFile { .. }), "{error}");
        assert!(error.to_string().contains(STRUCTURE_FILE), "{error}");
    }

    #[test]
    fn validate_requires_base_footprint_root_and_base_link() -> anyhow::Result<()> {
        let structure = Structure::parse(CANONICAL_ROBOT)?;
        structure.clone().into_canonical(None)?;
        assert_eq!(structure.root_link_name()?, BASE_FOOTPRINT_LINK);
        Ok(())
    }

    #[test]
    fn validate_rejects_noncanonical_root() -> anyhow::Result<()> {
        let structure = Structure::parse(
            r#"<robot name="test-bot">
  <link name="base_link" />
</robot>
"#,
        )?;

        let error = structure
            .into_canonical(None)
            .expect_err("missing canonical footprint");
        assert!(
            matches!(&error, UrdfError::NonCanonicalRoot { found } if found == BASE_LINK),
            "{error}"
        );
        assert!(
            error
                .to_string()
                .contains("root link must be 'base_footprint'"),
            "{error}"
        );
        Ok(())
    }

    #[test]
    fn a_component_fragment_is_held_to_the_tree_rules_only() -> anyhow::Result<()> {
        for (urdf, expected) in [
            (
                r#"<robot name="test-bot">
  <link name="base_footprint" />
  <joint name="root" type="fixed">
    <parent link="base_footprint" />
    <child link="missing_link" />
  </joint>
</robot>
"#,
                "references unknown child link 'missing_link'",
            ),
            (
                r#"<robot name="test-bot">
  <link name="a" />
  <link name="b" />
  <joint name="a_to_b" type="fixed">
    <parent link="a" />
    <child link="b" />
  </joint>
  <joint name="b_to_a" type="fixed">
    <parent link="b" />
    <child link="a" />
  </joint>
</robot>
"#,
                "joint cycle",
            ),
            (
                r#"<robot name="test-bot">
  <link name="root_a" />
  <link name="root_b" />
  <link name="shared_child" />
  <joint name="a_to_child" type="fixed">
    <parent link="root_a" />
    <child link="shared_child" />
  </joint>
  <joint name="b_to_child" type="fixed">
    <parent link="root_b" />
    <child link="shared_child" />
  </joint>
</robot>
"#,
                "child of multiple joints",
            ),
        ] {
            let error = Structure::parse(urdf)?
                .validate_tree()
                .expect_err("a malformed fragment must fail");
            assert!(error.to_string().contains(expected), "{error}");
        }
        Ok(())
    }
}
