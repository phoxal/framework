//! `plan` - straight-line path planner boundary.
//!
//! This runtime subscribes to `mission/state` (for the active goal),
//! `localize/state`, and `map/revision`, then publishes `plan/path` and
//! `plan/state` for downstream controllers. The default planner is intentionally
//! honest: it interpolates a straight line from the current pose to the goal at a
//! fixed waypoint spacing and reports typed refusals when required inputs are
//! missing or unusable.
//!
//! When the mission is not Active, has no goal, or localization/map is missing,
//! it publishes an empty path and the matching `Refusal`; non-finite or otherwise
//! invalid inputs are refused as `Unreachable`. The emitted path carries the map
//! revision it was built against.

use anyhow::Result;
use phoxal::api::y2026_1 as api;
use phoxal::prelude::*;

const WAYPOINT_SPACING_M: f64 = 0.25;

#[derive(Clone, Debug, PartialEq)]
enum PlanDecision {
    Refused(api::plan::Refusal),
    Ready(api::plan::Path),
}

impl PlanDecision {
    fn decide(
        mission_state: Option<&api::mission::State>,
        localize: Option<&api::localize::LocalizationState>,
        map_revision: Option<&api::map::Revision>,
    ) -> Self {
        let Some(mission_state) = mission_state else {
            return Self::Refused(api::plan::Refusal::MissionInactive);
        };
        if mission_state.phase != api::mission::Phase::Active {
            return Self::Refused(api::plan::Refusal::MissionInactive);
        }
        let Some(goal) = mission_state.goal.as_ref() else {
            return Self::Refused(api::plan::Refusal::NoGoal);
        };
        let Some(localize) = localize else {
            return Self::Refused(api::plan::Refusal::NoLocalization);
        };
        let Some(map_revision) = map_revision else {
            return Self::Refused(api::plan::Refusal::NoMap);
        };
        if !valid_goal(goal) || !valid_localization(localize) || !valid_map_revision(map_revision) {
            return Self::Refused(api::plan::Refusal::Unreachable);
        }

        Self::Ready(api::plan::Path {
            poses: straight_line_path(localize, goal),
            map_revision: Some(map_revision.revision),
        })
    }

    fn state(&self) -> api::plan::State {
        match self {
            Self::Refused(refusal) => api::plan::State {
                has_path: false,
                refusal: Some(*refusal),
            },
            Self::Ready(_) => api::plan::State {
                has_path: true,
                refusal: None,
            },
        }
    }

    fn path(&self) -> api::plan::Path {
        match self {
            Self::Ready(path) => path.clone(),
            Self::Refused(_) => empty_path(),
        }
    }
}

#[derive(phoxal::Runtime)]
#[phoxal(id = "plan", api = y2026_1)]
struct Plan {
    latest_mission_state: Option<api::mission::State>,
    mission_state: Subscriber<api::mission::State>,
    localize: Latest<api::localize::LocalizationState>,
    map_revision: Latest<api::map::Revision>,
    path: Publisher<api::plan::Path>,
    state: Publisher<api::plan::State>,
}

#[phoxal::runtime]
impl Plan {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {
            latest_mission_state: None,
            mission_state: ctx
                .subscribe(api::topic::new().mission().state())
                .subscriber()
                .await?,
            localize: ctx
                .subscribe(api::topic::new().localize().state())
                .latest()
                .await?,
            map_revision: ctx
                .subscribe(api::topic::new().map().revision())
                .latest()
                .await?,
            path: ctx.publisher(api::topic::new().plan().path()).await?,
            state: ctx.publisher(api::topic::new().plan().state()).await?,
        })
    }

    #[step(hz = 10)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        while let Some(received) = self.mission_state.try_recv() {
            self.latest_mission_state = Some(received.body);
        }

        let localize = self.localize.latest();
        let map_revision = self.map_revision.latest();
        let decision = PlanDecision::decide(
            self.latest_mission_state.as_ref(),
            localize.as_ref(),
            map_revision.as_ref(),
        );

        self.path.publish_at(step.time(), decision.path()).await?;
        self.state.publish_at(step.time(), decision.state()).await?;
        Ok(())
    }
}

fn empty_path() -> api::plan::Path {
    api::plan::Path {
        poses: Vec::new(),
        map_revision: None,
    }
}

