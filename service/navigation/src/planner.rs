use phoxal::api;

const WAYPOINT_SPACING_M: f64 = 0.25;
const MAP_QUERY_CELLS: f64 = 128.0;
pub(crate) const MAX_PATH_POSES: usize = 4096;
const MAX_PATH_EXTENT_M: f64 = WAYPOINT_SPACING_M * MAX_PATH_POSES as f64;

/// Return the finite planning envelope supported by the stock map query.
///
/// The map owner serves a 64x64 grid and navigation requests at most a
/// 128-cell window. The independent pose cap keeps a malformed revision from
/// turning a large but finite resolution into an allocation request.
pub(crate) fn planning_extent(resolution_m: f32) -> Option<f64> {
    let map_extent = f64::from(resolution_m) * MAP_QUERY_CELLS;
    (map_extent.is_finite() && map_extent > 0.0).then_some(map_extent.min(MAX_PATH_EXTENT_M))
}

pub(crate) fn straight_line(
    start: &api::localize::LocalizationState,
    goal: &api::navigation::Pose,
    map_revision: Option<u64>,
    max_extent_m: f64,
) -> Option<api::navigation::Path> {
    if !start.is_usable()
        || !valid_pose(goal)
        || !valid_xy(start.x_m, start.y_m, max_extent_m)
        || !valid_xy(goal.x_m, goal.y_m, max_extent_m)
    {
        return None;
    }

    let dx = goal.x_m - start.x_m;
    let dy = goal.y_m - start.y_m;
    let distance = dx.hypot(dy);
    if !distance.is_finite() || distance > max_extent_m {
        return None;
    }
    let segment_count = (distance / WAYPOINT_SPACING_M).ceil().max(1.0);
    if !segment_count.is_finite() || segment_count > MAX_PATH_POSES as f64 {
        return None;
    }
    let segments = segment_count as usize;
    let travel_yaw = dy.atan2(dx);
    let poses = (1..=segments)
        .map(|index| {
            let t = index as f64 / segments as f64;
            api::navigation::Pose {
                x_m: start.x_m + dx * t,
                y_m: start.y_m + dy * t,
                yaw_rad: if index == segments {
                    goal.yaw_rad.or(Some(start.yaw_rad))
                } else {
                    Some(travel_yaw)
                },
            }
        })
        .collect();

    Some(api::navigation::Path {
        poses,
        map_revision,
    })
}

pub(crate) fn valid_path(path: &api::navigation::Path, max_extent_m: f64) -> bool {
    if path.poses.is_empty() || path.poses.len() > MAX_PATH_POSES {
        return false;
    }
    if !path
        .poses
        .iter()
        .all(|pose| valid_pose(pose) && valid_xy(pose.x_m, pose.y_m, max_extent_m))
    {
        return false;
    }
    path.poses
        .windows(2)
        .try_fold(0.0, |extent, pair| {
            let segment = (pair[1].x_m - pair[0].x_m).hypot(pair[1].y_m - pair[0].y_m);
            let next = extent + segment;
            (segment.is_finite() && next.is_finite() && next <= max_extent_m).then_some(next)
        })
        .is_some()
}

fn valid_pose(pose: &api::navigation::Pose) -> bool {
    pose.x_m.is_finite() && pose.y_m.is_finite() && pose.yaw_rad.is_none_or(f64::is_finite)
}

fn valid_xy(x_m: f64, y_m: f64, max_extent_m: f64) -> bool {
    max_extent_m.is_finite()
        && max_extent_m > 0.0
        && x_m.abs() <= max_extent_m
        && y_m.abs() <= max_extent_m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn straight_line_reaches_the_goal() {
        let path = straight_line(
            &localization(0.0, 0.0),
            &api::navigation::Pose {
                x_m: 1.0,
                y_m: 0.0,
                yaw_rad: Some(0.5),
            },
            Some(7),
            planning_extent(0.05).unwrap(),
        )
        .unwrap();
        assert_eq!(path.poses.last().unwrap().x_m, 1.0);
        assert_eq!(path.poses.last().unwrap().yaw_rad, Some(0.5));
        assert_eq!(path.map_revision, Some(7));
    }

    #[test]
    fn far_goal_is_rejected_before_waypoint_allocation() {
        assert!(
            straight_line(
                &localization(0.0, 0.0),
                &api::navigation::Pose {
                    x_m: f64::MAX,
                    y_m: 0.0,
                    yaw_rad: None,
                },
                Some(7),
                planning_extent(0.05).unwrap(),
            )
            .is_none()
        );
    }

    #[test]
    fn follow_path_is_bounded_by_pose_count_and_extent() {
        let extent = planning_extent(0.05).unwrap();
        let oversized = api::navigation::Path {
            poses: vec![
                api::navigation::Pose {
                    x_m: 0.0,
                    y_m: 0.0,
                    yaw_rad: None,
                };
                MAX_PATH_POSES + 1
            ],
            map_revision: Some(7),
        };
        assert!(!valid_path(&oversized, extent));

        let too_long = api::navigation::Path {
            poses: vec![
                api::navigation::Pose {
                    x_m: 0.0,
                    y_m: 0.0,
                    yaw_rad: None,
                },
                api::navigation::Pose {
                    x_m: extent,
                    y_m: extent,
                    yaw_rad: None,
                },
            ],
            map_revision: Some(7),
        };
        assert!(!valid_path(&too_long, extent));
    }

    fn localization(x_m: f64, y_m: f64) -> api::localize::LocalizationState {
        api::localize::LocalizationState {
            x_m,
            y_m,
            yaw_rad: 0.0,
            confidence: 1.0,
        }
    }
}
