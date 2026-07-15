use phoxal_api::v1 as api;

const WAYPOINT_SPACING_M: f64 = 0.25;

pub(crate) fn straight_line(
    start: &api::localize::LocalizationState,
    goal: &api::navigation::Pose,
    map_revision: Option<u64>,
) -> Option<api::navigation::Path> {
    if !valid_localization(start) || !valid_pose(goal) {
        return None;
    }

    let dx = goal.x_m - start.x_m;
    let dy = goal.y_m - start.y_m;
    let distance = dx.hypot(dy);
    let segments = (distance / WAYPOINT_SPACING_M).ceil().max(1.0) as usize;
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

pub(crate) fn valid_path(path: &api::navigation::Path) -> bool {
    !path.poses.is_empty() && path.poses.iter().all(valid_pose)
}

fn valid_pose(pose: &api::navigation::Pose) -> bool {
    pose.x_m.is_finite() && pose.y_m.is_finite() && pose.yaw_rad.is_none_or(f64::is_finite)
}

fn valid_localization(localization: &api::localize::LocalizationState) -> bool {
    localization.x_m.is_finite()
        && localization.y_m.is_finite()
        && localization.yaw_rad.is_finite()
        && localization.confidence.is_finite()
        && localization.confidence > 0.0
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
        )
        .unwrap();
        assert_eq!(path.poses.last().unwrap().x_m, 1.0);
        assert_eq!(path.poses.last().unwrap().yaw_rad, Some(0.5));
        assert_eq!(path.map_revision, Some(7));
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
