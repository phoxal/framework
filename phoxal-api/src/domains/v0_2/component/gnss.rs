//! Checked v0.2 GNSS fixes.

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "SampleWire")]
pub struct Sample {
    pub latitude: f64,
    pub longitude: f64,
    pub altitude: f64,
    pub position_covariance: [f64; 9],
}
#[derive(serde::Deserialize)]
struct SampleWire {
    latitude: f64,
    longitude: f64,
    altitude: f64,
    position_covariance: [f64; 9],
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidSample(&'static str);
impl std::fmt::Display for InvalidSample {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for InvalidSample {}
impl Sample {
    pub fn try_new(
        latitude: f64,
        longitude: f64,
        altitude: f64,
        position_covariance: [f64; 9],
    ) -> Result<Self, InvalidSample> {
        if !(latitude.is_finite()
            && (-90.0..=90.0).contains(&latitude)
            && longitude.is_finite()
            && (-180.0..=180.0).contains(&longitude)
            && altitude.is_finite()
            && position_covariance
                .iter()
                .all(|v| v.is_finite() && *v >= 0.0))
        {
            return Err(InvalidSample(
                "GNSS position must be finite and bounded; covariance must be finite and nonnegative",
            ));
        }
        Ok(Self {
            latitude,
            longitude,
            altitude,
            position_covariance,
        })
    }
}
impl TryFrom<SampleWire> for Sample {
    type Error = InvalidSample;
    fn try_from(v: SampleWire) -> Result<Self, Self::Error> {
        Self::try_new(v.latitude, v.longitude, v.altitude, v.position_covariance)
    }
}
