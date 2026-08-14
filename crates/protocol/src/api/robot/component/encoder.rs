use std::time::Duration;
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(try_from = "SampleWire")]
pub struct Sample {
    pub position_rad: f64,
    pub velocity_radps: f32,
}
#[derive(serde::Deserialize)]
struct SampleWire {
    position_rad: f64,
    velocity_radps: f32,
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
    pub const STALE_AFTER: Duration = Duration::from_millis(200);
    pub fn try_new(position_rad: f64, velocity_radps: f32) -> Result<Self, InvalidSample> {
        if !(position_rad.is_finite() && velocity_radps.is_finite()) {
            return Err(InvalidSample("encoder values must be finite"));
        }
        Ok(Self {
            position_rad,
            velocity_radps,
        })
    }
}
impl TryFrom<SampleWire> for Sample {
    type Error = InvalidSample;
    fn try_from(v: SampleWire) -> Result<Self, Self::Error> {
        Self::try_new(v.position_rad, v.velocity_radps)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn constructor_rejects_nonfinite() {
        assert!(Sample::try_new(f64::NAN, 0.0).is_err());
        assert!(Sample::try_new(0.0, f32::NAN).is_err());
    }
}

phoxal_macros::protocol_fragment! {
    path robot / component(instance) / encoder(capability);

    sample: Sample<Sample>;
}
