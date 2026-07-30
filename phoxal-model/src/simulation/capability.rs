//! Canonical simulation facts normalized from a versioned `simulation.yaml`.

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
    Range(Range),
    Lidar(Lidar),
    Mmwave(Mmwave),
    Microphone(Microphone),
    Speaker,
    Battery,
    Led,
    EmergencyStop,
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
            Self::Range(_) => "range",
            Self::Lidar(_) => "lidar",
            Self::Mmwave(_) => "mmwave",
            Self::Microphone(_) => "microphone",
            Self::Speaker => "speaker",
            Self::Battery => "battery",
            Self::Led => "led",
            Self::EmergencyStop => "emergency_stop",
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ActuatorType {
    #[default]
    Velocity,
    Position,
    Torque,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CameraProjection {
    Planar,
    Cylindrical,
    Spherical,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Motor {
    pub actuator_type: ActuatorType,
    pub acceleration_radps2: Option<f64>,
    pub control_pid: Option<Vec<f64>>,
    pub sampling_period_torque_hz: Option<f64>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Encoder {
    pub sampling_period_hz: f64,
    pub resolution: Option<f64>,
    pub noise: Option<f64>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Accelerometer {
    pub sampling_period_hz: f64,
    pub resolution: Option<f64>,
    pub lookup_table: Option<Vec<Vec<f64>>>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Gyroscope {
    pub sampling_period_hz: f64,
    pub resolution: Option<f64>,
    pub lookup_table: Option<Vec<Vec<f64>>>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Magnetometer {
    pub sampling_period_hz: f64,
    pub resolution: Option<f64>,
    pub lookup_table: Option<Vec<Vec<f64>>>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Imu {
    pub sampling_period_hz: f64,
    pub resolution: Option<f64>,
    pub noise: Option<f64>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Gnss {
    pub sampling_period_hz: f64,
    pub resolution: Option<f64>,
    pub accuracy: Option<f64>,
    pub noise_correlation: Option<f64>,
    pub speed_resolution: Option<f64>,
    pub speed_noise: Option<f64>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Camera {
    pub sampling_period_hz: f64,
    pub projection: Option<CameraProjection>,
    pub near: Option<f64>,
    pub far: Option<f64>,
    pub exposure: Option<f64>,
    pub anti_aliasing: Option<bool>,
    pub ambient_occlusion_radius: Option<f64>,
    pub bloom_threshold: Option<f64>,
    pub noise: Option<f64>,
    pub motion_blur: Option<f64>,
    pub noise_mask_url: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Depth {
    pub sampling_period_hz: f64,
    pub noise: Option<f64>,
    pub resolution: Option<f64>,
    pub motion_blur: Option<f64>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Range {
    pub sampling_period_hz: f64,
    pub noise: Option<f64>,
    pub resolution: Option<f64>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Lidar {
    pub sampling_period_hz: f64,
    pub noise: Option<f64>,
    pub resolution: Option<f64>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Mmwave {
    pub sampling_period_hz: f64,
    pub noise: Option<f64>,
    pub resolution: Option<f64>,
    pub lookup_table: Option<Vec<Vec<f64>>>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Microphone {
    pub sampling_period_hz: f64,
    pub aperture: Option<f64>,
}
