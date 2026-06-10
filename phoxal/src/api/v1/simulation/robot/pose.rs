#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Pose {
    pub x_m: f64,
    pub y_m: f64,
    pub yaw_rad: f64,
}
