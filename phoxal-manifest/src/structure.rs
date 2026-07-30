use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
#[cfg(test)]
use std::ops::Deref;
use std::path::Path;
// Allowed by deloper, the phoxal-core-structure should be used instead of urdf_rs when using the structure.
pub use urdf_rs::*;

const BASE_FOOTPRINT_LINK: &str = "base_footprint";
const BASE_LINK: &str = "base_link";
const MODEL_URI_PREFIX: &str = "model://";
const PACKAGE_URI_PREFIX: &str = "package://";
#[cfg(test)]
const STRUCTURE_FILE: &str = "structure.urdf";

#[derive(Clone, Debug)]
pub struct Structure {
    robot: Robot,
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
        Ok(Self::new(wire.into()))
    }
}

impl Structure {
    #[must_use]
    pub fn new(robot: Robot) -> Self {
        Self { robot }
    }

    pub fn from_urdf_str(urdf: &str) -> anyhow::Result<Structure> {
        let robot = read_from_string(urdf).context("Failed to parse URDF structure")?;
        Ok(Self::new(robot))
    }

    #[cfg(test)]
    pub fn read_from_dir(path: impl AsRef<Path>) -> anyhow::Result<Structure> {
        let path = path.as_ref();
        Self::read_from_file(path.join(STRUCTURE_FILE))
    }

    pub fn read_from_file(path: impl AsRef<Path>) -> anyhow::Result<Structure> {
        let path = path.as_ref();
        let urdf = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read structure file {}", path.display()))?;
        Self::from_urdf_str(&urdf)
            .with_context(|| format!("Failed to parse structure file {}", path.display()))
    }

    #[cfg(test)]
    pub fn write_to_dir(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        let path = path.as_ref();
        std::fs::create_dir_all(path)
            .with_context(|| format!("Failed to create structure directory {}", path.display()))?;
        std::fs::write(
            path.join(STRUCTURE_FILE),
            write_to_string(&self.robot).context("Failed to serialize URDF structure")?,
        )
        .with_context(|| {
            format!(
                "Failed to write structure file {}",
                path.join(STRUCTURE_FILE).display()
            )
        })?;

        Ok(())
    }

    /// Convert parsed URDF into the normalized canonical structure value.
    pub(crate) fn into_canonical(self) -> anyhow::Result<phoxal_model::structure::Structure> {
        self.validate()?;
        let value = serde_json::to_value(wire::Structure::from(&self.robot))
            .context("failed to normalize URDF structure")?;
        phoxal_model::__private::structure_from_compiler_value(value).map_err(anyhow::Error::from)
    }

    #[cfg(test)]
    pub fn validate_fragment(&self) -> anyhow::Result<()> {
        self.validate_tree()?;
        Ok(())
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        let root_link = self.validate_tree()?;
        validate_robot_frame_conventions(&self.robot, root_link)?;
        Ok(())
    }

    fn validate_tree(&self) -> anyhow::Result<&str> {
        write_to_string(&self.robot)
            .context("failed to serialize assembled URDF for validation")?;
        validate_links_and_joints(&self.robot)?;
        validate_meshes(&self.robot)?;
        self.root_link_name()
    }

