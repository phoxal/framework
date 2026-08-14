#[derive(
    phoxal_macros::DescribeWire,
    Copy,
    Eq,
    Clone,
    Debug,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SensorHealth {
    Nominal,
    Degraded,
    Fault,
}
#[derive(
    phoxal_macros::DescribeWire, Copy, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub struct Limits {
    pub min_m: f32,
    pub max_m: f32,
}
#[derive(
    phoxal_macros::DescribeWire, Copy, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub struct SampleQuality {
    pub valid: bool,
    pub confidence: Option<f32>,
}
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(try_from = "SampleWire")]
pub struct Sample {
    pub distance_m: f32,
    pub limits: Option<Limits>,
    pub quality: Option<SampleQuality>,
    pub health: SensorHealth,
}
#[derive(serde::Deserialize)]
struct SampleWire {
    distance_m: f32,
    limits: Option<Limits>,
    quality: Option<SampleQuality>,
    health: SensorHealth,
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
        distance_m: f32,
        limits: Option<Limits>,
        quality: Option<SampleQuality>,
        health: SensorHealth,
    ) -> Result<Self, InvalidSample> {
        if !distance_m.is_finite() || distance_m < 0.0 {
            return Err(InvalidSample(
                "range distance must be finite and nonnegative",
            ));
        }
        if let Some(l) = limits
            && !(l.min_m.is_finite()
                && l.max_m.is_finite()
                && l.min_m >= 0.0
                && l.min_m <= l.max_m
                && distance_m >= l.min_m
                && distance_m <= l.max_m)
        {
            return Err(InvalidSample(
                "range limits must be finite, ordered, and contain distance",
            ));
        }
        if !quality
            .and_then(|q| q.confidence)
            .is_none_or(|c| c.is_finite() && (0.0..=1.0).contains(&c))
        {
            return Err(InvalidSample(
                "range confidence must be finite and in [0, 1]",
            ));
        }
        Ok(Self {
            distance_m,
            limits,
            quality,
            health,
        })
    }
}
impl TryFrom<SampleWire> for Sample {
    type Error = InvalidSample;
    fn try_from(v: SampleWire) -> Result<Self, Self::Error> {
        Self::try_new(v.distance_m, v.limits, v.quality, v.health)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn constructor_requires_ordered_containing_limits() {
        assert!(
            Sample::try_new(
                2.0,
                Some(Limits {
                    min_m: 3.0,
                    max_m: 1.0
                }),
                None,
                SensorHealth::Nominal
            )
            .is_err()
        );
        assert!(
            Sample::try_new(
                2.0,
                Some(Limits {
                    min_m: 0.0,
                    max_m: 1.0
                }),
                None,
                SensorHealth::Nominal
            )
            .is_err()
        );
    }
}

phoxal_macros::protocol_fragment! {
    path robot / component(instance) / range(capability);

    topic sample: Sample<Sample>;
}
