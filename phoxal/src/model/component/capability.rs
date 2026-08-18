//! Canonical component capabilities normalized from versioned source documents.

use std::fmt;

use crate::identity::{ComponentInstanceId, JointId, LinkId};

/// The canonical purpose assigned to a component capability by an authored
/// robot manifest.
///
/// Roles are source/runtime contract facts rather than service names. Keeping
/// the closed vocabulary in the canonical model means every reader of a
/// compiled manifest agrees on the same spelling and set of values.
#[derive(
    phoxal_macros::DescribeWire,
    serde::Serialize,
    serde::Deserialize,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CapabilityRole {
    Localization,
    Mapping,
    Traversability,
    Odometry,
    Perception,
    Safety,
}

impl CapabilityRole {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Localization => "localization",
            Self::Mapping => "mapping",
            Self::Traversability => "traversability",
            Self::Odometry => "odometry",
            Self::Perception => "perception",
            Self::Safety => "safety",
        }
    }
}

impl fmt::Display for CapabilityRole {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(
    phoxal_macros::DescribeWire, serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq,
)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Capability {
    Motor(Motor),
    Encoder(Encoder),
    Accelerometer(Accelerometer),
    Gyroscope(Gyroscope),
    Magnetometer(Magnetometer),
    Imu(Imu),
    Gnss(Gnss),
    Camera(Camera),
    Depth(Depth),
    EmergencyStop(EmergencyStop),
    Range(Range),
    Lidar(Lidar),
    Mmwave(Mmwave),
    Microphone(Microphone),
    Speaker(Speaker),
    Battery(Battery),
    Led(Led),
}

#[derive(
    phoxal_macros::DescribeWire,
    serde::Serialize,
    serde::Deserialize,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum EncoderType {
    Incremental,
    Absolute,
}

#[derive(
    phoxal_macros::DescribeWire,
    serde::Serialize,
    serde::Deserialize,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum MotorCommand {
    Position,
    Velocity,
    Torque,
}

/// The component-local structural item a capability is attached to.
#[derive(
    phoxal_macros::DescribeWire, serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq,
)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StructuralTarget {
    Joint { id: JointId },
    Link { id: LinkId },
}

/// Which kind of structural item a target names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StructuralKind {
    Link,
    Joint,
}

impl fmt::Display for StructuralKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Link => "link",
            Self::Joint => "joint",
        })
    }
}

impl StructuralTarget {
    /// Which kind of structural item this target names.
    #[must_use]
    pub const fn kind(&self) -> StructuralKind {
        match self {
            Self::Joint { .. } => StructuralKind::Joint,
            Self::Link { .. } => StructuralKind::Link,
        }
    }

    /// This target as it appears in the robot's flattened structure, under the
    /// instance that mounts the component.
    #[must_use]
    pub fn namespaced(&self, component_id: &ComponentInstanceId) -> Self {
        match self {
            Self::Joint { id } => Self::Joint {
                id: id.namespaced(component_id),
            },
            Self::Link { id } => Self::Link {
                id: id.namespaced(component_id),
            },
        }
    }
}

#[derive(
    phoxal_macros::DescribeWire,
    serde::Serialize,
    serde::Deserialize,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum LidarOutput {
    Ranges,
    Points,
}

#[derive(
    phoxal_macros::DescribeWire,
    serde::Serialize,
    serde::Deserialize,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CameraMode {
    Mono,
    Rgb,
}

#[derive(
    phoxal_macros::DescribeWire,
    serde::Serialize,
    serde::Deserialize,
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum GnssCoordinateSystem {
    #[default]
    Local,
    Wgs84,
}

/// The device kind a capability describes.
///
/// A component declares the kind and a simulation models it; the two must
/// agree, which is why the kind is one shared type rather than two parallel
/// vocabularies compared as strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CapabilityKind {
    Motor,
    Encoder,
    Accelerometer,
    Gyroscope,
    Magnetometer,
    Imu,
    Gnss,
    Camera,
    Depth,
    EmergencyStop,
    Range,
    Lidar,
    Mmwave,
    Microphone,
    Speaker,
    Battery,
    Led,
}

