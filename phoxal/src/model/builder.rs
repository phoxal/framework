//! Compose a canonical [`Robot`] programmatically.
//!
//! This is the in-memory counterpart to the document compiler: a tool, a test,
//! or a robot project states the robot it wants and gets back the same
//! validated [`Robot`] a compiled bundle yields. Nothing here reads or parses a
//! document, and nothing here touches the filesystem.
//!
//! It is deliberately **not** a second authoring surface. A real robot is
//! described by authored YAML and URDF, compiled by `phoxal-manifest`; that
//! remains the only way a robot is shipped. What this offers is the ability to
//! build a model without documents at all - for a test that wants to assert on
//! exactly the robot it just stated, or for a tool that composes a model from
//! somewhere other than a bundle.
//!
//! Every value is normalized and validated when [`RobotBuilder::build`] runs,
//! through the same entry points the document compiler uses, so a built robot
//! is a robot the runtime accepts or an explicit [`ModelError`].
//!
//! # What gets generated
//!
//! A canonical robot is only valid with a consistent link tree, and most
//! callers do not care what that tree looks like. Anything not stated is
//! generated:
//!
//! - The robot is rooted at `base_footprint` with `base_link` fixed beneath it,
//!   unless a stated joint already attaches `base_link`.
//! - A mount link that no stated joint attaches is added beneath `base_link` by
//!   a fixed joint named `<link>_joint`.
//! - A component is rooted at `mount`, which is the link the frame graph
//!   attaches to the robot link its instance is mounted on.
//! - A capability whose target no stated joint provides gets one: a joint
//!   target `j` becomes a continuous joint `j` from `mount` to a new link
//!   `j_link`, and a link target `l` becomes that link, fixed to `mount` by a
//!   joint `l_joint`. Two capabilities naming one target share it, which is how
//!   a motor and the encoder measuring it end up on a single joint.
//! - A stated [`Link`] that no stated joint attaches is added the same way a
//!   mount link is, so giving a link a body is enough to put it on the robot.
//!
//! Generated links carry a unit inertial and no geometry, and generated joints
//! sit at their parent's origin turning about Z. State a [`Joint`] or a
//! [`Link`] to say otherwise: between them they reach every field the canonical
//! [`structure`](crate::model::structure) carries.
//!
//! # Why the structural values are stated twice
//!
//! [`Joint`], [`Link`], [`Inertial`], [`Inertia`], [`Visual`], [`Collision`],
//! [`Material`], [`JointLimit`], [`Calibration`], [`Dynamics`], [`Mimic`] and
//! [`Safety`] name the same facts as their counterparts in
//! [`structure`](crate::model::structure), and exist only because the canonical types
//! deliberately cannot be built from raw values. A canonical structural value
//! exists only as part of a validated [`Structure`], so it has no public
//! constructor and none is added here; what the builder holds is a plain
//! statement of intent, borrowing its names, which it normalizes into the
//! canonical structure document and hands to the one construction seam. The
//! canonical values therefore stay unreachable in an unvalidated state, and
//! nothing about the serialized form depends on this module.
//!
//! [`Geometry`] is the exception, and is used directly: it is already a plain
//! public vocabulary with no invariant of its own beyond the dimension check
//! [`RobotBuilder::build`] runs, so mirroring it would only risk the two
//! drifting apart.
//!
//! ```
//! use crate::model::builder::{Kinematics, RobotBuilder};
//!
//! let robot = RobotBuilder::new("rover")
//!     .component_type("drive_motor", |motor| {
//!         motor.motor("spin", "axle").encoder("count", "axle")
//!     })
//!     .component("left_drive", "drive_motor")
//!     .component("right_drive", "drive_motor")
//!     .kinematics(Kinematics::Differential {
//!         left_actuators: &["left_drive.spin"],
//!         right_actuators: &["right_drive.spin"],
//!         left_encoders: &["left_drive.count"],
//!         right_encoders: &["right_drive.count"],
//!         wheel_radius_m: 0.1,
//!         wheel_base_m: 0.4,
//!     })
//!     .build()?;
//!
//! assert_eq!(robot.component_ids().len(), 2);
//! # Ok::<(), crate::model::ModelError>(())
//! ```

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::model::asset::AssetId;
use crate::model::compiler::{self, RobotParts};
use crate::model::component::Component;
use crate::model::component::capability::{
    Accelerometer, Battery, Camera, CameraMode, Capability, Depth, EmergencyStop, Encoder,
    EncoderType, Gnss, GnssCoordinateSystem, Gyroscope, Imu, Led, Lidar, LidarOutput, Magnetometer,
    Microphone, Mmwave, Motor, MotorCommand, Range, Speaker, StructuralTarget,
};
use crate::model::error::ModelError;
use crate::model::identity::{
    CapabilityId, CapabilityRef, ComponentInstanceId, ComponentTypeId, JointId, LinkId, RobotId,
    ServiceId,
};
use crate::model::robot::{KinematicConfig, MotionLimits, Robot};
use crate::model::simulation;
use crate::model::structure::{BASE_FOOTPRINT_LINK, BASE_LINK, Geometry, JointKind, Structure};

/// The root link of every component structure this module generates.
pub const COMPONENT_ROOT_LINK: &str = "mount";

/// The suffix naming the link a generated joint moves.
const JOINT_CHILD_SUFFIX: &str = "_link";
/// The suffix naming the fixed joint that holds a generated link in place.
const LINK_JOINT_SUFFIX: &str = "_joint";
/// The suffix of the mount link generated for an instance that states none.
const MOUNT_LINK_SUFFIX: &str = "_mount";
/// The joint attaching `base_link` beneath the root, when none is stated.
const BASE_JOINT: &str = "base_joint";

/// The envelope a built robot clamps motion to unless limits are stated.
const DEFAULT_MOTION_LIMITS: MotionLimits = MotionLimits {
    max_linear_speed_mps: 1.0,
    max_angular_speed_radps: 1.0,
};

/// The rate a generated sensor capability publishes at.
const DEFAULT_PUBLISH_RATE_HZ: f64 = 50.0;

/// The drive geometry of a built robot, and the capabilities realizing it.
///
/// This mirrors [`KinematicConfig`] one variant at a time, taking each
/// capability as the `component.capability` string an authored document writes
/// rather than an already-parsed reference. The references must resolve to
/// motors and encoders the robot declares, which [`RobotBuilder::build`]
/// checks.
///
/// ```
/// use crate::model::builder::{Kinematics, RobotBuilder};
///
/// let robot = RobotBuilder::new("car")
///     .component_type("steer", |steer| steer.motor("turn", "kingpin"))
///     .component_type("drive", |drive| drive.motor("spin", "axle"))
///     .component("front", "steer")
///     .component("rear", "drive")
///     .kinematics(Kinematics::Ackermann {
///         steering_actuator: "front.turn",
///         drive_actuator: "rear.spin",
///         steering_encoder: None,
///         drive_encoder: None,
///         wheel_base_m: 2.5,
///         track_m: 1.5,
///         max_steering_angle_rad: 0.6,
///     })
///     .build()?;
///
/// assert!(robot.motion().kinematic().drive_kinematics().is_ok());
/// # Ok::<(), crate::model::ModelError>(())
/// ```
#[derive(Clone, Copy, Debug)]
pub enum Kinematics<'a> {
    /// Two independently driven sides.
    Differential {
        left_actuators: &'a [&'a str],
        right_actuators: &'a [&'a str],
        left_encoders: &'a [&'a str],
        right_encoders: &'a [&'a str],
        wheel_radius_m: f64,
        wheel_base_m: f64,
    },
    /// Four independently driven wheels with 45-degree rollers.
    Mecanum {
        front_left_actuator: &'a str,
        front_right_actuator: &'a str,
        rear_left_actuator: &'a str,
        rear_right_actuator: &'a str,
        wheel_radius_m: f64,
        wheel_base_m: f64,
        track_m: f64,
    },
    /// One steered axle and one driven axle.
    Ackermann {
        steering_actuator: &'a str,
        drive_actuator: &'a str,
        steering_encoder: Option<&'a str>,
        drive_encoder: Option<&'a str>,
        wheel_base_m: f64,
        track_m: f64,
        max_steering_angle_rad: f64,
    },
    /// Actuators and encoders whose geometry the model does not describe.
    ///
    /// This is what a robot with no drive at all declares, and it is what a
    /// builder starts with.
    Omnidirectional {
        actuators: &'a [&'a str],
        encoders: &'a [&'a str],
    },
}

/// One joint of a built structure, and the link it moves.
///
/// The child link is created if no other joint already provides it, so stating
/// a joint is also how a structure grows a link.
///
/// Every field but the three names has a default: the joint sits at its
/// parent's origin, turns about Z, is [`JointKind::Fixed`], carries the all-zero
/// limits a URDF joint without a `<limit>` compiles to, and states no
/// calibration, dynamics, mimic or safety.
///
/// ```
/// use crate::model::builder::{Joint, JointLimit, RobotBuilder};
/// use crate::model::structure::JointKind;
///
/// let robot = RobotBuilder::new("rover")
///     .joint(Joint {
///         name: "mast_joint",
///         kind: JointKind::Revolute,
///         parent: "base_link",
///         child: "mast",
///         xyz: [0.0, 0.0, 0.4],
///         limit: JointLimit {
///             lower: -1.5,
///             upper: 1.5,
///             effort: 8.0,
///             velocity: 2.0,
///         },
///         ..Joint::default()
///     })
///     .build()?;
///
/// let mast = robot.structure().joint("mast_joint").expect("the stated joint");
/// assert_eq!(mast.limit().upper(), 1.5);
/// assert!(robot.structure().link("mast").is_some());
/// # Ok::<(), crate::model::ModelError>(())
/// ```
#[derive(Clone, Copy, Debug)]
pub struct Joint<'a> {
    /// The joint's own identity, unique within its structure.
    pub name: &'a str,
    /// Which degree of freedom the joint has.
    pub kind: JointKind,
    /// The link this joint hangs from, which must already exist.
    pub parent: &'a str,
    /// The link this joint moves, created here if nothing else provides it.
    pub child: &'a str,
    /// The child's offset from the parent, in metres.
    pub xyz: [f64; 3],
    /// The child's roll, pitch and yaw relative to the parent, in radians.
    pub rpy: [f64; 3],
    /// The axis a movable joint turns or slides along, in the parent's frame.
    pub axis: [f64; 3],
    /// How far, how hard and how fast the joint may be driven.
    pub limit: JointLimit,
    /// Where the joint's reference switch trips, when it has one.
    pub calibration: Option<Calibration>,
    /// The joint's passive damping and friction, when they are modelled.
    pub dynamics: Option<Dynamics>,
    /// The joint this one follows instead of being driven independently.
    pub mimic: Option<Mimic<'a>>,
    /// The soft envelope a safety controller holds the joint inside.
    pub safety: Option<Safety>,
}

