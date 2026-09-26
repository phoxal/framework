use crate::config::OakDLiteConfig;
use anyhow::Result;
use anyhow::anyhow;
use phoxal::runtime::InitContext;
use phoxal::runtime::Runtime;
use phoxal::runtime::StepContext;

const BACKEND_UNAVAILABLE: &str = "oak_d_lite hardware backend unavailable: refusing to publish fabricated camera, depth, or IMU measurements";

/// Driver state retained by the Runtime owner.
#[derive(Debug)]
pub struct OakDLiteState;

/// The OAK-D Lite hardware component driver.
pub struct OakDLite;

/// The hardware backend is intentionally unavailable until a real DepthAI
/// transport can publish camera, depth, and IMU observations.
#[phoxal::runtime(period_ms = 10, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for OakDLite {
    type Config = OakDLiteConfig;
    type State = OakDLiteState;

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
    use super::{BACKEND_UNAVAILABLE, OakDLite, OakDLiteConfig};
    use phoxal::contract::{MethodDescriptor, MethodShape};
    use phoxal::runtime::Runtime;
    use phoxal::runtime::outputs::OutputSet;
    use phoxal::runtime::{ExecutionTime, initialize};

    #[test]
    fn generated_methods_cover_every_declared_capability() {
        fn assert_observation_method<P: MethodDescriptor>(method: P, payload: &str) {
            assert_eq!(method.signature().shape, MethodShape::Observation);
            assert_eq!(method.signature().service, payload);
            assert!(!method.signature().descriptor_set().is_empty());
        }
        let camera = "phoxal.component.oak_d_lite.v1.CameraFrame";
        assert_observation_method(crate::api::service_methods::u0::LEFT_MONO, camera);
        assert_observation_method(crate::api::service_methods::u0::RGB, camera);
        assert_observation_method(crate::api::service_methods::u0::RIGHT_MONO, camera);
        assert_observation_method(
            crate::api::service_methods::u0::DEPTH,
            "phoxal.component.oak_d_lite.v1.DepthFrame",
        );
        assert_observation_method(
            crate::api::service_methods::u0::IMU,
            "phoxal.component.oak_d_lite.v1.ImuSample",
        );
        assert_observation_method(
            crate::api::service_methods::u0::ACCELEROMETER,
            "phoxal.component.oak_d_lite.v1.AccelerometerSample",
        );
        assert_observation_method(
            crate::api::service_methods::u0::GYROSCOPE,
            "phoxal.component.oak_d_lite.v1.GyroscopeSample",
        );
        assert_eq!(
            <OakDLite as Runtime>::Outputs::FIELDS
                .iter()
                .map(|field| field.name)
                .collect::<Vec<_>>(),
            [
                "accelerometer",
                "depth",
                "gyroscope",
                "imu",
                "left_mono",
                "rgb",
                "right_mono"
            ]
        );
        assert!(
            <OakDLite as Runtime>::Outputs::FIELDS
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
