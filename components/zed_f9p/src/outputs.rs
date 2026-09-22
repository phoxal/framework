use phoxal::runtime::Sample;
use phoxal_component_zed_f9p::GnssSample;
use phoxal_component_zed_f9p::zed_f9p;

/// ZED-F9P observations admitted at one Runtime boundary.
#[phoxal::runtime::outputs]
#[derive(Default)]
pub struct ZedF9pOutputs {
    /// WGS84 latitude, longitude, altitude, and covariance observations.
    #[phoxal::runtime::outputs::sample(
        port = zed_f9p::methods::GNSS.__sample_port(),
        max_items = 8,
        max_bytes = 8_192
    )]
    pub gnss: Vec<Sample<GnssSample>>,
}