impl Default for Joint<'_> {
    fn default() -> Self {
        Self {
            name: "",
            kind: JointKind::Fixed,
            parent: "",
            child: "",
            xyz: [0.0; 3],
            rpy: [0.0; 3],
            axis: [0.0, 0.0, 1.0],
            limit: JointLimit::default(),
            calibration: None,
            dynamics: None,
            mimic: None,
            safety: None,
        }
    }
}

/// How far, how hard and how fast a joint may be driven.
///
/// The default is all zeroes, which is what the document compiler emits for a
/// URDF joint that authors no `<limit>`. The range must be finite and
/// non-inverted, which [`RobotBuilder::build`] checks.
#[derive(Clone, Copy, Debug, Default)]
pub struct JointLimit {
    /// The lowest position the joint may reach, in metres or radians.
    pub lower: f64,
    /// The highest position the joint may reach, in metres or radians.
    pub upper: f64,
    /// The largest force or torque the joint may apply, in N or Nm.
    pub effort: f64,
    /// The largest speed the joint may move at, in m/s or rad/s.
    pub velocity: f64,
}

/// Where a joint's reference switch trips.
///
/// Either end may be left unstated, which is what a switch that only reports
/// one edge means.
#[derive(Clone, Copy, Debug, Default)]
pub struct Calibration {
    /// The position the switch rises at, in metres or radians.
    pub rising: Option<f64>,
    /// The position the switch falls at, in metres or radians.
    pub falling: Option<f64>,
}

/// A joint's passive damping and friction.
///
/// Both must be finite and non-negative, which [`RobotBuilder::build`] checks.
#[derive(Clone, Copy, Debug, Default)]
pub struct Dynamics {
    /// Resistance proportional to speed, in Ns/m or Nms/rad.
    pub damping: f64,
    /// Resistance opposing motion at any speed, in N or Nm.
    pub friction: f64,
}

/// The joint another joint follows, and the affine relation it follows it by.
///
/// The named joint must exist in the same structure, which
/// [`RobotBuilder::build`] checks.
#[derive(Clone, Copy, Debug)]
pub struct Mimic<'a> {
    /// The joint whose position drives this one.
    pub joint: &'a str,
    /// The factor the driving position is scaled by; unstated means one.
    pub multiplier: Option<f64>,
    /// The constant added after scaling; unstated means zero.
    pub offset: Option<f64>,
}

impl<'a> Mimic<'a> {
    /// Follow `joint` one for one, with no offset.
    #[must_use]
    pub const fn new(joint: &'a str) -> Self {
        Self {
            joint,
            multiplier: None,
            offset: None,
        }
    }
}

/// The soft envelope a safety controller holds a joint inside.
///
/// The range must be finite and non-inverted, which [`RobotBuilder::build`]
/// checks.
#[derive(Clone, Copy, Debug, Default)]
pub struct Safety {
    /// Where the controller starts pushing back, at the low end.
    pub soft_lower_limit: f64,
    /// Where the controller starts pushing back, at the high end.
    pub soft_upper_limit: f64,
    /// The position gain the controller pushes back with.
    pub k_position: f64,
    /// The velocity gain the controller pushes back with.
    pub k_velocity: f64,
}

/// One link of a built structure: the mass it has, and the shapes it is drawn
/// and collided with.
///
/// A link that no stated joint attaches is added beneath the structure's body
/// frame by a fixed joint named `<link>_joint`, exactly as a mount link is, so
/// stating a link is also how a structure grows one. Naming a link some joint
/// already provides gives that link its body instead.
///
/// Every field but the name has a default: a link carries the same unit
/// inertial a generated link does, and no geometry at all.
///
/// ```
/// use crate::model::builder::{Inertia, Inertial, Link, RobotBuilder};
///
/// let robot = RobotBuilder::new("rover")
///     .link(Link {
///         name: "base_link",
///         inertial: Inertial {
///             mass_kg: 12.0,
///             inertia: Inertia {
///                 ixx: 0.8,
///                 iyy: 1.2,
///                 izz: 1.6,
///                 ..Inertia::default()
///             },
///             ..Inertial::default()
///         },
///         ..Link::default()
///     })
///     .build()?;
///
/// let base = robot.structure().link("base_link").expect("the stated link");
/// assert_eq!(base.inertial().mass_kg(), 12.0);
/// # Ok::<(), crate::model::ModelError>(())
/// ```
#[derive(Clone, Debug, Default)]
pub struct Link<'a> {
    /// The link's own identity, unique within its structure.
    pub name: &'a str,
    /// The link's mass properties.
    pub inertial: Inertial,
    /// The shapes the link is drawn with.
    pub visuals: Vec<Visual<'a>>,
    /// The shapes the link collides with.
    pub collisions: Vec<Collision<'a>>,
}

/// The mass properties of one link.
///
/// The default is the unit inertial a generated link carries: one kilogram at
/// the link's own origin, with a unit tensor.
#[derive(Clone, Copy, Debug)]
pub struct Inertial {
    /// The centre of mass, offset from the link's origin, in metres.
    pub xyz: [f64; 3],
    /// The inertia frame's roll, pitch and yaw relative to the link, in radians.
    pub rpy: [f64; 3],
    /// The link's mass, in kilograms.
    pub mass_kg: f64,
    /// The inertia tensor about the centre of mass.
    pub inertia: Inertia,
}

impl Default for Inertial {
    fn default() -> Self {
        Self {
            xyz: [0.0; 3],
            rpy: [0.0; 3],
            mass_kg: 1.0,
            inertia: Inertia::default(),
        }
    }
}

/// The symmetric inertia tensor of one link, in kg*m^2.
///
/// The default is the unit tensor. The tensor must describe a physically
/// realizable body, which [`RobotBuilder::build`] checks.
#[derive(Clone, Copy, Debug)]
pub struct Inertia {
    /// The moment about X.
    pub ixx: f64,
    /// The product of inertia between X and Y.
    pub ixy: f64,
    /// The product of inertia between X and Z.
    pub ixz: f64,
    /// The moment about Y.
    pub iyy: f64,
    /// The product of inertia between Y and Z.
    pub iyz: f64,
    /// The moment about Z.
    pub izz: f64,
}

impl Default for Inertia {
    fn default() -> Self {
        Self {
            ixx: 1.0,
            ixy: 0.0,
            ixz: 0.0,
            iyy: 1.0,
            iyz: 0.0,
            izz: 1.0,
        }
    }
}

/// One shape a link is drawn with.
///
/// The shape itself is the one thing a visual cannot default, so
/// [`Visual::new`] takes it and leaves the rest to `..`.
///
/// ```
/// use crate::model::AssetId;
/// use crate::model::builder::{Link, Material, RobotBuilder, Visual};
/// use crate::model::structure::Geometry;
///
/// let robot = RobotBuilder::new("rover")
///     .link(Link {
///         name: "base_link",
///         visuals: vec![Visual {
///             material: Some(Material {
///                 color: Some([0.2, 0.2, 0.2, 1.0]),
///                 ..Material::new("carbon")
///             }),
///             ..Visual::new(Geometry::Mesh {
///                 asset: AssetId::new("meshes/chassis.stl")?,
///                 scale: None,
///             })
///         }],
///         ..Link::default()
///     })
///     .build()?;
///
/// let base = robot.structure().link("base_link").expect("the stated link");
/// assert_eq!(base.visuals().len(), 1);
/// # Ok::<(), crate::model::ModelError>(())
/// ```
#[derive(Clone, Debug)]
pub struct Visual<'a> {
    /// The visual's own name, when the structure gives it one.
    pub name: Option<&'a str>,
    /// The shape's offset from the link's origin, in metres.
    pub xyz: [f64; 3],
    /// The shape's roll, pitch and yaw relative to the link, in radians.
    pub rpy: [f64; 3],
    /// The shape itself.
    pub geometry: Geometry,
    /// How the shape is rendered, when the structure says.
    pub material: Option<Material<'a>>,
}

impl Visual<'_> {
    /// An unnamed, unpainted visual of `geometry` at the link's own origin.
    #[must_use]
    pub fn new(geometry: Geometry) -> Self {
        Self {
            name: None,
            xyz: [0.0; 3],
            rpy: [0.0; 3],
            geometry,
            material: None,
        }
    }
}

/// One shape a link collides with.
///
/// The shape itself is the one thing a collision cannot default, so
/// [`Collision::new`] takes it and leaves the rest to `..`.
#[derive(Clone, Debug)]
pub struct Collision<'a> {
    /// The collision's own name, when the structure gives it one.
    pub name: Option<&'a str>,
    /// The shape's offset from the link's origin, in metres.
    pub xyz: [f64; 3],
    /// The shape's roll, pitch and yaw relative to the link, in radians.
    pub rpy: [f64; 3],
    /// The shape itself.
    pub geometry: Geometry,
}

impl Collision<'_> {
    /// An unnamed collision of `geometry` at the link's own origin.
    #[must_use]
    pub fn new(geometry: Geometry) -> Self {
        Self {
            name: None,
            xyz: [0.0; 3],
            rpy: [0.0; 3],
            geometry,
        }
    }
}

/// How a visual is rendered.
///
/// A material is stated where it is used, and may also be added to the
/// structure's own catalogue with [`RobotBuilder::material`].
#[derive(Clone, Debug)]
pub struct Material<'a> {
    /// The material's name, which is how a structure refers to it.
    pub name: &'a str,
    /// Linear RGBA in `0.0..=1.0`, when the material states a colour.
    pub color: Option<[f64; 4]>,
    /// The texture image, when the material states one.
    pub texture: Option<AssetId>,
}

impl<'a> Material<'a> {
    /// A material named `name`, with neither colour nor texture.
    #[must_use]
    pub const fn new(name: &'a str) -> Self {
        Self {
            name,
            color: None,
            texture: None,
        }
    }
}

