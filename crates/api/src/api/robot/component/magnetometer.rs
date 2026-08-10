#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "SampleWire")]
pub struct Sample {
    pub magnetic_field: [f32; 3],
}
#[derive(serde::Deserialize)]
struct SampleWire {
    magnetic_field: [f32; 3],
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
    pub fn try_new(magnetic_field: [f32; 3]) -> Result<Self, InvalidSample> {
        if !magnetic_field.iter().all(|v| v.is_finite()) {
            return Err(InvalidSample("magnetometer values must be finite"));
        }
        Ok(Self { magnetic_field })
    }
}
impl TryFrom<SampleWire> for Sample {
    type Error = InvalidSample;
    fn try_from(v: SampleWire) -> Result<Self, Self::Error> {
        Self::try_new(v.magnetic_field)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn constructor_rejects_nonfinite() {
        assert!(Sample::try_new([f32::NEG_INFINITY, 0.0, 0.0]).is_err());
    }
}

phoxal_macros::phoxal_api_fragment! {
    path robot / component(instance) / magnetometer(capability);

    topic sample: Sample<Sample>;
}
