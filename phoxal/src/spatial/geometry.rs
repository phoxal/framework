pub const SUPPORT_SURFACE_MARGIN_Z_M: f32 = 0.08;

pub fn point_in_polygon_xy(point: [f32; 2], polygon: &[[f32; 2]]) -> bool {
    if polygon.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut previous = polygon[polygon.len() - 1];
    for current in polygon {
        let dy = previous[1] - current[1];
        let crosses = dy.abs() > f32::EPSILON
            && (current[1] > point[1]) != (previous[1] > point[1])
            && point[0] < (previous[0] - current[0]) * (point[1] - current[1]) / dy + current[0];
        if crosses {
            inside = !inside;
        }
        previous = *current;
    }
    inside
}

pub fn is_above_support_surface(point_z_m: f32, support_surface_z_m: f32) -> bool {
    point_z_m > support_surface_z_m + SUPPORT_SURFACE_MARGIN_Z_M
}

pub(crate) fn convex_hull_xy(mut points: Vec<[f32; 2]>) -> anyhow::Result<Vec<[f32; 2]>> {
    points.retain(|point| point[0].is_finite() && point[1].is_finite());
    points.sort_by(|left, right| {
        left[0]
            .total_cmp(&right[0])
            .then_with(|| left[1].total_cmp(&right[1]))
    });
    points.dedup();

    if points.len() < 3 {
        anyhow::bail!("safety collision primitive must project to at least three XY points");
    }

    let mut lower = Vec::new();
    for point in &points {
        while lower.len() >= 2
            && cross_2d(lower[lower.len() - 2], lower[lower.len() - 1], *point) <= f32::EPSILON
        {
            lower.pop();
        }
        lower.push(*point);
    }

    let mut upper = Vec::new();
    for point in points.iter().rev() {
        while upper.len() >= 2
            && cross_2d(upper[upper.len() - 2], upper[upper.len() - 1], *point) <= f32::EPSILON
        {
            upper.pop();
        }
        upper.push(*point);
    }

    lower.pop();
    upper.pop();
    let polygon = lower.into_iter().chain(upper).collect::<Vec<_>>();
    if polygon.len() < 3 {
        anyhow::bail!("safety collision primitive must project to a non-degenerate XY polygon");
    }
    Ok(polygon)
}

pub fn wrap_angle(angle_rad: f32) -> f32 {
    (angle_rad + std::f32::consts::PI).rem_euclid(2.0 * std::f32::consts::PI) - std::f32::consts::PI
}

fn cross_2d(origin: [f32; 2], left: [f32; 2], right: [f32; 2]) -> f32 {
    (left[0] - origin[0]) * (right[1] - origin[1]) - (left[1] - origin[1]) * (right[0] - origin[0])
}

#[cfg(test)]
mod tests {
    use super::{
        SUPPORT_SURFACE_MARGIN_Z_M, convex_hull_xy, is_above_support_surface, point_in_polygon_xy,
        wrap_angle,
    };

    const TOLERANCE: f32 = 1e-6;

    fn assert_close(left: f32, right: f32) {
        assert!(
            (left - right).abs() <= TOLERANCE,
            "expected {left} to be within {TOLERANCE} of {right}"
        );
    }

    fn assert_points_close(left: &[[f32; 2]], right: &[[f32; 2]]) {
        assert_eq!(left.len(), right.len());
        for (left, right) in left.iter().zip(right) {
            assert_close(left[0], right[0]);
            assert_close(left[1], right[1]);
        }
    }

    #[test]
    fn convex_hull_orders_square_corners_counter_clockwise() {
        let hull = convex_hull_xy(vec![[1.0, 1.0], [0.0, 1.0], [0.0, 0.0], [1.0, 0.0]])
            .expect("square hull");

        assert_points_close(&hull, &[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]);
    }

    #[test]
    fn convex_hull_orders_triangle_corners_counter_clockwise() {
        let hull = convex_hull_xy(vec![[0.5, 1.0], [1.0, 0.0], [0.0, 0.0]]).expect("triangle hull");

        assert_points_close(&hull, &[[0.0, 0.0], [1.0, 0.0], [0.5, 1.0]]);
    }

    #[test]
    fn convex_hull_rejects_degenerate_inputs() {
        assert!(convex_hull_xy(vec![[0.0, 0.0], [1.0, 1.0]]).is_err());
        assert!(convex_hull_xy(vec![[0.0, 0.0], [1.0, 1.0], [2.0, 2.0], [3.0, 3.0]]).is_err());
    }

    #[test]
    fn convex_hull_deduplicates_points_and_filters_nan() {
        let hull = convex_hull_xy(vec![
            [1.0, 1.0],
            [0.0, 1.0],
            [f32::NAN, 0.0],
            [0.0, 0.0],
            [1.0, 0.0],
            [1.0, 0.0],
        ])
        .expect("finite square hull");

        assert_points_close(&hull, &[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]);
    }

    #[test]
    fn convex_hull_drops_near_collinear_interior_points() {
        let hull = convex_hull_xy(vec![
            [0.0, 0.0],
            [0.5, f32::EPSILON * 0.25],
            [1.0, 0.0],
            [1.0, 1.0],
            [0.0, 1.0],
        ])
        .expect("near-collinear interior point is culled");

        assert_points_close(&hull, &[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]);
    }

    #[test]
    fn point_in_polygon_reports_inside_outside_and_boundary_cases() {
        let polygon = [[0.0, 0.0], [2.0, 0.0], [2.0, 2.0], [0.0, 2.0]];

        assert!(point_in_polygon_xy([1.0, 1.0], &polygon));
        assert!(!point_in_polygon_xy([3.0, 1.0], &polygon));
        assert!(point_in_polygon_xy([0.0, 0.0], &polygon));
        assert!(point_in_polygon_xy([1.0, 0.0], &polygon));
    }

    #[test]
    fn point_in_polygon_rejects_polygons_with_fewer_than_three_vertices() {
        assert!(!point_in_polygon_xy([0.0, 0.0], &[]));
        assert!(!point_in_polygon_xy([0.0, 0.0], &[[0.0, 0.0], [1.0, 0.0]]));
    }

    #[test]
    fn wrap_angle_normalizes_to_negative_pi_inclusive_range() {
        assert_close(wrap_angle(0.0), 0.0);
        assert_eq!(wrap_angle(std::f32::consts::PI), -std::f32::consts::PI);
        assert_eq!(wrap_angle(-std::f32::consts::PI), -std::f32::consts::PI);
        assert_close(
            wrap_angle(1.5 * std::f32::consts::PI),
            -0.5 * std::f32::consts::PI,
        );
        assert_close(
            wrap_angle(-1.5 * std::f32::consts::PI),
            0.5 * std::f32::consts::PI,
        );
        assert_close(wrap_angle(4.0 * std::f32::consts::PI + 0.25), 0.25);
    }

    #[test]
    fn is_above_support_surface_uses_strict_margin_boundary() {
        let support_z_m = 0.2;

        assert!(is_above_support_surface(
            support_z_m + SUPPORT_SURFACE_MARGIN_Z_M + 0.001,
            support_z_m
        ));
        assert!(!is_above_support_surface(
            support_z_m + SUPPORT_SURFACE_MARGIN_Z_M - 0.001,
            support_z_m
        ));
        assert!(!is_above_support_surface(
            support_z_m + SUPPORT_SURFACE_MARGIN_Z_M,
            support_z_m
        ));
    }
}