/// One joint as the builder holds it, with its names owned.
#[derive(Debug)]
struct JointSpec {
    name: String,
    kind: JointKind,
    parent: String,
    child: String,
    xyz: [f64; 3],
    rpy: [f64; 3],
    axis: [f64; 3],
    limit: JointLimit,
    calibration: Option<Calibration>,
    dynamics: Option<Dynamics>,
    mimic: Option<MimicSpec>,
    safety: Option<Safety>,
}

/// One mimic relationship as the builder holds it, with its joint owned.
#[derive(Debug)]
struct MimicSpec {
    joint: String,
    multiplier: Option<f64>,
    offset: Option<f64>,
}

impl From<Joint<'_>> for JointSpec {
    fn from(joint: Joint<'_>) -> Self {
        Self {
            name: joint.name.to_owned(),
            kind: joint.kind,
            parent: joint.parent.to_owned(),
            child: joint.child.to_owned(),
            xyz: joint.xyz,
            rpy: joint.rpy,
            axis: joint.axis,
            limit: joint.limit,
            calibration: joint.calibration,
            dynamics: joint.dynamics,
            mimic: joint.mimic.map(MimicSpec::from),
            safety: joint.safety,
        }
    }
}

impl From<Mimic<'_>> for MimicSpec {
    fn from(mimic: Mimic<'_>) -> Self {
        Self {
            joint: mimic.joint.to_owned(),
            multiplier: mimic.multiplier,
            offset: mimic.offset,
        }
    }
}

/// The links and materials of one structure, each keyed by its own name and
/// already normalized into the canonical document the compiler reads.
///
/// The document is the only route into a [`Structure`], so the builder keeps
/// what it was told in that form rather than in a third copy of the shape.
#[derive(Debug, Default)]
struct Bodies {
    links: BTreeMap<String, Value>,
    materials: BTreeMap<String, Value>,
}

impl Bodies {
    /// State one link's body, replacing any earlier statement of it.
    fn link(&mut self, link: &Link<'_>) {
        self.links.insert(link.name.to_owned(), link_value(link));
    }

    /// Add one material to the catalogue, replacing any earlier one of its name.
    fn material(&mut self, material: &Material<'_>) {
        self.materials
            .insert(material.name.to_owned(), material_value(material));
    }
}

/// One component type as the builder holds it.
#[derive(Debug, Default)]
struct TypeSpec {
    capabilities: BTreeMap<String, Capability>,
    joints: Vec<JointSpec>,
    bodies: Bodies,
    simulated: BTreeMap<String, simulation::Capability>,
    contact_materials: BTreeMap<String, String>,
}

/// One mounted instance as the builder holds it.
#[derive(Debug)]
struct InstanceSpec {
    component_type: String,
    mount_link: Option<String>,
    direction_signs: BTreeMap<String, i8>,
    /// The hardware connection block, present exactly when a component driver
    /// runs for this instance.
    driver: Option<serde_json::Value>,
}

/// Composes a canonical [`Robot`] from stated facts.
///
/// No method here fails. A rejected value is held until [`Self::build`], which
/// reports the first one as a typed [`ModelError`], so a chain reads as one
/// statement rather than a sequence of fallible steps.
///
/// ```
/// use crate::model::builder::RobotBuilder;
///
/// let robot = RobotBuilder::new("rover")
///     .component_type("rgbd", |camera| camera.camera("rgb", "lens"))
///     .component("front_camera", "rgbd")
///     .build()?;
///
/// assert_eq!(robot.id().as_str(), "rover");
/// # Ok::<(), crate::model::ModelError>(())
/// ```
#[derive(Debug)]
pub struct RobotBuilder {
    id: String,
    motion_limits: MotionLimits,
    services: BTreeMap<String, Option<serde_json::Value>>,
    /// The drive, already normalized. Held as a `Result` so that a malformed
    /// capability reference is reported by [`RobotBuilder::build`] rather than
    /// forcing every caller to handle one mid-chain.
    kinematic: Result<KinematicConfig, ModelError>,
    joints: Vec<JointSpec>,
    bodies: Bodies,
    types: BTreeMap<String, TypeSpec>,
    instances: BTreeMap<String, InstanceSpec>,
}

/// Declares one component type: the capabilities and structure every instance
/// of it has, and how a simulated world models it.
///
/// Reached through [`RobotBuilder::component_type`].
#[derive(Debug)]
pub struct ComponentTypeBuilder {
    spec: TypeSpec,
}

/// Configures one mounted component instance.
///
/// Reached through [`RobotBuilder::component_with`].
#[derive(Debug)]
pub struct ComponentBuilder {
    spec: InstanceSpec,
}

impl RobotBuilder {
    /// A robot with the given id, no components and no drive.
    ///
    /// It starts with an omnidirectional kinematic config declaring no
    /// actuators - the one geometry that describes nothing a robot without a
    /// drive would have to invent - and runs no services.
    ///
    /// ```
    /// use crate::model::builder::RobotBuilder;
    ///
    /// let robot = RobotBuilder::new("rover").build()?;
    ///
    /// assert_eq!(robot.id().as_str(), "rover");
    /// assert_eq!(robot.services().len(), 0);
    /// # Ok::<(), crate::model::ModelError>(())
    /// ```
    #[must_use]
    pub fn new(id: &str) -> Self {
        Self {
            id: id.to_owned(),
            motion_limits: DEFAULT_MOTION_LIMITS,
            services: BTreeMap::new(),
            kinematic: Ok(KinematicConfig::Omnidirectional {
                actuators: Vec::new(),
                encoders: Vec::new(),
            }),
            joints: Vec::new(),
            bodies: Bodies::default(),
            types: BTreeMap::new(),
            instances: BTreeMap::new(),
        }
    }

    /// Run one service on this robot, with the given configuration.
    ///
    /// Declaring the same service twice replaces the earlier configuration.
    ///
    /// ```
    /// use crate::model::builder::RobotBuilder;
    ///
    /// let robot = RobotBuilder::new("rover")
    ///     .service("drive", None)
    ///     .service("mission", Some(serde_json::json!({ "speed": 1 })))
    ///     .build()?;
    ///
    /// assert_eq!(robot.service_config("mission"), Some(&serde_json::json!({ "speed": 1 })));
    /// assert_eq!(robot.service_config("drive"), None);
    /// # Ok::<(), crate::model::ModelError>(())
    /// ```
    #[must_use]
    pub fn service(mut self, id: &str, config: Option<serde_json::Value>) -> Self {
        self.services.insert(id.to_owned(), config);
        self
    }

    /// Clamp this robot's motion to the given envelope.
    ///
    /// The limits must be finite, positive and representable as `f32`, which
    /// [`Self::build`] checks.
    #[must_use]
    pub const fn motion_limits(mut self, limits: MotionLimits) -> Self {
        self.motion_limits = limits;
        self
    }

    /// Drive this robot with the given geometry.
    #[must_use]
    pub fn kinematics(mut self, kinematics: Kinematics<'_>) -> Self {
        self.kinematic = kinematics.into_config();
        self
    }

    /// Add one joint, and its child link, to the robot's own structure.
    ///
    /// Use this when the robot's link tree is part of what is being stated;
    /// a robot that says nothing still gets the conventional base frames and a
    /// mount link per instance.
    #[must_use]
    pub fn joint(mut self, joint: Joint<'_>) -> Self {
        self.joints.push(joint.into());
        self
    }

    /// Give one link of the robot's own structure a body.
    ///
    /// A link no stated joint attaches is added beneath `base_link` by a fixed
    /// joint named `<link>_joint`, so this is enough on its own to put a link
    /// on the robot. Stating the same link twice replaces the earlier body.
    ///
    /// ```
    /// use crate::model::AssetId;
    /// use crate::model::builder::{
    ///     Collision, Inertia, Inertial, Link, Material, RobotBuilder, Visual,
    /// };
    /// use crate::model::structure::Geometry;
    ///
    /// let robot = RobotBuilder::new("rover")
    ///     .link(Link {
    ///         name: "chassis",
    ///         inertial: Inertial {
    ///             xyz: [0.0, 0.0, 0.05],
    ///             mass_kg: 12.0,
    ///             inertia: Inertia {
    ///                 ixx: 0.8,
    ///                 iyy: 1.2,
    ///                 izz: 1.6,
    ///                 ..Inertia::default()
    ///             },
    ///             ..Inertial::default()
    ///         },
    ///         visuals: vec![Visual {
    ///             name: Some("shell"),
    ///             material: Some(Material {
    ///                 color: Some([0.2, 0.2, 0.2, 1.0]),
    ///                 texture: Some(AssetId::new("textures/carbon.png")?),
    ///                 ..Material::new("carbon")
    ///             }),
    ///             ..Visual::new(Geometry::Mesh {
    ///                 asset: AssetId::new("meshes/chassis.stl")?,
    ///                 scale: None,
    ///             })
    ///         }],
    ///         collisions: vec![Collision::new(Geometry::Box {
    ///             size: [0.6, 0.4, 0.2],
    ///         })],
    ///         ..Link::default()
    ///     })
    ///     .build()?;
    ///
    /// let chassis = robot.structure().link("chassis").expect("the stated link");
    /// assert_eq!(chassis.inertial().mass_kg(), 12.0);
    /// assert_eq!(chassis.collisions().len(), 1);
    /// # Ok::<(), crate::model::ModelError>(())
    /// ```
    #[must_use]
    pub fn link(mut self, link: Link<'_>) -> Self {
        self.bodies.link(&link);
        self
    }

    /// Add one material to the robot structure's own catalogue.
    ///
    /// This is the structure-level material table, which is one of the places a
    /// bundle's declared assets are read from; a visual states the material it
    /// is drawn with itself. Restating a name replaces the earlier material.
    #[must_use]
    pub fn material(mut self, material: Material<'_>) -> Self {
        self.bodies.material(&material);
        self
    }

