//! Deterministic native Robot source derived from one admitted plan.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use anyhow::{Context, Result, bail, ensure};
use nalgebra::{Isometry3, Matrix3, Translation3, UnitQuaternion, Vector3};
use phoxal::identity::ExecutionId;
use phoxal::model::AssetId;
use phoxal::model::Robot;
use phoxal::model::component::capability::{Capability as DeclaredCapability, CapabilityKind};
use phoxal::model::geometry::Geometry;
use phoxal::model::identity::ComponentInstanceId;
use phoxal::model::simulation::{CameraProjection, Capability as SimulatedCapability};
use phoxal::model::structure::{Joint, JointKind, Link, Material, Pose, Structure};

use crate::{ROBOT_CONTROLLER_PACKAGE, generation};
use phoxal_simulator_webots_shared::plan::{CapabilityBinding, PlannedTarget, RobotSimulationPlan};

/// Stable DEF used for import, verification, rollback, and removal.
#[must_use]
pub fn robot_definition(execution: ExecutionId) -> String {
    let suffix = execution
        .to_string()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("PHOXAL_ROBOT_{suffix}")
}

/// Render one complete built-in Webots Robot without external dependencies.
pub fn render_robot(
    robot: &Robot,
    plan: &RobotSimulationPlan,
    assets: &BTreeMap<AssetId, Vec<u8>>,
    execution: ExecutionId,
    pose: Pose,
    supervisor_endpoint: &str,
    host_endpoint: &str,
) -> Result<String> {
    ensure!(
        plan.robot == robot.id().to_string(),
        "plan and Robot disagree"
    );
    for binding in &plan.capabilities {
        ensure!(
            robot.capability(binding.reference()).is_some(),
            "binding {} names a missing capability",
            binding.reference()
        );
    }
    let definition = robot_definition(execution);
    let [x, y, z] = pose.xyz();
    let [ax, ay, az, angle] = generation::axis_angle(pose);
    let mut out = String::new();
    writeln!(out, "DEF {definition} Robot {{")?;
    writeln!(
        out,
        "  translation {} {} {}",
        generation::number(x),
        generation::number(y),
        generation::number(z)
    )?;
    writeln!(
        out,
        "  rotation {} {} {} {}",
        generation::number(ax),
        generation::number(ay),
        generation::number(az),
        generation::number(angle)
    )?;
    writeln!(out, "  name \"phoxal-{execution}\"")?;
    writeln!(out, "  controller \"{ROBOT_CONTROLLER_PACKAGE}\"")?;
    writeln!(
        out,
        "  controllerArgs [\"--connect\", \"{}\", \"--host-connect\", \"{}\"]",
        generation::quoted(supervisor_endpoint),
        generation::quoted(host_endpoint)
    )?;
    writeln!(out, "  synchronization TRUE")?;
    let structure = robot.structure();
    let root = structure
        .link(structure.root_link().as_str())
        .context("validated Robot structure has no root link")?;
    render_link_body(
        &mut out, robot, plan, assets, execution, structure, None, root, 2, true,
    )?;
    writeln!(out, "}}")?;
    ensure!(
        !out.contains("<extern>"),
        "Robot source contains external controller"
    );
    ensure!(
        !out.contains("EXTERNPROTO"),
        "Robot source contains EXTERNPROTO"
    );
    Ok(out)
}

#[allow(
    clippy::too_many_arguments,
    reason = "recursive rendering carries one immutable robot/plan/structure context"
)]
fn render_link_body(
    out: &mut String,
    robot: &Robot,
    plan: &RobotSimulationPlan,
    assets: &BTreeMap<AssetId, Vec<u8>>,
    execution: ExecutionId,
    structure: &Structure,
    namespace: Option<&ComponentInstanceId>,
    link: &Link,
    indent: usize,
    root: bool,
) -> Result<()> {
    if !root {
        writeln!(
            out,
            "{:indent$}name \"{}\"",
            "",
            generation::quoted(&structural_name(namespace, link.name().as_str()))
        )?;
    }
    if let Some(material) = contact_material(plan, namespace, link.name().as_str()) {
        writeln!(
            out,
            "{:indent$}contactMaterial \"{}\"",
            "",
            generation::quoted(material)
        )?;
    }
    let assembly =
        resolve_fixed_assembly(robot, structure, namespace, link, &Isometry3::identity())?;
    writeln!(out, "{:indent$}children [", "")?;
    render_fixed_assembly(out, robot, plan, assets, execution, &assembly, indent + 2)?;
    writeln!(out, "{:indent$}]", "")?;
    let collisions = collect_assembly_collisions(&assembly);
    if !collisions.is_empty() {
        writeln!(out, "{:indent$}boundingObject Group {{", "")?;
        writeln!(out, "{:width$}children [", "", width = indent + 2)?;
        for collision in &collisions {
            render_shape_at(
                out,
                &collision.origin,
                collision.geometry,
                None,
                assets,
                execution,
                indent + 4,
                false,
            )?;
        }
        writeln!(out, "{:width$}]", "", width = indent + 2)?;
        writeln!(out, "{:indent$}}}", "")?;
    }
    let mass = collect_assembly_mass(&assembly);
    if let Some(inertial) = mass.finalize() {
        let center = inertial.center;
        let matrix = inertial.inertia;
        writeln!(out, "{:indent$}physics Physics {{", "")?;
        writeln!(out, "{:width$}density -1", "", width = indent + 2)?;
        writeln!(
            out,
            "{:width$}mass {}",
            "",
            generation::number(inertial.mass),
            width = indent + 2
        )?;
        writeln!(
            out,
            "{:width$}centerOfMass [ {} {} {} ]",
            "",
            generation::number(center[0]),
            generation::number(center[1]),
            generation::number(center[2]),
            width = indent + 2
        )?;
        writeln!(
            out,
            "{:width$}inertiaMatrix [ {} {} {} {} {} {} ]",
            "",
            generation::number(matrix[(0, 0)]),
            generation::number(matrix[(1, 1)]),
            generation::number(matrix[(2, 2)]),
            generation::number(matrix[(0, 1)]),
            generation::number(matrix[(0, 2)]),
            generation::number(matrix[(1, 2)]),
            width = indent + 2
        )?;
        writeln!(out, "{:indent$}}}", "")?;
    }
    Ok(())
}

