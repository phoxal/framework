use crate::api::bno085::v1::AccelerometerSample;
use crate::api::bno085::v1::GyroscopeSample;
use crate::api::bno085::v1::ImuSample;
use crate::api::bno085::v1::bno085;
use phoxal::runtime::Sample;

/// BNO085 observations admitted at one Runtime boundary.
#[phoxal::runtime::outputs]
#[derive(Default)]
pub struct Bno085Outputs {
    /// Fused orientation and inertial measurements.
    #[phoxal::runtime::outputs::sample(
        port = bno085::methods::IMU.__sample_port(),
        max_items = 16,
        max_bytes = 16_384
    )]
    pub imu: Vec<Sample<ImuSample>>,
    /// Linear acceleration observations.
    #[phoxal::runtime::outputs::sample(
        port = bno085::methods::ACCELEROMETER.__sample_port(),
        max_items = 16,
        max_bytes = 8_192
    )]
    pub accelerometer: Vec<Sample<AccelerometerSample>>,
    /// Angular velocity observations.
    #[phoxal::runtime::outputs::sample(
        port = bno085::methods::GYROSCOPE.__sample_port(),
        max_items = 16,
        max_bytes = 8_192
    )]
    pub gyroscope: Vec<Sample<GyroscopeSample>>,
}
