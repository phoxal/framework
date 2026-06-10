#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Target {
    pub linear_x_mps: f64,
    pub angular_z_radps: f64,
}
