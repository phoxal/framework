use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EncoderType {
    Incremental,
    Absolute,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MotorCommand {
    Position,
    Velocity,
    Torque,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LidarOutput {
    Ranges,
    Points,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CameraMode {
    Mono,
    Rgb,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GnssCoordinateSystem {
    #[default]
    Local,
    Wgs84,
}

impl Capability {
    const fn default_encoder_type() -> EncoderType {
        EncoderType::Incremental
    }

    const fn default_counts_per_revolution() -> u32 {
        1024
    }

    const fn default_gear_ratio() -> f64 {
        1.0
    }

    #[must_use]
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Motor { .. } => "motor",
            Self::Encoder { .. } => "encoder",
            Self::Imu { .. } => "imu",
            Self::Accelerometer { .. } => "accelerometer",
            Self::Gyroscope { .. } => "gyroscope",
            Self::Magnetometer { .. } => "magnetometer",
            Self::Gnss { .. } => "gnss",
            Self::Camera { .. } => "camera",
            Self::Depth { .. } => "depth",
            Self::EmergencyStop { .. } => "emergency_stop",
            Self::Range { .. } => "range",
            Self::Lidar { .. } => "lidar",
            Self::Mmwave { .. } => "mmwave",
            Self::Microphone { .. } => "microphone",
            Self::Speaker { .. } => "speaker",
            Self::Battery { .. } => "battery",
            Self::Led { .. } => "led",
        }
    }

    #[must_use]
    pub fn target(&self) -> &StructuralTarget {
        match self {
            Self::Motor(nm) => &nm.target,
            Self::Encoder(nm) => &nm.target,
            Self::Accelerometer(nm) => &nm.target,
            Self::Gyroscope(nm) => &nm.target,
            Self::Magnetometer(nm) => &nm.target,
            Self::Imu(nm) => &nm.target,
            Self::Gnss(nm) => &nm.target,
            Self::Camera(nm) => &nm.target,
            Self::Depth(nm) => &nm.target,
            Self::EmergencyStop(nm) => &nm.target,
            Self::Range(nm) => &nm.target,
            Self::Lidar(nm) => &nm.target,
            Self::Mmwave(nm) => &nm.target,
            Self::Microphone(nm) => &nm.target,
            Self::Speaker(nm) => &nm.target,
            Self::Battery(nm) => &nm.target,
            Self::Led(nm) => &nm.target,
        }
    }

    pub fn target_mut(&mut self) -> &mut StructuralTarget {
        match self {
            Self::Motor(nm) => &mut nm.target,
            Self::Encoder(nm) => &mut nm.target,
            Self::Accelerometer(nm) => &mut nm.target,
            Self::Gyroscope(nm) => &mut nm.target,
            Self::Magnetometer(nm) => &mut nm.target,
            Self::Imu(nm) => &mut nm.target,
            Self::Gnss(nm) => &mut nm.target,
            Self::Camera(nm) => &mut nm.target,
            Self::Depth(nm) => &mut nm.target,
            Self::EmergencyStop(nm) => &mut nm.target,
            Self::Range(nm) => &mut nm.target,
            Self::Lidar(nm) => &mut nm.target,
            Self::Mmwave(nm) => &mut nm.target,
            Self::Microphone(nm) => &mut nm.target,
            Self::Speaker(nm) => &mut nm.target,
            Self::Battery(nm) => &mut nm.target,
            Self::Led(nm) => &mut nm.target,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Motor {
    pub target: StructuralTarget,
    pub command: MotorCommand,
    #[serde(default = "Capability::default_gear_ratio")]
    pub gear_ratio: f64,
    #[serde(default)]
    pub max_torque_nm: Option<f64>,
    #[serde(default)]
    pub max_velocity_radps: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Encoder {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    #[serde(default = "Capability::default_gear_ratio")]
    pub gear_ratio: f64,
    #[serde(default = "Capability::default_encoder_type")]
    pub encoder_type: EncoderType,
    #[serde(default = "Capability::default_counts_per_revolution")]
    pub counts_per_revolution: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Accelerometer {
    /// Publishes raw accelerometer samples in the sensor-local frame in m/s^2.
    ///
    /// This capability does not imply gravity compensation, zero-bias removal,
    /// or motion-state filtering. Small non-zero readings while stationary are
    /// valid unless a producer-specific contract says otherwise.
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    #[serde(default)]
    pub axes: Option<[bool; 3]>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Gyroscope {
    /// Publishes raw angular velocity samples in the sensor-local frame in rad/s.
    ///
    /// This capability does not imply zero-bias removal or rest-state filtering.
    /// Small non-zero readings while stationary are valid unless a producer-specific
    /// contract says otherwise.
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    #[serde(default)]
    pub axes: Option<[bool; 3]>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Magnetometer {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    #[serde(default)]
    pub axes: Option<[bool; 3]>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Imu {
    /// Publishes orientation samples in the sensor-local frame.
    ///
    /// Orientation is reported independently of the raw accelerometer and gyroscope
    /// streams; consumers must not assume those streams are de-biased, filtered, or
    /// fused to match this orientation estimate exactly.
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    #[serde(default)]
    pub axes: Option<[bool; 3]>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Gnss {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    #[serde(default)]
    pub coordinate_system: GnssCoordinateSystem,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Camera {
    pub target: StructuralTarget,
    pub mode: CameraMode,
    pub publish_rate_hz: f64,
    pub width_px: u32,
    pub height_px: u32,
    #[serde(default)]
    pub field_of_view_rad: Option<f64>,
}

/// Shared depth capability.
///
/// Contract:
/// - payload data stores unsigned 16-bit millimeter samples
/// - `width_px` and `height_px` are static metadata and are not repeated in
///   each payload
/// - published payloads contain complete grids with valid non-zero samples
/// - pixels represent forward-axis depth, not radial range
/// - columns increase to sensor-right and rows increase downward
///
/// Any simulation or hardware driver that publishes this capability should
/// follow that same geometry rule.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Depth {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    pub width_px: u32,
    pub height_px: u32,
    #[serde(default)]
    pub field_of_view_rad: Option<f64>,
    #[serde(default)]
    pub min_range_m: Option<f64>,
    #[serde(default)]
    pub max_range_m: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Range {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    pub min_range_m: f64,
    pub max_range_m: f64,
    pub field_of_view_rad: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct EmergencyStop {
    pub target: StructuralTarget,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Lidar {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    pub output: LidarOutput,
    #[serde(default)]
    pub min_range_m: Option<f64>,
    #[serde(default)]
    pub max_range_m: Option<f64>,
    #[serde(default)]
    pub horizontal_fov_rad: Option<f64>,
    #[serde(default)]
    pub horizontal_resolution_rad: Option<f64>,
    #[serde(default)]
    pub vertical_fov_rad: Option<f64>,
    #[serde(default)]
    pub vertical_resolution_rad: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Mmwave {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Microphone {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Speaker {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Battery {
    pub target: StructuralTarget,
    pub publish_rate_hz: f64,
    #[serde(default)]
    pub voltage_v: Option<f64>,
    #[serde(default)]
    pub capacity_ah: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Led {
    pub target: StructuralTarget,
}

#[cfg(test)]
mod tests {
    use super::{Capability, GnssCoordinateSystem, StructuralTarget};

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
        assert_eq!(
            StructuralTarget::Link {
                id: "sensor_link".to_string()
            }
            .namespaced("front_sensor"),
            StructuralTarget::Link {
                id: "front_sensor__sensor_link".to_string()
            }
        );
    }

    #[test]
    fn gnss_coordinate_system_defaults_to_local_in_source_schema() {
        let yaml = r#"
kind: gnss
publish_rate_hz: 10.0
target:
  kind: link
  id: sensor_link
"#;

        let capability: Capability = serde_yaml::from_str(yaml).expect("valid GNSS capability");
        let Capability::Gnss(gnss) = capability else {
            panic!("expected GNSS capability");
        };

        assert_eq!(gnss.coordinate_system, GnssCoordinateSystem::Local);
    }

    #[test]
    fn gnss_coordinate_system_accepts_wgs84_in_source_schema() {
        let yaml = r#"
kind: gnss
publish_rate_hz: 10.0
coordinate_system: wgs84
target:
  kind: link
  id: sensor_link
"#;

        let capability: Capability = serde_yaml::from_str(yaml).expect("valid GNSS capability");
        let Capability::Gnss(gnss) = capability else {
            panic!("expected GNSS capability");
        };

        assert_eq!(gnss.coordinate_system, GnssCoordinateSystem::Wgs84);
    }
}
