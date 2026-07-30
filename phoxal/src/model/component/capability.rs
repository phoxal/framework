//! Canonical component capabilities normalized from versioned source documents.

use crate::model::source::component::v0::capability as source;

#[derive(Debug, Clone, PartialEq)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderType {
    Incremental,
    Absolute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotorCommand {
    Position,
    Velocity,
    Torque,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StructuralTarget {
    Joint { id: String },
    Link { id: String },
}

pub const MODULE_INSTANCE_SEPARATOR: &str = "__";

impl StructuralTarget {
    #[must_use]
    pub fn namespaced(&self, component_id: &str) -> Self {
        match self {
            Self::Joint { id } => Self::Joint {
                id: format!("{component_id}{MODULE_INSTANCE_SEPARATOR}{id}"),
            },
            Self::Link { id } => Self::Link {
                id: format!("{component_id}{MODULE_INSTANCE_SEPARATOR}{id}"),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LidarOutput {
    Ranges,
    Points,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraMode {
    Mono,
    Rgb,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GnssCoordinateSystem {
    #[default]
    Local,
    Wgs84,
}

impl Capability {
    #[must_use]
    pub const fn kind_name(&self) -> &'static str {
        match self {
            Self::Motor(_) => "motor",
            Self::Encoder(_) => "encoder",
            Self::Accelerometer(_) => "accelerometer",
            Self::Gyroscope(_) => "gyroscope",
            Self::Magnetometer(_) => "magnetometer",
            Self::Imu(_) => "imu",
            Self::Gnss(_) => "gnss",
            Self::Camera(_) => "camera",
            Self::Depth(_) => "depth",
            Self::EmergencyStop(_) => "emergency_stop",
            Self::Range(_) => "range",
            Self::Lidar(_) => "lidar",
            Self::Mmwave(_) => "mmwave",
            Self::Microphone(_) => "microphone",
            Self::Speaker(_) => "speaker",
            Self::Battery(_) => "battery",
            Self::Led(_) => "led",
        }
    }

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

#[derive(Debug, Clone, PartialEq)]
pub struct Motor {
    pub target: StructuralTarget,
    pub command: MotorCommand,
    pub gear_ratio: f64,
    pub max_torque_nm: Option<f64>,
    pub max_velocity_radps: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Encoder {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    pub gear_ratio: f64,
    pub encoder_type: EncoderType,
    pub counts_per_revolution: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Accelerometer {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    pub axes: Option<[bool; 3]>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Gyroscope {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    pub axes: Option<[bool; 3]>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Magnetometer {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    pub axes: Option<[bool; 3]>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Imu {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    pub axes: Option<[bool; 3]>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Gnss {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    pub coordinate_system: GnssCoordinateSystem,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Camera {
    pub target: StructuralTarget,
    pub mode: CameraMode,
    pub publish_rate_hz: f64,
    pub width_px: u32,
    pub height_px: u32,
    pub field_of_view_rad: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Depth {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    pub width_px: u32,
    pub height_px: u32,
    pub field_of_view_rad: Option<f64>,
    pub min_range_m: Option<f64>,
    pub max_range_m: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Range {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    pub min_range_m: f64,
    pub max_range_m: f64,
    pub field_of_view_rad: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmergencyStop {
    pub target: StructuralTarget,
}

#[derive(Debug, Clone, PartialEq)]
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

#[derive(Debug, Clone, PartialEq)]
pub struct Mmwave {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Microphone {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Speaker {
    pub target: StructuralTarget,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Battery {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    pub voltage_v: f64,
    pub capacity_ah: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Led {
    pub target: StructuralTarget,
}

impl From<source::StructuralTarget> for StructuralTarget {
    fn from(value: source::StructuralTarget) -> Self {
        match value {
            source::StructuralTarget::Joint { id } => Self::Joint { id },
            source::StructuralTarget::Link { id } => Self::Link { id },
        }
    }
}

impl From<source::EncoderType> for EncoderType {
    fn from(value: source::EncoderType) -> Self {
        match value {
            source::EncoderType::Incremental => Self::Incremental,
            source::EncoderType::Absolute => Self::Absolute,
        }
    }
}

impl From<source::MotorCommand> for MotorCommand {
    fn from(value: source::MotorCommand) -> Self {
        match value {
            source::MotorCommand::Position => Self::Position,
            source::MotorCommand::Velocity => Self::Velocity,
            source::MotorCommand::Torque => Self::Torque,
        }
    }
}

impl From<source::LidarOutput> for LidarOutput {
    fn from(value: source::LidarOutput) -> Self {
        match value {
            source::LidarOutput::Ranges => Self::Ranges,
            source::LidarOutput::Points => Self::Points,
        }
    }
}

impl From<source::CameraMode> for CameraMode {
    fn from(value: source::CameraMode) -> Self {
        match value {
            source::CameraMode::Mono => Self::Mono,
            source::CameraMode::Rgb => Self::Rgb,
        }
    }
}

impl From<source::GnssCoordinateSystem> for GnssCoordinateSystem {
    fn from(value: source::GnssCoordinateSystem) -> Self {
        match value {
            source::GnssCoordinateSystem::Local => Self::Local,
            source::GnssCoordinateSystem::Wgs84 => Self::Wgs84,
        }
    }
}

macro_rules! convert_targeted {
    ($source:ty => $target:ident { $($field:ident),* $(,)? }) => {
        impl From<$source> for $target {
            fn from(value: $source) -> Self {
                Self {
                    target: value.target.into(),
                    $($field: value.$field),*
                }
            }
        }
    };
}

convert_targeted!(source::Accelerometer => Accelerometer { publish_rate_hz, axes });
convert_targeted!(source::Gyroscope => Gyroscope { publish_rate_hz, axes });
convert_targeted!(source::Magnetometer => Magnetometer { publish_rate_hz, axes });
convert_targeted!(source::Imu => Imu { publish_rate_hz, axes });
convert_targeted!(source::Range => Range {
    publish_rate_hz,
    min_range_m,
    max_range_m,
    field_of_view_rad,
});
convert_targeted!(source::EmergencyStop => EmergencyStop {});
convert_targeted!(source::Mmwave => Mmwave { publish_rate_hz });
convert_targeted!(source::Microphone => Microphone { publish_rate_hz });
convert_targeted!(source::Speaker => Speaker {});
convert_targeted!(source::Battery => Battery {
    publish_rate_hz,
    voltage_v,
    capacity_ah,
});
convert_targeted!(source::Led => Led {});

impl From<source::Motor> for Motor {
    fn from(value: source::Motor) -> Self {
        Self {
            target: value.target.into(),
            command: value.command.into(),
            gear_ratio: value.gear_ratio,
            max_torque_nm: value.max_torque_nm,
            max_velocity_radps: value.max_velocity_radps,
        }
    }
}

impl From<source::Encoder> for Encoder {
    fn from(value: source::Encoder) -> Self {
        Self {
            target: value.target.into(),
            publish_rate_hz: value.publish_rate_hz,
            gear_ratio: value.gear_ratio,
            encoder_type: value.encoder_type.into(),
            counts_per_revolution: value.counts_per_revolution,
        }
    }
}

impl From<source::Gnss> for Gnss {
    fn from(value: source::Gnss) -> Self {
        Self {
            target: value.target.into(),
            publish_rate_hz: value.publish_rate_hz,
            coordinate_system: value.coordinate_system.into(),
        }
    }
}

impl From<source::Camera> for Camera {
    fn from(value: source::Camera) -> Self {
        Self {
            target: value.target.into(),
            mode: value.mode.into(),
            publish_rate_hz: value.publish_rate_hz,
            width_px: value.width_px,
            height_px: value.height_px,
            field_of_view_rad: value.field_of_view_rad,
        }
    }
}

impl From<source::Depth> for Depth {
    fn from(value: source::Depth) -> Self {
        Self {
            target: value.target.into(),
            publish_rate_hz: value.publish_rate_hz,
            width_px: value.width_px,
            height_px: value.height_px,
            field_of_view_rad: value.field_of_view_rad,
            min_range_m: value.min_range_m,
            max_range_m: value.max_range_m,
        }
    }
}

impl From<source::Lidar> for Lidar {
    fn from(value: source::Lidar) -> Self {
        Self {
            target: value.target.into(),
            publish_rate_hz: value.publish_rate_hz,
            output: value.output.into(),
            min_range_m: value.min_range_m,
            max_range_m: value.max_range_m,
            horizontal_fov_rad: value.horizontal_fov_rad,
            horizontal_resolution_rad: value.horizontal_resolution_rad,
            vertical_fov_rad: value.vertical_fov_rad,
            vertical_resolution_rad: value.vertical_resolution_rad,
        }
    }
}

impl From<source::Capability> for Capability {
    fn from(value: source::Capability) -> Self {
        match value {
            source::Capability::Motor(config) => Self::Motor(config.into()),
            source::Capability::Encoder(config) => Self::Encoder(config.into()),
            source::Capability::Accelerometer(config) => Self::Accelerometer(config.into()),
            source::Capability::Gyroscope(config) => Self::Gyroscope(config.into()),
            source::Capability::Magnetometer(config) => Self::Magnetometer(config.into()),
            source::Capability::Imu(config) => Self::Imu(config.into()),
            source::Capability::Gnss(config) => Self::Gnss(config.into()),
            source::Capability::Camera(config) => Self::Camera(config.into()),
            source::Capability::Depth(config) => Self::Depth(config.into()),
            source::Capability::EmergencyStop(config) => Self::EmergencyStop(config.into()),
            source::Capability::Range(config) => Self::Range(config.into()),
            source::Capability::Lidar(config) => Self::Lidar(config.into()),
            source::Capability::Mmwave(config) => Self::Mmwave(config.into()),
            source::Capability::Microphone(config) => Self::Microphone(config.into()),
            source::Capability::Speaker(config) => Self::Speaker(config.into()),
            source::Capability::Battery(config) => Self::Battery(config.into()),
            source::Capability::Led(config) => Self::Led(config.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Camera, CameraMode, StructuralTarget};

    #[test]
    fn namespaces_structural_targets_with_component_instance_id() {
        assert_eq!(
            StructuralTarget::Joint {
                id: "motor_joint".to_string()
            }
            .namespaced("left_drive"),
            StructuralTarget::Joint {
                id: "left_drive__motor_joint".to_string()
            }
        );
    }

    #[test]
    fn fully_populated_camera_source_converts_without_field_loss() {
        use crate::model::source::component::v0::capability as source;

        let canonical = Camera::from(source::Camera {
            target: source::StructuralTarget::Link {
                id: "camera_link".to_string(),
            },
            mode: source::CameraMode::Rgb,
            publish_rate_hz: 29.97,
            width_px: 1920,
            height_px: 1080,
            field_of_view_rad: Some(1.2),
        });
        assert_eq!(
            canonical,
            Camera {
                target: StructuralTarget::Link {
                    id: "camera_link".to_string(),
                },
                mode: CameraMode::Rgb,
                publish_rate_hz: 29.97,
                width_px: 1920,
                height_px: 1080,
                field_of_view_rad: Some(1.2),
            }
        );
    }
}