fn contact_material<'a>(
    plan: &'a RobotSimulationPlan,
    namespace: Option<&ComponentInstanceId>,
    link: &str,
) -> Option<&'a str> {
    let component = namespace?;
    plan.links
        .iter()
        .find(|planned| planned.component == *component && planned.link == link)
        .and_then(|planned| planned.contact_material.as_deref())
}

fn structural_name(namespace: Option<&ComponentInstanceId>, local: &str) -> String {
    namespace.map_or_else(
        || local.to_owned(),
        |component| format!("{component}__{local}"),
    )
}

fn render_pose_wrapper(
    out: &mut String,
    transform: &Isometry3<f64>,
    indent: usize,
    render: impl FnOnce(&mut String) -> Result<()>,
) -> Result<()> {
    if is_identity(transform) {
        return render(out);
    }
    writeln!(out, "{:indent$}Pose {{", "")?;
    render_isometry(out, transform, indent + 2)?;
    writeln!(out, "{:width$}children [", "", width = indent + 2)?;
    render(out)?;
    writeln!(out, "{:width$}]", "", width = indent + 2)?;
    writeln!(out, "{:indent$}}}", "")?;
    Ok(())
}

struct StagedCollision<'a> {
    origin: Isometry3<f64>,
    geometry: &'a Geometry,
}

/// One fixed rigid assembly, resolved once for its rendering-adjacent physics
/// facts. The same transform tree feeds collision and inertial lowering.
struct ResolvedAssemblyLink<'a> {
    structure: &'a Structure,
    namespace: Option<&'a ComponentInstanceId>,
    link: &'a Link,
    transform: Isometry3<f64>,
}

fn resolve_fixed_assembly<'a>(
    robot: &'a Robot,
    structure: &'a Structure,
    namespace: Option<&'a ComponentInstanceId>,
    root: &'a Link,
    transform: &Isometry3<f64>,
) -> Result<Vec<ResolvedAssemblyLink<'a>>> {
    let mut resolved = Vec::new();
    resolve_fixed_assembly_at(&mut resolved, robot, structure, namespace, root, transform)?;
    Ok(resolved)
}

fn resolve_fixed_assembly_at<'a>(
    resolved: &mut Vec<ResolvedAssemblyLink<'a>>,
    robot: &'a Robot,
    structure: &'a Structure,
    namespace: Option<&'a ComponentInstanceId>,
    link: &'a Link,
    transform: &Isometry3<f64>,
) -> Result<()> {
    resolved.push(ResolvedAssemblyLink {
        structure,
        namespace,
        link,
        transform: *transform,
    });
    if namespace.is_none() {
        for component in robot
            .components()
            .filter(|component| component.instance().mount_link() == link.name())
        {
            let mounted = component.component_type().structure();
            let root = mounted
                .link(mounted.root_link().as_str())
                .with_context(|| format!("component {} has no root link", component.id()))?;
            resolve_fixed_assembly_at(
                resolved,
                robot,
                mounted,
                Some(component.id()),
                root,
                transform,
            )?;
        }
    }
    for joint in structure
        .child_joints(link.name().as_str())
        .filter(|joint| joint.kind() == JointKind::Fixed)
    {
        let child = structure
            .link(joint.child().as_str())
            .with_context(|| format!("joint {} has no child link", joint.name()))?;
        let child_transform = transform * pose_to_isometry(joint.origin());
        resolve_fixed_assembly_at(
            resolved,
            robot,
            structure,
            namespace,
            child,
            &child_transform,
        )?;
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "lowering needs the complete native context"
)]
fn render_fixed_assembly(
    out: &mut String,
    robot: &Robot,
    plan: &RobotSimulationPlan,
    assets: &BTreeMap<AssetId, Vec<u8>>,
    execution: ExecutionId,
    assembly: &[ResolvedAssemblyLink<'_>],
    indent: usize,
) -> Result<()> {
    for resolved in assembly {
        let mut devices = String::new();
        render_link_devices(
            &mut devices,
            robot,
            plan,
            execution,
            resolved.namespace,
            resolved.link,
            indent + 2,
        )?;
        if !devices.is_empty() {
            render_pose_wrapper(out, &resolved.transform, indent, |out| {
                out.push_str(&devices);
                Ok(())
            })?;
        }
        for visual in resolved.link.visuals() {
            let origin = resolved.transform * pose_to_isometry(visual.origin());
            render_shape_at(
                out,
                &origin,
                visual.geometry(),
                visual.material(),
                assets,
                execution,
                indent,
                true,
            )?;
        }
        for joint in resolved
            .structure
            .child_joints(resolved.link.name().as_str())
            .filter(|joint| joint.kind() != JointKind::Fixed)
        {
            let mut rendered = String::new();
            render_joint(
                &mut rendered,
                robot,
                plan,
                assets,
                execution,
                resolved.structure,
                resolved.namespace,
                joint,
                indent + 2,
            )?;
            render_pose_wrapper(out, &resolved.transform, indent, |out| {
                out.push_str(&rendered);
                Ok(())
            })?;
        }
    }
    Ok(())
}

fn collect_assembly_collisions<'a>(
    assembly: &'a [ResolvedAssemblyLink<'a>],
) -> Vec<StagedCollision<'a>> {
    assembly
        .iter()
        .flat_map(|resolved| {
            resolved.link.collisions().map(|collision| StagedCollision {
                origin: resolved.transform * pose_to_isometry(collision.origin()),
                geometry: collision.geometry(),
            })
        })
        .collect()
}

#[derive(Default)]
struct MassProperties {
    mass: f64,
    weighted_center: Vector3<f64>,
    inertia_about_root: Matrix3<f64>,
}

struct ResolvedMassProperties {
    mass: f64,
    center: Vector3<f64>,
    inertia: Matrix3<f64>,
}

impl MassProperties {
    fn add_link(&mut self, link: &Link, transform: &Isometry3<f64>) {
        let inertial = link.inertial();
        let mass = inertial.mass_kg();
        if mass <= 0.0 {
            return;
        }
        let inertial_transform = transform * pose_to_isometry(inertial.origin());
        let center = inertial_transform.translation.vector;
        let [ixx, ixy, ixz, iyy, iyz, izz] = inertial.inertia().values();
        let local = Matrix3::new(ixx, ixy, ixz, ixy, iyy, iyz, ixz, iyz, izz);
        let rotation = inertial_transform.rotation.to_rotation_matrix();
        let rotated = rotation.matrix() * local * rotation.matrix().transpose();
        self.mass += mass;
        self.weighted_center += center * mass;
        self.inertia_about_root += rotated + parallel_axis(mass, &center);
    }