impl CapabilityKind {
    /// The canonical snake_case name, as it appears in authored documents.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Motor => "motor",
            Self::Encoder => "encoder",
            Self::Accelerometer => "accelerometer",
            Self::Gyroscope => "gyroscope",
            Self::Magnetometer => "magnetometer",
            Self::Imu => "imu",
            Self::Gnss => "gnss",
            Self::Camera => "camera",
            Self::Depth => "depth",
            Self::EmergencyStop => "emergency_stop",
            Self::Range => "range",
            Self::Lidar => "lidar",
            Self::Mmwave => "mmwave",
            Self::Microphone => "microphone",
            Self::Speaker => "speaker",
            Self::Battery => "battery",
            Self::Led => "led",
        }
    }
}

impl fmt::Display for CapabilityKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Capability {
    /// The device kind this capability describes.
    #[must_use]
    pub const fn kind(&self) -> CapabilityKind {
        match self {
            Self::Motor(_) => CapabilityKind::Motor,
            Self::Encoder(_) => CapabilityKind::Encoder,
            Self::Accelerometer(_) => CapabilityKind::Accelerometer,
            Self::Gyroscope(_) => CapabilityKind::Gyroscope,
            Self::Magnetometer(_) => CapabilityKind::Magnetometer,
            Self::Imu(_) => CapabilityKind::Imu,
            Self::Gnss(_) => CapabilityKind::Gnss,
            Self::Camera(_) => CapabilityKind::Camera,
            Self::Depth(_) => CapabilityKind::Depth,
            Self::EmergencyStop(_) => CapabilityKind::EmergencyStop,
            Self::Range(_) => CapabilityKind::Range,
            Self::Lidar(_) => CapabilityKind::Lidar,
            Self::Mmwave(_) => CapabilityKind::Mmwave,
            Self::Microphone(_) => CapabilityKind::Microphone,
            Self::Speaker(_) => CapabilityKind::Speaker,
            Self::Battery(_) => CapabilityKind::Battery,
            Self::Led(_) => CapabilityKind::Led,
        }
    }

    /// The structural item this capability is attached to.
    #[must_use]
    pub const fn target(&self) -> &StructuralTarget {
        match self {
            Self::Motor(value) => &value.target,
            Self::Encoder(value) => &value.target,
            Self::Accelerometer(value) => &value.target,
            Self::Gyroscope(value) => &value.target,
            Self::Magnetometer(value) => &value.target,
            Self::Imu(value) => &value.target,
            Self::Gnss(value) => &value.target,
            Self::Camera(value) => &value.target,
            Self::Depth(value) => &value.target,
            Self::EmergencyStop(value) => &value.target,
            Self::Range(value) => &value.target,
            Self::Lidar(value) => &value.target,
            Self::Mmwave(value) => &value.target,
            Self::Microphone(value) => &value.target,
            Self::Speaker(value) => &value.target,
            Self::Battery(value) => &value.target,
            Self::Led(value) => &value.target,
        }
    }
}

#[derive(
    phoxal_macros::DescribeWire, serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq,
)]
#[serde(deny_unknown_fields)]
pub struct Motor {
    pub target: StructuralTarget,
    pub command: MotorCommand,
    pub gear_ratio: f64,
    pub max_torque_nm: Option<f64>,
    pub max_velocity_radps: Option<f64>,
}

#[derive(
    phoxal_macros::DescribeWire, serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq,
)]
#[serde(deny_unknown_fields)]
pub struct Encoder {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    pub gear_ratio: f64,
    pub encoder_type: EncoderType,
    pub counts_per_revolution: u32,
}

#[derive(
    phoxal_macros::DescribeWire, serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq,
)]
#[serde(deny_unknown_fields)]
pub struct Accelerometer {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    pub axes: Option<[bool; 3]>,
}

#[derive(
    phoxal_macros::DescribeWire, serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq,
)]
#[serde(deny_unknown_fields)]
pub struct Gyroscope {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    pub axes: Option<[bool; 3]>,
}

#[derive(
    phoxal_macros::DescribeWire, serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq,
)]
#[serde(deny_unknown_fields)]
pub struct Magnetometer {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    pub axes: Option<[bool; 3]>,
}

#[derive(
    phoxal_macros::DescribeWire, serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq,
)]
#[serde(deny_unknown_fields)]
pub struct Imu {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    pub axes: Option<[bool; 3]>,
}

#[derive(
    phoxal_macros::DescribeWire, serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq,
)]
#[serde(deny_unknown_fields)]
pub struct Gnss {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    pub coordinate_system: GnssCoordinateSystem,
}