    /// Declare one component type.
    ///
    /// Declaring the same type twice replaces the earlier declaration, so a
    /// type is stated once and mounted as many times as needed.
    ///
    /// ```
    /// use crate::model::builder::RobotBuilder;
    ///
    /// let robot = RobotBuilder::new("rover")
    ///     .component_type("drive_motor", |motor| {
    ///         motor.motor("spin", "axle").encoder("count", "axle")
    ///     })
    ///     .component("left_drive", "drive_motor")
    ///     .component("right_drive", "drive_motor")
    ///     .build()?;
    ///
    /// assert_eq!(robot.capability_refs(|_| true).len(), 4);
    /// # Ok::<(), crate::model::ModelError>(())
    /// ```
    #[must_use]
    pub fn component_type(
        mut self,
        component_type: &str,
        declare: impl FnOnce(ComponentTypeBuilder) -> ComponentTypeBuilder,
    ) -> Self {
        self.types.insert(
            component_type.to_owned(),
            declare(ComponentTypeBuilder {
                spec: TypeSpec::default(),
            })
            .spec,
        );
        self
    }

    /// Mount one instance of `component_type` on a generated mount link named
    /// `<instance>_mount`.
    ///
    /// The type must be declared by [`Self::component_type`], which
    /// [`Self::build`] checks.
    #[must_use]
    pub fn component(self, instance: &str, component_type: &str) -> Self {
        self.component_with(instance, component_type, |mounted| mounted)
    }

    /// Mount one instance of `component_type`, stating where it sits and how
    /// its actuators are turned.
    ///
    /// Mounting the same instance twice replaces the earlier mount.
    ///
    /// ```
    /// use crate::model::builder::RobotBuilder;
    ///
    /// let robot = RobotBuilder::new("rover")
    ///     .component_type("drive_motor", |motor| motor.motor("spin", "axle"))
    ///     .component_with("right_drive", "drive_motor", |mounted| {
    ///         mounted
    ///             .mounted_on("right_wheel_mount")
    ///             .direction_sign("spin", -1)
    ///     })
    ///     .build()?;
    ///
    /// let (_motor, sign) = robot.require_motor(&"right_drive.spin".parse()?)?;
    /// assert_eq!(sign, -1);
    /// # Ok::<(), crate::model::ModelError>(())
    /// ```
    #[must_use]
    pub fn component_with(
        mut self,
        instance: &str,
        component_type: &str,
        mount: impl FnOnce(ComponentBuilder) -> ComponentBuilder,
    ) -> Self {
        self.instances.insert(
            instance.to_owned(),
            mount(ComponentBuilder {
                spec: InstanceSpec {
                    component_type: component_type.to_owned(),
                    mount_link: None,
                    direction_signs: BTreeMap::new(),
                    driver: None,
                },
            })
            .spec,
        );
        self
    }

    /// Normalize, assemble and validate the robot.
    ///
    /// # Errors
    ///
    /// Returns the first [`ModelError`] the stated robot violates: an
    /// identifier that is not a normalized token, a capability reference that
    /// does not resolve to the kind its kinematic role needs, a structure that
    /// is not a single link tree, or any other invariant the canonical model
    /// enforces on a compiled bundle.
    ///
    /// ```
    /// use crate::model::{IdentifierKind, ModelError};
    /// use crate::model::builder::RobotBuilder;
    ///
    /// let rejected = RobotBuilder::new("Rover").build();
    ///
    /// assert!(matches!(
    ///     rejected,
    ///     Err(ModelError::NotNormalized { kind: IdentifierKind::RobotId, .. })
    /// ));
    /// ```
    pub fn build(self) -> Result<Robot, ModelError> {
        let id = RobotId::new(self.id)?;
        let kinematic = self.kinematic?;
        let component_types = build_types(self.types)?;
        let mut services = BTreeMap::new();
        for (service, config) in self.services {
            services.insert(ServiceId::new(service)?, compiler::service(config));
        }
        let mut components = BTreeMap::new();
        let mut mounts = BTreeSet::new();
        for (instance, spec) in self.instances {
            let instance = ComponentInstanceId::new(instance)?;
            let mount_link = LinkId::new(
                spec.mount_link
                    .unwrap_or_else(|| format!("{instance}{MOUNT_LINK_SUFFIX}")),
            );
            mounts.insert(mount_link.clone());
            let mut direction_signs = BTreeMap::new();
            for (capability, sign) in spec.direction_signs {
                direction_signs.insert(CapabilityId::new(capability)?, sign);
            }
            components.insert(
                instance,
                compiler::component_instance(
                    ComponentTypeId::new(spec.component_type)?,
                    mount_link,
                    direction_signs,
                    BTreeMap::new(),
                    spec.driver,
                ),
            );
        }
        let structure = robot_structure(&id, self.joints, &mounts, &self.bodies)?;
        compiler::robot(RobotParts {
            id,
            kinematic,
            motion_limits: self.motion_limits,
            services,
            components,
            component_types,
            structure,
        })
    }
}

impl ComponentTypeBuilder {
    /// Declare one capability, exactly as the canonical model carries it.
    ///
    /// Every shorthand below is this method with one kind's defaults filled in;
    /// reach for this one when a capability needs parameters, or a structural
    /// target, that its shorthand does not offer.
    ///
    /// ```
    /// use crate::model::builder::RobotBuilder;
    /// use crate::model::component::capability::{
    ///     Capability, Motor, MotorCommand, StructuralTarget,
    /// };
    /// use crate::model::identity::JointId;
    ///
    /// let robot = RobotBuilder::new("arm-bot")
    ///     .component_type("joint_motor", |joint_motor| {
    ///         joint_motor.capability(
    ///             "lift",
    ///             Capability::Motor(Motor {
    ///                 target: StructuralTarget::Joint { id: JointId::new("elbow") },
    ///                 command: MotorCommand::Position,
    ///                 gear_ratio: 50.0,
    ///                 max_torque_nm: Some(12.0),
    ///                 max_velocity_radps: None,
    ///             }),
    ///         )
    ///     })
    ///     .component("arm", "joint_motor")
    ///     .build()?;
    ///
    /// let (motor, _sign) = robot.require_motor(&"arm.lift".parse()?)?;
    /// assert_eq!(motor.gear_ratio, 50.0);
    /// # Ok::<(), crate::model::ModelError>(())
    /// ```
    #[must_use]
    pub fn capability(mut self, capability: &str, declared: Capability) -> Self {
        self.spec
            .capabilities
            .insert(capability.to_owned(), declared);
        self
    }

    /// Add one joint, and its child link, to this component's structure.
    ///
    /// A component that states nothing still gets a joint or link for every
    /// capability target it declares.
    #[must_use]
    pub fn joint(mut self, joint: Joint<'_>) -> Self {
        self.spec.joints.push(joint.into());
        self
    }

    /// Give one link of this component's structure a body.
    ///
    /// A link no stated joint attaches is added beneath `mount` by a fixed
    /// joint named `<link>_joint`, so this is enough on its own to put a link
    /// on the component. Stating the same link twice replaces the earlier body.
    ///
    /// ```
    /// use crate::model::builder::{Link, RobotBuilder, Visual};
    /// use crate::model::structure::Geometry;
    ///
    /// let robot = RobotBuilder::new("rover")
    ///     .component_type("rgbd", |camera| {
    ///         camera.camera("rgb", "lens").link(Link {
    ///             name: "lens",
    ///             visuals: vec![Visual::new(Geometry::Cylinder {
    ///                 radius: 0.02,
    ///                 length: 0.01,
    ///             })],
    ///             ..Link::default()
    ///         })
    ///     })
    ///     .component("front_camera", "rgbd")
    ///     .build()?;
    ///
    /// let camera = robot
    ///     .component("front_camera")
    ///     .expect("the instance is mounted");
    /// let lens = camera
    ///     .component_type()
    ///     .structure()
    ///     .link("lens")
    ///     .expect("the stated link");
    /// assert_eq!(lens.visuals().len(), 1);
    /// # Ok::<(), crate::model::ModelError>(())
    /// ```
    #[must_use]
    pub fn link(mut self, link: Link<'_>) -> Self {
        self.spec.bodies.link(&link);
        self
    }

    /// Add one material to this component structure's own catalogue.
    ///
    /// The component counterpart of [`RobotBuilder::material`]. Restating a
    /// name replaces the earlier material.
    #[must_use]
    pub fn material(mut self, material: Material<'_>) -> Self {
        self.spec.bodies.material(&material);
        self
    }

    /// Model one of this type's capabilities in a simulated world.
    ///
    /// The named capability must be one this type declares, of the same kind,
    /// which [`RobotBuilder::build`] checks.
    ///
    /// ```
    /// use crate::model::builder::RobotBuilder;
    /// use crate::model::simulation;
    ///
    /// let robot = RobotBuilder::new("rover")
    ///     .component_type("drive_motor", |motor| {
    ///         motor.motor("spin", "axle").simulated(
    ///             "spin",
    ///             simulation::Capability::Motor(simulation::Motor::default()),
    ///         )
    ///     })
    ///     .component("left_drive", "drive_motor")
    ///     .build()?;
    ///
    /// let drive = robot.component("left_drive").expect("the instance is mounted");
    /// assert!(drive.simulation().is_some());
    /// # Ok::<(), crate::model::ModelError>(())
    /// ```
    #[must_use]
    pub fn simulated(mut self, capability: &str, simulated: simulation::Capability) -> Self {
        self.spec.simulated.insert(capability.to_owned(), simulated);
        self
    }

    /// Give one component-local link a simulated contact material.
    #[must_use]
    pub fn contact_material(mut self, link: &str, material: &str) -> Self {
        self.spec
            .contact_materials
            .insert(link.to_owned(), material.to_owned());
        self
    }

    /// A velocity motor driving `joint`, geared one to one.
    #[must_use]
    pub fn motor(self, capability: &str, joint: &str) -> Self {
        self.capability(
            capability,
            Capability::Motor(Motor {
                target: joint_target(joint),
                command: MotorCommand::Velocity,
                gear_ratio: 1.0,
                max_torque_nm: None,
                max_velocity_radps: None,
            }),
        )
    }

    /// An incremental encoder measuring `joint`, geared one to one.
    #[must_use]
    pub fn encoder(self, capability: &str, joint: &str) -> Self {
        self.capability(
            capability,
            Capability::Encoder(Encoder {
                target: joint_target(joint),
                publish_rate_hz: DEFAULT_PUBLISH_RATE_HZ,
                gear_ratio: 1.0,
                encoder_type: EncoderType::Incremental,
                counts_per_revolution: 4096,
            }),
        )
    }

    /// A three-axis accelerometer on `link`.
    #[must_use]
    pub fn accelerometer(self, capability: &str, link: &str) -> Self {
        self.capability(
            capability,
            Capability::Accelerometer(Accelerometer {
                target: link_target(link),
                publish_rate_hz: DEFAULT_PUBLISH_RATE_HZ,
                axes: None,
            }),
        )
    }

