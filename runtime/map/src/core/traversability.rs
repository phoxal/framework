use phoxal::api::map::v1::{Grid, OccupancyCell, Resolution, TraversabilityCell};

use super::occupancy::{GRID_RESOLUTION_M, OccupancyGrid};

impl OccupancyGrid {
    /// Evaluate traversability cells from the current occupancy grid and the
    /// robot's 2D bounding radius. Pure function; no side effects.
    ///
    /// MVP limitations:
    /// - 2D only. No slope, cliff, support, or overhead-clearance evaluation.
    /// - Inflation is a simple Euclidean halo. No directional dilation, cost
    ///   gradient, or separate danger radius.
    /// - Cliff and Unsupported variants are never produced in this phase.
    pub fn traversability(&self, body_radius_m: f64) -> Grid<TraversabilityCell> {
        let occupancy_grid = self.to_local_grid();
        let inflation_cells = (body_radius_m / GRID_RESOLUTION_M).ceil() as i32;
        let mut cells = occupancy_grid
            .cells
            .iter()
            .map(|cell| match cell {
                OccupancyCell::Unknown => TraversabilityCell::Unknown,
                OccupancyCell::Free => TraversabilityCell::Free,
                OccupancyCell::Occupied => TraversabilityCell::Occupied,
                _ => TraversabilityCell::Unknown,
            })
            .collect::<Vec<_>>();

        if inflation_cells > 0 {
            for y_cell in 0..occupancy_grid.height_cells as i32 {
                for x_cell in 0..occupancy_grid.width_cells as i32 {
                    let source_index = cell_index(
                        x_cell,
                        y_cell,
                        occupancy_grid.width_cells,
                        occupancy_grid.height_cells,
                    );
                    let Some(source_index) = source_index else {
                        continue;
                    };
                    if occupancy_grid.cells[source_index] != OccupancyCell::Occupied {
                        continue;
                    }

                    inflate_from_occupied_cell(
                        &mut cells,
                        x_cell,
                        y_cell,
                        inflation_cells,
                        occupancy_grid.width_cells,
                        occupancy_grid.height_cells,
                    );
                }
            }
        }

        Grid {
            origin_xy_m: occupancy_grid.origin_xy_m,
            resolution: Resolution {
                xy_m: GRID_RESOLUTION_M,
                z_m: None,
            },
            width_cells: occupancy_grid.width_cells,
            height_cells: occupancy_grid.height_cells,
            cells,
        }
    }
}

fn inflate_from_occupied_cell(
    cells: &mut [TraversabilityCell],
    source_x_cell: i32,
    source_y_cell: i32,
    inflation_cells: i32,
    width_cells: u32,
    height_cells: u32,
) {
    let radius_squared = inflation_cells * inflation_cells;
    for dy in -inflation_cells..=inflation_cells {
        for dx in -inflation_cells..=inflation_cells {
            if dx * dx + dy * dy > radius_squared {
                continue;
            }
            let Some(index) = cell_index(
                source_x_cell + dx,
                source_y_cell + dy,
                width_cells,
                height_cells,
            ) else {
                continue;
            };
            if cells[index] == TraversabilityCell::Free {
                cells[index] = TraversabilityCell::Inflated;
            }
        }
    }
}

fn cell_index(x_cell: i32, y_cell: i32, width_cells: u32, height_cells: u32) -> Option<usize> {
    if x_cell < 0 || y_cell < 0 || x_cell >= width_cells as i32 || y_cell >= height_cells as i32 {
        return None;
    }

    Some((y_cell as u32 * width_cells + x_cell as u32) as usize)
}

#[cfg(test)]
mod tests {
    use crate::core::occupancy::{GRID_HEIGHT_CELLS, GRID_WIDTH_CELLS};

    use super::*;

    const UNKNOWN: u8 = 0;
    const FREE: u8 = 1;
    const OCCUPIED: u8 = 2;

    #[test]
    fn empty_occupancy_produces_all_unknown_traversability() {
        let occupancy = OccupancyGrid::centered_at([0.0, 0.0]);

        let traversability = occupancy.traversability(0.30);

        assert_eq!(traversability.width_cells, GRID_WIDTH_CELLS);
        assert_eq!(traversability.height_cells, GRID_HEIGHT_CELLS);
        assert!(
            traversability
                .cells
                .iter()
                .all(|cell| *cell == TraversabilityCell::Unknown)
        );
    }