#[derive(
    phoxal_macros::DescribeWire, serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq,
)]
#[serde(deny_unknown_fields)]
pub struct Camera {
    pub target: StructuralTarget,
    pub mode: CameraMode,
    pub publish_rate_hz: f64,
    pub width_px: u32,
    pub height_px: u32,
    pub field_of_view_rad: Option<f64>,
}

#[derive(
    phoxal_macros::DescribeWire, serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq,
)]
#[serde(deny_unknown_fields)]
pub struct Depth {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    pub width_px: u32,
    pub height_px: u32,
    pub field_of_view_rad: Option<f64>,
    pub min_range_m: Option<f64>,
    pub max_range_m: Option<f64>,
}

#[derive(
    phoxal_macros::DescribeWire, serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq,
)]
#[serde(deny_unknown_fields)]
pub struct Range {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    pub min_range_m: f64,
    pub max_range_m: f64,
    pub field_of_view_rad: f64,
}

#[derive(
    phoxal_macros::DescribeWire, serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq,
)]
#[serde(deny_unknown_fields)]
pub struct EmergencyStop {
    pub target: StructuralTarget,
}

#[derive(
    phoxal_macros::DescribeWire, serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq,
)]
#[serde(deny_unknown_fields)]
pub struct Lidar {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    pub output: LidarOutput,
    pub min_range_m: Option<f64>,
    pub max_range_m: Option<f64>,
    pub horizontal_fov_rad: Option<f64>,
    pub horizontal_resolution_rad: Option<f64>,
    pub vertical_fov_rad: Option<f64>,
    pub vertical_resolution_rad: Option<f64>,
}

#[derive(
    phoxal_macros::DescribeWire, serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq,
)]
#[serde(deny_unknown_fields)]
pub struct Mmwave {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
}

#[derive(
    phoxal_macros::DescribeWire, serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq,
)]
#[serde(deny_unknown_fields)]
pub struct Microphone {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
}

#[derive(
    phoxal_macros::DescribeWire, serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq,
)]
#[serde(deny_unknown_fields)]
pub struct Speaker {
    pub target: StructuralTarget,
}

#[derive(
    phoxal_macros::DescribeWire, serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq,
)]
#[serde(deny_unknown_fields)]
pub struct Battery {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    pub voltage_v: f64,
    pub capacity_ah: f64,
}

#[derive(
    phoxal_macros::DescribeWire, serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq,
)]
#[serde(deny_unknown_fields)]
pub struct Led {
    pub target: StructuralTarget,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::ComponentInstanceId;

    #[test]
    fn a_structural_target_is_a_tagged_id_on_the_wire() {
        // Typing the id must not change the serialized shape: it is still a
        // `kind` tag beside a bare string.
        for (json, target) in [
            (
                r#"{"kind":"joint","id":"axle"}"#,
                StructuralTarget::Joint {
                    id: JointId::new("axle"),
                },
            ),
            (
                r#"{"kind":"link","id":"body"}"#,
                StructuralTarget::Link {
                    id: LinkId::new("body"),
                },
            ),
        ] {
            assert_eq!(serde_json::to_string(&target).unwrap(), json);
            assert_eq!(
                serde_json::from_str::<StructuralTarget>(json).unwrap(),
                target
            );
        }
    }

    #[test]
    fn namespacing_a_target_keeps_its_kind() {
        let instance = ComponentInstanceId::new("left_drive").unwrap();
        let target = StructuralTarget::Joint {
            id: JointId::new("axle"),
        };
        assert_eq!(
            target.namespaced(&instance),
            StructuralTarget::Joint {
                id: JointId::new("left_drive__axle"),
            }
        );
        assert_eq!(target.namespaced(&instance).kind(), StructuralKind::Joint);
    }

    #[test]
    fn a_capability_reports_the_kind_it_is() {
        let motor = Capability::Motor(Motor {
            target: StructuralTarget::Link {
                id: LinkId::new("body"),
            },
            command: MotorCommand::Velocity,
            gear_ratio: 1.0,
            max_torque_nm: None,
            max_velocity_radps: None,
        });
        assert_eq!(motor.kind(), CapabilityKind::Motor);
        assert_eq!(motor.kind().as_str(), "motor");
    }
}
