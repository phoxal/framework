use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Parameters {
    Motor(Motor),
    Encoder(Encoder),
    Accelerometer(Empty),
    Gyroscope(Empty),
    Magnetometer(Empty),
    Imu(Empty),
    Gnss(Empty),
    Camera(Empty),
    Depth(Empty),
    Range(Empty),
    Lidar(Empty),
    Mmwave(Empty),
    Microphone(Empty),
    Speaker(Empty),
    Battery(Empty),
    Led(Empty),
}

impl Parameters {
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
            Self::Speaker(_) => "speaker",
            Self::Battery(_) => "battery",
            Self::Led(_) => "led",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Motor {
    #[serde(default = "default_direction_sign")]
    pub direction_sign: i8,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Encoder {
    #[serde(default = "default_direction_sign")]
    pub direction_sign: i8,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Empty {}

const fn default_direction_sign() -> i8 {
    1
}
