//! Canonical simulation facts normalized from a versioned `simulation.yaml`.

use crate::model::source::simulation::v0 as source;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ActuatorType {
    #[default]
    Velocity,
    Position,
    Torque,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraProjection {
    Planar,
    Cylindrical,
    Spherical,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Motor {
    pub actuator_type: ActuatorType,
    pub acceleration_radps2: Option<f64>,
    pub control_pid: Option<Vec<f64>>,
    pub sampling_period_torque_hz: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Encoder {
    pub sampling_period_hz: f64,
    pub resolution: Option<f64>,
    pub noise: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Accelerometer {
    pub sampling_period_hz: f64,
    pub resolution: Option<f64>,
    pub lookup_table: Option<Vec<Vec<f64>>>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Gyroscope {
    pub sampling_period_hz: f64,
    pub resolution: Option<f64>,
    pub lookup_table: Option<Vec<Vec<f64>>>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Magnetometer {
    pub sampling_period_hz: f64,
    pub resolution: Option<f64>,
    pub lookup_table: Option<Vec<Vec<f64>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Imu {
    pub sampling_period_hz: f64,
    pub resolution: Option<f64>,
    pub noise: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Gnss {
    pub sampling_period_hz: f64,
    pub resolution: Option<f64>,
    pub accuracy: Option<f64>,
    pub noise_correlation: Option<f64>,
    pub speed_resolution: Option<f64>,
    pub speed_noise: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Default)]
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

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Depth {
    pub sampling_period_hz: f64,
    pub noise: Option<f64>,
    pub resolution: Option<f64>,
    pub motion_blur: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Range {
    pub sampling_period_hz: f64,
    pub noise: Option<f64>,
    pub resolution: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Lidar {
    pub sampling_period_hz: f64,
    pub noise: Option<f64>,
    pub resolution: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Mmwave {
    pub sampling_period_hz: f64,
    pub noise: Option<f64>,
    pub resolution: Option<f64>,
    pub lookup_table: Option<Vec<Vec<f64>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Microphone {
    pub sampling_period_hz: f64,
    pub aperture: Option<f64>,
}

impl From<source::capability::ActuatorType> for ActuatorType {
    fn from(value: source::capability::ActuatorType) -> Self {
        match value {
            source::capability::ActuatorType::Velocity => Self::Velocity,
            source::capability::ActuatorType::Position => Self::Position,
            source::capability::ActuatorType::Torque => Self::Torque,
        }
    }
}

impl From<source::capability::CameraProjection> for CameraProjection {
    fn from(value: source::capability::CameraProjection) -> Self {
        match value {
            source::capability::CameraProjection::Planar => Self::Planar,
            source::capability::CameraProjection::Cylindrical => Self::Cylindrical,
            source::capability::CameraProjection::Spherical => Self::Spherical,
        }
    }
}

macro_rules! copy_source {
    ($source:ty => $target:ident { $($field:ident),+ $(,)? }) => {
        impl From<$source> for $target {
            fn from(value: $source) -> Self {
                Self {
                    $($field: value.$field),+
                }
            }
        }
    };
}

copy_source!(source::capability::Encoder => Encoder {
    sampling_period_hz,
    resolution,
    noise,
});
copy_source!(source::capability::Accelerometer => Accelerometer {
    sampling_period_hz,
    resolution,
    lookup_table,
});
copy_source!(source::capability::Gyroscope => Gyroscope {
    sampling_period_hz,
    resolution,
    lookup_table,
});
copy_source!(source::capability::Magnetometer => Magnetometer {
    sampling_period_hz,
    resolution,
    lookup_table,
});
copy_source!(source::capability::Imu => Imu {
    sampling_period_hz,
    resolution,
    noise,
});
copy_source!(source::capability::Gnss => Gnss {
    sampling_period_hz,
    resolution,
    accuracy,
    noise_correlation,
    speed_resolution,
    speed_noise,
});
copy_source!(source::capability::Depth => Depth {
    sampling_period_hz,
    noise,
    resolution,
    motion_blur,
});
copy_source!(source::capability::Range => Range {
    sampling_period_hz,
    noise,
    resolution,
});
copy_source!(source::capability::Lidar => Lidar {
    sampling_period_hz,
    noise,
    resolution,
});
copy_source!(source::capability::Mmwave => Mmwave {
    sampling_period_hz,
    noise,
    resolution,
    lookup_table,
});
copy_source!(source::capability::Microphone => Microphone {
    sampling_period_hz,
    aperture,
});

impl From<source::capability::Motor> for Motor {
    fn from(value: source::capability::Motor) -> Self {
        Self {
            actuator_type: value.actuator_type.into(),
            acceleration_radps2: value.acceleration_radps2,
            control_pid: value.control_pid,
            sampling_period_torque_hz: value.sampling_period_torque_hz,
        }
    }
}

impl From<source::capability::Camera> for Camera {
    fn from(value: source::capability::Camera) -> Self {
        Self {
            sampling_period_hz: value.sampling_period_hz,
            projection: value.projection.map(Into::into),
            near: value.near,
            far: value.far,
            exposure: value.exposure,
            anti_aliasing: value.anti_aliasing,
            ambient_occlusion_radius: value.ambient_occlusion_radius,
            bloom_threshold: value.bloom_threshold,
            noise: value.noise,
            motion_blur: value.motion_blur,
            noise_mask_url: value.noise_mask_url,
        }
    }
}

impl From<source::capability::Capability> for Capability {
    fn from(value: source::capability::Capability) -> Self {
        match value {
            source::capability::Capability::Motor(config) => Self::Motor(config.into()),
            source::capability::Capability::Encoder(config) => Self::Encoder(config.into()),
            source::capability::Capability::Accelerometer(config) => {
                Self::Accelerometer(config.into())
            }
            source::capability::Capability::Gyroscope(config) => Self::Gyroscope(config.into()),
            source::capability::Capability::Magnetometer(config) => {
                Self::Magnetometer(config.into())
            }
            source::capability::Capability::Imu(config) => Self::Imu(config.into()),
            source::capability::Capability::Gnss(config) => Self::Gnss(config.into()),
            source::capability::Capability::Camera(config) => Self::Camera(config.into()),
            source::capability::Capability::Depth(config) => Self::Depth(config.into()),
            source::capability::Capability::Range(config) => Self::Range(config.into()),
            source::capability::Capability::Lidar(config) => Self::Lidar(config.into()),
            source::capability::Capability::Mmwave(config) => Self::Mmwave(config.into()),
            source::capability::Capability::Microphone(config) => Self::Microphone(config.into()),
            source::capability::Capability::Speaker => Self::Speaker,
            source::capability::Capability::Battery => Self::Battery,
            source::capability::Capability::Led => Self::Led,
            source::capability::Capability::EmergencyStop => Self::EmergencyStop,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Camera, CameraProjection};
    use crate::model::source::simulation::v0::capability as source;

    #[test]
    fn fully_populated_camera_source_converts_without_field_loss() {
        let canonical = Camera::from(source::Camera {
            sampling_period_hz: 30.0,
            projection: Some(source::CameraProjection::Cylindrical),
            near: Some(0.1),
            far: Some(50.0),
            exposure: Some(1.5),
            anti_aliasing: Some(true),
            ambient_occlusion_radius: Some(2.0),
            bloom_threshold: Some(0.8),
            noise: Some(0.01),
            motion_blur: Some(0.2),
            noise_mask_url: Some("mask.png".to_string()),
        });
        assert_eq!(
            canonical,
            Camera {
                sampling_period_hz: 30.0,
                projection: Some(CameraProjection::Cylindrical),
                near: Some(0.1),
                far: Some(50.0),
                exposure: Some(1.5),
                anti_aliasing: Some(true),
                ambient_occlusion_radius: Some(2.0),
                bloom_threshold: Some(0.8),
                noise: Some(0.01),
                motion_blur: Some(0.2),
                noise_mask_url: Some("mask.png".to_string()),
            }
        );
    }
}
