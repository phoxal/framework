use phoxal_api_explore::v1::{Frontier, GoalCandidate};
use phoxal_api_map::v1::{Grid, MapRevisionId, TraversabilityCell};
use phoxal_api_mission::v1::{GoalPose, GoalTolerance};

use crate::frontiers::cell_at_xy;

const DEFAULT_FRAME_ID: &str = "map";
const DISTINCT_CENTROID_EPSILON_M: f64 = 0.5;
const GOAL_TOLERANCE_POS_M: f64 = 0.25;
const GOAL_TOLERANCE_YAW_RAD: f64 = 0.3;

pub(crate) fn score_candidates(
    frontiers: &[Frontier],
    grid: &Grid<TraversabilityCell>,
    robot_xy_m: [f64; 2],
    map_revision: MapRevisionId,
    last_centroids: &[[f64; 2]],
) -> Vec<GoalCandidate> {
    let mut candidates = frontiers
        .iter()
        .filter_map(|frontier| {
            let centroid = centroid(&frontier.points_xy_m)?;
            if !is_reachable(grid, centroid) || recently_emitted(centroid, last_centroids) {
                return None;
            }

            let distance_m = distance(robot_xy_m, centroid);
            let score = (frontier.points_xy_m.len() as f64) / (1.0 + distance_m);
            let yaw_rad = (centroid[1] - robot_xy_m[1]).atan2(centroid[0] - robot_xy_m[0]);
            Some(GoalCandidate {
                id: format!("candidate_{}", frontier.id),
                goal: GoalPose::Pose2 {
                    frame_id: frontier_frame(frontier),
                    map_revision: Some(map_revision),
                    xy_m: centroid,
                    yaw_rad,
                },
                tolerance: GoalTolerance {
                    pos_m: GOAL_TOLERANCE_POS_M,
                    yaw_rad: Some(GOAL_TOLERANCE_YAW_RAD),
                },
                score,
            })
        })
        .collect::<Vec<_>>();

    candidates.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.id.cmp(&right.id))
    });
    candidates
}

pub(crate) fn candidate_centroids(candidates: &[GoalCandidate]) -> Vec<[f64; 2]> {
    candidates
        .iter()
        .filter_map(|candidate| match &candidate.goal {
            GoalPose::Pose2 { xy_m, .. } => Some(*xy_m),
            GoalPose::Pose3 { .. } => None,
        })
        .collect()
}

fn frontier_frame(frontier: &Frontier) -> String {
    if frontier.frame_id.is_empty() {
        DEFAULT_FRAME_ID.to_string()
    } else {
        frontier.frame_id.clone()
    }
}

fn is_reachable(grid: &Grid<TraversabilityCell>, centroid: [f64; 2]) -> bool {
    cell_at_xy(grid, centroid) == Some(TraversabilityCell::Free)
}

fn recently_emitted(centroid: [f64; 2], last_centroids: &[[f64; 2]]) -> bool {
    last_centroids
        .iter()
        .any(|last| distance(*last, centroid) < DISTINCT_CENTROID_EPSILON_M)
}

fn centroid(points_xy_m: &[[f64; 2]]) -> Option<[f64; 2]> {
    if points_xy_m.is_empty() {
        return None;
    }

    let mut sum = [0.0, 0.0];
    for point in points_xy_m {
        sum[0] += point[0];
        sum[1] += point[1];
    }
    let count = points_xy_m.len() as f64;
    Some([sum[0] / count, sum[1] / count])
}

fn distance(left: [f64; 2], right: [f64; 2]) -> f64 {
    let dx = left[0] - right[0];
    let dy = left[1] - right[1];
    (dx * dx + dy * dy).sqrt()
}

#[cfg(test)]
mod tests {
    use phoxal_api_explore::v1::Frontier;
    use phoxal_api_map::v1::{Grid, MapRevisionId, Resolution, TraversabilityCell};
    use phoxal_api_mission::v1::GoalPose;

    use super::score_candidates;

    #[test]
    fn candidates_are_ordered_by_distance_penalized_score() {
        let candidates = score_candidates(
            &[
                frontier(
                    "far_large",
                    vec![[5.5, 0.5], [5.5, 1.5], [5.5, 2.5], [5.5, 3.5]],
                ),
                frontier("near_small", vec![[1.5, 0.5], [1.5, 1.5]]),
            ],
            &grid(8, 8, TraversabilityCell::Free),
            [0.5, 0.5],
            map_revision(),
            &[],
        );

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].id, "candidate_near_small");
        assert_eq!(candidates[1].id, "candidate_far_large");
    }

    #[test]
    fn candidates_with_non_free_centroid_cells_are_filtered() {
        let mut grid = grid(4, 4, TraversabilityCell::Free);
        grid.cells[5] = TraversabilityCell::Inflated;

        let candidates = score_candidates(
            &[frontier("inflated_centroid", vec![[1.5, 1.5]])],
            &grid,
            [0.5, 0.5],
            map_revision(),
            &[],
        );

        assert!(candidates.is_empty());
    }

    #[test]
    fn near_duplicate_of_last_centroid_is_filtered() {
        let candidates = score_candidates(
            &[
                frontier("repeat", vec![[1.2, 1.2]]),
                frontier("distinct", vec![[3.5, 1.5]]),
            ],
            &grid(5, 5, TraversabilityCell::Free),
            [0.5, 0.5],
            map_revision(),
            &[[1.0, 1.0]],
        );

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].id, "candidate_distinct");
    }

    #[test]
    fn candidate_goal_carries_map_revision_and_tolerance() {
        let revision = map_revision();
        let candidates = score_candidates(
            &[frontier("target", vec![[2.5, 0.5]])],
            &grid(4, 4, TraversabilityCell::Free),
            [0.5, 0.5],
            revision,
            &[],
        );

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].tolerance.pos_m, 0.25);
        assert_eq!(candidates[0].tolerance.yaw_rad, Some(0.3));
        match &candidates[0].goal {
            GoalPose::Pose2 {
                frame_id,
                map_revision,
                xy_m,
                ..
            } => {
                assert_eq!(frame_id, "map");
                assert_eq!(*map_revision, Some(revision));
                assert_eq!(*xy_m, [2.5, 0.5]);
            }
            GoalPose::Pose3 { .. } => panic!("explore candidates must be planar goals"),
        }
    }

    fn frontier(id: &str, points_xy_m: Vec<[f64; 2]>) -> Frontier {
        Frontier {
            id: id.to_string(),
            frame_id: "map".to_string(),
            points_xy_m,
        }
    }

    fn grid(
        width_cells: u32,
        height_cells: u32,
        fill: TraversabilityCell,
    ) -> Grid<TraversabilityCell> {
        Grid {
            origin_xy_m: [0.0, 0.0],
            resolution: Resolution {
                xy_m: 1.0,
                z_m: None,
            },
            width_cells,
            height_cells,
            cells: vec![fill; (width_cells * height_cells) as usize],
        }
    }

    const fn map_revision() -> MapRevisionId {
        MapRevisionId {
            epoch: 3,
            sequence: 9,
        }
    }
}