    pub fn root_link_name(&self) -> anyhow::Result<&str> {
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
            [] => anyhow::bail!("structure.urdf does not define a root link"),
            _ => anyhow::bail!(
                "structure.urdf defines multiple root links: {}",
                roots.join(", ")
            ),
        }
    }

    #[cfg(test)]
    pub fn link(&self, link_id: &str) -> Option<&urdf_rs::Link> {
        self.robot.links.iter().find(|link| link.name == link_id)
    }

    #[cfg(test)]
    pub fn joint(&self, joint_id: &str) -> Option<&urdf_rs::Joint> {
        self.robot
            .joints
            .iter()
            .find(|joint| joint.name == joint_id)
    }

    /// Merge `component`'s structure onto `mount_link`, namespacing every component link and joint
    /// with `{component_id}__` and attaching the component's root link to `mount_link` with a fixed
    /// joint named `{component_id}__mount_attach`.
    ///
    /// Example: mounting the ddsm115 component as instance `front_left_drive` on
    /// `front_left_wheel_mount` yields links `front_left_drive__mount`,
    /// `front_left_drive__rotor_link` and joints `front_left_drive__mount_attach` (fixed, parent
    /// `front_left_wheel_mount`) and `front_left_drive__motor_joint` (continuous).
    #[cfg(test)]
    pub fn with_mounted_component(
        &self,
        component_id: &str,
        mount_link: &str,
        component: &Structure,
    ) -> anyhow::Result<Structure> {
        if self.link(mount_link).is_none() {
            bail!("mount link '{mount_link}' does not exist in structure.urdf");
        }

        let component_root = component.root_link_name()?;
        let mut existing_links = self
            .robot
            .links
            .iter()
            .map(|link| link.name.clone())
            .collect::<HashSet<_>>();
        let mut existing_joints = self
            .robot
            .joints
            .iter()
            .map(|joint| joint.name.clone())
            .collect::<HashSet<_>>();
        let mut namespaced_links = Vec::new();
        let mut namespaced_joints = Vec::new();

        for link in &component.robot.links {
            let namespaced_name = format!("{component_id}__{}", link.name);
            if !existing_links.insert(namespaced_name.clone()) {
                bail!("structure.urdf contains duplicate link name '{namespaced_name}'");
            }
            namespaced_links.push((link, namespaced_name));
        }

        for joint in &component.robot.joints {
            let namespaced_name = format!("{component_id}__{}", joint.name);
            if !existing_joints.insert(namespaced_name.clone()) {
                bail!("structure.urdf contains duplicate joint name '{namespaced_name}'");
            }
            namespaced_joints.push((joint, namespaced_name));
        }

        let attach_joint_name = format!("{component_id}__mount_attach");
        if !existing_joints.insert(attach_joint_name.clone()) {
            bail!("structure.urdf contains duplicate joint name '{attach_joint_name}'");
        }

        let mut robot = self.robot.clone();

        for (link, namespaced_name) in namespaced_links {
            let mut link = link.clone();
            link.name = namespaced_name;
            robot.links.push(link);
        }

        for (joint, namespaced_name) in namespaced_joints {
            let mut joint = joint.clone();
            joint.name = namespaced_name;
            joint.parent.link = format!("{component_id}__{}", joint.parent.link);
            joint.child.link = format!("{component_id}__{}", joint.child.link);
            robot.joints.push(joint);
        }

        robot.joints.push(urdf_rs::Joint {
            name: attach_joint_name,
            joint_type: urdf_rs::JointType::Fixed,
            origin: urdf_rs::Pose::default(),
            parent: urdf_rs::LinkName {
                link: mount_link.to_string(),
            },
            child: urdf_rs::LinkName {
                link: format!("{component_id}__{component_root}"),
            },
            axis: urdf_rs::Axis::default(),
            limit: urdf_rs::JointLimit::default(),
            calibration: None,
            dynamics: None,
            mimic: None,
            safety_controller: None,
        });

        Ok(Structure::new(robot))
    }
}

fn validate_links_and_joints(robot: &Robot) -> anyhow::Result<()> {
    validate_unique_names(
        &robot
            .links
            .iter()
            .map(|link| link.name.as_str())
            .collect::<Vec<_>>(),
        "link",
    )?;
    validate_unique_names(
        &robot
            .joints
            .iter()
            .map(|joint| joint.name.as_str())
            .collect::<Vec<_>>(),
        "joint",
    )?;
    validate_unique_joint_children(robot)?;

    let link_ids = robot
        .links
        .iter()
        .map(|link| link.name.as_str())
        .collect::<HashSet<_>>();
    for joint in &robot.joints {
        if !link_ids.contains(joint.parent.link.as_str()) {
            bail!(
                "joint '{}' references unknown parent link '{}'",
                joint.name,
                joint.parent.link
            );
        }
        if !link_ids.contains(joint.child.link.as_str()) {
            bail!(
                "joint '{}' references unknown child link '{}'",
                joint.name,
                joint.child.link
            );
        }
        if joint.parent.link == joint.child.link {
            bail!(
                "joint '{}' cannot use '{}' as both parent and child",
                joint.name,
                joint.parent.link
            );
        }
    }
    validate_acyclic_link_graph(robot)?;
    Ok(())
}

fn validate_unique_joint_children(robot: &Robot) -> anyhow::Result<()> {
    let mut child_to_joint = std::collections::HashMap::new();
    for joint in &robot.joints {
        if let Some(existing_joint) = child_to_joint.insert(joint.child.link.as_str(), &joint.name)
        {
            bail!(
                "link '{}' is the child of multiple joints: '{}' and '{}'",
                joint.child.link,
                existing_joint,
                joint.name
            );
        }
    }
    Ok(())
}

