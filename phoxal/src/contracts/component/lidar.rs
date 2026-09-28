//! Planar laser scans. Other ranging devices use their own observation model.

/// A planar scan in the named sensor frame, with zero angle along x and
/// increasing angles counter-clockwise about positive z.
/// The transport capture timestamp names the first ray; ray i was captured
/// i * time_increment_s later and has angle angle_min_rad + i * angle_increment_rad.
/// Bounds are inclusive. NaN marks an invalid ray, positive infinity no return
/// within range, and negative infinity a return below the minimum range.
/// Intensities are empty when unavailable, otherwise one device-unit value per ray.
#[phoxal::message(package = "phoxal.component.lidar.v1")]
pub struct LaserScan {
    #[phoxal(tag = 1)]
    pub sensor_frame_id: String,
    #[phoxal(tag = 2)]
    pub angle_min_rad: f64,
    #[phoxal(tag = 3)]
    pub angle_increment_rad: f64,
    #[phoxal(tag = 4)]
    pub time_increment_s: f64,
    #[phoxal(tag = 5)]
    pub min_range_m: f64,
    #[phoxal(tag = 6)]
    pub max_range_m: f64,
    #[phoxal(tag = 7)]
    pub ranges_m: Vec<f64>,
    #[phoxal(tag = 8)]
    pub intensities: Vec<f64>,
}
