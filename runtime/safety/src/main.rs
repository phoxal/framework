//! `safety` — the official battery + drive safety monitor.
//!
//! This official runtime targets API version `y2026_1`. It consumes
//! `battery` plus `drive/state`, and publishes the robot's aggregate
//! `safety/state` posture plus the current motion authorization.

use std::f64::consts::PI;

use phoxal::api::y2026_1 as api;
use phoxal::prelude::*;

const BATTERY_CRITICAL_RATIO: f32 = 0.10;
const BATTERY_LOW_RATIO: f32 = 0.25;
const AUTHORIZATION_TTL_NS: u64 = 100_000_000;

#[derive(phoxal::Runtime)]
#[phoxal(id = "safety", api = y2026_1)]
struct Safety {
    // Runtime-private typed state (not handles).
    last_battery: Option<(api::battery::State, u64)>,
    last_drive: Option<(api::drive::State, u64)>,
    // Handles.
    battery: Subscriber<api::battery::State>,
    drive: Subscriber<api::drive::State>,
    authorization: Publisher<api::safety::SafetyAuthorization>,
    state: Publisher<api::safety::Status>,
}

#[phoxal::runtime]
impl Safety {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        let battery = ctx
            .subscribe(api::topic::new().battery().state())
            .subscriber()
            .await?;
        let drive = ctx
            .subscribe(api::topic::new().drive().state())
            .subscriber()
            .await?;
        let authorization = ctx
            .publisher(api::topic::new().safety().authorization())
            .await?;
        let state = ctx.publisher(api::topic::new().safety().state()).await?;

        Ok(Self {
            last_battery: None,
            last_drive: None,
            battery,
            drive,
            authorization,
            state,
        })
    }

    #[step(hz = 10)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        while let Some(received) = self.battery.try_recv() {
            self.last_battery = Some((received.body, received.metadata.produced_at_ns));
        }
        while let Some(received) = self.drive.try_recv() {
            self.last_drive = Some((received.body, received.metadata.produced_at_ns));
        }

        let status = assess(
            self.last_battery.as_ref().map(|(body, _)| body),
            self.last_drive.as_ref().map(|(body, _)| body),
        );
        let authorization = authorize(&status, step.time().time_ns());
        self.authorization
            .publish_at(step.time(), authorization)
            .await?;
        self.state.publish_at(step.time(), status).await?;
        Ok(())
    }
}

/// Missing inputs contribute no concern in this first version; the monitor
/// fails open until it has an explicit battery or drive posture to evaluate.
fn assess(
    battery: Option<&api::battery::State>,
    drive: Option<&api::drive::State>,
) -> api::safety::Status {
    let mut active_reasons = Vec::new();
    let mut decision = api::safety::SafetyDecision::Allow;

    if let Some(battery) = battery {
        if battery.charge_ratio < BATTERY_CRITICAL_RATIO {
            active_reasons.push(reason(api::safety::SafetyReasonCode::BatteryCritical));
            decision = worst_decision(decision, api::safety::SafetyDecision::Stop);
        } else if battery.charge_ratio < BATTERY_LOW_RATIO {
            active_reasons.push(reason(api::safety::SafetyReasonCode::BatteryLow));
            decision = worst_decision(decision, api::safety::SafetyDecision::Slow);
        }
    }

    if let Some(drive) = drive {
        match drive.stop_reason {
            Some(api::drive::StopReason::Fault) => {
                active_reasons.push(reason(api::safety::SafetyReasonCode::DriveFault));
                decision = worst_decision(decision, api::safety::SafetyDecision::Stop);
            }
            Some(api::drive::StopReason::EmergencyStop) => {
                active_reasons.push(reason(api::safety::SafetyReasonCode::EmergencyStopEngaged));
                decision = worst_decision(decision, api::safety::SafetyDecision::EmergencyStop);
            }
            Some(api::drive::StopReason::NoTarget) | None => {}
        }
    }

    api::safety::Status {
        decision,
        active_reasons,
    }
}

fn authorize(status: &api::safety::Status, now_ns: u64) -> api::safety::SafetyAuthorization {
    api::safety::SafetyAuthorization {
        decision: status.decision,
        approved_motion: approved_motion(status.decision),
        reasons: status.active_reasons.clone(),
        source_revision: api::safety::SafetySourceRevision {
            localization: None,
            map: None,
        },
        expires_at_ns: Some(now_ns.saturating_add(AUTHORIZATION_TTL_NS)),
    }
}

fn approved_motion(decision: api::safety::SafetyDecision) -> api::safety::MotionConstraint {
    match decision {
        api::safety::SafetyDecision::Allow => motion_constraint(-1.0, 1.0, -PI, PI),
        api::safety::SafetyDecision::Slow => motion_constraint(-0.1, 0.1, -0.5, 0.5),
        api::safety::SafetyDecision::Stop
        | api::safety::SafetyDecision::EmergencyStop
        | api::safety::SafetyDecision::UnknownConservative => motion_constraint(0.0, 0.0, 0.0, 0.0),
    }
}

fn motion_constraint(
    linear_min: f64,
    linear_max: f64,
    angular_min: f64,
    angular_max: f64,
) -> api::safety::MotionConstraint {
    api::safety::MotionConstraint {
        linear_x_mps: api::safety::Constraint {
            min: linear_min,
            max: linear_max,
        },
        angular_z_radps: api::safety::Constraint {
            min: angular_min,
            max: angular_max,
        },
    }
}