fn straight_line_path(
    start: &api::localize::LocalizationState,
    goal: &api::mission::Goal,
) -> Vec<api::plan::PathPose> {
    let dx = goal.x_m - start.x_m;
    let dy = goal.y_m - start.y_m;
    let distance = dx.hypot(dy);
    let final_yaw = goal.yaw_rad.or(Some(start.yaw_rad));

    if distance <= f64::EPSILON {
        return vec![api::plan::PathPose {
            x_m: goal.x_m,
            y_m: goal.y_m,
            yaw_rad: final_yaw,
        }];
    }

    let travel_yaw = dy.atan2(dx);
    let segments = (distance / WAYPOINT_SPACING_M).ceil().max(1.0) as usize;
    let mut poses = Vec::with_capacity(segments);
    for index in 1..=segments {
        let t = index as f64 / segments as f64;
        poses.push(api::plan::PathPose {
            x_m: start.x_m + dx * t,
            y_m: start.y_m + dy * t,
            yaw_rad: if index == segments {
                final_yaw.or(Some(travel_yaw))
            } else {
                Some(travel_yaw)
            },
        });
    }
    poses
}

fn valid_goal(goal: &api::mission::Goal) -> bool {
    goal.x_m.is_finite() && goal.y_m.is_finite() && goal.yaw_rad.is_none_or(f64::is_finite)
}

fn valid_localization(localize: &api::localize::LocalizationState) -> bool {
    localize.x_m.is_finite()
        && localize.y_m.is_finite()
        && localize.yaw_rad.is_finite()
        && localize.confidence.is_finite()
        && localize.confidence > 0.0
}

fn valid_map_revision(map_revision: &api::map::Revision) -> bool {
    map_revision.resolution_m.is_finite() && map_revision.resolution_m > 0.0
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<Plan>()
}

#[cfg(test)]
mod tests {
    use std::f64::consts::{FRAC_PI_4, PI};

    use phoxal::api::ContractBody;
    use phoxal::api::y2026_1 as api;

    use super::{Plan, PlanDecision, WAYPOINT_SPACING_M, empty_path, straight_line_path};

    const EPS: f64 = 1e-9;

    #[test]
    fn straight_line_path_reaches_goal() {
        let poses = straight_line_path(&localize(0.0, 0.0, 0.0), &goal(1.0, 0.0, Some(PI)));

        assert_close(poses.last().unwrap().x_m, 1.0);
        assert_close(poses.last().unwrap().y_m, 0.0);
        assert_eq!(poses.last().unwrap().yaw_rad, Some(PI));
    }

    #[test]
    fn straight_line_path_uses_requested_spacing() {
        let poses = straight_line_path(&localize(0.0, 0.0, 0.0), &goal(1.0, 0.0, None));

        assert_eq!(poses.len(), 4);
        let mut previous = (0.0, 0.0);
        for pose in poses {
            assert_close(distance(previous, (pose.x_m, pose.y_m)), WAYPOINT_SPACING_M);
            previous = (pose.x_m, pose.y_m);
        }
    }

    #[test]
    fn straight_line_path_intermediate_poses_face_travel() {
        let poses = straight_line_path(&localize(0.0, 0.0, 0.0), &goal(1.0, 1.0, Some(PI)));

        for pose in &poses[..poses.len() - 1] {
            assert_eq!(pose.yaw_rad, Some(FRAC_PI_4));
        }
        assert_eq!(poses.last().unwrap().yaw_rad, Some(PI));
    }

    #[test]
    fn straight_line_path_handles_zero_distance() {
        let poses = straight_line_path(&localize(1.0, 2.0, 0.75), &goal(1.0, 2.0, None));

        assert_eq!(
            poses,
            vec![api::plan::PathPose {
                x_m: 1.0,
                y_m: 2.0,
                yaw_rad: Some(0.75),
            }]
        );
    }

    #[test]
    fn decision_refuses_without_active_mission() {
        assert_eq!(
            PlanDecision::decide(None, None, None),
            PlanDecision::Refused(api::plan::Refusal::MissionInactive)
        );
        assert_eq!(
            PlanDecision::Refused(api::plan::Refusal::MissionInactive).state(),
            api::plan::State {
                has_path: false,
                refusal: Some(api::plan::Refusal::MissionInactive),
            }
        );
        assert_eq!(
            PlanDecision::Refused(api::plan::Refusal::MissionInactive).path(),
            empty_path()
        );
    }

