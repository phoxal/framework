use serde::{Deserialize, Serialize};

/// Simulation-specific parameters for every component defined in the architecture.
/// These extend the physical (URDF) and component/model parameters with simulator-specific noise, resolutions, and properties.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

impl Capability {
    #[must_use]
    pub fn kind_name(&self) -> &'static str {
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
        }
    }

    pub fn validate(&self, field: &str, errors: &mut Vec<String>) {
        match self {
            Self::Motor(config) => {
                validate_optional_finite(
                    config.acceleration_radps2,
                    field,
                    "acceleration_radps2",
                    errors,
                );
                validate_optional_positive(
                    config.sampling_period_torque_hz,
                    field,
                    "sampling_period_torque_hz",
                    errors,
                );
                if let Some(pid) = &config.control_pid {
                    if pid.len() != 3 {
                        errors.push(format!("{field}.control_pid must contain exactly 3 terms"));
                    }
                    for value in pid {
                        if !value.is_finite() {
                            errors.push(format!("{field}.control_pid values must be finite"));
                            break;
                        }
                    }
                }
            }
            Self::Encoder(config) => {
                validate_sampling(config.sampling_period_hz, field, errors);
                validate_optional_finite(config.resolution, field, "resolution", errors);
                validate_optional_finite(config.noise, field, "noise", errors);
            }
            Self::Accelerometer(config) => {
                validate_sampling(config.sampling_period_hz, field, errors);
                validate_optional_finite(config.resolution, field, "resolution", errors);
                validate_table(
                    config.lookup_table.as_deref(),
                    field,
                    "lookup_table",
                    errors,
                );
            }
            Self::Gyroscope(config) => {
                validate_sampling(config.sampling_period_hz, field, errors);
                validate_optional_finite(config.resolution, field, "resolution", errors);
                validate_table(
                    config.lookup_table.as_deref(),
                    field,
                    "lookup_table",
                    errors,
                );
            }
            Self::Magnetometer(config) => {
                validate_sampling(config.sampling_period_hz, field, errors);
                validate_optional_finite(config.resolution, field, "resolution", errors);
                validate_table(
                    config.lookup_table.as_deref(),
                    field,
                    "lookup_table",
                    errors,
                );
            }
            Self::Imu(config) => {
                validate_sampling(config.sampling_period_hz, field, errors);
                validate_optional_finite(config.resolution, field, "resolution", errors);
                validate_optional_finite(config.noise, field, "noise", errors);
            }
            Self::Gnss(config) => {
                validate_sampling(config.sampling_period_hz, field, errors);
                validate_optional_finite(config.resolution, field, "resolution", errors);
                validate_optional_finite(config.accuracy, field, "accuracy", errors);
                validate_optional_finite(
                    config.noise_correlation,
                    field,
                    "noise_correlation",
                    errors,
                );
                validate_optional_finite(
                    config.speed_resolution,
                    field,
                    "speed_resolution",
                    errors,
                );
                validate_optional_finite(config.speed_noise, field, "speed_noise", errors);
            }
            Self::Camera(config) => {
                validate_sampling(config.sampling_period_hz, field, errors);
                validate_optional_finite(config.near, field, "near", errors);
                validate_optional_finite(config.far, field, "far", errors);
                validate_optional_finite(config.exposure, field, "exposure", errors);
                validate_optional_finite(
                    config.ambient_occlusion_radius,
                    field,
                    "ambient_occlusion_radius",
                    errors,
                );
                validate_optional_finite(config.bloom_threshold, field, "bloom_threshold", errors);
                validate_optional_finite(config.noise, field, "noise", errors);
                validate_optional_finite(config.motion_blur, field, "motion_blur", errors);
            }
            Self::Depth(config) => {
                validate_sampling(config.sampling_period_hz, field, errors);
                validate_optional_finite(config.noise, field, "noise", errors);
                validate_optional_finite(config.resolution, field, "resolution", errors);
                validate_optional_finite(config.motion_blur, field, "motion_blur", errors);
            }
            Self::Range(config) => {
                validate_sampling(config.sampling_period_hz, field, errors);
                validate_optional_finite(config.noise, field, "noise", errors);
                validate_optional_finite(config.resolution, field, "resolution", errors);
            }
            Self::Lidar(config) => {
                validate_sampling(config.sampling_period_hz, field, errors);
                validate_optional_finite(config.noise, field, "noise", errors);
                validate_optional_finite(config.resolution, field, "resolution", errors);
            }
            Self::Mmwave(config) => {
                validate_sampling(config.sampling_period_hz, field, errors);
                validate_optional_finite(config.noise, field, "noise", errors);
                validate_optional_finite(config.resolution, field, "resolution", errors);
                validate_table(
                    config.lookup_table.as_deref(),
                    field,
                    "lookup_table",
                    errors,
                );
            }
            Self::Microphone(config) => {
                validate_sampling(config.sampling_period_hz, field, errors);
                validate_optional_finite(config.aperture, field, "aperture", errors);
            }
            Self::Speaker | Self::Battery | Self::Led => {}
        }
    }
}

