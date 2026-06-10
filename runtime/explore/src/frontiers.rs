use std::collections::VecDeque;

use phoxal::api::v1::explore::Frontier;
use phoxal::api::v1::map::{Grid, TraversabilityCell};

pub(crate) fn detect_frontiers_in_frame(
    grid: &Grid<TraversabilityCell>,
    frame_id: &str,
) -> Vec<Frontier> {
    let Some(cell_count) = cell_count(grid) else {
        return Vec::new();
    };
    if grid.cells.len() != cell_count {
        return Vec::new();
    }

    let width = grid.width_cells as usize;
    let height = grid.height_cells as usize;
    let mut frontier_cells = vec![false; cell_count];
    for y in 0..height {
        for x in 0..width {
            let index = cell_index(width, x, y);
            if grid.cells[index] == TraversabilityCell::Free
                && neighbors4(width, height, x, y)
                    .any(|neighbor| grid.cells[neighbor] == TraversabilityCell::Unknown)
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
            frontiers.push(frontier_from_points(frame_id, points));
        }
    }
    frontiers
}

fn world_xy(grid: &Grid<TraversabilityCell>, x: usize, y: usize) -> [f64; 2] {
    let resolution = grid.resolution.xy_m;
    [
        grid.origin_xy_m[0] + ((x as f64) + 0.5) * resolution,
        grid.origin_xy_m[1] + ((y as f64) + 0.5) * resolution,
    ]
}

fn collect_component(
    grid: &Grid<TraversabilityCell>,
    frontier_cells: &[bool],
    visited: &mut [bool],
    start_x: usize,
    start_y: usize,
) -> Vec<[f64; 2]> {
    let width = grid.width_cells as usize;
    let height = grid.height_cells as usize;
    let mut queue = VecDeque::from([(start_x, start_y)]);
    let mut points = Vec::new();

    while let Some((x, y)) = queue.pop_front() {
        let index = cell_index(width, x, y);
        if visited[index] || !frontier_cells[index] {
            continue;
        }

        visited[index] = true;
        points.push(world_xy(grid, x, y));

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

fn frontier_from_points(frame_id: &str, points_xy_m: Vec<[f64; 2]>) -> Frontier {
    let centroid = centroid(&points_xy_m);
    Frontier {
        id: format!(
            "frontier_{:.0}_{:.0}_{}",
            centroid[0] * 1000.0,
            centroid[1] * 1000.0,
            points_xy_m.len()
        ),
        frame_id: frame_id.to_string(),
        points_xy_m,
    }
}

fn centroid(points_xy_m: &[[f64; 2]]) -> [f64; 2] {
    let mut sum = [0.0, 0.0];
    for point in points_xy_m {
        sum[0] += point[0];
        sum[1] += point[1];
    }
    let count = points_xy_m.len() as f64;
    [sum[0] / count, sum[1] / count]
}

fn cell_count(grid: &Grid<TraversabilityCell>) -> Option<usize> {
    (grid.width_cells as usize).checked_mul(grid.height_cells as usize)
}

pub(crate) fn cell_at_xy(
    grid: &Grid<TraversabilityCell>,
    xy_m: [f64; 2],
) -> Option<TraversabilityCell> {
    let resolution = grid.resolution.xy_m;
    if resolution <= 0.0 {
        return None;
    }

    let x = ((xy_m[0] - grid.origin_xy_m[0]) / resolution).floor();
    let y = ((xy_m[1] - grid.origin_xy_m[1]) / resolution).floor();
    if x < 0.0 || y < 0.0 {
        return None;
    }

    let x = x as usize;
    let y = y as usize;
    let width = grid.width_cells as usize;
    let height = grid.height_cells as usize;
    if x >= width || y >= height {
        return None;
    }

    grid.cells.get(cell_index(width, x, y)).copied()
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

#[cfg(test)]
mod tests {
    use phoxal::api::v1::map::{Grid, Resolution, TraversabilityCell};

    use super::detect_frontiers_in_frame;

    #[test]
    fn free_boundary_cells_adjacent_to_unknown_form_frontier() {
        let grid = grid(
            3,
            2,
            vec![
                TraversabilityCell::Free,
                TraversabilityCell::Free,
                TraversabilityCell::Unknown,
                TraversabilityCell::Free,
                TraversabilityCell::Free,
                TraversabilityCell::Unknown,
            ],
        );

        let frontiers = detect_frontiers_in_frame(&grid, "map");

        assert_eq!(frontiers.len(), 1);
        assert_eq!(frontiers[0].frame_id, "map");
        assert_eq!(frontiers[0].points_xy_m, vec![[1.5, 0.5], [1.5, 1.5]]);
    }

    #[test]
    fn all_free_grid_has_no_frontiers() {
        let grid = grid(2, 2, vec![TraversabilityCell::Free; 4]);

        assert!(detect_frontiers_in_frame(&grid, "map").is_empty());
    }

    #[test]
    fn all_unknown_grid_has_no_frontiers() {
        let grid = grid(2, 2, vec![TraversabilityCell::Unknown; 4]);

        assert!(detect_frontiers_in_frame(&grid, "map").is_empty());
    }

    fn grid(
        width_cells: u32,
        height_cells: u32,
        cells: Vec<TraversabilityCell>,
    ) -> Grid<TraversabilityCell> {
        Grid {
            origin_xy_m: [0.0, 0.0],
            resolution: Resolution {
                xy_m: 1.0,
                z_m: None,
            },
            width_cells,
            height_cells,
            cells,
        }
    }
}
