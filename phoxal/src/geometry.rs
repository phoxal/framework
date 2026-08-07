//! Planar geometry every participant that reasons about pose or heading needs.
//!
//! These are the few pieces of arithmetic that must agree across the whole
//! robot: two participants that wrap a yaw differently, or measure distance
//! differently, disagree about where the robot is. They live here so there is
//! one definition to read and one to test.

/// Wrap `radians` into the half-open interval `(-PI, PI]`.
///
/// The interval is half-open on purpose. `rem_euclid` maps the two physically
/// identical bounds `-PI` and `+PI` to different representations, so a heading
/// that crosses straight behind the robot would flip sign depending on which
/// side it approached from. Folding `-PI` onto `+PI` gives every angle exactly
/// one representation, which is what makes a difference of two normalized
/// angles a well-defined signed error.
#[must_use]
pub fn normalize_angle(radians: f64) -> f64 {
    let normalized = (radians + std::f64::consts::PI).rem_euclid(2.0 * std::f64::consts::PI)
        - std::f64::consts::PI;
    if normalized <= -std::f64::consts::PI {
        std::f64::consts::PI
    } else {
        normalized
    }
}

/// Euclidean distance between two points in the same planar frame.
///
/// Both points are plain `(x, y)` pairs in metres: the callers hold poses,
/// waypoints and tracked detections in different structs, and the distance
/// between them is the same number regardless of which struct it came out of.
#[must_use]
pub fn planar_distance(from: (f64, f64), to: (f64, f64)) -> f64 {
    (to.0 - from.0).hypot(to.1 - from.1)
}

#[cfg(test)]
mod tests {
    use super::{normalize_angle, planar_distance};
    use std::f64::consts::{FRAC_PI_2, PI};

    #[test]
    fn angles_inside_the_interval_are_unchanged() {
        assert_eq!(normalize_angle(0.0), 0.0);
        assert_eq!(normalize_angle(FRAC_PI_2), FRAC_PI_2);
        assert_eq!(normalize_angle(-FRAC_PI_2), -FRAC_PI_2);
    }

    #[test]
    fn angles_outside_the_interval_wrap_into_it() {
        assert!((normalize_angle(3.0 * PI) - PI).abs() < 1e-12);
        assert!((normalize_angle(2.0 * PI + FRAC_PI_2) - FRAC_PI_2).abs() < 1e-12);
        assert!((normalize_angle(-2.0 * PI - FRAC_PI_2) + FRAC_PI_2).abs() < 1e-12);
    }

    /// The half-open convention: both spellings of "straight behind" normalize
    /// to the same value, so a heading error never flips sign at the seam.
    #[test]
    fn both_bounds_normalize_to_positive_pi() {
        assert_eq!(normalize_angle(PI), PI);
        assert_eq!(normalize_angle(-PI), PI);
        assert_eq!(normalize_angle(3.0 * PI), normalize_angle(-PI));
    }

    #[test]
    fn planar_distance_is_symmetric_and_zero_on_the_same_point() {
        assert_eq!(planar_distance((1.5, -2.0), (1.5, -2.0)), 0.0);
        assert_eq!(planar_distance((0.0, 0.0), (3.0, 4.0)), 5.0);
        assert_eq!(
            planar_distance((3.0, 4.0), (0.0, 0.0)),
            planar_distance((0.0, 0.0), (3.0, 4.0))
        );
    }
}
