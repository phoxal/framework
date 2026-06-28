//! `follow` - the official pure-pursuit path follower.
//!
//! It consumes a planned path plus the current planar localization estimate and
//! publishes a body-twist follow target. `motion` arbitrates this target into the
//! final `drive/target`; this runtime never commands actuators directly.

use std::f64::consts::{FRAC_PI_2, PI};

use anyhow::Result;
use phoxal::api::y2026_1 as api;
use phoxal::prelude::*;

const DEFAULT_FRAME_ID: &str = "map";
const GOAL_TOLERANCE_M: f64 = 0.15;
const LOOKAHEAD_M: f64 = 0.40;
const K_LINEAR: f64 = 0.8;
const K_ANGULAR: f64 = 1.5;
const MAX_LINEAR_MPS: f64 = 0.5;
const MAX_ANGULAR_RADPS: f64 = 1.5;
const LOCALIZE_STALE_NS: u64 = 1_000_000_000;
const MIN_LOCALIZE_CONFIDENCE: f32 = 0.25;

#[derive(Clone)]
struct Timed<T> {
    body: T,
    produced_at_ns: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ControlOutput {
    linear_x_mps: f64,
    angular_z_radps: f64,
    target_index: Option<usize>,
    finished: bool,
}

struct FollowUpdate {
    state: api::follow::State,
    target: Option<api::follow::Target>,
}

#[derive(phoxal::Runtime)]
#[phoxal(id = "follow", api = y2026_1)]
struct Follow {
    // Runtime-private typed state.
    last_path: Option<Timed<api::plan::Path>>,
    last_localize: Option<Timed<api::localize::LocalizationState>>,
    // Handles.
    path: Subscriber<api::plan::Path>,
    localize: Subscriber<api::localize::LocalizationState>,
    target: Publisher<api::follow::Target>,
    state: Publisher<api::follow::State>,
}

#[phoxal::runtime]
impl Follow {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        let path = ctx
            .subscribe(api::topic::new().plan().path())
            .subscriber()
            .await?;
        let localize = ctx
            .subscribe(api::topic::new().localize().state())
            .subscriber()
            .await?;
        let target = ctx.publisher(api::topic::new().follow().target()).await?;
        let state = ctx.publisher(api::topic::new().follow().state()).await?;

        Ok(Self {
            last_path: None,
            last_localize: None,
            path,
            localize,
            target,
            state,
        })
    }

    #[step(hz = 20)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        while let Some(received) = self.path.try_recv() {
            self.last_path = Some(Timed {
                body: received.body,
                produced_at_ns: received.metadata.produced_at_ns,
            });
        }
        while let Some(received) = self.localize.try_recv() {
            self.last_localize = Some(Timed {
                body: received.body,
                produced_at_ns: received.metadata.produced_at_ns,
            });
        }

        let update = follow(
            self.last_path.as_ref(),
            self.last_localize.as_ref(),
            step.time().time_ns(),
        );
        if let Some(target) = update.target {
            self.target.publish_at(step.time(), target).await?;
        }
        self.state.publish_at(step.time(), update.state).await?;
        Ok(())
    }
}

fn follow(
    path: Option<&Timed<api::plan::Path>>,
    localize: Option<&Timed<api::localize::LocalizationState>>,
    now_ns: u64,
) -> FollowUpdate {
    let Some(path) = path else {
        return inactive_update(false);
    };
    let Some(localize) = localize else {
        return inactive_update(false);
    };
    if now_ns.saturating_sub(localize.produced_at_ns) > LOCALIZE_STALE_NS {
        return inactive_update(false);
    }
    if localize.body.confidence < MIN_LOCALIZE_CONFIDENCE {
        return inactive_update(false);
    }

    let output = pursue(&path.body.poses, &localize.body);
    FollowUpdate {
        state: state_from_output(output),
        target: Some(target_from_output(&path.body, output)),
    }
}

