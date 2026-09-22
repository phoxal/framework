use phoxal::runtime::Sample;
use phoxal_component_oak_d_lite::AccelerometerSample;
use phoxal_component_oak_d_lite::CameraFrame;
use phoxal_component_oak_d_lite::DepthFrame;
use phoxal_component_oak_d_lite::GyroscopeSample;
use phoxal_component_oak_d_lite::ImuSample;
use phoxal_component_oak_d_lite::oak_d_lite;

/// OAK-D Lite observations admitted at one Runtime boundary.
#[phoxal::runtime::outputs]
#[derive(Default)]
pub struct OakDLiteOutputs {
    /// Left monochrome camera frames.
    #[phoxal::runtime::outputs::sample(
        port = oak_d_lite::methods::LEFT_MONO.__sample_port(),
        max_items = 4,
        max_bytes = 8_388_608
    )]
    pub left_mono: Vec<Sample<CameraFrame>>,
    /// RGB camera frames.
    #[phoxal::runtime::outputs::sample(
        port = oak_d_lite::methods::RGB.__sample_port(),
        max_items = 4,
        max_bytes = 8_388_608
    )]
    pub rgb: Vec<Sample<CameraFrame>>,
    /// Right monochrome camera frames.
    #[phoxal::runtime::outputs::sample(
        port = oak_d_lite::methods::RIGHT_MONO.__sample_port(),
        max_items = 4,
        max_bytes = 8_388_608
    )]
    pub right_mono: Vec<Sample<CameraFrame>>,
    /// Passive-stereo depth frames.
    #[phoxal::runtime::outputs::sample(
        port = oak_d_lite::methods::DEPTH.__sample_port(),
        max_items = 4,
        max_bytes = 8_388_608
    )]
    pub depth: Vec<Sample<DepthFrame>>,
    /// Fused inertial observations.
    #[phoxal::runtime::outputs::sample(
        port = oak_d_lite::methods::IMU.__sample_port(),
        max_items = 16,
        max_bytes = 16_384
    )]
    pub imu: Vec<Sample<ImuSample>>,
    /// Linear acceleration observations.
    #[phoxal::runtime::outputs::sample(
        port = oak_d_lite::methods::ACCELEROMETER.__sample_port(),
        max_items = 16,
        max_bytes = 8_192
    )]
    pub accelerometer: Vec<Sample<AccelerometerSample>>,
    /// Angular velocity observations.
    #[phoxal::runtime::outputs::sample(
        port = oak_d_lite::methods::GYROSCOPE.__sample_port(),
        max_items = 16,
        max_bytes = 8_192
    )]
    pub gyroscope: Vec<Sample<GyroscopeSample>>,
}
