//! OAK-D Lite component contract and hardware driver.

use anyhow::{Result, anyhow};
use phoxal::runtime::{InitContext, Runtime, Sample, StepContext};

include!(concat!(
    env!("OUT_DIR"),
    "/phoxal.component.oak_d_lite.v1.rs"
));

/// Public typed ports owned by the OAK-D Lite component contract.
pub use oak_d_lite::ports;

/// The original descriptor closure retained for independent contract
/// inspection and native artifact extraction.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/phoxal-descriptors.bin"));

const BACKEND_UNAVAILABLE: &str = "oak_d_lite hardware backend unavailable: refusing to publish fabricated camera, depth, or IMU measurements";

/// The component driver's authored configuration.
#[derive(Debug, Default, serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
pub struct OakDLiteConfig {}

/// Driver state retained by the Runtime owner.
#[derive(Debug)]
pub struct OakDLiteState;

/// OAK-D Lite observations admitted at one Runtime boundary.
#[phoxal::runtime::outputs]
#[derive(Default)]
pub struct OakDLiteOutputs {
    /// Left monochrome camera frames.
    #[phoxal::runtime::outputs::sample(
        port = ports::LEFT_MONO,
        max_items = 4,
        max_bytes = 8_388_608
    )]
    pub left_mono: Vec<Sample<CameraFrame>>,
    /// RGB camera frames.
    #[phoxal::runtime::outputs::sample(
        port = ports::RGB,
        max_items = 4,
        max_bytes = 8_388_608
    )]
    pub rgb: Vec<Sample<CameraFrame>>,
    /// Right monochrome camera frames.
    #[phoxal::runtime::outputs::sample(
        port = ports::RIGHT_MONO,
        max_items = 4,
        max_bytes = 8_388_608
    )]
    pub right_mono: Vec<Sample<CameraFrame>>,
    /// Passive-stereo depth frames.
    #[phoxal::runtime::outputs::sample(
        port = ports::DEPTH,
        max_items = 4,
        max_bytes = 8_388_608
    )]
    pub depth: Vec<Sample<DepthFrame>>,
    /// Fused inertial observations.
    #[phoxal::runtime::outputs::sample(
        port = ports::IMU,
        max_items = 16,
        max_bytes = 16_384
    )]
    pub imu: Vec<Sample<ImuSample>>,
    /// Linear acceleration observations.
    #[phoxal::runtime::outputs::sample(
        port = ports::ACCELEROMETER,
        max_items = 16,
        max_bytes = 8_192
    )]
    pub accelerometer: Vec<Sample<AccelerometerSample>>,
    /// Angular velocity observations.
    #[phoxal::runtime::outputs::sample(
        port = ports::GYROSCOPE,
        max_items = 16,
        max_bytes = 8_192
    )]
    pub gyroscope: Vec<Sample<GyroscopeSample>>,
}

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
        BACKEND_UNAVAILABLE, FILE_DESCRIPTOR_SET, OakDLite, OakDLiteConfig, OakDLiteOutputs, ports,
    };
    use phoxal::runtime::outputs::OutputSet;
    use phoxal::runtime::{ExecutionTime, initialize};
    use phoxal_port::{PortDescriptor, PortKind};

    #[test]
    fn generated_ports_cover_every_declared_capability() {
        fn assert_sample_port<P: PortDescriptor>(port: P) {
            assert_eq!(port.signature().kind, PortKind::Sample);
            assert_eq!(
                port.signature().service,
                "phoxal.component.oak_d_lite.v1.OakDLite"
            );
            assert!(!port.signature().descriptor_set().is_empty());
        }
        assert_sample_port(ports::LEFT_MONO);
        assert_sample_port(ports::RGB);
        assert_sample_port(ports::RIGHT_MONO);
        assert_sample_port(ports::DEPTH);
        assert_sample_port(ports::IMU);
        assert_sample_port(ports::ACCELEROMETER);
        assert_sample_port(ports::GYROSCOPE);
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