    #[cfg(test)]
    fn extend(&mut self, other: Self) {
        self.mass += other.mass;
        self.weighted_center += other.weighted_center;
        self.inertia_about_root += other.inertia_about_root;
    }

    fn finalize(self) -> Option<ResolvedMassProperties> {
        if self.mass <= 0.0 {
            return None;
        }
        let center = self.weighted_center / self.mass;
        Some(ResolvedMassProperties {
            mass: self.mass,
            center,
            inertia: self.inertia_about_root - parallel_axis(self.mass, &center),
        })
    }
}

fn collect_assembly_mass(assembly: &[ResolvedAssemblyLink<'_>]) -> MassProperties {
    let mut mass = MassProperties::default();
    for resolved in assembly {
        mass.add_link(resolved.link, &resolved.transform);
    }
    mass
}

fn parallel_axis(mass: f64, displacement: &Vector3<f64>) -> Matrix3<f64> {
    mass * (Matrix3::identity() * displacement.dot(displacement)
        - displacement * displacement.transpose())
}

fn pose_to_isometry(pose: Pose) -> Isometry3<f64> {
    let [x, y, z] = pose.xyz();
    let [roll, pitch, yaw] = pose.rpy();
    Isometry3::from_parts(
        Translation3::new(x, y, z),
        UnitQuaternion::from_euler_angles(roll, pitch, yaw),
    )
}

fn is_identity(transform: &Isometry3<f64>) -> bool {
    transform.translation.vector.norm() <= 1.0e-12 && transform.rotation.angle() <= 1.0e-12
}

fn render_isometry(out: &mut String, transform: &Isometry3<f64>, indent: usize) -> Result<()> {
    let translation = transform.translation.vector;
    let rotation = transform
        .rotation
        .axis_angle()
        .map_or([0.0, 0.0, 1.0, 0.0], |(axis, angle)| {
            [axis.x, axis.y, axis.z, angle]
        });
    render_pose(
        out,
        [translation.x, translation.y, translation.z],
        rotation,
        indent,
    )
}

fn render_link_devices(
    out: &mut String,
    robot: &Robot,
    plan: &RobotSimulationPlan,
    execution: ExecutionId,
    namespace: Option<&ComponentInstanceId>,
    link: &Link,
    indent: usize,
) -> Result<()> {
    let target = structural_name(namespace, link.name().as_str());
    for binding in plan
        .capabilities
        .iter()
        .filter(|binding| matches!(binding.target(), PlannedTarget::Link { id } if id == &target))
    {
        let declared = robot
            .capability(binding.reference())
            .with_context(|| format!("planned capability {} disappeared", binding.reference()))?;
        let simulated = robot
            .component(binding.reference().component_id.as_str())
            .and_then(|component| component.simulation())
            .and_then(|simulation| {
                simulation.capability(binding.reference().capability_id.as_str())
            })
            .with_context(|| format!("planned simulation {} disappeared", binding.reference()))?;
        render_link_device(out, binding, declared, simulated, execution, indent)?;
    }
    Ok(())
}

fn render_link_device(
    out: &mut String,
    binding: &CapabilityBinding,
    declared: &DeclaredCapability,
    simulated: &SimulatedCapability,
    execution: ExecutionId,
    indent: usize,
) -> Result<()> {
    let name = generation::quoted(binding.native_device());
    match (declared, simulated) {
        (DeclaredCapability::Accelerometer(_), SimulatedCapability::Accelerometer(config)) => {
            writeln!(out, "{:indent$}Accelerometer {{", "")?;
            writeln!(out, "{:width$}name \"{name}\"", "", width = indent + 2)?;
            render_resolution(out, config.resolution, indent + 2)?;
            render_lookup_table(out, config.lookup_table.as_deref(), indent + 2)?;
            writeln!(out, "{:indent$}}}", "")?;
        }
        (DeclaredCapability::Gyroscope(_), SimulatedCapability::Gyroscope(config)) => {
            writeln!(out, "{:indent$}Gyro {{", "")?;
            writeln!(out, "{:width$}name \"{name}\"", "", width = indent + 2)?;
            render_resolution(out, config.resolution, indent + 2)?;
            render_lookup_table(out, config.lookup_table.as_deref(), indent + 2)?;
            writeln!(out, "{:indent$}}}", "")?;
        }
        (DeclaredCapability::Imu(_), SimulatedCapability::Imu(config)) => {
            writeln!(out, "{:indent$}InertialUnit {{", "")?;
            writeln!(out, "{:width$}name \"{name}\"", "", width = indent + 2)?;
            render_resolution(out, config.resolution, indent + 2)?;
            if let Some(noise) = config.noise {
                writeln!(
                    out,
                    "{:width$}noise {}",
                    "",
                    generation::number(noise),
                    width = indent + 2
                )?;
            }
            writeln!(out, "{:indent$}}}", "")?;
            for (node, suffix) in [("Accelerometer", "__accel"), ("Gyro", "__gyro")] {
                writeln!(out, "{:indent$}{node} {{", "")?;
                writeln!(
                    out,
                    "{:width$}name \"{}{}\"",
                    "",
                    name,
                    suffix,
                    width = indent + 2
                )?;
                writeln!(out, "{:indent$}}}", "")?;
            }
        }
        (DeclaredCapability::Gnss(_), SimulatedCapability::Gnss(config)) => {
            render_gnss_node(out, &name, config, indent)?;
        }
        (DeclaredCapability::Camera(declared), SimulatedCapability::Camera(config)) => {
            writeln!(out, "{:indent$}Camera {{", "")?;
            writeln!(out, "{:width$}name \"{name}\"", "", width = indent + 2)?;
            writeln!(
                out,
                "{:width$}width {}",
                "",
                declared.width_px,
                width = indent + 2
            )?;
            writeln!(
                out,
                "{:width$}height {}",
                "",
                declared.height_px,
                width = indent + 2
            )?;
            if let Some(fov) = declared.field_of_view_rad {
                writeln!(
                    out,
                    "{:width$}fieldOfView {}",
                    "",
                    generation::number(fov),
                    width = indent + 2
                )?;
            }
            if let Some(projection) = config.projection {
                let projection = match projection {
                    CameraProjection::Planar => "planar",
                    CameraProjection::Cylindrical => "cylindrical",
                    CameraProjection::Spherical => "spherical",
                };
                writeln!(
                    out,
                    "{:width$}projection \"{projection}\"",
                    "",
                    width = indent + 2
                )?;
            }
            render_camera_bounds(out, config.near, config.far, indent + 2)?;
            render_optional_number(out, "exposure", config.exposure, indent + 2)?;
            render_optional_bool(out, "antiAliasing", config.anti_aliasing, indent + 2)?;
            render_optional_number(
                out,
                "ambientOcclusionRadius",
                config.ambient_occlusion_radius,
                indent + 2,
            )?;
            render_optional_number(out, "bloomThreshold", config.bloom_threshold, indent + 2)?;
            render_optional_number(out, "motionBlur", config.motion_blur, indent + 2)?;
            render_noise(out, config.noise, indent + 2)?;
            if let Some(mask) = &config.noise_mask_url {
                writeln!(
                    out,
                    "{:width$}noiseMaskUrl \"../assets/robots/{}/{}\"",
                    "",
                    execution,
                    generation::quoted(mask),
                    width = indent + 2
                )?;
            }
            writeln!(out, "{:indent$}}}", "")?;
        }
        (DeclaredCapability::Depth(declared), SimulatedCapability::Depth(config)) => {
            writeln!(out, "{:indent$}RangeFinder {{", "")?;
            writeln!(out, "{:width$}name \"{name}\"", "", width = indent + 2)?;
            writeln!(
                out,
                "{:width$}width {}",
                "",
                declared.width_px,
                width = indent + 2
            )?;
            writeln!(
                out,
                "{:width$}height {}",
                "",
                declared.height_px,
                width = indent + 2
            )?;
            if let Some(fov) = declared.field_of_view_rad {
                writeln!(
                    out,
                    "{:width$}fieldOfView {}",
                    "",
                    generation::number(fov),
                    width = indent + 2
                )?;
            }
            render_optional_number(out, "minRange", declared.min_range_m, indent + 2)?;
            render_optional_number(out, "maxRange", declared.max_range_m, indent + 2)?;
            render_resolution(out, config.resolution, indent + 2)?;
            render_noise(out, config.noise, indent + 2)?;
            render_optional_number(out, "motionBlur", config.motion_blur, indent + 2)?;
            writeln!(out, "{:indent$}}}", "")?;
        }
        (DeclaredCapability::Range(declared), SimulatedCapability::Range(config)) => {
            writeln!(out, "{:indent$}DistanceSensor {{", "")?;
            writeln!(out, "{:width$}name \"{name}\"", "", width = indent + 2)?;
            writeln!(out, "{:width$}type \"laser\"", "", width = indent + 2)?;
            writeln!(
                out,
                "{:width$}aperture {}",
                "",
                generation::number(declared.field_of_view_rad),
                width = indent + 2
            )?;
            writeln!(out, "{:width$}lookupTable [", "", width = indent + 2)?;
            for distance in [declared.min_range_m, declared.max_range_m] {
                writeln!(
                    out,
                    "{:width$}{} {} {}",
                    "",
                    generation::number(distance),
                    generation::number(distance),
                    generation::number(config.noise.unwrap_or(0.0)),
                    width = indent + 4
                )?;
            }
            writeln!(out, "{:width$}]", "", width = indent + 2)?;
            render_resolution(out, config.resolution, indent + 2)?;
            writeln!(out, "{:indent$}}}", "")?;
        }
        _ => bail!(
            "planned link device {} does not match its compiled capability",
            binding.reference()
        ),
    }
    Ok(())
}

fn render_gnss_node(
    out: &mut String,
    name: &str,
    config: &phoxal::model::simulation::Gnss,
    indent: usize,
) -> Result<()> {
    writeln!(out, "{:indent$}GPS {{", "")?;
    writeln!(out, "{:width$}name \"{name}\"", "", width = indent + 2)?;
    render_resolution(out, config.resolution, indent + 2)?;
    render_optional_number(out, "accuracy", config.accuracy, indent + 2)?;
    render_optional_number(
        out,
        "noiseCorrelation",
        config.noise_correlation,
        indent + 2,
    )?;
    render_optional_number(out, "speedResolution", config.speed_resolution, indent + 2)?;
    render_optional_number(out, "speedNoise", config.speed_noise, indent + 2)?;
    writeln!(out, "{:indent$}}}", "")?;
    Ok(())
}

fn render_resolution(out: &mut String, value: Option<f64>, indent: usize) -> Result<()> {
    if let Some(value) = value {
        writeln!(
            out,
            "{:indent$}resolution {}",
            "",
            generation::number(value)
        )?;
    }
    Ok(())
}

fn render_noise(out: &mut String, value: Option<f64>, indent: usize) -> Result<()> {
    if let Some(value) = value {
        writeln!(out, "{:indent$}noise {}", "", generation::number(value))?;
    }
    Ok(())
}

fn render_optional_number(
    out: &mut String,
    field: &str,
    value: Option<f64>,
    indent: usize,
) -> Result<()> {
    if let Some(value) = value {
        writeln!(out, "{:indent$}{field} {}", "", generation::number(value))?;
    }
    Ok(())
}

fn render_optional_bool(
    out: &mut String,
    field: &str,
    value: Option<bool>,
    indent: usize,
) -> Result<()> {
    if let Some(value) = value {
        writeln!(
            out,
            "{:indent$}{field} {}",
            "",
            if value { "TRUE" } else { "FALSE" }
        )?;
    }
    Ok(())
}

fn render_camera_bounds(
    out: &mut String,
    near: Option<f64>,
    far: Option<f64>,
    indent: usize,
) -> Result<()> {
    if let Some(value) = near {
        writeln!(out, "{:indent$}near {}", "", generation::number(value))?;
    }
    if let Some(value) = far {
        writeln!(out, "{:indent$}far {}", "", generation::number(value))?;
    }
    Ok(())
}

fn render_lookup_table(out: &mut String, table: Option<&[Vec<f64>]>, indent: usize) -> Result<()> {
    let Some(table) = table else {
        return Ok(());
    };
    writeln!(out, "{:indent$}lookupTable [", "")?;
    for row in table {
        ensure!(
            row.len() == 3,
            "Webots lookup-table rows must have three values"
        );
        writeln!(
            out,
            "{:width$}{} {} {}",
            "",
            generation::number(row[0]),
            generation::number(row[1]),
            generation::number(row[2]),
            width = indent + 2
        )?;
    }
    writeln!(out, "{:indent$}]", "")?;
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "joint rendering needs the immutable context and its indentation"
)]
fn render_joint(
    out: &mut String,
    robot: &Robot,
    plan: &RobotSimulationPlan,
    assets: &BTreeMap<AssetId, Vec<u8>>,
    execution: ExecutionId,
    structure: &Structure,
    namespace: Option<&ComponentInstanceId>,
    joint: &Joint,
    indent: usize,
) -> Result<()> {
    let child = structure
        .link(joint.child().as_str())
        .with_context(|| format!("joint {} has no child link", joint.name()))?;
    let pose = joint.origin();
    let [x, y, z] = pose.xyz();
    let [ax, ay, az, angle] = generation::axis_angle(pose);
    match joint.kind() {
        JointKind::Fixed => {
            writeln!(out, "{:indent$}Solid {{", "")?;
            render_pose(out, [x, y, z], [ax, ay, az, angle], indent + 2)?;
            render_link_body(
                out,
                robot,
                plan,
                assets,
                execution,
                structure,
                namespace,
                child,
                indent + 2,
                false,
            )?;
            writeln!(out, "{:indent$}}}", "")?;
        }
        kind @ (JointKind::Revolute | JointKind::Continuous | JointKind::Prismatic) => {
            let (node, parameters, motor) = match kind {
                JointKind::Revolute | JointKind::Continuous => {
                    ("HingeJoint", "HingeJointParameters", "RotationalMotor")
                }
                JointKind::Prismatic => ("SliderJoint", "JointParameters", "LinearMotor"),
                _ => unreachable!(),
            };
            writeln!(out, "{:indent$}{node} {{", "")?;
            writeln!(
                out,
                "{:width$}jointParameters {parameters} {{",
                "",
                width = indent + 2
            )?;
            let dynamics = joint.dynamics();
            render_joint_parameters(
                out,
                kind,
                joint.axis(),
                [x, y, z],
                [joint.limit().lower(), joint.limit().upper()],
                dynamics.map_or(0.0, |value| value.damping()),
                dynamics.map_or(0.0, |value| value.friction()),
                indent + 4,
            )?;
            writeln!(out, "{:width$}}}", "", width = indent + 2)?;
            let devices = plan
                .capabilities
                .iter()
                .filter(|binding| {
                    matches!(binding.target(), PlannedTarget::Joint { id } if id == &structural_name(namespace, joint.name().as_str()))
                })
                .collect::<Vec<_>>();
            if !devices.is_empty() {
                writeln!(out, "{:width$}device [", "", width = indent + 2)?;
                for binding in devices {
                    match binding.kind() {
                        CapabilityKind::Motor => {
                            let declared =
                                robot.capability(binding.reference()).with_context(|| {
                                    format!("planned motor {} disappeared", binding.reference())
                                })?;
                            let simulated = robot
                                .component(binding.reference().component_id.as_str())
                                .and_then(|component| component.simulation())
                                .and_then(|simulation| {
                                    simulation
                                        .capability(binding.reference().capability_id.as_str())
                                })
                                .with_context(|| {
                                    format!(
                                        "planned motor simulation {} disappeared",
                                        binding.reference()
                                    )
                                })?;
                            let (
                                DeclaredCapability::Motor(declared),
                                SimulatedCapability::Motor(simulated),
                            ) = (declared, simulated)
                            else {
                                bail!("planned motor {} changed kind", binding.reference());
                            };
                            writeln!(out, "{:width$}{motor} {{", "", width = indent + 4)?;
                            writeln!(
                                out,
                                "{:width$}name \"{}\"",
                                "",
                                generation::quoted(binding.native_device()),
                                width = indent + 6
                            )?;
                            if let Some(velocity) = declared.max_velocity_radps {
                                writeln!(
                                    out,
                                    "{:width$}maxVelocity {}",
                                    "",
                                    generation::number(velocity / declared.gear_ratio.abs()),
                                    width = indent + 6
                                )?;
                            }
                            if let Some(torque) = declared.max_torque_nm {
                                writeln!(
                                    out,
                                    "{:width$}maxTorque {}",
                                    "",
                                    generation::number(torque * declared.gear_ratio.abs()),
                                    width = indent + 6
                                )?;
                            }
                            if let Some(acceleration) = simulated.acceleration_radps2 {
                                writeln!(
                                    out,
                                    "{:width$}acceleration {}",
                                    "",
                                    generation::number(acceleration / declared.gear_ratio.abs()),
                                    width = indent + 6
                                )?;
                            }
                            if let Some(pid) = &simulated.control_pid {
                                writeln!(
                                    out,
                                    "{:width$}controlPID {} {} {}",
                                    "",
                                    generation::number(pid[0]),
                                    generation::number(pid[1]),
                                    generation::number(pid[2]),
                                    width = indent + 6
                                )?;
                            }
                            writeln!(out, "{:width$}}}", "", width = indent + 4)?;
                        }
                        CapabilityKind::Encoder => {
                            let simulated = robot
                                .component(binding.reference().component_id.as_str())
                                .and_then(|component| component.simulation())
                                .and_then(|simulation| {
                                    simulation
                                        .capability(binding.reference().capability_id.as_str())
                                })
                                .with_context(|| {
                                    format!(
                                        "planned encoder simulation {} disappeared",
                                        binding.reference()
                                    )
                                })?;
                            let SimulatedCapability::Encoder(simulated) = simulated else {
                                bail!("planned encoder {} changed kind", binding.reference());
                            };
                            writeln!(out, "{:width$}PositionSensor {{", "", width = indent + 4)?;
                            writeln!(
                                out,
                                "{:width$}name \"{}\"",
                                "",
                                generation::quoted(binding.native_device()),
                                width = indent + 6
                            )?;
                            render_resolution(out, simulated.resolution, indent + 6)?;
                            render_noise(out, simulated.noise, indent + 6)?;
                            writeln!(out, "{:width$}}}", "", width = indent + 4)?;
                        }
                        other => bail!("unsupported planned joint device {other}"),
                    }
                }
                writeln!(out, "{:width$}]", "", width = indent + 2)?;
            }
            writeln!(out, "{:width$}endPoint Solid {{", "", width = indent + 2)?;
            render_pose(out, [x, y, z], [ax, ay, az, angle], indent + 4)?;
            render_link_body(
                out,
                robot,
                plan,
                assets,
                execution,
                structure,
                namespace,
                child,
                indent + 4,
                false,
            )?;
            writeln!(out, "{:width$}}}", "", width = indent + 2)?;
            writeln!(out, "{:indent$}}}", "")?;
        }
        JointKind::Floating | JointKind::Planar | JointKind::Spherical => {
            bail!(
                "Webots generation does not support {:?} joint {}",
                joint.kind(),
                joint.name()
            );
        }
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "the Webots joint record has seven independent validated fields"
)]
fn render_joint_parameters(
    out: &mut String,
    kind: JointKind,
    axis: [f64; 3],
    anchor: [f64; 3],
    limits: [f64; 2],
    damping: f64,
    friction: f64,
    indent: usize,
) -> Result<()> {
    writeln!(
        out,
        "{:indent$}axis {} {} {}",
        "",
        generation::number(axis[0]),
        generation::number(axis[1]),
        generation::number(axis[2])
    )?;
    if matches!(kind, JointKind::Revolute | JointKind::Continuous) {
        writeln!(
            out,
            "{:indent$}anchor {} {} {}",
            "",
            generation::number(anchor[0]),
            generation::number(anchor[1]),
            generation::number(anchor[2])
        )?;
    }
    writeln!(
        out,
        "{:indent$}dampingConstant {}",
        "",
        generation::number(damping)
    )?;
    writeln!(
        out,
        "{:indent$}staticFriction {}",
        "",
        generation::number(friction)
    )?;
    if matches!(kind, JointKind::Revolute | JointKind::Prismatic) {
        writeln!(
            out,
            "{:indent$}minStop {}",
            "",
            generation::number(limits[0])
        )?;
        writeln!(
            out,
            "{:indent$}maxStop {}",
            "",
            generation::number(limits[1])
        )?;
    }
    Ok(())
}

