//! The occupancy grid an exploration query reasons over, and the two questions
//! it is asked: where the known-free space borders the unknown, and which of
//! those borders is worth driving to.

use std::collections::VecDeque;

use phoxal::api;
use phoxal::geometry::planar_distance;

const MAP_FRAME: &str = "map";

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OccupancyGrid {
    pub(crate) origin_x_m: f64,
    pub(crate) origin_y_m: f64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) resolution_m: f32,
    pub(crate) cells: Vec<api::map::Occupancy>,
}

impl OccupancyGrid {
    /// Materialize the extent the map owner actually returned. The request is
    /// deliberately absent from this function: a partial window is allowed,
    /// and reconstructing an origin from a request would put cells at the
    /// wrong world coordinates.
    pub(crate) fn from_submap(response: api::map::SubmapResponse) -> Option<Self> {
        let window = match response {
            api::map::SubmapResponse::Window(window)
            | api::map::SubmapResponse::Partial { window } => window,
            api::map::SubmapResponse::OutOfBounds { .. } => return None,
        };
        let cell_count = cell_count(window.width, window.height)?;
        let resolution = f64::from(window.resolution_m);
        let epsilon = resolution * 1.0e-6;
        if window.cells.len() != cell_count
            || !window.resolution_m.is_finite()
            || window.resolution_m <= 0.0
            || window.frame_id != MAP_FRAME
            || !window.origin_pose.x_m.is_finite()
            || !window.origin_pose.y_m.is_finite()
            || !window.origin_pose.yaw_rad.is_finite()
            || window.origin_pose.yaw_rad.abs() > f64::EPSILON
            || !window.cell_origin.x_m.is_finite()
            || !window.cell_origin.y_m.is_finite()
            || !bounds_are_valid(&window.requested)
            || !bounds_are_valid(&window.covered)
            || window.covered.min_x_m < window.requested.min_x_m
            || window.covered.min_y_m < window.requested.min_y_m
            || window.covered.max_x_m > window.requested.max_x_m
            || window.covered.max_y_m > window.requested.max_y_m
            || (window.origin_pose.x_m - window.cell_origin.x_m).abs() > epsilon
            || (window.origin_pose.y_m - window.cell_origin.y_m).abs() > epsilon
            || (window.cell_origin.x_m - window.covered.min_x_m).abs() > epsilon
            || (window.cell_origin.y_m - window.covered.min_y_m).abs() > epsilon
            || (window.covered.max_x_m
                - window.covered.min_x_m
                - f64::from(window.width) * resolution)
                .abs()
                > epsilon
            || (window.covered.max_y_m
                - window.covered.min_y_m
                - f64::from(window.height) * resolution)
                .abs()
                > epsilon
        {
            return None;
        }

        Some(Self {
            origin_x_m: window.cell_origin.x_m,
            origin_y_m: window.cell_origin.y_m,
            width: window.width,
            height: window.height,
            resolution_m: window.resolution_m,
            cells: window.cells,
        })
    }

