#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Sample {
    pub latitude_deg: f64,
    pub longitude_deg: f64,
    pub altitude_m: f64,
}