fn render_pose(out: &mut String, xyz: [f64; 3], rotation: [f64; 4], indent: usize) -> Result<()> {
    writeln!(
        out,
        "{:indent$}translation {} {} {}",
        "",
        generation::number(xyz[0]),
        generation::number(xyz[1]),
        generation::number(xyz[2])
    )?;
    writeln!(
        out,
        "{:indent$}rotation {} {} {} {}",
        "",
        generation::number(rotation[0]),
        generation::number(rotation[1]),
        generation::number(rotation[2]),
        generation::number(rotation[3])
    )?;
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "native shape rendering carries its authored facts plus staging context"
)]
fn render_shape_at(
    out: &mut String,
    transform: &Isometry3<f64>,
    geometry: &Geometry,
    material: Option<&Material>,
    assets: &BTreeMap<AssetId, Vec<u8>>,
    execution: ExecutionId,
    indent: usize,
    appearance: bool,
) -> Result<()> {
    let wrapper = if appearance { "Transform" } else { "Pose" };
    writeln!(out, "{:indent$}{wrapper} {{", "")?;
    render_isometry(out, transform, indent + 2)?;
    writeln!(out, "{:width$}children [", "", width = indent + 2)?;
    match geometry {
        Geometry::Mesh { asset, scale } => {
            ensure!(
                material.is_none(),
                "Robot mesh visuals must use their bundled materials"
            );
            let decoded = crate::obj::decode(asset, assets).with_context(|| {
                format!("Robot mesh asset {asset} is outside the supported subset")
            })?;
            if appearance {
                writeln!(out, "{:width$}Transform {{", "", width = indent + 4)?;
                if let Some(scale) = scale {
                    writeln!(
                        out,
                        "{:width$}scale {} {} {}",
                        "",
                        generation::number(scale[0]),
                        generation::number(scale[1]),
                        generation::number(scale[2]),
                        width = indent + 6
                    )?;
                }
                writeln!(out, "{:width$}children [", "", width = indent + 6)?;
                decoded.render_visual(out, indent + 8, |primitive| {
                    Ok(format!(
                        "../.phoxal/textures/robots/{}/{}",
                        execution,
                        generation::extracted_image_path(asset.as_str(), primitive)
                    ))
                })?;
                writeln!(out, "{:width$}]", "", width = indent + 6)?;
                writeln!(out, "{:width$}}}", "", width = indent + 4)?;
            } else {
                decoded.render_collision_scaled(out, indent + 4, scale.unwrap_or([1.0; 3]))?;
            }
        }
        primitive => {
            writeln!(out, "{:width$}Shape {{", "", width = indent + 4)?;
            if appearance {
                render_material(out, material, assets, execution, indent + 6)?;
            }
            write!(out, "{:width$}geometry ", "", width = indent + 6)?;
            render_primitive(out, primitive)?;
            writeln!(out, "{:width$}}}", "", width = indent + 4)?;
        }
    }
    writeln!(out, "{:width$}]", "", width = indent + 2)?;
    writeln!(out, "{:indent$}}}", "")?;
    Ok(())
}

