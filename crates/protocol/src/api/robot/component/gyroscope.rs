#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(try_from = "SampleWire")]
pub struct Sample {
    pub angular_velocity: [f32; 3],
}
#[derive(serde::Deserialize)]
struct SampleWire {
    angular_velocity: [f32; 3],
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
    pub fn try_new(angular_velocity: [f32; 3]) -> Result<Self, InvalidSample> {
        if !angular_velocity.iter().all(|v| v.is_finite()) {
            return Err(InvalidSample("gyroscope values must be finite"));
        }
        Ok(Self { angular_velocity })
    }
}
impl TryFrom<SampleWire> for Sample {
    type Error = InvalidSample;
    fn try_from(v: SampleWire) -> Result<Self, Self::Error> {
        Self::try_new(v.angular_velocity)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn constructor_rejects_nonfinite() {
        assert!(Sample::try_new([f32::INFINITY, 0.0, 0.0]).is_err());
    }
}

phoxal_macros::protocol_fragment! {
    path robot / component(instance) / gyroscope(capability);

    topic sample: Sample<Sample>;
}