    #[test]
    fn decision_refuses_paused_or_cleared_mission() {
        let paused = api::mission::State {
            phase: api::mission::Phase::Paused,
            goal: Some(goal(1.0, 0.0, None)),
            detail: None,
        };
        let active_without_goal = api::mission::State {
            phase: api::mission::Phase::Active,
            goal: None,
            detail: None,
        };

        assert_eq!(
            PlanDecision::decide(
                Some(&paused),
                Some(&localize(0.0, 0.0, 0.0)),
                Some(&revision(1))
            ),
            PlanDecision::Refused(api::plan::Refusal::MissionInactive)
        );
        assert_eq!(
            PlanDecision::decide(
                Some(&active_without_goal),
                Some(&localize(0.0, 0.0, 0.0)),
                Some(&revision(1))
            ),
            PlanDecision::Refused(api::plan::Refusal::NoGoal)
        );
    }

    #[test]
    fn decision_refuses_missing_localization() {
        assert_eq!(
            PlanDecision::decide(
                Some(&active_state(goal(1.0, 0.0, None))),
                None,
                Some(&revision(7))
            ),
            PlanDecision::Refused(api::plan::Refusal::NoLocalization)
        );
    }

    #[test]
    fn decision_refuses_missing_map() {
        assert_eq!(
            PlanDecision::decide(
                Some(&active_state(goal(1.0, 0.0, None))),
                Some(&localize(0.0, 0.0, 0.0)),
                None
            ),
            PlanDecision::Refused(api::plan::Refusal::NoMap)
        );
    }

    #[test]
    fn decision_refuses_invalid_inputs_as_unreachable() {
        let bad_goal = api::mission::Goal {
            x_m: f64::NAN,
            y_m: 0.0,
            yaw_rad: None,
        };

        assert_eq!(
            PlanDecision::decide(
                Some(&active_state(bad_goal)),
                Some(&localize(0.0, 0.0, 0.0)),
                Some(&revision(1))
            ),
            PlanDecision::Refused(api::plan::Refusal::Unreachable)
        );
    }

    #[test]
    fn decision_ready_builds_path_from_latest_revision() {
        let decision = PlanDecision::decide(
            Some(&active_state(goal(1.0, 0.0, Some(PI)))),
            Some(&localize(0.0, 0.0, 0.0)),
            Some(&revision(42)),
        );

        let PlanDecision::Ready(path) = decision else {
            panic!("expected ready path");
        };
        assert_eq!(path.map_revision, Some(42));
        assert!(!path.poses.is_empty());
        assert_eq!(path.poses.last().unwrap().yaw_rad, Some(PI));
    }

    #[test]
    fn emit_apis_reports_plan_contracts() {
        let json = phoxal::runtime::emit_apis_json::<Plan>();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["artifact"]["id"], "plan");
        assert_eq!(value["api_version"], "y2026_1");

        let contracts = value["required_contracts"].as_array().unwrap();
        assert_contract::<api::mission::State>(contracts, "subscribe");
        assert_contract::<api::localize::LocalizationState>(contracts, "subscribe");
        assert_contract::<api::map::Revision>(contracts, "subscribe");
        assert_contract::<api::plan::Path>(contracts, "publish");
        assert_contract::<api::plan::State>(contracts, "publish");
    }

    fn assert_contract<B>(contracts: &[serde_json::Value], direction: &str)
    where
        B: ContractBody,
    {
        assert!(contracts.iter().any(|c| {
            c["family"] == B::FAMILY && c["topic"] == B::TOPIC && c["direction"] == direction
        }));
    }

    fn goal(x_m: f64, y_m: f64, yaw_rad: Option<f64>) -> api::mission::Goal {
        api::mission::Goal { x_m, y_m, yaw_rad }
    }

    fn active_state(goal: api::mission::Goal) -> api::mission::State {
        api::mission::State {
            phase: api::mission::Phase::Active,
            goal: Some(goal),
            detail: None,
        }
    }

    fn localize(x_m: f64, y_m: f64, yaw_rad: f64) -> api::localize::LocalizationState {
        api::localize::LocalizationState {
            x_m,
            y_m,
            yaw_rad,
            confidence: 1.0,
        }
    }

    fn revision(revision: u64) -> api::map::Revision {
        api::map::Revision {
            revision,
            resolution_m: 0.05,
        }
    }

    fn distance(left: (f64, f64), right: (f64, f64)) -> f64 {
        let dx = left.0 - right.0;
        let dy = left.1 - right.1;
        dx.hypot(dy)
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= EPS,
            "{actual} differs from {expected}"
        );
    }
}
