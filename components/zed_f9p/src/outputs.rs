use crate::api::zed_f9p::v1::GnssSample;
use crate::api::zed_f9p::v1::zed_f9p;
use phoxal::runtime::Sample;

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
