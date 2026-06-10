use serde::{Deserialize, Serialize};

use phoxal::api::v1::map::{Grid, OccupancyCell, Resolution};

pub const GRID_EXTENT_M: f64 = 5.0;
pub const GRID_RESOLUTION_M: f64 = 0.10;
pub const GRID_WIDTH_CELLS: u32 = 50;
pub const GRID_HEIGHT_CELLS: u32 = 50;

const UNKNOWN: u8 = 0;
const FREE: u8 = 1;
const OCCUPIED: u8 = 2;
const BEAM_STEP_M: f64 = GRID_RESOLUTION_M / 2.0;

#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::module_name_repetitions)]
pub struct OccupancyGrid {
    origin_xy_m: [f64; 2],
    cells: Vec<u8>,
}

impl OccupancyGrid {
    pub fn centered_at(anchor_xy_m: [f64; 2]) -> Self {
        Self {
            origin_xy_m: [
                anchor_xy_m[0] - GRID_EXTENT_M / 2.0,
                anchor_xy_m[1] - GRID_EXTENT_M / 2.0,
            ],
            cells: vec![UNKNOWN; (GRID_WIDTH_CELLS * GRID_HEIGHT_CELLS) as usize],
        }
    }

    pub fn integrate_ray(
        &mut self,
        origin_xy_m: [f64; 2],
        angle_rad: f64,
        clear_distance_m: f64,
        occupied_distance_m: Option<f64>,
    ) {
        if !origin_xy_m.iter().all(|value| value.is_finite())
            || !angle_rad.is_finite()
            || !clear_distance_m.is_finite()
            || clear_distance_m < 0.0
        {
            return;
        }

        let direction = [angle_rad.cos(), angle_rad.sin()];
        let mut traveled_m = 0.0;
        while traveled_m <= clear_distance_m {
            if let Some([x_cell, y_cell]) = self.world_xy_to_cell([
                origin_xy_m[0] + traveled_m * direction[0],
                origin_xy_m[1] + traveled_m * direction[1],
            ]) {
                self.mark_free(x_cell, y_cell);
            }
            traveled_m += BEAM_STEP_M;
        }

        let Some(occupied_distance_m) = occupied_distance_m else {
            return;
        };
        if !occupied_distance_m.is_finite() || occupied_distance_m < 0.0 {
            return;
        }
        if let Some([x_cell, y_cell]) = self.world_xy_to_cell([
            origin_xy_m[0] + occupied_distance_m * direction[0],
            origin_xy_m[1] + occupied_distance_m * direction[1],
        ]) {
            self.mark_occupied(x_cell, y_cell);
        }
    }

    pub fn to_snapshot(&self) -> OccupancySnapshot {
        OccupancySnapshot {
            width_cells: GRID_WIDTH_CELLS,
            height_cells: GRID_HEIGHT_CELLS,
            origin_xy_m: self.origin_xy_m,
            resolution_m: GRID_RESOLUTION_M,
            cells: self.cells.clone(),
        }
    }

    pub fn to_local_grid(&self) -> Grid<OccupancyCell> {
        Grid {
            origin_xy_m: self.origin_xy_m,
            resolution: Resolution {
                xy_m: GRID_RESOLUTION_M,
                z_m: None,
            },
            width_cells: GRID_WIDTH_CELLS,
            height_cells: GRID_HEIGHT_CELLS,
            cells: self.cells.iter().map(|cell| byte_to_cell(*cell)).collect(),
        }
    }

    /// Test-only access for scenario harnesses that need exact cell fixtures.
    ///
    /// Production mutation should stay centralized in the beam integrator.
    pub fn cells_mut(&mut self) -> &mut Vec<u8> {
        &mut self.cells
    }

    fn mark_free(&mut self, x_cell: i32, y_cell: i32) {
        let Some(index) = cell_index(x_cell, y_cell) else {
            return;
        };
        if self.cells[index] != OCCUPIED {
            self.cells[index] = FREE;
        }
    }

    fn mark_occupied(&mut self, x_cell: i32, y_cell: i32) {
        let Some(index) = cell_index(x_cell, y_cell) else {
            return;
        };
        self.cells[index] = OCCUPIED;
    }

    fn world_xy_to_cell(&self, world_xy_m: [f64; 2]) -> Option<[i32; 2]> {
        if !world_xy_m.iter().all(|value| value.is_finite()) {
            return None;
        }

        Some([
            ((world_xy_m[0] - self.origin_xy_m[0]) / GRID_RESOLUTION_M).floor() as i32,
            ((world_xy_m[1] - self.origin_xy_m[1]) / GRID_RESOLUTION_M).floor() as i32,
        ])
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::module_name_repetitions)]
pub struct OccupancySnapshot {
    pub width_cells: u32,
    pub height_cells: u32,
    pub origin_xy_m: [f64; 2],
    pub resolution_m: f64,
    pub cells: Vec<u8>,
}

