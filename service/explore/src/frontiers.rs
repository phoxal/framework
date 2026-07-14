use std::collections::VecDeque;

use phoxal_api::v1 as api;

const UNKNOWN: u8 = 255;
const FREE_MAX: u8 = 20;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OccupancyGrid {
    pub(crate) origin_x_m: f64,
    pub(crate) origin_y_m: f64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) resolution_m: f32,
    pub(crate) cells: Vec<u8>,
}

impl OccupancyGrid {
    pub(crate) fn from_submap(
        request: &api::map::SubmapRequest,
        response: api::map::SubmapResponse,
    ) -> Option<Self> {
        let cell_count = cell_count(response.width, response.height)?;
        if response.cells.len() != cell_count {
            return None;
        }
        if !response.resolution_m.is_finite() || response.resolution_m <= 0.0 {
            return None;
        }

        Some(Self {
            origin_x_m: request.min_x_m,
            origin_y_m: request.min_y_m,
            width: response.width,
            height: response.height,
            resolution_m: response.resolution_m,
            cells: response.cells,
        })
    }

    pub(crate) fn cell_at_xy(&self, x_m: f64, y_m: f64) -> Option<u8> {
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
        self.cells.get(cell_index(width, x, y)).copied()
    }

    fn world_xy(&self, x: usize, y: usize) -> (f64, f64) {
        let resolution = f64::from(self.resolution_m);
        (
            self.origin_x_m + (x as f64 + 0.5) * resolution,
            self.origin_y_m + (y as f64 + 0.5) * resolution,
        )
    }
}

pub(crate) fn detect_frontiers(grid: &OccupancyGrid) -> Vec<api::explore::Frontier> {
    let Some(cell_count) = cell_count(grid.width, grid.height) else {
        return Vec::new();
    };
    if grid.cells.len() != cell_count {
        return Vec::new();
    }

    let width = grid.width as usize;
    let height = grid.height as usize;
    let mut frontier_cells = vec![false; cell_count];

    for y in 0..height {
        for x in 0..width {
            let index = cell_index(width, x, y);
            if is_free(grid.cells[index])
                && neighbors4(width, height, x, y).any(|n| is_unknown(grid.cells[n]))
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

            let points = collect_component(grid, &frontier_cells, &mut visited, x, y);
            if let Some(frontier) = frontier_from_points(points) {
                frontiers.push(frontier);
            }
        }
    }
    frontiers
}

pub(crate) fn is_free(cell: u8) -> bool {
    cell <= FREE_MAX
}

fn is_unknown(cell: u8) -> bool {
    cell == UNKNOWN
}

fn collect_component(
    grid: &OccupancyGrid,
    frontier_cells: &[bool],
    visited: &mut [bool],
    start_x: usize,
    start_y: usize,
) -> Vec<(f64, f64)> {
    let width = grid.width as usize;
    let height = grid.height as usize;
    let mut queue = VecDeque::from([(start_x, start_y)]);
    let mut points = Vec::new();

    while let Some((x, y)) = queue.pop_front() {
        let index = cell_index(width, x, y);
        if visited[index] || !frontier_cells[index] {
            continue;
        }

        visited[index] = true;
        points.push(grid.world_xy(x, y));

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

fn frontier_from_points(points: Vec<(f64, f64)>) -> Option<api::explore::Frontier> {
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
    Some(api::explore::Frontier {
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
    use super::{OccupancyGrid, detect_frontiers};

    #[test]
    fn free_boundary_cells_adjacent_to_unknown_form_frontier() {
        let grid = grid(3, 2, vec![0, 0, 255, 0, 0, 255]);

        let frontiers = detect_frontiers(&grid);

        assert_eq!(frontiers.len(), 1);
        assert_eq!(frontiers[0].size, 2);
        assert_eq!(frontiers[0].x_m, 1.5);
        assert_eq!(frontiers[0].y_m, 1.0);
    }

    #[test]
    fn all_free_grid_has_no_frontiers() {
        assert!(detect_frontiers(&grid(2, 2, vec![0; 4])).is_empty());
    }

    #[test]
    fn all_unknown_grid_has_no_frontiers() {
        assert!(detect_frontiers(&grid(2, 2, vec![255; 4])).is_empty());
    }

    #[test]
    fn invalid_cell_count_is_rejected() {
        let grid = grid(2, 2, vec![0; 3]);

        assert!(detect_frontiers(&grid).is_empty());
    }

    fn grid(width: u32, height: u32, cells: Vec<u8>) -> OccupancyGrid {
        OccupancyGrid {
            origin_x_m: 0.0,
            origin_y_m: 0.0,
            width,
            height,
            resolution_m: 1.0,
            cells,
        }
    }
}
