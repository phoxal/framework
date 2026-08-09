#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "SampleWire")]
pub struct Sample {
    pub linear_acceleration: [f32; 3],
}
#[derive(serde::Deserialize)]
struct SampleWire {
    linear_acceleration: [f32; 3],
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
    pub fn try_new(linear_acceleration: [f32; 3]) -> Result<Self, InvalidSample> {
        if !linear_acceleration.iter().all(|v| v.is_finite()) {
            return Err(InvalidSample("accelerometer values must be finite"));
        }
        Ok(Self {
            linear_acceleration,
        })
    }
}
impl TryFrom<SampleWire> for Sample {
    type Error = InvalidSample;
    fn try_from(v: SampleWire) -> Result<Self, Self::Error> {
        Self::try_new(v.linear_acceleration)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn constructor_rejects_nonfinite() {
        assert!(Sample::try_new([f32::NAN, 0.0, 0.0]).is_err());
    }
}

phoxal_macros::phoxal_api_fragment! {
    path component(instance) / accelerometer(capability);

    version v0_1;

    topic sample: Sample<Sample>;
}
