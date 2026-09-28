//! The standard satellite-fix vocabulary.
//!
//! A producer-independent identity under `phoxal.component.gnss.v1`:
//! GNSS-bearing drivers (for example ZED-F9P) and the native simulator
//! publish this exact shape.

/// One GNSS fix.
///
/// Latitude and longitude are WGS84 degrees; altitude is the height above
/// the WGS84 ellipsoid in metres (not a geoid-corrected mean-sea-level
/// height). `position_covariance` is empty when the producer reports no
/// estimate; otherwise it carries exactly nine values forming the full
/// row-major 3×3 covariance of the position error in the local
/// east-north-up frame at the fix, with every symmetric pair populated
/// by equal duplicated entries (`m[i][j] == m[j][i]`, including the
/// diagonal). Consumers read any cell directly without mirroring logic.
#[phoxal::message(package = "phoxal.component.gnss.v1")]
pub struct GnssSample {
    #[phoxal(tag = 1)]
    pub latitude_deg: f64,
    #[phoxal(tag = 2)]
    pub longitude_deg: f64,
    #[phoxal(tag = 3)]
    pub altitude_m: f64,
    #[phoxal(tag = 4)]
    pub position_covariance: Vec<f64>,
}

#[cfg(test)]
mod tests {
    use prost::Name;

    use super::GnssSample;
    use crate::schema::MessageSchema;

    #[test]
    fn retains_public_protobuf_identity_and_schema() {
        assert_eq!(GnssSample::PACKAGE, "phoxal.component.gnss.v1");
        assert_eq!(GnssSample::WIRE_NAME, "phoxal.component.gnss.v1.GnssSample");
        assert!(GnssSample::retain_schema() > 0);
    }
}
