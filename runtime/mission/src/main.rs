//! `mission` — the official minimal go-to-goal mission controller.
//!
//! This scheduled runtime subscribes to `localize/state`, computes a simple
//! proportional pose-to-goal command, and publishes `drive/target`. It is the
//! first producer at the head of the control pipeline:
//! mission -> drive/target -> drive -> ddsm115 -> odometry -> localize -> mission.

use std::f64::consts::PI;

use phoxal::api::y2026_1 as api;
use phoxal::prelude::*;

const ARRIVAL_TOLERANCE_M: f64 = 0.05;
const MAX_LINEAR_MPS: f64 = 0.4;
const MAX_ANGULAR_RADPS: f64 = 1.5;
const K_LINEAR: f64 = 0.8;
const K_ANGULAR: f64 = 1.0;
const FACING_TOLERANCE_RAD: f64 = 0.5;

/// A localization fix older than this, or below [`MIN_CONFIDENCE`], is not trusted
/// to drive on — mission commands a stop instead. Without this, mission would keep
/// republishing *fresh* targets from a frozen pose, so `drive`'s own stale-target
/// guard would never trip and the robot could run open-loop on a dead localizer.
const FIX_STALE_NS: u64 = 1_000_000_000; // 1 s
const MIN_CONFIDENCE: f32 = 0.25;

/// Fixed first-version mission goal. Real missions will later load waypoints
/// from the bundle/config instead of compiling them into the runtime.
const GOAL: Goal = Goal { x_m: 1.0, y_m: 0.0 };

#[derive(Clone, Copy)]
struct Goal {
    x_m: f64,
    y_m: f64,
}

#[derive(phoxal::Runtime)]
#[phoxal(id = "mission", api = y2026_1)]
struct Mission {
    // Runtime-private typed state (not handles): latest fix + its production time.
    last_localize: Option<(api::localize::LocalizationState, u64)>,
    // Handles.
    localize: Subscriber<api::localize::LocalizationState>,
    target: Publisher<api::drive::Target>,
}

#[phoxal::runtime]
impl Mission {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        let localize = ctx
            .subscribe(api::topic::new().localize().state())
            .subscriber()
            .await?;
        let target = ctx.publisher(api::topic::new().drive().target()).await?;

        Ok(Self {
            last_localize: None,
            localize,
            target,
        })
    }

    #[step(hz = 20)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        while let Some(received) = self.localize.try_recv() {
            self.last_localize = Some((received.body, received.metadata.produced_at_ns));
        }

        let target = plan(
            self.last_localize.as_ref().map(|(pose, at)| (pose, *at)),
            step.time().time_ns(),
            GOAL,
        );

        self.target.publish_at(step.time(), target).await?;
        Ok(())
    }
}

/// Decide the command for the current fix: stop unless there is a fresh,
/// confident localization estimate to act on; otherwise run the go-to-goal
/// controller. Keeping this pure makes the trust gate unit-testable.
fn plan(
    fix: Option<(&api::localize::LocalizationState, u64)>,
    now_ns: u64,
    goal: Goal,
) -> api::drive::Target {
    let Some((pose, produced_at_ns)) = fix else {
        return stop_target();
    };
    if pose.confidence < MIN_CONFIDENCE {
        return stop_target();
    }
    if now_ns.saturating_sub(produced_at_ns) > FIX_STALE_NS {
        return stop_target();
    }
    control(pose, goal)
}

fn control(pose: &api::localize::LocalizationState, goal: Goal) -> api::drive::Target {
    let dx = goal.x_m - pose.x_m;
    let dy = goal.y_m - pose.y_m;
    let dist = dx.hypot(dy);

    if dist < ARRIVAL_TOLERANCE_M {
        return stop_target();
    }

    let desired_heading = dy.atan2(dx);
    let heading_err = normalize_angle(desired_heading - pose.yaw_rad);
    let angular = (K_ANGULAR * heading_err).clamp(-MAX_ANGULAR_RADPS, MAX_ANGULAR_RADPS);
    let linear = if heading_err.abs() < FACING_TOLERANCE_RAD {
        (K_LINEAR * dist).clamp(0.0, MAX_LINEAR_MPS)
    } else {
        0.0
    };

    api::drive::Target {
        linear_x_mps: linear as f32,
        angular_z_radps: angular as f32,
        curvature_limit_radpm: None,
    }
}

fn normalize_angle(angle_rad: f64) -> f64 {
    let two_pi = 2.0 * PI;
    let normalized = (angle_rad + PI).rem_euclid(two_pi) - PI;
    if normalized <= -PI { PI } else { normalized }
}

