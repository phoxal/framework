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
//!
//! Generated links carry a unit inertial and no geometry, and generated joints
//! sit at their parent's origin turning about Z. State a [`Joint`] to say
//! otherwise.
//!
//! ```
//! use phoxal_model::builder::{Kinematics, RobotBuilder};
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
//! # Ok::<(), phoxal_model::ModelError>(())
//! ```

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::compiler::{self, RobotParts};
use crate::component::Component;
use crate::component::capability::{
    Accelerometer, Battery, Camera, CameraMode, Capability, Depth, EmergencyStop, Encoder,
    EncoderType, Gnss, GnssCoordinateSystem, Gyroscope, Imu, Led, Lidar, LidarOutput, Magnetometer,
    Microphone, Mmwave, Motor, MotorCommand, Range, Speaker, StructuralTarget,
};
use crate::error::ModelError;
use crate::identity::{
    CapabilityId, CapabilityRef, ComponentInstanceId, ComponentTypeId, JointId, LinkId, RobotId,
    RobotNamespace,
};
use crate::robot::{Clock, KinematicConfig, MotionLimits, Robot};
use crate::simulation;
use crate::structure::{BASE_FOOTPRINT_LINK, BASE_LINK, JointKind, Structure};

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

/// The namespace a built robot carries unless one is stated.
const DEFAULT_NAMESPACE: &str = "dev";

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
/// use phoxal_model::builder::{Kinematics, RobotBuilder};
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
/// # Ok::<(), phoxal_model::ModelError>(())
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
/// parent's origin, turns about Z, and is [`JointKind::Fixed`].
///
/// ```
/// use phoxal_model::builder::{Joint, RobotBuilder};
/// use phoxal_model::structure::JointKind;
///
/// let robot = RobotBuilder::new("rover")
///     .joint(Joint {
///         name: "mast_joint",
///         kind: JointKind::Revolute,
///         parent: "base_link",
///         child: "mast",
///         xyz: [0.0, 0.0, 0.4],
///         ..Joint::default()
///     })
///     .build()?;
///
/// assert!(robot.structure().link("mast").is_some());
/// # Ok::<(), phoxal_model::ModelError>(())
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
        }
    }
}

/// One component type as the builder holds it.
#[derive(Debug, Default)]
struct TypeSpec {
    capabilities: BTreeMap<String, Capability>,
    joints: Vec<JointSpec>,
    simulated: BTreeMap<String, simulation::Capability>,
    contact_materials: BTreeMap<String, String>,
}

/// One mounted instance as the builder holds it.
#[derive(Debug)]
struct InstanceSpec {
    component_type: String,
    mount_link: Option<String>,
    direction_signs: BTreeMap<String, i8>,
}

/// Composes a canonical [`Robot`] from stated facts.
///
/// No method here fails. A rejected value is held until [`Self::build`], which
/// reports the first one as a typed [`ModelError`], so a chain reads as one
/// statement rather than a sequence of fallible steps.
///
/// ```
/// use phoxal_model::builder::RobotBuilder;
///
/// let robot = RobotBuilder::new("rover")
///     .component_type("rgbd", |camera| camera.camera("rgb", "lens"))
///     .component("front_camera", "rgbd")
///     .build()?;
///
/// assert_eq!(robot.identity().id().as_str(), "rover");
/// # Ok::<(), phoxal_model::ModelError>(())
/// ```
#[derive(Debug)]
pub struct RobotBuilder {
    id: String,
    namespace: String,
    clock: Clock,
    motion_limits: MotionLimits,
    /// The drive, already normalized. Held as a `Result` so that a malformed
    /// capability reference is reported by [`RobotBuilder::build`] rather than
    /// forcing every caller to handle one mid-chain.
    kinematic: Result<KinematicConfig, ModelError>,
    joints: Vec<JointSpec>,
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
    /// It starts on the real clock, in the `dev` namespace, with an
    /// omnidirectional kinematic config declaring no actuators - the one
    /// geometry that describes nothing a robot without a drive would have to
    /// invent.
    ///
    /// ```
    /// use phoxal_model::builder::RobotBuilder;
    ///
    /// let robot = RobotBuilder::new("rover").build()?;
    ///
    /// assert_eq!(robot.identity().namespace().as_str(), "dev");
    /// assert_eq!(robot.clock(), phoxal_model::Clock::Real);
    /// # Ok::<(), phoxal_model::ModelError>(())
    /// ```
    #[must_use]
    pub fn new(id: &str) -> Self {
        Self {
            id: id.to_owned(),
            namespace: DEFAULT_NAMESPACE.to_owned(),
            clock: Clock::Real,
            motion_limits: DEFAULT_MOTION_LIMITS,
            kinematic: Ok(KinematicConfig::Omnidirectional {
                actuators: Vec::new(),
                encoders: Vec::new(),
            }),
            joints: Vec::new(),
            types: BTreeMap::new(),
            instances: BTreeMap::new(),
        }
    }

