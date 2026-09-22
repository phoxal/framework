use crate::config::OakDLiteConfig;
use crate::outputs::OakDLiteOutputs;
use anyhow::Result;
use anyhow::anyhow;
use phoxal::runtime::InitContext;
use phoxal::runtime::Runtime;
use phoxal::runtime::StepContext;
#[cfg(test)]
use phoxal_component_oak_d_lite::FILE_DESCRIPTOR_SET;
#[cfg(test)]
use phoxal_component_oak_d_lite::oak_d_lite;

const BACKEND_UNAVAILABLE: &str = "oak_d_lite hardware backend unavailable: refusing to publish fabricated camera, depth, or IMU measurements";

/// Driver state retained by the Runtime owner.
#[derive(Debug)]
pub struct OakDLiteState;

/// The OAK-D Lite hardware component driver.
pub struct OakDLite;

#[phoxal::runtime::outputs]
impl OakDLite {}

/// The hardware backend is intentionally unavailable until a real DepthAI
/// transport can publish camera, depth, and IMU observations.
#[phoxal::runtime(period_ms = 10, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for OakDLite {
    type Config = OakDLiteConfig;
    type State = OakDLiteState;
    type Inputs = ();
    type Outputs = OakDLiteOutputs;

    fn init(&self, _ctx: &InitContext, _config: Self::Config) -> Result<Self::State> {
        Err(anyhow!(BACKEND_UNAVAILABLE))
    }

    fn step(
        &self,
        _ctx: &StepContext,
        _state: Self::State,
        _inputs: &Self::Inputs,
    ) -> Result<(Self::State, Self::Outputs)> {
        Err(anyhow!(BACKEND_UNAVAILABLE))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BACKEND_UNAVAILABLE, FILE_DESCRIPTOR_SET, OakDLite, OakDLiteConfig, OakDLiteOutputs,
        oak_d_lite,
    };
    use phoxal::contract::{MethodDescriptor, MethodShape};
    use phoxal::runtime::outputs::OutputSet;
    use phoxal::runtime::{ExecutionTime, initialize};

    #[test]
    fn generated_methods_cover_every_declared_capability() {
        fn assert_observation_method<P: MethodDescriptor>(method: P) {
            assert_eq!(method.signature().shape, MethodShape::Observation);
            assert_eq!(
                method.signature().service,
                "phoxal.component.oak_d_lite.v1.OakDLite"
            );
            assert!(!method.signature().descriptor_set().is_empty());
        }
        assert_observation_method(oak_d_lite::methods::LEFT_MONO);
        assert_observation_method(oak_d_lite::methods::RGB);
        assert_observation_method(oak_d_lite::methods::RIGHT_MONO);
        assert_observation_method(oak_d_lite::methods::DEPTH);
        assert_observation_method(oak_d_lite::methods::IMU);
        assert_observation_method(oak_d_lite::methods::ACCELEROMETER);
        assert_observation_method(oak_d_lite::methods::GYROSCOPE);
        assert!(!FILE_DESCRIPTOR_SET.is_empty());
        assert_eq!(
            OakDLiteOutputs::FIELDS
                .iter()
                .map(|field| field.name)
                .collect::<Vec<_>>(),
            [
                "left_mono",
                "rgb",
                "right_mono",
                "depth",
                "imu",
                "accelerometer",
                "gyroscope"
            ]
        );
        assert!(
            OakDLiteOutputs::FIELDS
                .iter()
                .all(|field| field.port_signature.is_some())
        );
    }

    #[test]
    fn initialization_fails_before_publishing_without_hardware() {
        let result = initialize(
            &OakDLite,
            ExecutionTime::default(),
            OakDLiteConfig::default(),
        );
        let error = result.expect_err("setup must reject an unavailable hardware backend");
        assert_eq!(error.to_string(), BACKEND_UNAVAILABLE);
    }
}
