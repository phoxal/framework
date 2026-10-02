use crate::config::OakDLiteConfig;
use anyhow::Result;
use anyhow::anyhow;
use phoxal::runtime::Context;

const BACKEND_UNAVAILABLE: &str = "oak_d_lite hardware backend unavailable: refusing to publish fabricated camera, depth, or IMU measurements";

/// The OAK-D Lite hardware component driver.
pub struct OakDLite;

/// The OakDLite endpoint contract: the derived standard surface of
/// its declared capabilities.
#[phoxal::endpoints]
pub struct OakDLiteApi {}

/// The hardware backend is intentionally unavailable until a real DepthAI
/// transport can publish camera, depth, and IMU observations.
#[phoxal::runtime(contract = OakDLiteApi, period_ms = 10, timeout_ms = 100, init_timeout_ms = 1_000)]
impl OakDLite {
    #[init]
    fn new(_config: OakDLiteConfig) -> Result<Self> {
        Err(anyhow!(BACKEND_UNAVAILABLE))
    }

    #[step]
    fn unavailable(&mut self, _ctx: &mut Context<'_, Self>) -> Result<()> {
        Err(anyhow!(BACKEND_UNAVAILABLE))
    }
}

#[cfg(test)]
mod tests {
    use super::{BACKEND_UNAVAILABLE, OakDLiteConfig};
    use phoxal::contracts::component::{camera, imu};
    use phoxal::contracts::{MethodDescriptor, MethodShape};
    use phoxal::runtime::Runtime;
    use phoxal::runtime::outputs::OutputSet;
    use phoxal::runtime::{ExecutionTime, initialize};
    use phoxal::schema::MessageSchema;

    #[test]
    fn derived_methods_cover_every_declared_capability() {
        fn assert_observation_method<P: MethodDescriptor>(method: P, payload: &str) {
            assert_eq!(method.signature().shape, MethodShape::Observation);
            assert_eq!(method.signature().service, payload);
        }
        let frame = "phoxal.component.camera.v1.CameraFrame";
        assert_observation_method(crate::api::service_methods::u0::LEFT_MONO, frame);
        assert_observation_method(crate::api::service_methods::u0::RGB, frame);
        assert_observation_method(crate::api::service_methods::u0::RIGHT_MONO, frame);
        assert_observation_method(
            crate::api::service_methods::u0::DEPTH,
            "phoxal.component.camera.v1.DepthFrame",
        );
        assert_observation_method(
            crate::api::service_methods::u0::IMU,
            "phoxal.component.imu.v1.ImuSample",
        );
        assert_observation_method(
            crate::api::service_methods::u0::ACCELEROMETER,
            "phoxal.component.imu.v1.AccelerometerSample",
        );
        assert_observation_method(
            crate::api::service_methods::u0::GYROSCOPE,
            "phoxal.component.imu.v1.GyroscopeSample",
        );
        assert!(camera::CameraFrame::retain_schema() > 0);
        assert!(imu::ImuSample::retain_schema() > 0);
        assert_eq!(
            <super::phoxal_runtime_oak_d_lite::Adapter as Runtime>::Outputs::FIELDS
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
            <super::phoxal_runtime_oak_d_lite::Adapter as Runtime>::Outputs::FIELDS
                .iter()
                .all(|field| field.port_signature.is_some())
        );
    }

    #[test]
    fn initialization_fails_before_publishing_without_hardware() {
        let result = initialize(
            &super::phoxal_runtime_oak_d_lite::Adapter::new(),
            ExecutionTime::default(),
            OakDLiteConfig::default(),
        );
        let error = match result {
            Ok(_) => panic!("setup must reject an unavailable hardware backend"),
            Err(error) => error,
        };
        assert_eq!(error.to_string(), BACKEND_UNAVAILABLE);
    }
}