    /// A three-axis gyroscope on `link`.
    #[must_use]
    pub fn gyroscope(self, capability: &str, link: &str) -> Self {
        self.capability(
            capability,
            Capability::Gyroscope(Gyroscope {
                target: link_target(link),
                publish_rate_hz: DEFAULT_PUBLISH_RATE_HZ,
                axes: None,
            }),
        )
    }

    /// A three-axis magnetometer on `link`.
    #[must_use]
    pub fn magnetometer(self, capability: &str, link: &str) -> Self {
        self.capability(
            capability,
            Capability::Magnetometer(Magnetometer {
                target: link_target(link),
                publish_rate_hz: DEFAULT_PUBLISH_RATE_HZ,
                axes: None,
            }),
        )
    }

    /// A fused inertial measurement unit on `link`.
    #[must_use]
    pub fn imu(self, capability: &str, link: &str) -> Self {
        self.capability(
            capability,
            Capability::Imu(Imu {
                target: link_target(link),
                publish_rate_hz: DEFAULT_PUBLISH_RATE_HZ,
                axes: None,
            }),
        )
    }

    /// A satellite receiver on `link`, reporting in the robot's local frame.
    #[must_use]
    pub fn gnss(self, capability: &str, link: &str) -> Self {
        self.capability(
            capability,
            Capability::Gnss(Gnss {
                target: link_target(link),
                publish_rate_hz: 10.0,
                coordinate_system: GnssCoordinateSystem::Local,
            }),
        )
    }

    /// A 640x480 colour camera looking out of `link`.
    #[must_use]
    pub fn camera(self, capability: &str, link: &str) -> Self {
        self.capability(
            capability,
            Capability::Camera(Camera {
                target: link_target(link),
                mode: CameraMode::Rgb,
                publish_rate_hz: 30.0,
                width_px: 640,
                height_px: 480,
                field_of_view_rad: None,
            }),
        )
    }

    /// A 640x480 depth sensor looking out of `link`.
    #[must_use]
    pub fn depth(self, capability: &str, link: &str) -> Self {
        self.capability(
            capability,
            Capability::Depth(Depth {
                target: link_target(link),
                publish_rate_hz: 30.0,
                width_px: 640,
                height_px: 480,
                field_of_view_rad: None,
                min_range_m: None,
                max_range_m: None,
            }),
        )
    }

    /// An emergency stop input on `link`.
    #[must_use]
    pub fn emergency_stop(self, capability: &str, link: &str) -> Self {
        self.capability(
            capability,
            Capability::EmergencyStop(EmergencyStop {
                target: link_target(link),
            }),
        )
    }

    /// A narrow single-beam range finder on `link`.
    #[must_use]
    pub fn range(self, capability: &str, link: &str) -> Self {
        self.capability(
            capability,
            Capability::Range(Range {
                target: link_target(link),
                publish_rate_hz: 20.0,
                min_range_m: 0.05,
                max_range_m: 4.0,
                field_of_view_rad: 0.4,
            }),
        )
    }

    /// A planar lidar on `link`, publishing ranges.
    #[must_use]
    pub fn lidar(self, capability: &str, link: &str) -> Self {
        self.capability(
            capability,
            Capability::Lidar(Lidar {
                target: link_target(link),
                publish_rate_hz: 10.0,
                output: LidarOutput::Ranges,
                min_range_m: None,
                max_range_m: None,
                horizontal_fov_rad: None,
                horizontal_resolution_rad: None,
                vertical_fov_rad: None,
                vertical_resolution_rad: None,
            }),
        )
    }

    /// A millimetre-wave radar on `link`.
    #[must_use]
    pub fn mmwave(self, capability: &str, link: &str) -> Self {
        self.capability(
            capability,
            Capability::Mmwave(Mmwave {
                target: link_target(link),
                publish_rate_hz: 20.0,
            }),
        )
    }

    /// A microphone on `link`.
    #[must_use]
    pub fn microphone(self, capability: &str, link: &str) -> Self {
        self.capability(
            capability,
            Capability::Microphone(Microphone {
                target: link_target(link),
                publish_rate_hz: DEFAULT_PUBLISH_RATE_HZ,
            }),
        )
    }

    /// A speaker on `link`.
    #[must_use]
    pub fn speaker(self, capability: &str, link: &str) -> Self {
        self.capability(
            capability,
            Capability::Speaker(Speaker {
                target: link_target(link),
            }),
        )
    }

    /// A 12 V battery on `link`.
    #[must_use]
    pub fn battery(self, capability: &str, link: &str) -> Self {
        self.capability(
            capability,
            Capability::Battery(Battery {
                target: link_target(link),
                publish_rate_hz: 1.0,
                voltage_v: 12.0,
                capacity_ah: 5.0,
            }),
        )
    }

    /// An indicator light on `link`.
    #[must_use]
    pub fn led(self, capability: &str, link: &str) -> Self {
        self.capability(
            capability,
            Capability::Led(Led {
                target: link_target(link),
            }),
        )
    }
}

impl ComponentBuilder {
    /// Mount this instance on the named robot link rather than on the one
    /// generated from its instance id.
    ///
    /// The link is added beneath `base_link` unless a stated joint already
    /// attaches it.
    #[must_use]
    pub fn mounted_on(mut self, link: &str) -> Self {
        self.spec.mount_link = Some(link.to_owned());
        self
    }

    /// State which way this instance's capability is turned, as `1` or `-1`.
    ///
    /// This is what [`Robot::require_motor`] and [`Robot::require_encoder`]
    /// return beside the capability, so that a mirrored actuator is described
    /// once on the model rather than by every consumer that drives it.
    #[must_use]
    pub fn direction_sign(mut self, capability: &str, sign: i8) -> Self {
        self.spec
            .direction_signs
            .insert(capability.to_owned(), sign);
        self
    }

    /// Give this instance the hardware connection block that makes it a driven
    /// component.
    ///
    /// Its presence is what says a component driver runs for this instance,
    /// under the instance's own id, and the block is that driver's
    /// configuration. An instance without one is modelled and observed but
    /// launches no process.
    ///
    /// ```
    /// use crate::model::builder::RobotBuilder;
    ///
    /// let robot = RobotBuilder::new("rover")
    ///     .component_type("drive_motor", |motor| motor.motor("spin", "axle"))
    ///     .component_with("left_drive", "drive_motor", |mounted| {
    ///         mounted.driver(serde_json::json!({ "connection": "/dev/ttyUSB0" }))
    ///     })
    ///     .build()?;
    ///
    /// let left = robot.component("left_drive").expect("the mounted instance");
    /// assert!(left.instance().driver().is_some());
    /// # Ok::<(), crate::model::ModelError>(())
    /// ```
    #[must_use]
    pub fn driver(mut self, driver: serde_json::Value) -> Self {
        self.spec.driver = Some(driver);
        self
    }
}

impl Kinematics<'_> {
    /// The canonical config this states.
    fn into_config(self) -> Result<KinematicConfig, ModelError> {
        Ok(match self {
            Self::Differential {
                left_actuators,
                right_actuators,
                left_encoders,
                right_encoders,
                wheel_radius_m,
                wheel_base_m,
            } => KinematicConfig::Differential {
                left_actuators: references(left_actuators)?,
                right_actuators: references(right_actuators)?,
                left_encoders: references(left_encoders)?,
                right_encoders: references(right_encoders)?,
                wheel_radius_m,
                wheel_base_m,
            },
            Self::Mecanum {
                front_left_actuator,
                front_right_actuator,
                rear_left_actuator,
                rear_right_actuator,
                wheel_radius_m,
                wheel_base_m,
                track_m,
            } => KinematicConfig::Mecanum {
                front_left_actuator: front_left_actuator.parse()?,
                front_right_actuator: front_right_actuator.parse()?,
                rear_left_actuator: rear_left_actuator.parse()?,
                rear_right_actuator: rear_right_actuator.parse()?,
                wheel_radius_m,
                wheel_base_m,
                track_m,
            },
            Self::Ackermann {
                steering_actuator,
                drive_actuator,
                steering_encoder,
                drive_encoder,
                wheel_base_m,
                track_m,
                max_steering_angle_rad,
            } => KinematicConfig::Ackermann {
                steering_actuator: steering_actuator.parse()?,
                drive_actuator: drive_actuator.parse()?,
                steering_encoder: optional_reference(steering_encoder)?,
                drive_encoder: optional_reference(drive_encoder)?,
                wheel_base_m,
                track_m,
                max_steering_angle_rad,
            },
            Self::Omnidirectional {
                actuators,
                encoders,
            } => KinematicConfig::Omnidirectional {
                actuators: references(actuators)?,
                encoders: references(encoders)?,
            },
        })
    }
}

/// Normalize every component type, each with the simulation that models it.
fn build_types(
    types: BTreeMap<String, TypeSpec>,
) -> Result<BTreeMap<ComponentTypeId, Component>, ModelError> {
    let mut component_types = BTreeMap::new();
    for (component_type, spec) in types {
        let component_type = ComponentTypeId::new(component_type)?;
        let mut capabilities = BTreeMap::new();
        for (capability, declared) in spec.capabilities {
            capabilities.insert(CapabilityId::new(capability)?, declared);
        }
        let structure =
            component_structure(&component_type, &capabilities, spec.joints, &spec.bodies)?;
        // A simulation is only carried for a type that states one: an empty
        // simulation and no simulation are different facts.
        let simulation = if spec.simulated.is_empty() && spec.contact_materials.is_empty() {
            None
        } else {
            let mut simulated = BTreeMap::new();
            for (capability, modelled) in spec.simulated {
                simulated.insert(CapabilityId::new(capability)?, modelled);
            }
            Some(compiler::simulation(
                simulated,
                spec.contact_materials
                    .into_iter()
                    .map(|(link, material)| (LinkId::new(link), Some(material)))
                    .collect(),
            ))
        };
        component_types.insert(
            component_type,
            compiler::component(capabilities, structure, simulation),
        );
    }
    Ok(component_types)
}