    /// Every connected run of free cells that touches unknown space.
    ///
    /// A frontier cell is free and has an unknown 4-neighbor: that is the
    /// boundary the robot can stand on and still see something new. Connected
    /// runs are collapsed to their centroid so a caller gets one candidate per
    /// opening rather than one per cell.
    pub(crate) fn detect_frontiers(&self) -> Vec<api::navigation::Frontier> {
        let Some(cell_count) = cell_count(self.width, self.height) else {
            return Vec::new();
        };
        if self.cells.len() != cell_count {
            return Vec::new();
        }

        let width = self.width as usize;
        let height = self.height as usize;
        let mut frontier_cells = vec![false; cell_count];

        for y in 0..height {
            for x in 0..width {
                let index = cell_index(width, x, y);
                if is_free(&self.cells[index])
                    && neighbors4(width, height, x, y).any(|n| is_unknown(&self.cells[n]))
                {
                    frontier_cells[index] = true;
                }
            }
        }

        let mut visited = vec![false; cell_count];
        let mut frontiers = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let start = cell_index(width, x, y);
                if !frontier_cells[start] || visited[start] {
                    continue;
                }

                let points = self.collect_component(&frontier_cells, &mut visited, x, y);
                if let Some(frontier) = frontier_from_points(points) {
                    frontiers.push(frontier);
                }
            }
        }
        frontiers
    }

    /// Rank `frontiers` for a robot at `robot_xy_m`, best first.
    ///
    /// A frontier scores high when it is large and close, so exploration prefers
    /// a wide opening nearby over a single stray cell across the map. Candidates
    /// whose centroid is not itself a free cell of this grid are dropped: the
    /// centroid of a curved run can land in occupied or unknown space, and
    /// driving to it would be driving into a wall. Ties break on size and then
    /// on position so the ranking is total and the query is deterministic.
    pub(crate) fn score_frontiers(
        &self,
        frontiers: Vec<api::navigation::Frontier>,
        robot_xy_m: (f64, f64),
    ) -> Vec<api::navigation::Frontier> {
        let mut scored = frontiers
            .into_iter()
            .filter_map(|mut frontier| {
                if !frontier.x_m.is_finite()
                    || !frontier.y_m.is_finite()
                    || !robot_xy_m.0.is_finite()
                    || !robot_xy_m.1.is_finite()
                    || !self
                        .cell_at_xy(frontier.x_m, frontier.y_m)
                        .is_some_and(|cell| is_free(&cell))
                {
                    return None;
                }

                let distance_m = planar_distance(robot_xy_m, (frontier.x_m, frontier.y_m));
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

    pub(crate) fn cell_at_xy(&self, x_m: f64, y_m: f64) -> Option<api::map::Occupancy> {
        if !x_m.is_finite() || !y_m.is_finite() {
            return None;
        }
        let resolution = f64::from(self.resolution_m);
        let x = ((x_m - self.origin_x_m) / resolution).floor();
        let y = ((y_m - self.origin_y_m) / resolution).floor();
        if x < 0.0 || y < 0.0 {
            return None;
        }

        let x = x as usize;
        let y = y as usize;
        let width = self.width as usize;
        let height = self.height as usize;
        if x >= width || y >= height {
            return None;
        }
        self.cells.get(cell_index(width, x, y)).cloned()
    }

    fn world_xy(&self, x: usize, y: usize) -> (f64, f64) {
        let resolution = f64::from(self.resolution_m);
        (
            self.origin_x_m + (x as f64 + 0.5) * resolution,
            self.origin_y_m + (y as f64 + 0.5) * resolution,
        )
    }

    fn collect_component(
        &self,
        frontier_cells: &[bool],
        visited: &mut [bool],
        start_x: usize,
        start_y: usize,
    ) -> Vec<(f64, f64)> {
        let width = self.width as usize;
        let height = self.height as usize;
        let mut queue = VecDeque::from([(start_x, start_y)]);
        let mut points = Vec::new();

        while let Some((x, y)) = queue.pop_front() {
            let index = cell_index(width, x, y);
            if visited[index] || !frontier_cells[index] {
                continue;
            }

            visited[index] = true;
            points.push(self.world_xy(x, y));

            for neighbor in neighbors4(width, height, x, y) {
                let neighbor_x = neighbor % width;
                let neighbor_y = neighbor / width;
                if frontier_cells[neighbor] && !visited[neighbor] {
                    queue.push_back((neighbor_x, neighbor_y));
                }
            }
        }

        points
    }
}

fn bounds_are_valid(bounds: &api::map::Bounds) -> bool {
    [
        bounds.min_x_m,
        bounds.min_y_m,
        bounds.max_x_m,
        bounds.max_y_m,
    ]
    .into_iter()
    .all(f64::is_finite)
        && bounds.min_x_m < bounds.max_x_m
        && bounds.min_y_m < bounds.max_y_m
}

fn is_free(cell: &api::map::Occupancy) -> bool {
    matches!(cell, api::map::Occupancy::Free)
}

fn is_unknown(cell: &api::map::Occupancy) -> bool {
    matches!(cell, api::map::Occupancy::Unknown)
}

fn frontier_from_points(points: Vec<(f64, f64)>) -> Option<api::navigation::Frontier> {
    if points.is_empty() {
        return None;
    }

    let mut sum_x = 0.0;
    let mut sum_y = 0.0;
    for (x, y) in &points {
        sum_x += *x;
        sum_y += *y;
    }
    let size = points.len() as u32;
    let count = f64::from(size);
    Some(api::navigation::Frontier {
        x_m: sum_x / count,
        y_m: sum_y / count,
        size,
        score: 0.0,
    })
}

fn neighbors4(width: usize, height: usize, x: usize, y: usize) -> impl Iterator<Item = usize> {
    [
        x.checked_sub(1).map(|nx| (nx, y)),
        (x + 1 < width).then_some((x + 1, y)),
        y.checked_sub(1).map(|ny| (x, ny)),
        (y + 1 < height).then_some((x, y + 1)),
    ]
    .into_iter()
    .flatten()
    .map(move |(nx, ny)| cell_index(width, nx, ny))
}

fn cell_index(width: usize, x: usize, y: usize) -> usize {
    y * width + x
}

fn cell_count(width: u32, height: u32) -> Option<usize> {
    (width as usize).checked_mul(height as usize)
}

#[cfg(test)]
mod tests {
    use phoxal::api;

    use super::OccupancyGrid;

    #[test]
    fn free_boundary_cells_adjacent_to_unknown_form_frontier() {
        let grid = grid(3, 2, vec![0, 0, 255, 0, 0, 255]);

        let frontiers = grid.detect_frontiers();

        assert_eq!(frontiers.len(), 1);
        assert_eq!(frontiers[0].size, 2);
        assert_eq!(frontiers[0].x_m, 1.5);
        assert_eq!(frontiers[0].y_m, 1.0);
    }

    #[test]
    fn all_free_grid_has_no_frontiers() {
        assert!(grid(2, 2, vec![0; 4]).detect_frontiers().is_empty());
    }

    #[test]
    fn all_unknown_grid_has_no_frontiers() {
        assert!(grid(2, 2, vec![255; 4]).detect_frontiers().is_empty());
    }

    #[test]
    fn invalid_cell_count_is_rejected() {
        assert!(grid(2, 2, vec![0; 3]).detect_frontiers().is_empty());
    }

    #[test]
    fn frontiers_are_ordered_by_size_distance_score() {
        let scored = grid(8, 8, vec![0; 64]).score_frontiers(
            vec![frontier(5.5, 0.5, 4), frontier(1.5, 0.5, 2)],
            (0.5, 0.5),
        );

        assert_eq!(scored.len(), 2);
        assert_eq!(scored[0].x_m, 1.5);
        assert!(scored[0].score > scored[1].score);
    }

    #[test]
    fn frontier_with_non_free_centroid_cell_is_filtered() {
        let mut grid = grid(4, 4, vec![0; 16]);
        grid.cells[5] = api::map::Occupancy::Occupied;

        let scored = grid.score_frontiers(vec![frontier(1.5, 1.5, 3)], (0.5, 0.5));

        assert!(scored.is_empty());
    }

    /// Detection and scoring compose over a submap response as the exploration
    /// query drives them: a two-cell opening survives clustering and comes back
    /// ranked.
    #[test]
    fn submap_detection_and_scoring_preserve_cluster_size_and_score() {
        let response = api::map::SubmapResponse::Window(api::map::GridWindow {
            frame_id: "map".to_string(),
            origin_pose: api::map::Pose {
                x_m: 0.0,
                y_m: 0.0,
                yaw_rad: 0.0,
            },
            cell_origin: api::map::Point { x_m: 0.0, y_m: 0.0 },
            width: 3,
            height: 2,
            resolution_m: 1.0,
            cells: vec![
                api::map::Occupancy::Free,
                api::map::Occupancy::Free,
                api::map::Occupancy::Unknown,
                api::map::Occupancy::Free,
                api::map::Occupancy::Free,
                api::map::Occupancy::Unknown,
            ],
            revision: 1,
            requested: api::map::Bounds {
                min_x_m: 0.0,
                min_y_m: 0.0,
                max_x_m: 3.0,
                max_y_m: 2.0,
            },
            covered: api::map::Bounds {
                min_x_m: 0.0,
                min_y_m: 0.0,
                max_x_m: 3.0,
                max_y_m: 2.0,
            },
        });

        let grid = OccupancyGrid::from_submap(response).unwrap();
        let ranked = grid.score_frontiers(grid.detect_frontiers(), (0.5, 0.5));

        assert_eq!(ranked[0].size, 2);
        assert!(ranked[0].score > 0.0);
    }

    fn frontier(x_m: f64, y_m: f64, size: u32) -> api::navigation::Frontier {
        api::navigation::Frontier {
            x_m,
            y_m,
            size,
            score: 0.0,
        }
    }

    fn grid(width: u32, height: u32, cells: Vec<u8>) -> OccupancyGrid {
        OccupancyGrid {
            origin_x_m: 0.0,
            origin_y_m: 0.0,
            width,
            height,
            resolution_m: 1.0,
            cells: cells
                .into_iter()
                .map(|cell| match cell {
                    0..=20 => api::map::Occupancy::Free,
                    255 => api::map::Occupancy::Unknown,
                    _ => api::map::Occupancy::Occupied,
                })
                .collect(),
        }
    }
}