    #[test]
    fn free_cells_stay_free_with_no_obstacles() {
        let mut occupancy = OccupancyGrid::centered_at([0.0, 0.0]);
        occupancy.cells_mut().fill(FREE);

        let traversability = occupancy.traversability(0.30);

        assert!(
            traversability
                .cells
                .iter()
                .all(|cell| *cell == TraversabilityCell::Free)
        );
    }

    #[test]
    fn occupied_cell_inflates_neighbors_within_radius() {
        let mut occupancy = OccupancyGrid::centered_at([0.0, 0.0]);
        occupancy.cells_mut().fill(FREE);
        occupancy.cells_mut()[index(25, 25)] = OCCUPIED;

        let traversability = occupancy.traversability(0.30);

        assert_eq!(
            traversability.cells[index(25, 25)],
            TraversabilityCell::Occupied
        );
        assert_eq!(
            traversability.cells[index(22, 25)],
            TraversabilityCell::Inflated
        );
        assert_eq!(
            traversability.cells[index(23, 23)],
            TraversabilityCell::Inflated
        );
        assert_eq!(
            traversability.cells[index(21, 25)],
            TraversabilityCell::Free
        );
        assert_eq!(
            traversability.cells[index(22, 22)],
            TraversabilityCell::Free
        );
    }

    #[test]
    fn unknown_cells_stay_unknown_near_obstacles() {
        let mut occupancy = OccupancyGrid::centered_at([0.0, 0.0]);
        occupancy.cells_mut().fill(FREE);
        occupancy.cells_mut()[index(25, 25)] = OCCUPIED;
        occupancy.cells_mut()[index(24, 25)] = UNKNOWN;

        let traversability = occupancy.traversability(0.30);

        assert_eq!(
            traversability.cells[index(24, 25)],
            TraversabilityCell::Unknown
        );
        assert_eq!(
            traversability.cells[index(23, 25)],
            TraversabilityCell::Inflated
        );
    }

    #[test]
    fn occupied_cells_are_not_downgraded() {
        let mut occupancy = OccupancyGrid::centered_at([0.0, 0.0]);
        occupancy.cells_mut().fill(FREE);
        occupancy.cells_mut()[index(25, 25)] = OCCUPIED;
        occupancy.cells_mut()[index(26, 25)] = OCCUPIED;

        let traversability = occupancy.traversability(0.50);

        assert_eq!(
            traversability.cells[index(25, 25)],
            TraversabilityCell::Occupied
        );
        assert_eq!(
            traversability.cells[index(26, 25)],
            TraversabilityCell::Occupied
        );
        assert_eq!(
            traversability.cells[index(24, 25)],
            TraversabilityCell::Inflated
        );
    }

    #[test]
    fn inflation_zero_radius_produces_no_inflated_cells() {
        let mut occupancy = OccupancyGrid::centered_at([0.0, 0.0]);
        occupancy.cells_mut().fill(FREE);
        occupancy.cells_mut()[index(25, 25)] = OCCUPIED;

        let traversability = occupancy.traversability(0.0);

        assert!(
            traversability
                .cells
                .iter()
                .all(|cell| *cell != TraversabilityCell::Inflated)
        );
        assert_eq!(
            traversability.cells[index(25, 25)],
            TraversabilityCell::Occupied
        );
    }

    #[test]
    fn grid_dimensions_match_occupancy() {
        let occupancy = OccupancyGrid::centered_at([1.0, -1.0]);

        let traversability = occupancy.traversability(0.30);

        assert_eq!(traversability.width_cells, GRID_WIDTH_CELLS);
        assert_eq!(traversability.height_cells, GRID_HEIGHT_CELLS);
        assert_eq!(
            traversability.cells.len(),
            (GRID_WIDTH_CELLS * GRID_HEIGHT_CELLS) as usize
        );
        assert_eq!(traversability.origin_xy_m, [-1.5, -3.5]);
        assert_eq!(traversability.resolution.xy_m, GRID_RESOLUTION_M);
        assert_eq!(traversability.resolution.z_m, None);
    }

    fn index(x_cell: u32, y_cell: u32) -> usize {
        (y_cell * GRID_WIDTH_CELLS + x_cell) as usize
    }
}