/// The component structure its stated joints, links and capability targets
/// imply.
fn component_structure(
    component_type: &ComponentTypeId,
    capabilities: &BTreeMap<CapabilityId, Capability>,
    mut joints: Vec<JointSpec>,
    bodies: &Bodies,
) -> Result<Structure, ModelError> {
    for capability in capabilities.values() {
        match capability.target() {
            StructuralTarget::Joint { id } => {
                if !joints.iter().any(|joint| joint.name == id.as_str()) {
                    joints.push(generated_joint(
                        id.as_str(),
                        JointKind::Continuous,
                        COMPONENT_ROOT_LINK,
                        &format!("{id}{JOINT_CHILD_SUFFIX}"),
                    ));
                }
            }
            StructuralTarget::Link { id } => attach(
                &mut joints,
                COMPONENT_ROOT_LINK,
                COMPONENT_ROOT_LINK,
                id.as_str(),
            ),
        }
    }
    for link in bodies.links.keys() {
        attach(&mut joints, COMPONENT_ROOT_LINK, COMPONENT_ROOT_LINK, link);
    }
    structure(
        component_type.as_str(),
        COMPONENT_ROOT_LINK,
        &joints,
        bodies,
    )
}

/// The robot structure its stated joints, stated links and every mount link
/// imply.
fn robot_structure(
    id: &RobotId,
    mut joints: Vec<JointSpec>,
    mounts: &BTreeSet<LinkId>,
    bodies: &Bodies,
) -> Result<Structure, ModelError> {
    if !joints.iter().any(|joint| joint.child == BASE_LINK) {
        joints.push(generated_joint(
            BASE_JOINT,
            JointKind::Fixed,
            BASE_FOOTPRINT_LINK,
            BASE_LINK,
        ));
    }
    for link in mounts
        .iter()
        .map(LinkId::as_str)
        .chain(bodies.links.keys().map(String::as_str))
    {
        attach(&mut joints, BASE_FOOTPRINT_LINK, BASE_LINK, link);
    }
    structure(id.as_str(), BASE_FOOTPRINT_LINK, &joints, bodies)
}

/// Hang `link` beneath `parent` unless the structure already provides it.
///
/// The root is provided by being the root and any link a joint already moves is
/// provided by that joint; a link nothing provides would leave the structure in
/// pieces rather than as a single tree.
fn attach(joints: &mut Vec<JointSpec>, root: &str, parent: &str, link: &str) {
    if link == root || joints.iter().any(|joint| joint.child == link) {
        return;
    }
    joints.push(generated_joint(
        &format!("{link}{LINK_JOINT_SUFFIX}"),
        JointKind::Fixed,
        parent,
        link,
    ));
}

/// A joint the builder adds because nothing stated one.
fn generated_joint(name: &str, kind: JointKind, parent: &str, child: &str) -> JointSpec {
    JointSpec::from(Joint {
        name,
        kind,
        parent,
        child,
        ..Joint::default()
    })
}

/// The canonical structure rooted at `root`, with a link for the root and one
/// for every joint's child.
///
/// A link the caller gave a body carries it; every other link gets the unit
/// inertial and no geometry that being a frame requires and nothing more.
fn structure(
    name: &str,
    root: &str,
    joints: &[JointSpec],
    bodies: &Bodies,
) -> Result<Structure, ModelError> {
    let body_of = |link: &str| {
        bodies.links.get(link).cloned().unwrap_or_else(|| {
            link_value(&Link {
                name: link,
                ..Link::default()
            })
        })
    };
    let mut links = vec![body_of(root)];
    links.extend(joints.iter().map(|joint| body_of(&joint.child)));
    compiler::structure(json!({
        "name": name,
        "links": links,
        "joints": joints.iter().map(joint_value).collect::<Vec<_>>(),
        "materials": bodies.materials.values().collect::<Vec<_>>()
    }))
}

/// One link as the canonical structure document carries it.
fn link_value(link: &Link<'_>) -> Value {
    json!({
        "name": link.name,
        "inertial": inertial_value(link.inertial),
        "visuals": link.visuals.iter().map(visual_value).collect::<Vec<_>>(),
        "collisions": link.collisions.iter().map(collision_value).collect::<Vec<_>>()
    })
}

fn inertial_value(inertial: Inertial) -> Value {
    let Inertia {
        ixx,
        ixy,
        ixz,
        iyy,
        iyz,
        izz,
    } = inertial.inertia;
    json!({
        "origin": pose_value(inertial.xyz, inertial.rpy),
        "mass_kg": inertial.mass_kg,
        "inertia": { "ixx": ixx, "ixy": ixy, "ixz": ixz, "iyy": iyy, "iyz": iyz, "izz": izz }
    })
}

fn visual_value(visual: &Visual<'_>) -> Value {
    json!({
        "name": visual.name,
        "origin": pose_value(visual.xyz, visual.rpy),
        "geometry": visual.geometry,
        "material": visual.material.as_ref().map(material_value)
    })
}

fn collision_value(collision: &Collision<'_>) -> Value {
    json!({
        "name": collision.name,
        "origin": pose_value(collision.xyz, collision.rpy),
        "geometry": collision.geometry
    })
}

fn material_value(material: &Material<'_>) -> Value {
    json!({
        "name": material.name,
        "color": material.color,
        "texture": material.texture
    })
}

/// One joint as the canonical structure document carries it.
fn joint_value(joint: &JointSpec) -> Value {
    let JointLimit {
        lower,
        upper,
        effort,
        velocity,
    } = joint.limit;
    json!({
        "name": joint.name,
        "kind": joint.kind,
        "origin": pose_value(joint.xyz, joint.rpy),
        "parent": joint.parent,
        "child": joint.child,
        "axis": joint.axis,
        "limit": { "lower": lower, "upper": upper, "effort": effort, "velocity": velocity },
        "calibration": joint.calibration.map(|calibration| json!({
            "rising": calibration.rising,
            "falling": calibration.falling
        })),
        "dynamics": joint.dynamics.map(|dynamics| json!({
            "damping": dynamics.damping,
            "friction": dynamics.friction
        })),
        "mimic": joint.mimic.as_ref().map(|mimic| json!({
            "joint": mimic.joint,
            "multiplier": mimic.multiplier,
            "offset": mimic.offset
        })),
        "safety": joint.safety.map(|safety| json!({
            "soft_lower_limit": safety.soft_lower_limit,
            "soft_upper_limit": safety.soft_upper_limit,
            "k_position": safety.k_position,
            "k_velocity": safety.k_velocity
        }))
    })
}

fn pose_value(xyz: [f64; 3], rpy: [f64; 3]) -> Value {
    json!({ "xyz": xyz, "rpy": rpy })
}

fn joint_target(id: &str) -> StructuralTarget {
    StructuralTarget::Joint {
        id: JointId::new(id),
    }
}

fn link_target(id: &str) -> StructuralTarget {
    StructuralTarget::Link {
        id: LinkId::new(id),
    }
}

fn references(values: &[&str]) -> Result<Vec<CapabilityRef>, ModelError> {
    values.iter().map(|value| value.parse()).collect()
}

fn optional_reference(value: Option<&str>) -> Result<Option<CapabilityRef>, ModelError> {
    value.map(str::parse).transpose()
}

#[cfg(test)]
mod tests {
    use super::{
        Collision, Dynamics, Inertial, Joint, JointLimit, Kinematics, Link, Material, Mimic,
        RobotBuilder, Visual,
    };
    use crate::model::asset::AssetId;
    use crate::model::component::capability::{
        Capability, CapabilityKind, Motor, MotorCommand, StructuralTarget,
    };
    use crate::model::error::{IdentifierKind, ModelError, StructureError};
    use crate::model::identity::{CapabilityRef, JointId, LinkId};
    use crate::model::robot::{DriveKinematics, KinematicConfig, MotionLimits};
    use crate::model::simulation;
    use crate::model::structure::{Geometry, JointKind};

    fn reference(value: &str) -> CapabilityRef {
        value.parse().expect("a well formed capability reference")
    }

    /// Every kind the canonical model can declare has to survive the trip
    /// through the builder, because a kind that cannot be stated is a robot
    /// that cannot be composed without documents.
    #[test]
    fn every_capability_kind_reaches_a_validated_robot() {
        let robot = RobotBuilder::new("rover")
            .component_type("everything", |all| {
                all.motor("spin", "axle")
                    .encoder("count", "axle")
                    .accelerometer("accel", "imu_link")
                    .gyroscope("gyro", "imu_link")
                    .magnetometer("mag", "imu_link")
                    .imu("imu", "imu_link")
                    .gnss("fix", "antenna")
                    .camera("rgb", "lens")
                    .depth("depth", "lens")
                    .emergency_stop("estop", "panel")
                    .range("tof", "nose")
                    .lidar("scan", "dome")
                    .mmwave("radar", "nose")
                    .microphone("mic", "panel")
                    .speaker("horn", "panel")
                    .battery("pack", "chassis")
                    .led("beacon", "dome")
            })
            .component("kitchen_sink", "everything")
            .build()
            .expect("every capability kind composes a valid robot");

        let component = robot
            .component("kitchen_sink")
            .map(|component| component.component_type())
            .expect("the mounted type is loaded");
        let mut kinds = component
            .capabilities()
            .map(|(_, capability)| capability.kind())
            .collect::<Vec<_>>();
        kinds.sort_unstable();
        kinds.dedup();
        assert_eq!(
            kinds.len(),
            17,
            "every canonical capability kind must be reachable"
        );
        assert_eq!(robot.capability_refs(|_| true).len(), 17);
    }

    /// A capability is only usable if the structural item it names really
    /// exists, so both target kinds must resolve on the generated structure.
    #[test]
    fn both_structural_target_kinds_resolve() {
        let robot = RobotBuilder::new("rover")
            .component_type("drive_motor", |motor| {
                motor.motor("spin", "axle").encoder("count", "axle")
            })
            .component_type("rgbd", |camera| camera.camera("rgb", "lens"))
            .component("left_drive", "drive_motor")
            .component("front_camera", "rgbd")
            .build()
            .expect("a valid robot");

        // A link target resolves to the runtime frame it names.
        assert_eq!(
            robot
                .link_target_frame(&reference("front_camera.rgb"))
                .expect("the camera targets a link"),
            LinkId::new("front_camera__lens")
        );
        // A joint target names a joint the component structure carries, and
        // the motor and the encoder measuring it share one.
        let component = robot
            .component("left_drive")
            .map(|component| component.component_type())
            .expect("the mounted type is loaded");
        assert!(component.structure().joint("axle").is_some());
        assert!(component.structure().link("axle_link").is_some());
        for capability in ["left_drive.spin", "left_drive.count"] {
            let target = robot
                .capability(&reference(capability))
                .expect("the capability is declared")
                .target();
            assert_eq!(
                target,
                &StructuralTarget::Joint {
                    id: JointId::new("axle")
                },
                "{capability}"
            );
        }
    }

