//! The canonical model's failure vocabulary.
//!
//! Every variant carries the typed values that were rejected, so a caller can
//! match on the condition instead of parsing a message. The `Display` text is
//! what operators see in a compile failure, so it stays close to the domain
//! wording of the rule that was broken.

use std::fmt;

use crate::model::component::capability::{CapabilityKind, CapabilityRole, StructuralKind};
use crate::model::identity::{
    CapabilityId, CapabilityRef, ComponentInstanceId, ComponentTypeId, JointId, LinkId,
    MODULE_INSTANCE_SEPARATOR,
};
use crate::model::robot::KinematicKind;
use crate::model::structure::JointKind;

/// A canonical robot model that violates the runtime model's invariants.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    /// An identifier is not a normalized token.
    #[error("{kind} '{value}' is not normalized")]
    NotNormalized { kind: IdentifierKind, value: String },

    /// An identifier contains the separator reserved for flattening a
    /// component's structure into the robot's.
    #[error("{kind} '{value}' must not contain reserved separator '{MODULE_INSTANCE_SEPARATOR}'")]
    ReservedSeparator { kind: IdentifierKind, value: String },

    /// A capability reference is not written as `component.capability`.
    #[error("capability reference '{value}' must use component.capability")]
    MalformedCapabilityReference { value: String },

    /// A logical asset id is not a normalized relative forward-slash path.
    #[error("invalid logical asset id '{value}'")]
    MalformedAssetId { value: String },

    /// A reference names a capability the robot does not declare.
    #[error("unknown capability '{reference}'")]
    UnknownCapability { reference: CapabilityRef },

    /// A capability is not of the kind the caller requires.
    #[error("capability '{reference}' must reference a {expected}, found {actual}")]
    CapabilityKindMismatch {
        reference: CapabilityRef,
        expected: CapabilityKind,
        actual: CapabilityKind,
    },

    /// A capability does not target the kind of structural item the caller
    /// requires.
    #[error("capability '{reference}' must target a {expected}")]
    CapabilityTargetKind {
        reference: CapabilityRef,
        expected: StructuralKind,
    },

    /// A bound capability targets a structural item its component type does
    /// not define.
    #[error("{kind} target '{id}' for capability '{reference}' not found")]
    UnknownBoundTarget {
        reference: CapabilityRef,
        kind: StructuralKind,
        id: String,
    },

    /// A component type's capability targets a structural item that type does
    /// not define.
    #[error(
        "component type '{component_type}' capability '{capability_id}' references unknown {kind} '{id}'"
    )]
    UnknownDeclaredTarget {
        component_type: ComponentTypeId,
        capability_id: CapabilityId,
        kind: StructuralKind,
        id: String,
    },

    /// A component instance names a component type the model did not load.
    #[error("component '{instance}' references unknown component type '{component_type}'")]
    UnknownComponentType {
        instance: ComponentInstanceId,
        component_type: ComponentTypeId,
    },

    /// A component instance mounts on a link the robot structure lacks.
    #[error("component '{instance}' references unknown mount link '{link}'")]
    UnknownMountLink {
        instance: ComponentInstanceId,
        link: LinkId,
    },

    /// A direction sign is neither `-1` nor `1`.
    #[error("component '{instance}' capability '{capability_id}' direction sign must be -1 or 1")]
    DirectionSign {
        instance: ComponentInstanceId,
        capability_id: CapabilityId,
        value: i8,
    },

    /// A direction sign names a capability its component type does not declare.
    #[error(
        "component '{instance}' direction sign references unknown capability '{capability_id}'"
    )]
    UnknownDirectionSignCapability {
        instance: ComponentInstanceId,
        capability_id: CapabilityId,
    },

    /// A capability role assignment names a capability its component type does
    /// not declare.
    #[error(
        "component '{instance}' role assignment references unknown capability '{capability_id}'"
    )]
    UnknownRoleCapability {
        instance: ComponentInstanceId,
        capability_id: CapabilityId,
    },

    /// A persisted role key must name at least one role.
    ///
    /// The component instance is not named: this is raised while the instance is
    /// being decoded, before the map key that identifies it is in scope. serde
    /// prefixes the field path it was decoding, which names the instance.
    #[error("capability '{capability_id}' has an empty role assignment")]
    EmptyCapabilityRoles { capability_id: CapabilityId },

    /// A persisted role list may not repeat a role and rely on set coercion.
    #[error("capability '{capability_id}' repeats role '{role}'")]
    DuplicateCapabilityRole {
        capability_id: CapabilityId,
        role: CapabilityRole,
    },

    /// A simulated capability has no counterpart on the component type.
    #[error("simulation capability '{component_type}.{capability_id}' has no component capability")]
    SimulationWithoutCapability {
        component_type: ComponentTypeId,
        capability_id: CapabilityId,
    },

    /// A simulated capability models a different kind than the component
    /// declares, so the simulator would drive the wrong device.
    #[error(
        "simulation capability '{component_type}.{capability_id}' kind does not match component"
    )]
    SimulationCapabilityKindMismatch {
        component_type: ComponentTypeId,
        capability_id: CapabilityId,
        simulated: CapabilityKind,
        declared: CapabilityKind,
    },

    /// A joint uses a kind the runtime cannot drive.
    #[error("{owner} joint '{joint}' uses unsupported runtime kind '{kind:?}'")]
    UnsupportedJointKind {
        owner: JointOwner,
        joint: JointId,
        kind: JointKind,
    },

    /// A motion limit is not finite, positive, and representable as `f32`.
    ///
    /// The `f32` bound is not cosmetic: motion commands cross the bus as
    /// `f32`, so a limit that does not survive the narrowing is not a limit.
    #[error("motion {field} must be finite, positive, and fit in f32")]
    MotionLimit { field: MotionLimitField },

    /// A kinematic scalar is not finite and positive.
    #[error("{kinematics} {field} must be finite and positive")]
    KinematicScalar {
        kinematics: KinematicKind,
        field: KinematicScalarField,
    },

    /// Stock safety cannot conservatively account for a collision shape that
    /// is attached below a movable joint.
    #[error("stock safety footprint cannot include movable joint '{joint}'")]
    FootprintMovableJoint { joint: JointId },

    /// Mesh dimensions depend on asset contents and therefore cannot be
    /// compiled into the source-free stock safety envelope.
    #[error("stock safety footprint does not support mesh collision on link '{link}'")]
    FootprintMesh { link: LinkId },

    /// A footprint scalar must remain finite after all fixed-joint transforms.
    #[error("stock safety footprint contains a non-finite value")]
    FootprintNonFinite,

    /// A compiled footprint radius is invalid, which must never enter a
    /// bundle manifest or authorize motion.
    #[error("stock safety footprint radius must be finite and positive")]
    FootprintRadius,

    /// The canonical structure itself is invalid.
    #[error(transparent)]
    Structure(#[from] StructureError),
}