fn stop_target() -> api::drive::Target {
    api::drive::Target {
        linear_x_mps: 0.0,
        angular_z_radps: 0.0,
        curvature_limit_radpm: None,
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<Mission>()
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use phoxal::api::ContractBody;
    use phoxal::api::y2026_1 as api;

    use super::{
        ARRIVAL_TOLERANCE_M, FIX_STALE_NS, Goal, MAX_ANGULAR_RADPS, MAX_LINEAR_MPS, Mission,
        control, normalize_angle, plan,
    };

    const GOAL: Goal = Goal { x_m: 1.0, y_m: 0.0 };

    fn pose(x_m: f64, y_m: f64, yaw_rad: f64) -> api::localize::LocalizationState {
        api::localize::LocalizationState {
            x_m,
            y_m,
            yaw_rad,
            confidence: 1.0,
        }
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-6,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn arrived_commands_stop() {
        let target = control(&pose(1.0, 0.0, 1.25), Goal { x_m: 1.0, y_m: 0.0 });

        assert_eq!(target.linear_x_mps, 0.0);
        assert_eq!(target.angular_z_radps, 0.0);

        let near = control(
            &pose(1.0 - ARRIVAL_TOLERANCE_M / 2.0, 0.0, 0.0),
            Goal { x_m: 1.0, y_m: 0.0 },
        );
        assert_eq!(near.linear_x_mps, 0.0);
        assert_eq!(near.angular_z_radps, 0.0);
    }

    #[test]
    fn facing_goal_drives_forward() {
        let target = control(&pose(0.0, 0.0, 0.0), Goal { x_m: 1.0, y_m: 0.0 });

        assert!(target.linear_x_mps > 0.0);
        assert_close(f64::from(target.angular_z_radps), 0.0);
    }

    #[test]
    fn goal_behind_turns_in_place() {
        let target = control(
            &pose(0.0, 0.0, 0.0),
            Goal {
                x_m: -1.0,
                y_m: 0.0,
            },
        );

        assert_close(f64::from(target.linear_x_mps), 0.0);
        assert!(target.angular_z_radps.abs() > 0.0);
    }

    #[test]
    fn goal_to_the_left_turns_left() {
        let target = control(&pose(0.0, 0.0, 0.0), Goal { x_m: 0.0, y_m: 1.0 });

        assert!(target.angular_z_radps > 0.0);
        assert_close(f64::from(target.linear_x_mps), 0.0);
    }

    #[test]
    fn linear_and_angular_are_clamped() {
        let forward = control(
            &pose(0.0, 0.0, 0.0),
            Goal {
                x_m: 100.0,
                y_m: 0.0,
            },
        );
        assert_close(f64::from(forward.linear_x_mps), MAX_LINEAR_MPS);
        assert_close(f64::from(forward.angular_z_radps), 0.0);
        assert!(forward.linear_x_mps >= 0.0);

        let turning = control(
            &pose(0.0, 0.0, 0.0),
            Goal {
                x_m: 0.0,
                y_m: -100.0,
            },
        );
        assert_close(f64::from(turning.angular_z_radps), -MAX_ANGULAR_RADPS);
        assert!(f64::from(turning.angular_z_radps).abs() <= MAX_ANGULAR_RADPS);
        assert!(turning.linear_x_mps >= 0.0);
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
    fn no_fix_commands_stop() {
        let target = plan(None, 1_000_000_000, GOAL);
        assert_eq!(target.linear_x_mps, 0.0);
        assert_eq!(target.angular_z_radps, 0.0);
    }

    #[test]
    fn low_confidence_commands_stop() {
        let mut p = pose(0.0, 0.0, 0.0);
        p.confidence = 0.1; // below MIN_CONFIDENCE
        // Fresh fix, but untrusted → stop even though the goal is straight ahead.
        let target = plan(Some((&p, 1_000)), 1_000, GOAL);
        assert_eq!(target.linear_x_mps, 0.0);
        assert_eq!(target.angular_z_radps, 0.0);
    }

    #[test]
    fn stale_fix_commands_stop() {
        let p = pose(0.0, 0.0, 0.0); // confidence 1.0, goal straight ahead
        let produced_at_ns = 1_000;
        let now_ns = produced_at_ns + FIX_STALE_NS + 1;
        // A confident fix, but too old to act on → stop (drive would otherwise keep
        // getting fresh targets from a frozen pose).
        let target = plan(Some((&p, produced_at_ns)), now_ns, GOAL);
        assert_eq!(target.linear_x_mps, 0.0);
        assert_eq!(target.angular_z_radps, 0.0);

        // The same fix while still fresh drives forward.
        let fresh = plan(Some((&p, produced_at_ns)), produced_at_ns + 1, GOAL);
        assert!(fresh.linear_x_mps > 0.0);
    }

    #[test]
    fn emit_apis_reports_contracts() {
        let json = phoxal::runtime::emit_apis_json::<Mission>();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["artifact"]["id"], "mission");

        let contracts = value["required_contracts"].as_array().unwrap();
        assert!(contracts.iter().any(|c| {
            c["family"] == <api::localize::LocalizationState as ContractBody>::FAMILY
                && c["direction"] == "subscribe"
        }));
        assert!(contracts.iter().any(|c| {
            c["family"] == <api::drive::Target as ContractBody>::FAMILY
                && c["direction"] == "publish"
        }));
    }
}