    /// Scope this robot's runtime identities under `namespace`.
    #[must_use]
    pub fn namespace(mut self, namespace: &str) -> Self {
        self.namespace = namespace.to_owned();
        self
    }

    /// Put this robot on the given time domain.
    ///
    /// ```
    /// use phoxal_model::Clock;
    /// use phoxal_model::builder::RobotBuilder;
    ///
    /// let robot = RobotBuilder::new("rover").clock(Clock::Simulated).build()?;
    ///
    /// assert_eq!(robot.clock(), Clock::Simulated);
    /// # Ok::<(), phoxal_model::ModelError>(())
    /// ```
    #[must_use]
    pub const fn clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
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

    /// Declare one component type.
    ///
    /// Declaring the same type twice replaces the earlier declaration, so a
    /// type is stated once and mounted as many times as needed.
    ///
    /// ```
    /// use phoxal_model::builder::RobotBuilder;
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
    /// # Ok::<(), phoxal_model::ModelError>(())
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
    /// use phoxal_model::builder::RobotBuilder;
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
    /// # Ok::<(), phoxal_model::ModelError>(())
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
    /// use phoxal_model::{IdentifierKind, ModelError};
    /// use phoxal_model::builder::RobotBuilder;
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
        let namespace = RobotNamespace::new(self.namespace)?;
        let kinematic = self.kinematic?;
        let types = build_types(self.types)?;
        let mut component_instances = BTreeMap::new();
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
            component_instances.insert(
                instance.clone(),
                compiler::component_instance(
                    instance,
                    ComponentTypeId::new(spec.component_type)?,
                    mount_link,
                    direction_signs,
                ),
            );
        }
        let structure = robot_structure(&id, self.joints, &mounts)?;
        compiler::robot(RobotParts {
            id,
            namespace,
            clock: self.clock,
            kinematic,
            motion_limits: self.motion_limits,
            component_instances,
            component_types: types.components,
            simulation_types: types.simulations,
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
    /// use phoxal_model::builder::RobotBuilder;
    /// use phoxal_model::component::capability::{
    ///     Capability, Motor, MotorCommand, StructuralTarget,
    /// };
    /// use phoxal_model::identity::JointId;
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
    /// # Ok::<(), phoxal_model::ModelError>(())
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

    /// Model one of this type's capabilities in a simulated world.
    ///
    /// The named capability must be one this type declares, of the same kind,
    /// which [`RobotBuilder::build`] checks.
    ///
    /// ```
    /// use phoxal_model::builder::RobotBuilder;
    /// use phoxal_model::simulation;
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
    /// assert!(robot.simulation_for_instance("left_drive").is_some());
    /// # Ok::<(), phoxal_model::ModelError>(())
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

/// The normalized component types, and the simulations modelling them.
///
/// The two are produced together because a simulation is keyed by the type it
/// models, and returned together so neither can be assembled without the other.
#[derive(Debug)]
struct NormalizedTypes {
    components: BTreeMap<ComponentTypeId, Component>,
    simulations: BTreeMap<ComponentTypeId, simulation::Simulation>,
}

/// Normalize every component type, and the simulations that model them.
fn build_types(types: BTreeMap<String, TypeSpec>) -> Result<NormalizedTypes, ModelError> {
    let mut component_types = BTreeMap::new();
    let mut simulation_types = BTreeMap::new();
    for (component_type, spec) in types {
        let component_type = ComponentTypeId::new(component_type)?;
        let mut capabilities = BTreeMap::new();
        for (capability, declared) in spec.capabilities {
            capabilities.insert(CapabilityId::new(capability)?, declared);
        }
        let structure = component_structure(&component_type, &capabilities, spec.joints)?;
        // A simulation is only carried for a type that states one: an empty
        // simulation and no simulation are different facts.
        if !spec.simulated.is_empty() || !spec.contact_materials.is_empty() {
            let mut simulated = BTreeMap::new();
            for (capability, modelled) in spec.simulated {
                simulated.insert(CapabilityId::new(capability)?, modelled);
            }
            simulation_types.insert(
                component_type.clone(),
                compiler::simulation(
                    simulated,
                    spec.contact_materials
                        .into_iter()
                        .map(|(link, material)| (LinkId::new(link), Some(material)))
                        .collect(),
                ),
            );
        }
        component_types.insert(component_type, compiler::component(capabilities, structure));
    }
    Ok(NormalizedTypes {
        components: component_types,
        simulations: simulation_types,
    })
}

/// The component structure its stated joints and capability targets imply.
fn component_structure(
    component_type: &ComponentTypeId,
    capabilities: &BTreeMap<CapabilityId, Capability>,
    mut joints: Vec<JointSpec>,
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
            StructuralTarget::Link { id } => {
                if id.as_str() != COMPONENT_ROOT_LINK
                    && !joints.iter().any(|joint| joint.child == id.as_str())
                {
                    joints.push(generated_joint(
                        &format!("{id}{LINK_JOINT_SUFFIX}"),
                        JointKind::Fixed,
                        COMPONENT_ROOT_LINK,
                        id.as_str(),
                    ));
                }
            }
        }
    }
    structure(component_type.as_str(), COMPONENT_ROOT_LINK, &joints)
}