impl From<crate::identity::TopologyIdError> for ModelError {
    fn from(error: crate::identity::TopologyIdError) -> Self {
        use crate::identity::TopologyIdError;
        match error {
            TopologyIdError::Robot(value) => Self::NotNormalized {
                kind: IdentifierKind::RobotId,
                value,
            },
            TopologyIdError::ComponentInstance(value) => Self::NotNormalized {
                kind: IdentifierKind::ComponentInstance,
                value,
            },
            TopologyIdError::Service(value) => Self::NotNormalized {
                kind: IdentifierKind::Service,
                value,
            },
        }
    }
}

/// A canonical structure that violates the structural invariants.
#[derive(Debug, thiserror::Error)]
pub enum StructureError {
    /// Two structural items share one identity.
    #[error("duplicate {kind} identity '{name}'")]
    DuplicateIdentity { kind: StructuralKind, name: String },

    /// A joint attaches to a link the structure does not define.
    #[error("joint '{joint}' references unknown {role} link '{link}'")]
    UnknownJointLink {
        joint: JointId,
        role: LinkRole,
        link: LinkId,
    },

    /// A link has more than one parent joint, so it has no single pose.
    #[error("link '{link}' is the child of multiple joints")]
    MultipleParentJoints { link: LinkId },

    /// A joint attaches a link to itself.
    #[error("joint '{joint}' cannot use '{link}' as both parent and child")]
    SelfReferentialJoint { joint: JointId, link: LinkId },

    /// The link graph is not a single tree.
    #[error("structure must have exactly one root link, found {found}")]
    RootLinkCount { found: usize },

    /// The link graph contains a cycle, so no link has a defined world pose.
    #[error("structure contains a joint cycle involving '{link}'")]
    JointCycle { link: LinkId },

    /// A pose has a non-finite component.
    #[error("{owner} pose must be finite")]
    Pose { owner: PoseOwner },

    /// A link mass is not finite and non-negative.
    #[error("link '{link}' mass must be finite and non-negative")]
    Mass { link: LinkId },