fn validate_robot_frame_conventions(robot: &Robot, root_link: &str) -> anyhow::Result<()> {
    if root_link != BASE_FOOTPRINT_LINK {
        bail!(
            "robot structure.urdf root link must be '{}', found '{}'",
            BASE_FOOTPRINT_LINK,
            root_link
        );
    }
    if !robot.links.iter().any(|link| link.name == BASE_LINK) {
        bail!("robot structure.urdf must define link '{}'", BASE_LINK);
    }

    let Some(base_joint) = robot
        .joints
        .iter()
        .find(|joint| joint.child.link == BASE_LINK)
    else {
        bail!(
            "robot structure.urdf must attach '{}' under '{}' with a fixed joint",
            BASE_LINK,
            BASE_FOOTPRINT_LINK
        );
    };
    if base_joint.parent.link != BASE_FOOTPRINT_LINK {
        bail!(
            "robot structure.urdf must attach '{}' directly under '{}'",
            BASE_LINK,
            BASE_FOOTPRINT_LINK
        );
    }
    if base_joint.joint_type != urdf_rs::JointType::Fixed {
        bail!(
            "robot structure.urdf joint '{}' from '{}' to '{}' must be fixed",
            base_joint.name,
            BASE_FOOTPRINT_LINK,
            BASE_LINK
        );
    }

    Ok(())
}

fn validate_acyclic_link_graph(robot: &Robot) -> anyhow::Result<()> {
    let parent_by_child = robot
        .joints
        .iter()
        .map(|joint| (joint.child.link.as_str(), joint.parent.link.as_str()))
        .collect::<std::collections::HashMap<_, _>>();

    for link in &robot.links {
        let mut seen = HashSet::new();
        let mut current = Some(link.name.as_str());
        while let Some(link_id) = current {
            if !seen.insert(link_id) {
                bail!("structure.urdf contains a joint cycle involving '{link_id}'");
            }
            current = parent_by_child.get(link_id).copied();
        }
    }
    Ok(())
}

fn validate_unique_names(names: &[&str], kind: &str) -> anyhow::Result<()> {
    let mut seen = HashSet::new();

    for name in names {
        if !seen.insert(*name) {
            bail!("structure.urdf contains duplicate {kind} name '{name}'");
        }
    }

    Ok(())
}

fn validate_meshes(robot: &Robot) -> anyhow::Result<()> {
    for link in &robot.links {
        for visual in &link.visual {
            validate_geometry_mesh(&visual.geometry)?;
        }
        for collision in &link.collision {
            validate_geometry_mesh(&collision.geometry)?;
        }
    }

    Ok(())
}

fn validate_geometry_mesh(geometry: &Geometry) -> anyhow::Result<()> {
    let Geometry::Mesh { filename, .. } = geometry else {
        return Ok(());
    };

    let _ = mesh_relative_path(filename)?;
    Ok(())
}

fn mesh_relative_path(filename: &str) -> anyhow::Result<&Path> {
    if !filename.starts_with(PACKAGE_URI_PREFIX) && !filename.starts_with(MODEL_URI_PREFIX) {
        bail!(
            "structure mesh '{}' must start with 'package://' or 'model://'",
            filename
        );
    }

    let trimmed = filename
        .trim_start_matches(PACKAGE_URI_PREFIX)
        .trim_start_matches(MODEL_URI_PREFIX);
    let Some((_, relative_path)) = trimmed.split_once('/') else {
        bail!(
            "structure mesh '{}' must include a package/model name and relative path",
            filename
        );
    };

    let relative_path = Path::new(relative_path);
    if !relative_path.is_relative() {
        bail!(
            "structure mesh '{}' must resolve to a relative path",
            filename
        );
    }
    if relative_path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        bail!(
            "structure mesh '{}' must not contain parent directory segments",
            filename
        );
    }

    Ok(relative_path)
}

#[cfg(test)]
impl Deref for Structure {
    type Target = Robot;
    fn deref(&self) -> &Self::Target {
        &self.robot
    }
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
    use super::{BASE_FOOTPRINT_LINK, BASE_LINK, STRUCTURE_FILE, Structure};
    use anyhow::Context;
    use tempfile::tempdir;

    #[test]
    fn read_from_dir_fails_on_missing_file() -> anyhow::Result<()> {
        let temp_dir = tempdir()?;
        let structure_dir = temp_dir.path().join("robot");
        // No directory or file created

        let result = Structure::read_from_dir(&structure_dir);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Failed to read structure file"));
        assert!(err.contains(STRUCTURE_FILE));

        Ok(())
    }