    #[test]
    fn every_kinematic_config_validates_and_resolves() {
        let wheeled = |builder: RobotBuilder| {
            builder
                .component_type("drive_motor", |motor| {
                    motor.motor("spin", "axle").encoder("count", "axle")
                })
                .component("front_left", "drive_motor")
                .component("front_right", "drive_motor")
                .component("rear_left", "drive_motor")
                .component("rear_right", "drive_motor")
        };
        let differential = wheeled(RobotBuilder::new("rover"))
            .kinematics(Kinematics::Differential {
                left_actuators: &["front_left.spin", "rear_left.spin"],
                right_actuators: &["front_right.spin", "rear_right.spin"],
                left_encoders: &["front_left.count", "rear_left.count"],
                right_encoders: &["front_right.count", "rear_right.count"],
                wheel_radius_m: 0.1,
                wheel_base_m: 0.5,
            })
            .build()
            .expect("a valid differential robot");
        assert!(matches!(
            differential
                .motion()
                .kinematic()
                .drive_kinematics()
                .expect("the geometry is usable"),
            DriveKinematics::Differential(geometry) if geometry.wheel_radius_m == 0.1
        ));

        let mecanum = wheeled(RobotBuilder::new("rover"))
            .kinematics(Kinematics::Mecanum {
                front_left_actuator: "front_left.spin",
                front_right_actuator: "front_right.spin",
                rear_left_actuator: "rear_left.spin",
                rear_right_actuator: "rear_right.spin",
                wheel_radius_m: 0.1,
                wheel_base_m: 0.4,
                track_m: 0.6,
            })
            .build()
            .expect("a valid mecanum robot");
        assert!(matches!(
            mecanum
                .motion()
                .kinematic()
                .drive_kinematics()
                .expect("the geometry is usable"),
            DriveKinematics::Mecanum(geometry) if geometry.track_m == 0.6
        ));

        let ackermann = wheeled(RobotBuilder::new("rover"))
            .kinematics(Kinematics::Ackermann {
                steering_actuator: "front_left.spin",
                drive_actuator: "rear_left.spin",
                steering_encoder: Some("front_left.count"),
                drive_encoder: Some("rear_left.count"),
                wheel_base_m: 2.5,
                track_m: 1.5,
                max_steering_angle_rad: 0.6,
            })
            .build()
            .expect("a valid ackermann robot");
        assert!(matches!(
            ackermann
                .motion()
                .kinematic()
                .drive_kinematics()
                .expect("the geometry is usable"),
            DriveKinematics::Ackermann(geometry) if geometry.max_steering_angle_rad == 0.6
        ));

        let omnidirectional = wheeled(RobotBuilder::new("rover"))
            .kinematics(Kinematics::Omnidirectional {
                actuators: &["front_left.spin"],
                encoders: &["front_left.count"],
            })
            .build()
            .expect("a valid omnidirectional robot");
        assert_eq!(
            omnidirectional
                .motion()
                .kinematic()
                .drive_kinematics()
                .expect("an omnidirectional drive carries no scalars to reject"),
            DriveKinematics::Omnidirectional
        );
    }

    /// A drive resolves each side through `require_motor`/`require_encoder`,
    /// so the references a kinematic config carries have to name capabilities
    /// of the right kind on components the robot really mounts.
    #[test]
    fn a_kinematic_reference_must_name_a_capability_of_the_right_kind() {
        let miswired = RobotBuilder::new("rover")
            .component_type("drive_motor", |motor| {
                motor.motor("spin", "axle").encoder("count", "axle")
            })
            .component("left_drive", "drive_motor")
            .kinematics(Kinematics::Omnidirectional {
                actuators: &["left_drive.count"],
                encoders: &[],
            })
            .build();

        assert!(matches!(
            miswired,
            Err(ModelError::CapabilityKindMismatch {
                expected: CapabilityKind::Motor,
                actual: CapabilityKind::Encoder,
                ..
            })
        ));
    }

    #[test]
    fn direction_signs_come_back_beside_the_capability() {
        let robot = RobotBuilder::new("rover")
            .component_type("drive_motor", |motor| {
                motor.motor("spin", "axle").encoder("count", "axle")
            })
            .component("left_drive", "drive_motor")
            .component_with("right_drive", "drive_motor", |mounted| {
                mounted
                    .direction_sign("spin", -1)
                    .direction_sign("count", -1)
            })
            .build()
            .expect("a valid robot");

        for (capability, expected) in [("left_drive.spin", 1), ("right_drive.spin", -1)] {
            let (_motor, sign) = robot
                .require_motor(&reference(capability))
                .expect("the motor resolves");
            assert_eq!(sign, expected, "{capability}");
        }
        for (capability, expected) in [("left_drive.count", 1), ("right_drive.count", -1)] {
            let (_encoder, sign) = robot
                .require_encoder(&reference(capability))
                .expect("the encoder resolves");
            assert_eq!(sign, expected, "{capability}");
        }
    }

    #[test]
    fn a_direction_sign_that_is_not_a_direction_is_refused() {
        let rejected = RobotBuilder::new("rover")
            .component_type("drive_motor", |motor| motor.motor("spin", "axle"))
            .component_with("left_drive", "drive_motor", |mounted| {
                mounted.direction_sign("spin", 0)
            })
            .build();

        assert!(matches!(
            rejected,
            Err(ModelError::DirectionSign { value: 0, .. })
        ));
    }

    #[test]
    fn identity_services_and_limits_are_carried_as_stated() {
        let robot = RobotBuilder::new("rover")
            .service("drive", None)
            .service("mission", Some(serde_json::json!({ "speed": 1 })))
            .motion_limits(MotionLimits {
                max_linear_speed_mps: 0.6,
                max_angular_speed_radps: 2.0,
            })
            .build()
            .expect("a valid robot");

        assert_eq!(robot.id().as_str(), "rover");
        assert_eq!(robot.motion().limits().max_linear_speed_mps, 0.6);
        assert_eq!(
            robot
                .services()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            ["drive", "mission"]
        );
        // A declared service with no configuration and an undeclared one are
        // different facts, and `service` is what tells them apart.
        assert!(robot.service("drive").is_some());
        assert_eq!(robot.service_config("drive"), None);
        assert_eq!(
            robot.service_config("mission"),
            Some(&serde_json::json!({ "speed": 1 }))
        );
        assert!(robot.service("nope").is_none());
    }

    /// The structure a caller states is theirs; only what they leave out is
    /// generated, and the conventional base frames are always there.
    #[test]
    fn stated_structure_is_kept_and_the_rest_is_generated() {
        let robot = RobotBuilder::new("rover")
            .joint(Joint {
                name: "mast_joint",
                kind: JointKind::Revolute,
                parent: "base_link",
                child: "mast",
                xyz: [0.1, 0.0, 0.4],
                ..Joint::default()
            })
            .component_type("rgbd", |camera| camera.camera("rgb", "lens"))
            .component_with("front_camera", "rgbd", |mounted| mounted.mounted_on("mast"))
            .component("rear_camera", "rgbd")
            .build()
            .expect("a valid robot");

        let structure = robot.structure();
        assert_eq!(structure.root_link(), &LinkId::new("base_footprint"));
        let mast = structure.joint("mast_joint").expect("the stated joint");
        assert_eq!(mast.kind(), JointKind::Revolute);
        assert_eq!(mast.origin().xyz(), [0.1, 0.0, 0.4]);
        // The stated mount link is the one stated; the unstated one is
        // generated from the instance id.
        assert!(structure.link("mast").is_some());
        assert!(structure.link("rear_camera_mount").is_some());
        assert!(
            structure.joint("rear_camera_mount_joint").is_some(),
            "an unstated mount link is attached beneath base_link"
        );
    }

    /// A stated link is a body for a link the tree already has, or a new link
    /// hung where a mount link would be. Either way nothing else changes: a
    /// link nobody described keeps the unit inertial and no geometry.
    #[test]
    fn a_stated_link_carries_its_body_and_leaves_the_rest_generated() {
        let robot = RobotBuilder::new("rover")
            .link(Link {
                name: "base_link",
                inertial: Inertial {
                    mass_kg: 12.0,
                    ..Inertial::default()
                },
                ..Link::default()
            })
            .link(Link {
                name: "mast",
                collisions: vec![Collision::new(Geometry::Sphere { radius: 0.2 })],
                ..Link::default()
            })
            .build()
            .expect("a valid robot");

        let structure = robot.structure();
        // A body given to a link the base frames already provide does not
        // attach it a second time.
        assert_eq!(
            structure
                .link("base_link")
                .expect("the body frame")
                .inertial()
                .mass_kg(),
            12.0
        );
        assert!(structure.joint("base_link_joint").is_none());
        // A link nothing else provides is hung beneath the body frame.
        let mast_joint = structure.joint("mast_joint").expect("the generated joint");
        assert_eq!(mast_joint.parent(), &LinkId::new("base_link"));
        assert_eq!(
            structure
                .link("mast")
                .expect("the stated link")
                .collisions()
                .len(),
            1
        );
        // The root was never described, so it is still a bare frame.
        let root = structure.link("base_footprint").expect("the root link");
        assert_eq!(root.inertial().mass_kg(), 1.0);
        assert_eq!(root.visuals().len(), 0);
    }