/// The robot structure its stated joints and every mount link imply.
fn robot_structure(
    id: &RobotId,
    mut joints: Vec<JointSpec>,
    mounts: &BTreeSet<LinkId>,
) -> Result<Structure, ModelError> {
    if !joints.iter().any(|joint| joint.child == BASE_LINK) {
        joints.push(generated_joint(
            BASE_JOINT,
            JointKind::Fixed,
            BASE_FOOTPRINT_LINK,
            BASE_LINK,
        ));
    }
    for mount in mounts {
        // Mounting straight onto a base frame needs no link of its own.
        if mount == BASE_FOOTPRINT_LINK
            || mount == BASE_LINK
            || joints.iter().any(|joint| joint.child == mount.as_str())
        {
            continue;
        }
        joints.push(generated_joint(
            &format!("{mount}{LINK_JOINT_SUFFIX}"),
            JointKind::Fixed,
            BASE_LINK,
            mount.as_str(),
        ));
    }
    structure(id.as_str(), BASE_FOOTPRINT_LINK, &joints)
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
fn structure(name: &str, root: &str, joints: &[JointSpec]) -> Result<Structure, ModelError> {
    let mut links = vec![link_value(root)];
    links.extend(joints.iter().map(|joint| link_value(&joint.child)));
    compiler::structure(json!({
        "name": name,
        "links": links,
        "joints": joints.iter().map(joint_value).collect::<Vec<_>>(),
        "materials": []
    }))
}

/// A link with a unit inertial and no geometry: enough to be a frame, and
/// nothing more than the model requires.
fn link_value(name: &str) -> Value {
    json!({
        "name": name,
        "inertial": {
            "origin": pose_value([0.0; 3], [0.0; 3]),
            "mass_kg": 1.0,
            "inertia": { "ixx": 1.0, "ixy": 0.0, "ixz": 0.0, "iyy": 1.0, "iyz": 0.0, "izz": 1.0 }
        },
        "visuals": [],
        "collisions": []
    })
}

/// One joint as the canonical structure document carries it.
///
/// The limits are all zero, which is what the document compiler emits for a
/// URDF joint that authors no `<limit>`.
fn joint_value(joint: &JointSpec) -> Value {
    json!({
        "name": joint.name,
        "kind": joint.kind,
        "origin": pose_value(joint.xyz, joint.rpy),
        "parent": joint.parent,
        "child": joint.child,
        "axis": joint.axis,
        "limit": { "lower": 0.0, "upper": 0.0, "effort": 0.0, "velocity": 0.0 }
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
    use super::{Joint, Kinematics, RobotBuilder};
    use crate::component::capability::{
        Capability, CapabilityKind, Motor, MotorCommand, StructuralTarget,
    };
    use crate::error::{IdentifierKind, ModelError, StructureError};
    use crate::identity::{CapabilityRef, JointId, LinkId};
    use crate::robot::{Clock, DriveKinematics, KinematicConfig, MotionLimits};
    use crate::simulation;
    use crate::structure::JointKind;

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
            .component_for_instance("kitchen_sink")
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
            .component_for_instance("left_drive")
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
    fn identity_clock_and_limits_are_carried_as_stated() {
        let robot = RobotBuilder::new("rover")
            .namespace("lab")
            .clock(Clock::Simulated)
            .motion_limits(MotionLimits {
                max_linear_speed_mps: 0.6,
                max_angular_speed_radps: 2.0,
            })
            .build()
            .expect("a valid robot");

        assert_eq!(robot.identity().id().as_str(), "rover");
        assert_eq!(robot.identity().namespace().as_str(), "lab");
        assert_eq!(robot.clock(), Clock::Simulated);
        assert_eq!(robot.motion().limits().max_linear_speed_mps, 0.6);
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
            .simulation_for_instance("left_drive")
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
        assert!(robot.simulation_for_instance("front_camera").is_none());
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
            RobotBuilder::new("rover").namespace("lab space").build(),
            Err(ModelError::NotNormalized {
                kind: IdentifierKind::RobotNamespace,
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
                .component_instance("front_camera")
                .expect("the instance is mounted")
                .mount_link(),
            &LinkId::new("mast")
        );
    }

    #[test]
    fn a_robot_with_nothing_stated_is_still_a_valid_robot() {
        let robot = RobotBuilder::new("rover")
            .build()
            .expect("the defaults compose a valid robot");

        assert_eq!(robot.components().len(), 0);
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