fn pursue(
    poses: &[api::plan::PathPose],
    localization: &api::localize::LocalizationState,
) -> ControlOutput {
    let Some(final_pose) = poses.last() else {
        return zero_control(None, true);
    };

    let robot_xy = [localization.x_m, localization.y_m];
    let final_index = poses.len() - 1;
    let dist_to_goal = distance(robot_xy, pose_xy(final_pose));
    if dist_to_goal <= GOAL_TOLERANCE_M {
        return zero_control(Some(final_index), true);
    }

    let target_index = lookahead_index(poses, robot_xy);
    let target = &poses[target_index];
    let dx = target.x_m - localization.x_m;
    let dy = target.y_m - localization.y_m;
    let target_heading = dy.atan2(dx);

    let forward_error = normalize_angle(target_heading - localization.yaw_rad);
    let reverse = forward_error.abs() > FRAC_PI_2;
    let (steer_error, direction) = if reverse {
        (
            normalize_angle(target_heading + PI - localization.yaw_rad),
            -1.0,
        )
    } else {
        (forward_error, 1.0)
    };

    let angular_z_radps = (K_ANGULAR * steer_error).clamp(-MAX_ANGULAR_RADPS, MAX_ANGULAR_RADPS);
    let alignment = steer_error.cos().max(0.0);
    let linear_x_mps =
        (direction * K_LINEAR * dist_to_goal * alignment).clamp(-MAX_LINEAR_MPS, MAX_LINEAR_MPS);

    ControlOutput {
        linear_x_mps,
        angular_z_radps,
        target_index: Some(target_index),
        finished: false,
    }
}

fn lookahead_index(poses: &[api::plan::PathPose], robot_xy: [f64; 2]) -> usize {
    let nearest = nearest_index(poses, robot_xy);
    let mut distance_along_path = distance(robot_xy, pose_xy(&poses[nearest]));
    if distance_along_path >= LOOKAHEAD_M {
        return nearest;
    }

    for index in (nearest + 1)..poses.len() {
        distance_along_path += distance(pose_xy(&poses[index - 1]), pose_xy(&poses[index]));
        if distance_along_path >= LOOKAHEAD_M {
            return index;
        }
    }
    poses.len() - 1
}

fn nearest_index(poses: &[api::plan::PathPose], robot_xy: [f64; 2]) -> usize {
    poses
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            squared_distance(robot_xy, pose_xy(a))
                .total_cmp(&squared_distance(robot_xy, pose_xy(b)))
        })
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn state_from_output(output: ControlOutput) -> api::follow::State {
    api::follow::State {
        active: !output.finished,
        target_index: output.target_index.map(|index| index as u32),
        finished: output.finished,
    }
}

fn target_from_output(path: &api::plan::Path, output: ControlOutput) -> api::follow::Target {
    api::follow::Target {
        map_revision: path.goal_revision,
        built_from_localize_revision: None,
        frame_id: DEFAULT_FRAME_ID.to_string(),
        linear_x_mps: output.linear_x_mps,
        angular_z_radps: output.angular_z_radps,
    }
}

fn inactive_update(finished: bool) -> FollowUpdate {
    FollowUpdate {
        state: api::follow::State {
            active: false,
            target_index: None,
            finished,
        },
        target: None,
    }
}

fn zero_control(target_index: Option<usize>, finished: bool) -> ControlOutput {
    ControlOutput {
        linear_x_mps: 0.0,
        angular_z_radps: 0.0,
        target_index,
        finished,
    }
}

fn pose_xy(pose: &api::plan::PathPose) -> [f64; 2] {
    [pose.x_m, pose.y_m]
}

fn distance(a: [f64; 2], b: [f64; 2]) -> f64 {
    squared_distance(a, b).sqrt()
}

fn squared_distance(a: [f64; 2], b: [f64; 2]) -> f64 {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    dx * dx + dy * dy
}