    /// A component type states its structure exactly the way the robot does,
    /// rooted at `mount` instead of the base frames.
    #[test]
    fn a_component_type_states_its_own_links_joints_and_materials() {
        let robot = RobotBuilder::new("rover")
            .component_type("pan_tilt", |head| {
                head.motor("pan", "pan_joint")
                    .joint(Joint {
                        name: "pan_joint",
                        kind: JointKind::Revolute,
                        parent: "mount",
                        child: "lens",
                        limit: JointLimit {
                            lower: -3.0,
                            upper: 3.0,
                            effort: 1.0,
                            velocity: 4.0,
                        },
                        dynamics: Some(Dynamics {
                            damping: 0.05,
                            friction: 0.01,
                        }),
                        ..Joint::default()
                    })
                    .link(Link {
                        name: "lens",
                        visuals: vec![Visual::new(Geometry::Cylinder {
                            radius: 0.02,
                            length: 0.01,
                        })],
                        ..Link::default()
                    })
                    .link(Link {
                        name: "shade",
                        inertial: Inertial {
                            mass_kg: 0.05,
                            ..Inertial::default()
                        },
                        ..Link::default()
                    })
                    .material(Material {
                        color: Some([0.0, 0.0, 0.0, 1.0]),
                        ..Material::new("matte")
                    })
            })
            .component("head", "pan_tilt")
            .build()
            .expect("a valid robot");

        let structure = robot
            .component("head")
            .map(|component| component.component_type())
            .expect("the mounted type is loaded")
            .structure();
        assert_eq!(structure.root_link(), &LinkId::new("mount"));
        let pan = structure.joint("pan_joint").expect("the stated joint");
        assert_eq!(pan.limit().velocity(), 4.0);
        assert_eq!(
            pan.dynamics().map(|dynamics| dynamics.damping()),
            Some(0.05)
        );
        assert_eq!(
            structure
                .link("lens")
                .expect("the stated link")
                .visuals()
                .len(),
            1
        );
        // A stated link no joint attaches is hung beneath the component root.
        assert_eq!(
            structure
                .joint("shade_joint")
                .expect("the generated joint")
                .parent(),
            &LinkId::new("mount")
        );
        let catalogue = structure.materials().collect::<Vec<_>>();
        assert_eq!(catalogue.len(), 1);
        assert_eq!(catalogue[0].name(), "matte");
    }

    /// A joint's own fields are checked by the same canonical rules a compiled
    /// document is, and the builder is not a way around any of them.
    #[test]
    fn a_structural_value_the_model_refuses_is_refused_here_too() {
        let inverted = |limit| {
            RobotBuilder::new("rover")
                .joint(Joint {
                    name: "mast_joint",
                    kind: JointKind::Revolute,
                    parent: "base_link",
                    child: "mast",
                    limit,
                    ..Joint::default()
                })
                .build()
        };
        assert!(matches!(
            inverted(JointLimit {
                lower: 1.0,
                upper: -1.0,
                effort: 0.0,
                velocity: 0.0,
            }),
            Err(ModelError::Structure(StructureError::JointLimits { .. }))
        ));
        assert!(matches!(
            RobotBuilder::new("rover")
                .joint(Joint {
                    name: "mast_joint",
                    kind: JointKind::Revolute,
                    parent: "base_link",
                    child: "mast",
                    mimic: Some(Mimic::new("no_such_joint")),
                    ..Joint::default()
                })
                .build(),
            Err(ModelError::Structure(
                StructureError::UnknownMimicJoint { .. }
            ))
        ));
        assert!(matches!(
            RobotBuilder::new("rover")
                .link(Link {
                    name: "mast",
                    inertial: Inertial {
                        mass_kg: -1.0,
                        ..Inertial::default()
                    },
                    ..Link::default()
                })
                .build(),
            Err(ModelError::Structure(StructureError::Mass { .. }))
        ));
        assert!(matches!(
            RobotBuilder::new("rover")
                .link(Link {
                    name: "mast",
                    visuals: vec![Visual::new(Geometry::Sphere { radius: 0.0 })],
                    ..Link::default()
                })
                .build(),
            Err(ModelError::Structure(StructureError::Geometry { .. }))
        ));
    }

    #[test]
    fn a_simulation_is_carried_only_for_the_types_that_state_one() {
        let robot = RobotBuilder::new("rover")
            .component_type("drive_motor", |motor| {
                motor
                    .motor("spin", "axle")
                    .simulated(
                        "spin",
                        simulation::Capability::Motor(simulation::Motor::default()),
                    )
                    .contact_material("axle_link", "rubber")
            })
            .component_type("rgbd", |camera| camera.camera("rgb", "lens"))
            .component("left_drive", "drive_motor")
            .component("front_camera", "rgbd")
            .build()
            .expect("a valid robot");

        let simulation = robot
            .component("left_drive")
            .and_then(|component| component.simulation())
            .expect("the drive states a simulation");
        assert_eq!(
            simulation
                .capability("spin")
                .expect("the simulated motor")
                .kind(),
            CapabilityKind::Motor
        );
        assert_eq!(
            simulation
                .links()
                .next()
                .and_then(|(_, link)| link.contact_material()),
            Some("rubber")
        );
        assert!(
            robot
                .component("front_camera")
                .and_then(|component| component.simulation())
                .is_none()
        );
    }

    /// A simulation may only model a capability its component declares, of the
    /// same kind, and the builder must not be a way around that.
    #[test]
    fn a_simulation_cannot_model_a_capability_the_component_does_not_declare() {
        let rejected = RobotBuilder::new("rover")
            .component_type("drive_motor", |motor| {
                motor.motor("spin", "axle").simulated(
                    "nonexistent",
                    simulation::Capability::Motor(simulation::Motor::default()),
                )
            })
            .component("left_drive", "drive_motor")
            .build();

        assert!(matches!(
            rejected,
            Err(ModelError::SimulationWithoutCapability { .. })
        ));
    }

    /// Every rejection is a typed value the caller can match on, not a panic.
    #[test]
    fn a_rejected_robot_returns_the_condition_it_violated() {
        assert!(matches!(
            RobotBuilder::new("Rover").build(),
            Err(ModelError::NotNormalized {
                kind: IdentifierKind::RobotId,
                ..
            })
        ));
        assert!(matches!(
            RobotBuilder::new("rover")
                .kinematics(Kinematics::Omnidirectional {
                    actuators: &["not-a-reference"],
                    encoders: &[],
                })
                .build(),
            Err(ModelError::MalformedCapabilityReference { .. })
        ));
        assert!(matches!(
            RobotBuilder::new("rover")
                .component("left_drive", "never_declared")
                .build(),
            Err(ModelError::UnknownComponentType { .. })
        ));
        // A joint kind the runtime has no controller for is refused, rather
        // than becoming a joint nothing can drive.
        assert!(matches!(
            RobotBuilder::new("rover")
                .joint(Joint {
                    name: "wobble",
                    kind: JointKind::Spherical,
                    parent: "base_link",
                    child: "head",
                    ..Joint::default()
                })
                .build(),
            Err(ModelError::UnsupportedJointKind { .. })
        ));
        // A joint hanging from a link nothing provides leaves the structure in
        // pieces rather than a single tree.
        assert!(matches!(
            RobotBuilder::new("rover")
                .joint(Joint {
                    name: "head_joint",
                    parent: "neck",
                    child: "head",
                    ..Joint::default()
                })
                .build(),
            Err(ModelError::Structure(
                StructureError::UnknownJointLink { .. }
            ))
        ));
        // The source compiler owns the conservative footprint derivation;
        // unsupported collision geometry must not be turned into a missing
        // envelope for a runtime to discover later.
        assert!(matches!(
            RobotBuilder::new("rover")
                .link(Link {
                    name: "chassis",
                    collisions: vec![Collision::new(Geometry::Mesh {
                        asset: AssetId::new("meshes/chassis.stl").expect("normalized asset id"),
                        scale: None,
                    })],
                    ..Link::default()
                })
                .build(),
            Err(ModelError::FootprintMesh { .. })
        ));
        assert!(matches!(
            RobotBuilder::new("rover")
                .joint(Joint {
                    name: "arm_joint",
                    kind: JointKind::Revolute,
                    parent: "base_link",
                    child: "arm",
                    ..Joint::default()
                })
                .link(Link {
                    name: "arm",
                    collisions: vec![Collision::new(Geometry::Sphere { radius: 0.1 })],
                    ..Link::default()
                })
                .build(),
            Err(ModelError::FootprintMovableJoint { .. })
        ));
    }

    /// The general entry point has to reach parameters no shorthand offers,
    /// including a target kind the shorthand would not choose.
    #[test]
    fn the_general_capability_entry_point_carries_every_parameter() {
        let robot = RobotBuilder::new("arm-bot")
            .component_type("joint_motor", |joint_motor| {
                joint_motor.capability(
                    "lift",
                    Capability::Motor(Motor {
                        target: StructuralTarget::Link {
                            id: LinkId::new("housing"),
                        },
                        command: MotorCommand::Position,
                        gear_ratio: 50.0,
                        max_torque_nm: Some(12.0),
                        max_velocity_radps: Some(3.0),
                    }),
                )
            })
            .component("arm", "joint_motor")
            .build()
            .expect("a valid robot");

        let (motor, _sign) = robot
            .require_motor(&reference("arm.lift"))
            .expect("the motor resolves");
        assert_eq!(motor.command, MotorCommand::Position);
        assert_eq!(motor.gear_ratio, 50.0);
        assert_eq!(motor.max_torque_nm, Some(12.0));
        // A link-targeted motor is unusual but legal, and its target resolves
        // to a link the generated structure carries.
        assert_eq!(
            robot
                .link_target_frame(&reference("arm.lift"))
                .expect("the motor targets a link"),
            LinkId::new("arm__housing")
        );
    }

    #[test]
    fn a_restated_type_or_instance_replaces_the_earlier_one() {
        let robot = RobotBuilder::new("rover")
            .component_type("rgbd", |camera| camera.camera("rgb", "lens"))
            .component_type("rgbd", |camera| camera.camera("mono", "lens"))
            .component("front_camera", "rgbd")
            .component_with("front_camera", "rgbd", |mounted| mounted.mounted_on("mast"))
            .build()
            .expect("a valid robot");

        assert_eq!(
            robot
                .capability_refs(|_| true)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["front_camera.mono"]
        );
        assert_eq!(
            robot
                .component("front_camera")
                .expect("the instance is mounted")
                .instance()
                .mount_link(),
            &LinkId::new("mast")
        );
    }

    #[test]
    fn a_robot_with_nothing_stated_is_still_a_valid_robot() {
        let robot = RobotBuilder::new("rover")
            .build()
            .expect("the defaults compose a valid robot");

        assert_eq!(robot.component_ids().len(), 0);
        assert_eq!(
            robot.structure().root_link(),
            &LinkId::new("base_footprint")
        );
        assert!(robot.structure().link("base_link").is_some());
        assert!(matches!(
            robot.motion().kinematic(),
            KinematicConfig::Omnidirectional { .. }
        ));
    }
}
