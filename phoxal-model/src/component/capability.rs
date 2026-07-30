//! Canonical component capabilities normalized from versioned source documents.

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
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

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EncoderType {
    Incremental,
    Absolute,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MotorCommand {
    Position,
    Velocity,
    Torque,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
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

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LidarOutput {
    Ranges,
    Points,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CameraMode {
    Mono,
    Rgb,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
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

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Motor {
    pub target: StructuralTarget,
    pub command: MotorCommand,
    pub gear_ratio: f64,
    pub max_torque_nm: Option<f64>,
    pub max_velocity_radps: Option<f64>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Encoder {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    pub gear_ratio: f64,
    pub encoder_type: EncoderType,
    pub counts_per_revolution: u32,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Accelerometer {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    pub axes: Option<[bool; 3]>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Gyroscope {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    pub axes: Option<[bool; 3]>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Magnetometer {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    pub axes: Option<[bool; 3]>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Imu {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    pub axes: Option<[bool; 3]>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Gnss {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    pub coordinate_system: GnssCoordinateSystem,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Camera {
    pub target: StructuralTarget,
    pub mode: CameraMode,
    pub publish_rate_hz: f64,
    pub width_px: u32,
    pub height_px: u32,
    pub field_of_view_rad: Option<f64>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
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

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Range {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    pub min_range_m: f64,
    pub max_range_m: f64,
    pub field_of_view_rad: f64,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EmergencyStop {
    pub target: StructuralTarget,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
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

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Mmwave {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Microphone {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Speaker {
    pub target: StructuralTarget,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Battery {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    pub voltage_v: f64,
    pub capacity_ah: f64,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Led {
    pub target: StructuralTarget,
}