fn reason(code: api::safety::SafetyReasonCode) -> api::safety::SafetyReason {
    api::safety::SafetyReason { code, detail: None }
}

fn worst_decision(
    current: api::safety::SafetyDecision,
    candidate: api::safety::SafetyDecision,
) -> api::safety::SafetyDecision {
    if decision_rank(candidate) > decision_rank(current) {
        candidate
    } else {
        current
    }
}

fn decision_rank(decision: api::safety::SafetyDecision) -> u8 {
    match decision {
        api::safety::SafetyDecision::Allow => 0,
        api::safety::SafetyDecision::Slow => 1,
        api::safety::SafetyDecision::Stop => 2,
        api::safety::SafetyDecision::EmergencyStop => 3,
        api::safety::SafetyDecision::UnknownConservative => 4,
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<Safety>()
}

#[cfg(test)]
mod tests {
    use phoxal::api::ContractBody;

    use super::{Safety, api, assess, authorize};

    #[test]
    fn nominal_when_all_healthy() {
        let status = assess(Some(&battery(0.9)), Some(&drive(None)));

        assert_eq!(status.decision, api::safety::SafetyDecision::Allow);
        assert!(status.active_reasons.is_empty());
    }

    #[test]
    fn battery_low_slows() {
        let status = assess(Some(&battery(0.2)), None);

        assert_eq!(status.decision, api::safety::SafetyDecision::Slow);
        assert_eq!(
            status.active_reasons[0].code,
            api::safety::SafetyReasonCode::BatteryLow
        );
    }

    #[test]
    fn battery_critical_stops() {
        let status = assess(Some(&battery(0.05)), None);

        assert_eq!(status.decision, api::safety::SafetyDecision::Stop);
        assert_eq!(
            status.active_reasons[0].code,
            api::safety::SafetyReasonCode::BatteryCritical
        );
    }

    #[test]
    fn drive_fault_stops() {
        let status = assess(None, Some(&drive(Some(api::drive::StopReason::Fault))));

        assert_eq!(status.decision, api::safety::SafetyDecision::Stop);
        assert!(
            status
                .active_reasons
                .iter()
                .any(|r| r.code == api::safety::SafetyReasonCode::DriveFault)
        );
    }

    #[test]
    fn drive_emergency_stop_engages_emergency_stop() {
        let status = assess(
            None,
            Some(&drive(Some(api::drive::StopReason::EmergencyStop))),
        );

        assert_eq!(status.decision, api::safety::SafetyDecision::EmergencyStop);
        assert!(
            status
                .active_reasons
                .iter()
                .any(|r| r.code == api::safety::SafetyReasonCode::EmergencyStopEngaged)
        );
    }

    #[test]
    fn worst_decision_wins() {
        let status = assess(
            Some(&battery(0.2)),
            Some(&drive(Some(api::drive::StopReason::Fault))),
        );

        assert_eq!(status.decision, api::safety::SafetyDecision::Stop);
        assert!(
            status
                .active_reasons
                .iter()
                .any(|r| r.code == api::safety::SafetyReasonCode::BatteryLow)
        );
        assert!(
            status
                .active_reasons
                .iter()
                .any(|r| r.code == api::safety::SafetyReasonCode::DriveFault)
        );
    }

    #[test]
    fn missing_inputs_are_nominal() {
        let status = assess(None, None);

        assert_eq!(status.decision, api::safety::SafetyDecision::Allow);
        assert!(status.active_reasons.is_empty());
    }

    #[test]
    fn authorization_matches_status() {
        let status = assess(Some(&battery(0.2)), None);
        let authorization = authorize(&status, 1_000);

        assert_eq!(authorization.decision, api::safety::SafetyDecision::Slow);
        assert_eq!(authorization.expires_at_ns, Some(100_001_000));
        assert_eq!(authorization.reasons, status.active_reasons);
        assert_eq!(authorization.approved_motion.linear_x_mps.max, 0.1);
    }

    #[test]
    fn emit_apis_reports_y2026_1_safety_contracts() {
        let json = phoxal::runtime::emit_apis_json::<Safety>();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["artifact"]["id"], "safety");
        assert_eq!(value["api_version"], "y2026_1");

        let contracts = value["required_contracts"].as_array().unwrap();
        assert_contract::<api::battery::State>(contracts, "subscribe");
        assert_contract::<api::drive::State>(contracts, "subscribe");
        assert_contract::<api::safety::SafetyAuthorization>(contracts, "publish");
        assert_contract::<api::safety::Status>(contracts, "publish");
    }

    fn assert_contract<B>(contracts: &[serde_json::Value], direction: &str)
    where
        B: ContractBody,
    {
        assert!(contracts.iter().any(|c| {
            c["family"] == B::FAMILY && c["topic"] == B::TOPIC && c["direction"] == direction
        }));
    }

    fn battery(charge_ratio: f32) -> api::battery::State {
        api::battery::State {
            voltage_v: 15.0,
            current_a: 1.0,
            charge_ratio,
        }
    }

    fn drive(stop_reason: Option<api::drive::StopReason>) -> api::drive::State {
        let target = api::drive::Target {
            linear_x_mps: 0.0,
            angular_z_radps: 0.0,
            curvature_limit_radpm: None,
        };

        api::drive::State {
            target: target.clone(),
            limited_target: target,
            actuator_authority: api::drive::ActuatorAuthority::Active,
            stop_reason,
        }
    }
}