    #[test]
    fn read_from_dir_fails_on_malformed_urdf() -> anyhow::Result<()> {
        let temp_dir = tempdir()?;
        let structure_dir = temp_dir.path().join("robot");
        std::fs::create_dir_all(&structure_dir)?;
        std::fs::write(structure_dir.join(STRUCTURE_FILE), "invalid urdf")?;

        let result = Structure::read_from_dir(&structure_dir);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Failed to parse structure file"));
        assert!(err.contains(STRUCTURE_FILE));

        Ok(())
    }

    #[test]
    fn structure_roundtrips_through_directory() -> anyhow::Result<()> {
        let temp_dir = tempdir()?;
        let structure_dir = temp_dir.path().join("robot");
        let structure = Structure::new(urdf_rs::read_from_string(
            r#"<robot name="test-bot">
  <link name="base_footprint" />
  <link name="base_link" />
  <joint name="root" type="fixed">
    <parent link="base_footprint" />
    <child link="base_link" />
    <origin xyz="0 0 0" rpy="0 0 0" />
  </joint>
</robot>
"#,
        )?);

        structure.write_to_dir(&structure_dir)?;
        let loaded = Structure::read_from_dir(&structure_dir)?;

        assert_eq!(loaded.name, "test-bot");
        assert_eq!(loaded.links.len(), 2);
        Ok(())
    }

    #[test]
    fn from_urdf_str_parses_in_memory_content() -> anyhow::Result<()> {
        let structure = Structure::from_urdf_str(
            r#"<robot name="test-bot">
  <link name="base_footprint" />
  <link name="base_link" />
  <joint name="root" type="fixed">
    <parent link="base_footprint" />
    <child link="base_link" />
    <origin xyz="0 0 0" rpy="0 0 0" />
  </joint>
</robot>
"#,
        )?;

        assert_eq!(structure.name, "test-bot");
        assert_eq!(structure.links[0].name, "base_footprint");
        Ok(())
    }

    #[test]
    fn root_link_name_returns_single_root() -> anyhow::Result<()> {
        let structure = Structure::from_urdf_str(
            r#"<robot name="test-bot">
  <link name="base_footprint" />
  <link name="base_link" />
  <joint name="root" type="fixed">
    <parent link="base_footprint" />
    <child link="base_link" />
    <origin xyz="0 0 0" rpy="0 0 0" />
  </joint>
</robot>
"#,
        )?;

        assert_eq!(structure.root_link_name()?, "base_footprint");
        Ok(())
    }

    #[test]
    fn with_mounted_component_namespaces_and_attaches() -> anyhow::Result<()> {
        let base = Structure::from_urdf_str(
            r#"<robot name="test-bot">
  <link name="base_footprint" />
  <link name="base_link" />
  <link name="wheel_mount" />
  <joint name="root" type="fixed">
    <parent link="base_footprint" />
    <child link="base_link" />
    <origin xyz="0 0 0" rpy="0 0 0" />
  </joint>
  <joint name="wheel_mount_joint" type="fixed">
    <parent link="base_link" />
    <child link="wheel_mount" />
    <origin xyz="0 0 0" rpy="0 0 0" />
  </joint>
</robot>
"#,
        )?;
        let component = Structure::from_urdf_str(
            r#"<robot name="ddsm115">
  <link name="mount" />
  <link name="rotor_link" />
  <joint name="motor_joint" type="continuous">
    <parent link="mount" />
    <child link="rotor_link" />
    <origin xyz="0 0 0" rpy="0 0 0" />
  </joint>
</robot>
"#,
        )?;

        let result = base.with_mounted_component("front_left_drive", "wheel_mount", &component)?;

        let motor_joint = result
            .joint("front_left_drive__motor_joint")
            .context("missing namespaced motor joint")?;
        assert_eq!(motor_joint.joint_type, urdf_rs::JointType::Continuous);
        assert!(result.link("front_left_drive__rotor_link").is_some());

        let attach_joint = result
            .joint("front_left_drive__mount_attach")
            .context("missing mount attach joint")?;
        assert_eq!(attach_joint.joint_type, urdf_rs::JointType::Fixed);
        assert_eq!(attach_joint.parent.link, "wheel_mount");
        assert_eq!(attach_joint.child.link, "front_left_drive__mount");
        result.validate()?;
        Ok(())
    }

