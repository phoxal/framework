use phoxal::runtime::Sample;
use phoxal_component_bno085::AccelerometerSample;
use phoxal_component_bno085::GyroscopeSample;
use phoxal_component_bno085::ImuSample;
use phoxal_component_bno085::ports;

/// BNO085 observations admitted at one Runtime boundary.
#[phoxal::runtime::outputs]
#[derive(Default)]
pub struct Bno085Outputs {
    /// Fused orientation and inertial measurements.
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