fn validate_sampling(value: f64, field: &str, errors: &mut Vec<String>) {
    if !value.is_finite() || value <= f64::EPSILON {
        errors.push(format!("{field}.sampling_period_hz must be finite and > 0"));
    }
}

fn validate_optional_finite(value: Option<f64>, field: &str, name: &str, errors: &mut Vec<String>) {
    if let Some(value) = value
        && !value.is_finite()
    {
        errors.push(format!("{field}.{name} must be finite"));
    }
}

fn validate_optional_positive(
    value: Option<f64>,
    field: &str,
    name: &str,
    errors: &mut Vec<String>,
) {
    if let Some(value) = value
        && (!value.is_finite() || value <= f64::EPSILON)
    {
        errors.push(format!("{field}.{name} must be finite and > 0"));
    }
}

fn validate_table(table: Option<&[Vec<f64>]>, field: &str, name: &str, errors: &mut Vec<String>) {
    let Some(table) = table else {
        return;
    };
    for row in table {
        if row.iter().any(|value| !value.is_finite()) {
            errors.push(format!("{field}.{name} values must be finite"));
            return;
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ActuatorType {
    #[default]
    Velocity,
    Position,
    Torque,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum CameraProjection {
    Planar,
    Cylindrical,
    Spherical,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Motor {
    #[serde(default)]
    pub actuator_type: ActuatorType,
    #[serde(default)]
    pub acceleration_radps2: Option<f64>,
    #[serde(default)]
    pub control_pid: Option<Vec<f64>>,
    #[serde(default)]
    pub sampling_period_torque_hz: Option<f64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Encoder {
    pub sampling_period_hz: f64,
    #[serde(default)]
    pub resolution: Option<f64>,
    #[serde(default)]
    pub noise: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Accelerometer {
    pub sampling_period_hz: f64,
    #[serde(default)]
    pub resolution: Option<f64>,
    #[serde(default)]
    pub lookup_table: Option<Vec<Vec<f64>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Gyroscope {
    pub sampling_period_hz: f64,
    #[serde(default)]
    pub resolution: Option<f64>,
    #[serde(default)]
    pub lookup_table: Option<Vec<Vec<f64>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Magnetometer {
    pub sampling_period_hz: f64,
    #[serde(default)]
    pub resolution: Option<f64>,
    #[serde(default)]
    pub lookup_table: Option<Vec<Vec<f64>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Imu {
    pub sampling_period_hz: f64,
    #[serde(default)]
    pub resolution: Option<f64>,
    #[serde(default)]
    pub noise: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Gnss {
    pub sampling_period_hz: f64,
    #[serde(default)]
    pub resolution: Option<f64>,
    #[serde(default)]
    pub accuracy: Option<f64>,
    #[serde(default)]
    pub noise_correlation: Option<f64>,
    #[serde(default)]
    pub speed_resolution: Option<f64>,
    #[serde(default)]
    pub speed_noise: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Camera {
    pub sampling_period_hz: f64,
    #[serde(default)]
    pub projection: Option<CameraProjection>,
    #[serde(default)]
    pub near: Option<f64>,
    #[serde(default)]
    pub far: Option<f64>,
    #[serde(default)]
    pub exposure: Option<f64>,
    #[serde(default)]
    pub anti_aliasing: Option<bool>,
    #[serde(default)]
    pub ambient_occlusion_radius: Option<f64>,
    #[serde(default)]
    pub bloom_threshold: Option<f64>,
    #[serde(default)]
    pub noise: Option<f64>,
    #[serde(default)]
    pub motion_blur: Option<f64>,
    #[serde(default)]
    pub noise_mask_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Depth {
    pub sampling_period_hz: f64,
    #[serde(default)]
    pub noise: Option<f64>,
    #[serde(default)]
    pub resolution: Option<f64>,
    #[serde(default)]
    pub motion_blur: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Range {
    pub sampling_period_hz: f64,
    #[serde(default)]
    pub noise: Option<f64>,
    #[serde(default)]
    pub resolution: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Lidar {
    pub sampling_period_hz: f64,
    #[serde(default)]
    pub noise: Option<f64>,
    #[serde(default)]
    pub resolution: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Mmwave {
    pub sampling_period_hz: f64,
    #[serde(default)]
    pub noise: Option<f64>,
    #[serde(default)]
    pub resolution: Option<f64>,
    #[serde(default)]
    pub lookup_table: Option<Vec<Vec<f64>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Microphone {
    pub sampling_period_hz: f64,
    #[serde(default)]
    pub aperture: Option<f64>,
}