fn render_material(
    out: &mut String,
    material: Option<&Material>,
    assets: &BTreeMap<AssetId, Vec<u8>>,
    execution: ExecutionId,
    indent: usize,
) -> Result<()> {
    let explicit_color = material.and_then(Material::color);
    let color = explicit_color.unwrap_or([0.6, 0.6, 0.6, 1.0]);
    let texture = material.and_then(Material::texture);
    ensure!(
        texture.is_none() || explicit_color.is_none() || color == [1.0; 4],
        "Robot visual material cannot combine a texture with a non-white color exactly"
    );
    writeln!(out, "{:indent$}appearance PBRAppearance {{", "")?;
    writeln!(
        out,
        "{:width$}baseColor {} {} {}",
        "",
        generation::number(if texture.is_some() { 1.0 } else { color[0] }),
        generation::number(if texture.is_some() { 1.0 } else { color[1] }),
        generation::number(if texture.is_some() { 1.0 } else { color[2] }),
        width = indent + 2
    )?;
    if texture.is_none() && color[3] < 1.0 {
        writeln!(
            out,
            "{:width$}transparency {}",
            "",
            generation::number(1.0 - color[3]),
            width = indent + 2
        )?;
    }
    writeln!(out, "{:width$}roughness 0.7", "", width = indent + 2)?;
    if let Some(texture) = texture {
        let bytes = assets.get(texture).with_context(|| {
            format!("Robot material texture asset {texture} was not prefetched")
        })?;
        let format = image::guess_format(bytes).with_context(|| {
            format!("Robot material texture asset {texture} has no recognized image format")
        })?;
        ensure!(
            matches!(format, image::ImageFormat::Png | image::ImageFormat::Jpeg),
            "Robot material texture asset {texture} must be PNG or JPEG"
        );
        let decoded = image::load_from_memory_with_format(bytes, format)
            .with_context(|| format!("Robot material texture asset {texture} is not decodable"))?;
        ensure!(
            decoded.width() > 0 && decoded.height() > 0,
            "Robot material texture asset {texture} has zero dimensions"
        );
        writeln!(
            out,
            "{:width$}baseColorMap ImageTexture {{",
            "",
            width = indent + 2
        )?;
        writeln!(
            out,
            "{:width$}url [\"../assets/robots/{}/{}\"]",
            "",
            execution,
            generation::quoted(texture.as_str()),
            width = indent + 4
        )?;
        writeln!(out, "{:width$}}}", "", width = indent + 2)?;
    }
    writeln!(out, "{:indent$}}}", "")?;
    Ok(())
}

