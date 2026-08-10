#[derive(Copy, Eq, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensorHealth {
    Nominal,
    Degraded,
    Fault,
}
#[derive(Copy, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Bias {
    pub angular_velocity_radps: [f32; 3],
    pub linear_acceleration_mps2: [f32; 3],
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "SampleWire")]
pub struct Sample {
    pub orientation: Option<[f32; 4]>,
    pub angular_velocity_radps: [f32; 3],
    pub linear_acceleration_mps2: [f32; 3],
    pub covariance: Option<[f32; 9]>,
    pub noise_density: Option<[f32; 3]>,
    pub sensor_frame_id: Option<String>,
    pub health: SensorHealth,
    pub bias: Option<Bias>,
}
#[derive(serde::Deserialize)]
struct SampleWire {
    orientation: Option<[f32; 4]>,
    angular_velocity_radps: [f32; 3],
    linear_acceleration_mps2: [f32; 3],
    covariance: Option<[f32; 9]>,
    noise_density: Option<[f32; 3]>,
    sensor_frame_id: Option<String>,
    health: SensorHealth,
    bias: Option<Bias>,
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
    #[expect(
        clippy::too_many_arguments,
        reason = "the validating constructor receives the complete wire sample atomically"
    )]
    pub fn try_new(
        orientation: Option<[f32; 4]>,
        angular_velocity_radps: [f32; 3],
        linear_acceleration_mps2: [f32; 3],
        covariance: Option<[f32; 9]>,
        noise_density: Option<[f32; 3]>,
        sensor_frame_id: Option<String>,
        health: SensorHealth,
        bias: Option<Bias>,
    ) -> Result<Self, InvalidSample> {
        if !angular_velocity_radps
            .iter()
            .chain(linear_acceleration_mps2.iter())
            .all(|v| v.is_finite())
            || !covariance
                .iter()
                .flatten()
                .all(|v| v.is_finite() && *v >= 0.0)
            || !noise_density
                .iter()
                .flatten()
                .all(|v| v.is_finite() && *v >= 0.0)
            || bias.is_some_and(|b| {
                !b.angular_velocity_radps
                    .iter()
                    .chain(b.linear_acceleration_mps2.iter())
                    .all(|v| v.is_finite())
            })
        {
            return Err(InvalidSample(
                "IMU vectors, covariance, noise, and bias must be finite; covariance and noise nonnegative",
            ));
        }
        if let Some(q) = orientation {
            let n2: f32 = q.iter().map(|x| x * x).sum();
            if !q.iter().all(|v| v.is_finite()) || (n2.sqrt() - 1.0).abs() > 1e-3 {
                return Err(InvalidSample(
                    "IMU orientation must be a unit finite quaternion",
                ));
            }
        }
        if sensor_frame_id
            .as_ref()
            .is_some_and(|id| id.trim().is_empty())
        {
            return Err(InvalidSample("IMU frame id must be nonempty"));
        }
        Ok(Self {
            orientation,
            angular_velocity_radps,
            linear_acceleration_mps2,
            covariance,
            noise_density,
            sensor_frame_id,
            health,
            bias,
        })
    }
}
impl TryFrom<SampleWire> for Sample {
    type Error = InvalidSample;
    fn try_from(v: SampleWire) -> Result<Self, Self::Error> {
        Self::try_new(
            v.orientation,
            v.angular_velocity_radps,
            v.linear_acceleration_mps2,
            v.covariance,
            v.noise_density,
            v.sensor_frame_id,
            v.health,
            v.bias,
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn constructor_requires_unit_quaternion() {
        assert!(
            Sample::try_new(
                Some([2.0, 0.0, 0.0, 0.0]),
                [0.0; 3],
                [0.0; 3],
                None,
                None,
                None,
                SensorHealth::Nominal,
                None
            )
            .is_err()
        );
    }
}

phoxal_macros::phoxal_api_fragment! {
    path component(instance) / imu(capability);

    version v0_1;

    topic sample: Sample<Sample>;
}