fn cell_index(x_cell: i32, y_cell: i32) -> Option<usize> {
    if x_cell < 0
        || y_cell < 0
        || x_cell >= GRID_WIDTH_CELLS as i32
        || y_cell >= GRID_HEIGHT_CELLS as i32
    {
        return None;
    }

    Some((y_cell as u32 * GRID_WIDTH_CELLS + x_cell as u32) as usize)
}

fn byte_to_cell(cell: u8) -> OccupancyCell {
    match cell {
        UNKNOWN => OccupancyCell::Unknown,
        FREE => OccupancyCell::Free,
        OCCUPIED => OccupancyCell::Occupied,
        _ => OccupancyCell::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centered_grid_is_all_unknown() {
        let grid = OccupancyGrid::centered_at([0.0, 0.0]);

        assert_eq!(grid.origin_xy_m, [-2.5, -2.5]);
        assert_eq!(grid.cells.len(), 2500);
        assert!(grid.cells.iter().all(|cell| *cell == UNKNOWN));
    }

    #[test]
    fn beam_marks_free_then_occupied() {
        let mut grid = OccupancyGrid::centered_at([0.0, 0.0]);

        grid.integrate_ray([0.0, 0.0], 0.0, 1.0, Some(1.0));

        for x_cell in origin_x_cell()..terminal_x_cell(1.0) {
            assert_eq!(grid.cells[index(x_cell, sensor_y_cell())], FREE);
        }
        assert_eq!(
            grid.cells[index(terminal_x_cell(1.0), sensor_y_cell())],
            OCCUPIED
        );
    }

    #[test]
    fn occupied_is_sticky_after_free_pass() {
        let mut grid = OccupancyGrid::centered_at([0.0, 0.0]);

        grid.integrate_ray([0.0, 0.0], 0.0, 1.0, Some(1.0));
        grid.integrate_ray([0.0, 0.0], 0.0, 2.0, Some(2.0));

        assert_eq!(
            grid.cells[index(terminal_x_cell(1.0), sensor_y_cell())],
            OCCUPIED
        );
        assert_eq!(
            grid.cells[index(terminal_x_cell(2.0), sensor_y_cell())],
            OCCUPIED
        );
    }

    #[test]
    fn beam_outside_extent_marks_free_to_boundary_no_hit() {
        let mut grid = OccupancyGrid::centered_at([0.0, 0.0]);

        grid.integrate_ray([0.0, 0.0], 0.0, 50.0, None);

        for x_cell in origin_x_cell()..GRID_WIDTH_CELLS {
            assert_eq!(grid.cells[index(x_cell, sensor_y_cell())], FREE);
        }
        assert!(!grid.cells.contains(&OCCUPIED));
    }

    #[test]
    fn beam_with_invalid_distance_is_noop() {
        let mut grid = OccupancyGrid::centered_at([0.0, 0.0]);

        grid.integrate_ray([0.0, 0.0], 0.0, -1.0, Some(-1.0));
        grid.integrate_ray([f64::NAN, 0.0], 0.0, 1.0, Some(1.0));
        grid.integrate_ray([0.0, 0.0], f64::NAN, 1.0, Some(1.0));
        grid.integrate_ray([0.0, 0.0], 0.0, f64::NAN, Some(1.0));

        assert!(grid.cells.iter().all(|cell| *cell == UNKNOWN));
    }

    #[test]
    fn to_local_grid_round_trips_cell_codes() {
        let mut grid = OccupancyGrid::centered_at([1.0, -1.0]);

        grid.integrate_ray([1.0, -1.0], 0.0, 1.0, Some(1.0));
        let local_grid = grid.to_local_grid();

        assert_eq!(local_grid.origin_xy_m, [-1.5, -3.5]);
        assert_eq!(local_grid.resolution.xy_m, GRID_RESOLUTION_M);
        assert_eq!(local_grid.resolution.z_m, None);
        assert_eq!(local_grid.width_cells, GRID_WIDTH_CELLS);
        assert_eq!(local_grid.height_cells, GRID_HEIGHT_CELLS);
        for x_cell in origin_x_cell()..terminal_x_cell(1.0) {
            assert_eq!(
                local_grid.cells[index(x_cell, sensor_y_cell())],
                OccupancyCell::Free
            );
        }
        assert_eq!(
            local_grid.cells[index(terminal_x_cell(1.0), sensor_y_cell())],
            OccupancyCell::Occupied
        );
        assert_eq!(local_grid.cells[index(0, 0)], OccupancyCell::Unknown);
    }

    fn origin_x_cell() -> u32 {
        GRID_WIDTH_CELLS / 2
    }

    fn sensor_y_cell() -> u32 {
        GRID_HEIGHT_CELLS / 2
    }

    fn terminal_x_cell(distance_m: f64) -> u32 {
        origin_x_cell() + (distance_m / GRID_RESOLUTION_M) as u32
    }

    fn index(x_cell: u32, y_cell: u32) -> usize {
        (y_cell * GRID_WIDTH_CELLS + x_cell) as usize
    }
}