fn render_primitive(out: &mut String, geometry: &Geometry) -> Result<()> {
    match geometry {
        Geometry::Box { size } => writeln!(
            out,
            "Box {{ size {} {} {} }}",
            generation::number(size[0]),
            generation::number(size[1]),
            generation::number(size[2])
        )?,
        Geometry::Cylinder { radius, length } => writeln!(
            out,
            "Cylinder {{ radius {} height {} }}",
            generation::number(*radius),
            generation::number(*length)
        )?,
        Geometry::Capsule { radius, length } => writeln!(
            out,
            "Capsule {{ radius {} height {} }}",
            generation::number(*radius),
            generation::number(*length)
        )?,
        Geometry::Sphere { radius } => {
            writeln!(out, "Sphere {{ radius {} }}", generation::number(*radius))?;
        }
        Geometry::Mesh { .. } => bail!("mesh must use the decoded native renderer"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoxal::model::builder::{
        Collision, Inertial, Link, Material as BuilderMaterial, RobotBuilder, Visual,
    };
    use phoxal::model::geometry::Geometry;
    use phoxal::model::simulation;

    #[test]
    fn joint_parameters_retain_anchor_damping_friction_and_limits() {
        let mut revolute = String::new();
        render_joint_parameters(
            &mut revolute,
            JointKind::Revolute,
            [0.0, 1.0, 0.0],
            [1.0, 2.0, 3.0],
            [-0.5, 0.75],
            0.125,
            0.25,
            0,
        )
        .expect("revolute parameters render");
        assert!(revolute.contains("axis 0 1 0"));
        assert!(revolute.contains("anchor 1 2 3"));
        assert!(revolute.contains("dampingConstant 0.125"));
        assert!(revolute.contains("staticFriction 0.25"));
        assert!(revolute.contains("minStop -0.5"));
        assert!(revolute.contains("maxStop 0.75"));

        let mut prismatic = String::new();
        render_joint_parameters(
            &mut prismatic,
            JointKind::Prismatic,
            [1.0, 0.0, 0.0],
            [4.0, 5.0, 6.0],
            [-1.0, 2.0],
            0.0,
            0.0,
            0,
        )
        .expect("prismatic parameters render");
        assert!(!prismatic.contains("anchor"));
        assert!(prismatic.contains("minStop -1"));
        assert!(prismatic.contains("maxStop 2"));
    }

    #[test]
    fn fixed_transform_composition_moves_child_facts_into_one_assembly() {
        let parent: Pose = serde_json::from_value(serde_json::json!({
            "xyz": [1.0, 0.0, 0.0],
            "rpy": [0.0, 0.0, std::f64::consts::FRAC_PI_2]
        }))
        .expect("parent pose");
        let child: Pose = serde_json::from_value(serde_json::json!({
            "xyz": [1.0, 0.0, 0.0],
            "rpy": [0.0, 0.0, 0.0]
        }))
        .expect("child pose");
        let combined = pose_to_isometry(parent) * pose_to_isometry(child);
        assert!((combined.translation.x - 1.0).abs() < 1.0e-12);
        assert!((combined.translation.y - 1.0).abs() < 1.0e-12);
        assert!(combined.translation.z.abs() < 1.0e-12);
        assert!((combined.rotation.angle() - std::f64::consts::FRAC_PI_2).abs() < 1.0e-12);
    }

    #[test]
    fn fixed_point_masses_use_parallel_axis_combination_about_the_final_center() {
        let mut combined = MassProperties {
            mass: 1.0,
            weighted_center: Vector3::zeros(),
            inertia_about_root: Matrix3::zeros(),
        };
        let offset = Vector3::new(2.0, 0.0, 0.0);
        combined.extend(MassProperties {
            mass: 1.0,
            weighted_center: offset,
            inertia_about_root: parallel_axis(1.0, &offset),
        });
        let resolved = combined.finalize().expect("positive combined mass");
        assert_eq!(resolved.mass, 2.0);
        assert!((resolved.center.x - 1.0).abs() < 1.0e-12);
        assert!(resolved.center.y.abs() < 1.0e-12);
        assert!(resolved.center.z.abs() < 1.0e-12);
        assert!(resolved.inertia[(0, 0)].abs() < 1.0e-12);
        assert!((resolved.inertia[(1, 1)] - 2.0).abs() < 1.0e-12);
        assert!((resolved.inertia[(2, 2)] - 2.0).abs() < 1.0e-12);
    }

    #[test]
    fn gps_node_uses_world_info_for_its_coordinate_system() {
        let mut source = String::new();
        render_gnss_node(
            &mut source,
            "gnss",
            &simulation::Gnss {
                sampling_period_hz: 10.0,
                resolution: Some(0.01),
                accuracy: Some(0.5),
                ..Default::default()
            },
            0,
        )
        .expect("GPS renders");
        assert!(source.starts_with("GPS {\n"));
        assert!(source.contains("name \"gnss\""));
        assert!(source.contains("resolution 0.01"));
        assert!(source.contains("accuracy 0.5"));
        assert!(!source.contains("coordinateSystem"));
    }

    #[test]
    fn mounted_component_structure_is_inlined_with_namespaced_devices() {
        let robot = RobotBuilder::new("rover")
            .component_type("wheel", |builder| {
                builder.encoder("encoder", "axle").simulated(
                    "encoder",
                    simulation::Capability::Encoder(simulation::Encoder {
                        sampling_period_hz: 50.0,
                        ..Default::default()
                    }),
                )
            })
            .component("left", "wheel")
            .build()
            .expect("robot");
        let plan = RobotSimulationPlan::derive(&robot, 12, |_id| {
            Result::<Vec<u8>, &'static str>::Ok(vec![1])
        })
        .expect("complete plan");
        let pose: Pose = serde_json::from_value(serde_json::json!({
            "xyz": [0.0, 0.0, 0.0],
            "rpy": [0.0, 0.0, 0.0]
        }))
        .expect("pose");
        let source = render_robot(
            &robot,
            &plan,
            &BTreeMap::new(),
            ExecutionId::try_from(0x1000_0000_0000_0000_0000_0000_0000_0001).expect("execution"),
            pose,
            "tcp/127.0.0.1:7447",
            "tcp://127.0.0.1:7000",
        )
        .expect("Robot renders");
        assert!(
            !source.contains("name \"left__mount\""),
            "the massless component mount frame must not become an intermediate Solid"
        );
        assert!(source.contains("HingeJoint {"));
        assert!(source.contains("PositionSensor {"));
        assert!(source.contains("name \"left.encoder\""));
        assert!(!source.contains("EXTERNPROTO"));
        let _: webots_proto_ast::Proto = source.parse().expect("R2025a Robot source parses");
    }

    #[test]
    fn dynamically_imported_robot_mesh_uses_native_indexed_geometry() {
        let asset = AssetId::new("meshes/drive_motor.glb").expect("asset id");
        let bytes =
            include_bytes!("../../../../fixture/components/drive_motor/meshes/drive_motor.glb")
                .to_vec();
        let robot = RobotBuilder::new("mesh-robot")
            .link(Link {
                name: "body",
                visuals: vec![Visual::new(Geometry::Mesh {
                    asset: asset.clone(),
                    scale: Some([0.5, 0.75, 1.25]),
                })],
                collisions: vec![Collision::new(Geometry::Box {
                    size: [0.2, 0.3, 0.4],
                })],
                ..Link::default()
            })
            .build()
            .expect("mesh Robot");
        let assets = BTreeMap::from([(asset, bytes)]);
        let plan = RobotSimulationPlan::derive(&robot, 12, |id| {
            assets
                .get(id)
                .cloned()
                .ok_or_else(|| format!("missing {id}"))
        })
        .expect("mesh plan");
        let pose: Pose = serde_json::from_value(serde_json::json!({
            "xyz": [0.0, 0.0, 0.0],
            "rpy": [0.0, 0.0, 0.0]
        }))
        .expect("pose");
        let source = render_robot(
            &robot,
            &plan,
            &assets,
            ExecutionId::try_from(0x1000_0000_0000_0000_0000_0000_0000_0002).expect("execution"),
            pose,
            "tcp/127.0.0.1:7447",
            "tcp://127.0.0.1:7000",
        )
        .expect("Robot renders");
        assert!(source.contains("geometry IndexedFaceSet"));
        assert!(source.contains("scale 0.5 0.75 1.25"));
        assert!(!source.contains("CadShape"));
        assert!(!source.contains("url [\"../assets/robots/"));
        let bounding = source
            .split_once("boundingObject Group")
            .expect("Robot has bounding object")
            .1;
        assert!(bounding.contains("Pose {"));
        assert!(!bounding.contains("Transform {"));
        let _: webots_proto_ast::Proto = source.parse().expect("native Robot source parses");
    }

    #[test]
    fn primitive_robot_visual_retains_its_authored_material() {
        let robot = RobotBuilder::new("painted-robot")
            .link(Link {
                name: "body",
                visuals: vec![Visual {
                    material: Some(BuilderMaterial {
                        name: "paint",
                        color: Some([0.2, 0.4, 0.6, 0.25]),
                        texture: None,
                    }),
                    ..Visual::new(Geometry::Box {
                        size: [0.2, 0.3, 0.4],
                    })
                }],
                collisions: vec![Collision::new(Geometry::Box {
                    size: [0.2, 0.3, 0.4],
                })],
                ..Link::default()
            })
            .build()
            .expect("painted Robot");
        let plan = RobotSimulationPlan::derive(&robot, 12, |_| Err("unexpected asset".to_owned()))
            .expect("painted plan");
        let source = render_robot(
            &robot,
            &plan,
            &BTreeMap::new(),
            ExecutionId::try_from(0x1000_0000_0000_0000_0000_0000_0000_0003).expect("execution"),
            serde_json::from_value(serde_json::json!({
                "xyz": [0.0, 0.0, 0.0],
                "rpy": [0.0, 0.0, 0.0]
            }))
            .expect("pose"),
            "tcp/127.0.0.1:7447",
            "tcp://127.0.0.1:7000",
        )
        .expect("painted Robot renders");
        assert!(source.contains("baseColor 0.20000000000000001"), "{source}");
        assert!(!source.contains("baseColor 0.6 0.6 0.6"), "{source}");
        assert!(source.contains("transparency 0.75"), "{source}");
        let _: webots_proto_ast::Proto = source.parse().expect("painted Robot source parses");
    }

    #[test]
    fn component_mounted_on_fixed_descendant_contributes_collision_and_mass() {
        let robot = RobotBuilder::new("carrier")
            .link(Link {
                name: "payload_mount",
                ..Link::default()
            })
            .component_type("payload", |builder| {
                builder.link(Link {
                    name: "body",
                    inertial: Inertial {
                        mass_kg: 2.0,
                        ..Inertial::default()
                    },
                    collisions: vec![Collision::new(Geometry::Box {
                        size: [0.2, 0.3, 0.4],
                    })],
                    ..Link::default()
                })
            })
            .component_with("cargo", "payload", |mounted| {
                mounted.mounted_on("payload_mount")
            })
            .build()
            .expect("robot");
        let structure = robot.structure();
        let root = structure
            .link(structure.root_link().as_str())
            .expect("physical root");
        assert_ne!(
            robot
                .component("cargo")
                .expect("mounted component")
                .instance()
                .mount_link(),
            root.name(),
            "the fixture must exercise a fixed descendant mount"
        );

        let assembly =
            resolve_fixed_assembly(&robot, structure, None, root, &Isometry3::identity())
                .expect("resolved assembly");
        let collisions = collect_assembly_collisions(&assembly);
        assert!(collisions.iter().any(|collision| {
            matches!(collision.geometry, Geometry::Box { size } if *size == [0.2, 0.3, 0.4])
        }));
        let mass = collect_assembly_mass(&assembly)
            .finalize()
            .expect("positive mounted mass");
        assert!(mass.mass >= 2.0);
    }
}