    /// A link inertia tensor is not finite and positive semidefinite, so it
    /// does not describe a physically realizable body.
    #[error("link '{link}' inertia must be finite and positive semidefinite")]
    Inertia { link: LinkId },

    /// A geometry dimension is not finite and positive.
    #[error("link '{link}' geometry dimensions must be finite and positive")]
    Geometry { link: LinkId },

    /// A joint axis has a non-finite component.
    #[error("joint '{joint}' axis must be finite")]
    AxisNotFinite { joint: JointId },

    /// A movable joint has a zero-length axis, so its motion is undefined.
    #[error("joint '{joint}' axis must be non-zero")]
    AxisNotOriented { joint: JointId },

    /// A joint limit is not finite, or its bounds are inverted.
    #[error("joint '{joint}' limits must be finite with lower <= upper")]
    JointLimits { joint: JointId },

    /// Joint damping or friction is not finite and non-negative.
    #[error("joint '{joint}' dynamics must be finite and non-negative")]
    JointDynamics { joint: JointId },

    /// A joint mimics a joint the structure does not define.
    #[error("joint '{joint}' mimics unknown joint '{mimicked}'")]
    UnknownMimicJoint { joint: JointId, mimicked: JointId },

    /// A joint's software safety limits are not finite, or inverted.
    #[error("joint '{joint}' safety limits must be finite with lower <= upper")]
    JointSafety { joint: JointId },

    /// The robot's root link is not the conventional ground-projection frame.
    #[error("structure root link must be '{expected}', found '{found}'")]
    RootLinkName { expected: LinkId, found: LinkId },

    /// The robot structure has no `base_link` at all.
    #[error("structure must attach 'base_link' under 'base_footprint' with a fixed joint")]
    MissingBaseLink,

    /// The robot structure has a `base_link` that is not rigidly attached
    /// directly beneath the root.
    #[error("structure must attach 'base_link' directly under 'base_footprint' with a fixed joint")]
    MisattachedBaseLink,

    /// The canonical structure document does not match the canonical schema.
    #[error("canonical structure document is invalid: {0}")]
    Document(#[from] serde_json::Error),
}

/// What a rejected identifier was being used to name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentifierKind {
    RobotId,
    RobotLink,
    RobotJoint,
    ComponentType,
    ComponentInstance,
    Capability,
    Service,
}

impl fmt::Display for IdentifierKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::RobotId => "robot id",
            Self::RobotLink => "robot link",
            Self::RobotJoint => "robot joint",
            Self::ComponentType => "component type",
            Self::ComponentInstance => "component instance id",
            Self::Capability => "capability id",
            Self::Service => "service id",
        })
    }
}

/// Which structure a rejected joint belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JointOwner {
    /// The robot's own structure.
    Robot,
    /// One component type's structure.
    ComponentType(ComponentTypeId),
}

impl fmt::Display for JointOwner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Robot => formatter.write_str("robot"),
            Self::ComponentType(component_type) => write!(formatter, "{component_type}"),
        }
    }
}

/// Which end of a joint named a missing link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkRole {
    Parent,
    Child,
}

impl fmt::Display for LinkRole {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Parent => "parent",
            Self::Child => "child",
        })
    }
}

/// Which pose in the structure was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PoseOwner {
    LinkInertial(LinkId),
    LinkVisual(LinkId),
    LinkCollision(LinkId),
    Joint(JointId),
}

impl fmt::Display for PoseOwner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LinkInertial(link) => write!(formatter, "link '{link}' inertial"),
            Self::LinkVisual(link) => write!(formatter, "link '{link}' visual"),
            Self::LinkCollision(link) => write!(formatter, "link '{link}' collision"),
            Self::Joint(joint) => write!(formatter, "joint '{joint}'"),
        }
    }
}

/// Which motion limit was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MotionLimitField {
    MaxLinearSpeedMps,
    MaxAngularSpeedRadps,
}

impl fmt::Display for MotionLimitField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::MaxLinearSpeedMps => "max_linear_speed_mps",
            Self::MaxAngularSpeedRadps => "max_angular_speed_radps",
        })
    }
}

/// Which kinematic dimension was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KinematicScalarField {
    WheelRadiusM,
    WheelBaseM,
    TrackM,
    MaxSteeringAngleRad,
}

impl fmt::Display for KinematicScalarField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::WheelRadiusM => "wheel_radius_m",
            Self::WheelBaseM => "wheel_base_m",
            Self::TrackM => "track_m",
            Self::MaxSteeringAngleRad => "max_steering_angle_rad",
        })
    }
}
