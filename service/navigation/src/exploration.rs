use phoxal::api;

use crate::frontiers::{OccupancyGrid, detect_frontiers};
use crate::scoring::score_frontiers;

pub(crate) fn next_frontier(
    request: &api::map::SubmapRequest,
    response: api::map::SubmapResponse,
    robot_xy_m: (f64, f64),
) -> Option<api::navigation::Frontier> {
    let grid = OccupancyGrid::from_submap(request, response)?;
    score_frontiers(detect_frontiers(&grid), &grid, robot_xy_m)
        .into_iter()
        .next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_cluster_size_and_scoring() {
        let request = api::map::SubmapRequest {
            min_x_m: 0.0,
            min_y_m: 0.0,
            max_x_m: 3.0,
            max_y_m: 2.0,
        };
        let response = api::map::SubmapResponse {
            width: 3,
            height: 2,
            resolution_m: 1.0,
            cells: vec![0, 0, 255, 0, 0, 255],
        };
        let frontier = next_frontier(&request, response, (0.5, 0.5)).unwrap();
        assert_eq!(frontier.size, 2);
        assert!(frontier.score > 0.0);
    }
}
