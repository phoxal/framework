use phoxal_api::y2026_1 as api;

use crate::frontiers::{OccupancyGrid, is_free};

pub(crate) fn score_frontiers(
    frontiers: Vec<api::explore::Frontier>,
    grid: &OccupancyGrid,
    robot_xy_m: (f64, f64),
) -> Vec<api::explore::Frontier> {
    let mut scored = frontiers
        .into_iter()
        .filter_map(|mut frontier| {
            if !frontier.x_m.is_finite()
                || !frontier.y_m.is_finite()
                || !robot_xy_m.0.is_finite()
                || !robot_xy_m.1.is_finite()
                || !grid
                    .cell_at_xy(frontier.x_m, frontier.y_m)
                    .is_some_and(is_free)
            {
                return None;
            }

            let distance_m = distance(robot_xy_m, (frontier.x_m, frontier.y_m));
            frontier.score = (f64::from(frontier.size) / (1.0 + distance_m)) as f32;
            Some(frontier)
        })
        .collect::<Vec<_>>();

    scored.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| right.size.cmp(&left.size))
            .then_with(|| left.x_m.total_cmp(&right.x_m))
            .then_with(|| left.y_m.total_cmp(&right.y_m))
    });
    scored
}

fn distance(left: (f64, f64), right: (f64, f64)) -> f64 {
    let dx = left.0 - right.0;
    let dy = left.1 - right.1;
    dx.hypot(dy)
}

#[cfg(test)]
mod tests {
    use phoxal_api::y2026_1 as api;

    use super::score_frontiers;
    use crate::frontiers::OccupancyGrid;

    #[test]
    fn frontiers_are_ordered_by_size_distance_score() {
        let scored = score_frontiers(
            vec![frontier(5.5, 0.5, 4), frontier(1.5, 0.5, 2)],
            &grid(8, 8, 0),
            (0.5, 0.5),
        );

        assert_eq!(scored.len(), 2);
        assert_eq!(scored[0].x_m, 1.5);
        assert!(scored[0].score > scored[1].score);
    }

    #[test]
    fn frontier_with_non_free_centroid_cell_is_filtered() {
        let mut grid = grid(4, 4, 0);
        grid.cells[5] = 100;

        let scored = score_frontiers(vec![frontier(1.5, 1.5, 3)], &grid, (0.5, 0.5));

        assert!(scored.is_empty());
    }

    fn frontier(x_m: f64, y_m: f64, size: u32) -> api::explore::Frontier {
        api::explore::Frontier {
            x_m,
            y_m,
            size,
            score: 0.0,
        }
    }

    fn grid(width: u32, height: u32, fill: u8) -> OccupancyGrid {
        OccupancyGrid {
            origin_x_m: 0.0,
            origin_y_m: 0.0,
            width,
            height,
            resolution_m: 1.0,
            cells: vec![fill; (width * height) as usize],
        }
    }
}