    #[test]
    fn with_mounted_component_unknown_mount_link_errors() -> anyhow::Result<()> {
        let base = Structure::from_urdf_str(
            r#"<robot name="test-bot">
  <link name="base_footprint" />
  <link name="base_link" />
  <joint name="root" type="fixed">
    <parent link="base_footprint" />
    <child link="base_link" />
    <origin xyz="0 0 0" rpy="0 0 0" />
  </joint>
</robot>
"#,
        )?;
        let component = Structure::from_urdf_str(
            r#"<robot name="ddsm115">
  <link name="mount" />
</robot>
"#,
        )?;

        let result = base.with_mounted_component("front_left_drive", "missing_mount", &component);

        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn with_mounted_component_two_instances_are_independent() -> anyhow::Result<()> {
        let base = Structure::from_urdf_str(
            r#"<robot name="test-bot">
  <link name="base_footprint" />
  <link name="base_link" />
  <link name="left_mount" />
  <link name="right_mount" />
  <joint name="root" type="fixed">
    <parent link="base_footprint" />
    <child link="base_link" />
    <origin xyz="0 0 0" rpy="0 0 0" />
  </joint>
  <joint name="left_mount_joint" type="fixed">
    <parent link="base_link" />
    <child link="left_mount" />
    <origin xyz="0 0 0" rpy="0 0 0" />
  </joint>
  <joint name="right_mount_joint" type="fixed">
    <parent link="base_link" />
    <child link="right_mount" />
    <origin xyz="0 0 0" rpy="0 0 0" />
  </joint>
</robot>
"#,
        )?;
        let component = Structure::from_urdf_str(
            r#"<robot name="ddsm115">
  <link name="mount" />
  <link name="rotor_link" />
  <joint name="motor_joint" type="continuous">
    <parent link="mount" />
    <child link="rotor_link" />
    <origin xyz="0 0 0" rpy="0 0 0" />
  </joint>
</robot>
"#,
        )?;

        let result = base
            .with_mounted_component("left", "left_mount", &component)?
            .with_mounted_component("right", "right_mount", &component)?;

        assert!(result.joint("left__motor_joint").is_some());
        assert!(result.joint("right__motor_joint").is_some());
        result.validate()?;
        Ok(())
    }

    #[test]
    fn validate_requires_base_footprint_root_and_base_link() -> anyhow::Result<()> {
        let structure = Structure::from_urdf_str(
            r#"<robot name="test-bot">
  <link name="base_footprint" />
  <link name="base_link" />
  <joint name="root" type="fixed">
    <parent link="base_footprint" />
    <child link="base_link" />
    <origin xyz="0 0 0.2" rpy="0 0 0" />
  </joint>
</robot>
"#,
        )?;

        structure.validate()?;
        assert_eq!(structure.root_link_name()?, BASE_FOOTPRINT_LINK);
        assert!(structure.link(BASE_LINK).is_some());
        Ok(())
    }

    #[test]
    fn validate_rejects_noncanonical_root() -> anyhow::Result<()> {
        let structure = Structure::from_urdf_str(
            r#"<robot name="test-bot">
  <link name="base_link" />
</robot>
"#,
        )?;

        let error = structure
            .validate()
            .expect_err("missing canonical footprint");
        assert!(
            error
                .to_string()
                .contains("root link must be 'base_footprint'")
        );
        Ok(())
    }

    #[test]
    fn validate_fragment_rejects_unknown_joint_link() -> anyhow::Result<()> {
        let structure = Structure::from_urdf_str(
            r#"<robot name="test-bot">
  <link name="base_footprint" />
  <joint name="root" type="fixed">
    <parent link="base_footprint" />
    <child link="missing_link" />
  </joint>
</robot>
"#,
        )?;

        let error = structure
            .validate_fragment()
            .expect_err("unknown child link should fail");
        assert!(
            error
                .to_string()
                .contains("references unknown child link 'missing_link'")
        );
        Ok(())
    }

    #[test]
    fn validate_fragment_rejects_joint_cycles() -> anyhow::Result<()> {
        let structure = Structure::from_urdf_str(
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
        )?;

        let error = structure
            .validate_fragment()
            .expect_err("joint cycle should fail");
        assert!(error.to_string().contains("joint cycle"));
        Ok(())
    }

    #[test]
    fn validate_fragment_rejects_links_with_multiple_parent_joints() -> anyhow::Result<()> {
        let structure = Structure::from_urdf_str(
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
        )?;

        let error = structure
            .validate_fragment()
            .expect_err("multiple parent joints should fail");
        assert!(error.to_string().contains("child of multiple joints"));
        Ok(())
    }
}