fn normalize_angle(angle_rad: f64) -> f64 {
    let two_pi = 2.0 * PI;
    let normalized = (angle_rad + PI).rem_euclid(two_pi) - PI;
    if normalized <= -PI { PI } else { normalized }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<Follow>()
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use phoxal::api::ContractBody;

    use super::{
        Follow, GOAL_TOLERANCE_M, LOCALIZE_STALE_NS, MAX_ANGULAR_RADPS, MAX_LINEAR_MPS, Timed, api,
        follow, lookahead_index, normalize_angle, pursue,
    };

    #[test]
    fn empty_path_is_finished() {
        let output = pursue(&[], &localize(0.0, 0.0, 0.0));

        assert!(output.finished);
        assert_eq!(output.target_index, None);
        assert_eq!(output.linear_x_mps, 0.0);
        assert_eq!(output.angular_z_radps, 0.0);
    }

    #[test]
    fn goal_within_tolerance_is_finished() {
        let output = pursue(
            &[path_pose(GOAL_TOLERANCE_M / 2.0, 0.0)],
            &localize(0.0, 0.0, 0.0),
        );

        assert!(output.finished);
        assert_eq!(output.target_index, Some(0));
        assert_eq!(output.linear_x_mps, 0.0);
    }

    #[test]
    fn pursuit_drives_forward_to_goal_ahead() {
        let output = pursue(&[path_pose(1.0, 0.0)], &localize(0.0, 0.0, 0.0));

        assert!(!output.finished);
        assert!(output.linear_x_mps > 0.0);
        assert!(output.angular_z_radps.abs() < 0.05);
    }

    #[test]
    fn pursuit_turns_toward_lateral_goal() {
        let output = pursue(&[path_pose(0.0, 2.0)], &localize(0.0, 0.0, 0.0));

        assert!(output.angular_z_radps > 0.0);
        assert!(output.linear_x_mps < MAX_LINEAR_MPS);
    }

    #[test]
    fn pursuit_reverses_to_goal_behind() {
        let output = pursue(&[path_pose(-1.0, 0.0)], &localize(0.0, 0.0, 0.0));

        assert!(!output.finished);
        assert!(output.linear_x_mps < 0.0);
        assert!(output.angular_z_radps.abs() < 0.1);
    }

    #[test]
    fn pursuit_caps_linear_and_angular() {
        let output = pursue(&[path_pose(100.0, 100.0)], &localize(0.0, 0.0, 0.0));

        assert!(output.linear_x_mps.abs() <= MAX_LINEAR_MPS);
        assert!(output.angular_z_radps.abs() <= MAX_ANGULAR_RADPS);
    }

    #[test]
    fn lookahead_starts_from_nearest_path_progress() {
        let poses = [
            path_pose(0.0, 0.0),
            path_pose(0.2, 0.0),
            path_pose(0.5, 0.0),
            path_pose(1.0, 0.0),
        ];

        assert_eq!(lookahead_index(&poses, [0.06, 0.0]), 2);
    }

    #[test]
    fn follow_requires_path_and_fresh_trusted_localization() {
        let path = timed(1_000, path(Some(9), vec![path_pose(1.0, 0.0)]));
        let fresh = timed(1_000, localize(0.0, 0.0, 0.0));

        assert!(follow(None, Some(&fresh), 1_000).target.is_none());
        assert!(follow(Some(&path), None, 1_000).target.is_none());

        let stale = follow(Some(&path), Some(&fresh), 1_000 + LOCALIZE_STALE_NS + 1);
        assert!(stale.target.is_none());
        assert!(!stale.state.active);

        let mut low_confidence = localize(0.0, 0.0, 0.0);
        low_confidence.confidence = 0.0;
        let low_confidence = timed(1_000, low_confidence);
        assert!(
            follow(Some(&path), Some(&low_confidence), 1_000)
                .target
                .is_none()
        );
    }

    #[test]
    fn follow_target_carries_path_revision_marker() {
        let path = timed(1_000, path(Some(42), vec![path_pose(1.0, 0.0)]));
        let localize = timed(1_000, localize(0.0, 0.0, 0.0));

        let update = follow(Some(&path), Some(&localize), 1_000);
        let target = update.target.unwrap();

        assert_eq!(target.map_revision, Some(42));
        assert_eq!(target.built_from_localize_revision, None);
        assert_eq!(target.frame_id, "map");
        assert!(update.state.active);
        assert_eq!(update.state.target_index, Some(0));
    }

    #[test]
    fn normalize_angle_range() {
        assert_close(normalize_angle(PI), PI);
        assert_close(normalize_angle(-PI), PI);
        assert_close(normalize_angle(3.0 * PI), PI);
        assert!(normalize_angle(PI + 0.25) > -PI);
        assert!(normalize_angle(PI + 0.25) <= PI);
    }

    #[test]
    fn emit_apis_reports_follow_contracts() {
        let json = phoxal::runtime::emit_apis_json::<Follow>();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["artifact"]["id"], "follow");
        assert_eq!(value["api_version"], "y2026_1");

        let contracts = value["required_contracts"].as_array().unwrap();
        assert_contract::<api::plan::Path>(contracts, "subscribe");
        assert_contract::<api::localize::LocalizationState>(contracts, "subscribe");
        assert_contract::<api::follow::Target>(contracts, "publish");
        assert_contract::<api::follow::State>(contracts, "publish");
    }

    fn assert_contract<B>(contracts: &[serde_json::Value], direction: &str)
    where
        B: ContractBody,
    {
        assert!(contracts.iter().any(|c| {
            c["family"] == B::FAMILY && c["topic"] == B::TOPIC && c["direction"] == direction
        }));
    }

    fn path_pose(x_m: f64, y_m: f64) -> api::plan::PathPose {
        api::plan::PathPose {
            x_m,
            y_m,
            yaw_rad: None,
        }
    }

    fn path(goal_revision: Option<u64>, poses: Vec<api::plan::PathPose>) -> api::plan::Path {
        api::plan::Path {
            poses,
            goal_revision,
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

    fn timed<T>(produced_at_ns: u64, body: T) -> Timed<T> {
        Timed {
            body,
            produced_at_ns,
        }
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected {expected}, got {actual}"
        );
    }
}
