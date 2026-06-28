//! `safety` — the official battery + drive safety monitor.
//!
//! This official runtime targets API version `y2026_1`. It consumes
//! required `battery`/`drive/state` inputs plus emergency-stop sources, and
//! publishes the robot's aggregate `safety/state` posture plus the current
//! motion authorization.

use std::f64::consts::PI;

use phoxal::api::y2026_1 as api;
use phoxal::model::component::v1::capability::Capability;
use phoxal::model::v1::Robot;
use phoxal::prelude::*;

const BATTERY_CRITICAL_RATIO: f32 = 0.10;
const BATTERY_LOW_RATIO: f32 = 0.25;
const AUTHORIZATION_TTL_NS: u64 = 100_000_000;
const SOURCE_FRESH_NS: u64 = 1_000_000_000;

#[derive(Clone)]
struct Timed<T> {
    body: T,
    produced_at_ns: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RequiredSources {
    battery: bool,
    drive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EmergencyStopBinding {
    component_id: String,
    capability_id: String,
}

struct SafetyInputs<'a> {
    required: RequiredSources,
    battery: Option<&'a Timed<api::battery::State>>,
    drive: Option<&'a Timed<api::drive::State>>,
    emergency_stop_engaged: bool,
}

#[derive(phoxal::Runtime)]
#[phoxal(id = "safety", api = y2026_1)]
struct Safety {
    // Runtime-private typed state (not handles).
    required: RequiredSources,
    last_battery: Option<Timed<api::battery::State>>,
    last_drive: Option<Timed<api::drive::State>>,
    software_estop_engaged: bool,
    component_estop_engaged: Vec<bool>,
    // Handles.
    battery: Subscriber<api::battery::State>,
    drive: Subscriber<api::drive::State>,
    software_estop: Subscriber<api::safety::EmergencyStopRequest>,
    component_estops: Vec<Subscriber<api::component::emergency_stop::State>>,
    authorization: Publisher<api::safety::SafetyAuthorization>,
    state: Publisher<api::safety::Status>,
}

#[phoxal::runtime]
impl Safety {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        let robot = ctx.robot()?;
        let required = required_sources(robot);
        let emergency_stop_bindings = emergency_stop_bindings(robot);

        let battery = ctx
            .subscribe(api::topic::new().battery().state())
            .subscriber()
            .await?;
        let drive = ctx
            .subscribe(api::topic::new().drive().state())
            .subscriber()
            .await?;
        let software_estop = ctx
            .subscribe(api::topic::new().safety().estop())
            .subscriber()
            .await?;
        let mut component_estops = Vec::with_capacity(emergency_stop_bindings.len());
        for binding in &emergency_stop_bindings {
            component_estops.push(
                ctx.subscribe(
                    api::topic::new()
                        .component(&binding.component_id)
                        .emergency_stop(&binding.capability_id)
                        .state(),
                )
                .subscriber()
                .await?,
            );
        }
        let authorization = ctx
            .publisher(api::topic::new().safety().authorization())
            .await?;
        let state = ctx.publisher(api::topic::new().safety().state()).await?;

        Ok(Self {
            required,
            last_battery: None,
            last_drive: None,
            software_estop_engaged: false,
            component_estop_engaged: vec![false; emergency_stop_bindings.len()],
            battery,
            drive,
            software_estop,
            component_estops,
            authorization,
            state,
        })
    }

    #[step(hz = 10)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        while let Some(received) = self.battery.try_recv() {
            self.last_battery = Some(Timed {
                body: received.body,
                produced_at_ns: received.metadata.produced_at_ns,
            });
        }
        while let Some(received) = self.drive.try_recv() {
            self.last_drive = Some(Timed {
                body: received.body,
                produced_at_ns: received.metadata.produced_at_ns,
            });
        }
        while let Some(received) = self.software_estop.try_recv() {
            self.software_estop_engaged = received.body.engaged;
        }
        for (index, subscriber) in self.component_estops.iter_mut().enumerate() {
            while let Some(received) = subscriber.try_recv() {
                self.component_estop_engaged[index] = received.body.engaged;
            }
        }

        let now_ns = step.time().time_ns();
        let inputs = SafetyInputs {
            required: self.required,
            battery: self.last_battery.as_ref(),
            drive: self.last_drive.as_ref(),
            emergency_stop_engaged: emergency_stop_engaged(
                self.software_estop_engaged,
                &self.component_estop_engaged,
            ),
        };
        let status = assess(&inputs, now_ns);
        let authorization = authorize(&status, &inputs, now_ns);
        self.authorization
            .publish_at(step.time(), authorization)
            .await?;
        self.state.publish_at(step.time(), status).await?;
        Ok(())
    }
}

fn required_sources(robot: &Robot) -> RequiredSources {
    RequiredSources {
        battery: robot_declares_battery(robot),
        drive: true,
    }
}

fn robot_declares_battery(robot: &Robot) -> bool {
    robot.manifest.components().iter().any(|(_, instance)| {
        robot
            .components
            .get(&instance.component)
            .is_some_and(|component| {
                component
                    .capabilities
                    .values()
                    .any(|capability| matches!(capability, Capability::Battery(_)))
            })
    })
}

fn emergency_stop_bindings(robot: &Robot) -> Vec<EmergencyStopBinding> {
    let mut bindings = robot
        .manifest
        .components()
        .iter()
        .filter_map(|(component_id, instance)| {
            robot
                .components
                .get(&instance.component)
                .map(|component| (component_id, component))
        })
        .flat_map(|(component_id, component)| {
            component
                .capabilities
                .iter()
                .filter(|(_, capability)| matches!(capability, Capability::EmergencyStop(_)))
                .map(|(capability_id, _)| EmergencyStopBinding {
                    component_id: component_id.clone(),
                    capability_id: capability_id.clone(),
                })
        })
        .collect::<Vec<_>>();
    bindings.sort_by(|left, right| {
        left.component_id
            .cmp(&right.component_id)
            .then_with(|| left.capability_id.cmp(&right.capability_id))
    });
    bindings
}

fn assess(inputs: &SafetyInputs<'_>, now_ns: u64) -> api::safety::Status {
    let mut active_reasons = Vec::new();
    let mut decision = api::safety::SafetyDecision::Allow;

    if inputs.emergency_stop_engaged {
        return api::safety::Status {
            decision: api::safety::SafetyDecision::EmergencyStop,
            active_reasons: vec![reason(api::safety::SafetyReasonCode::EmergencyStopEngaged)],
        };
    }

    if source_stale(inputs.required.battery, inputs.battery, now_ns) {
        active_reasons.push(source_stale_reason("battery/state"));
        decision = worst_decision(decision, api::safety::SafetyDecision::Stop);
    }
    if source_stale(inputs.required.drive, inputs.drive, now_ns) {
        active_reasons.push(source_stale_reason("drive/state"));
        decision = worst_decision(decision, api::safety::SafetyDecision::Stop);
    }

    if let Some(battery) = fresh_body(inputs.battery, now_ns) {
        if battery.charge_ratio < BATTERY_CRITICAL_RATIO {
            active_reasons.push(reason(api::safety::SafetyReasonCode::BatteryCritical));
            decision = worst_decision(decision, api::safety::SafetyDecision::Stop);
        } else if battery.charge_ratio < BATTERY_LOW_RATIO {
            active_reasons.push(reason(api::safety::SafetyReasonCode::BatteryLow));
            decision = worst_decision(decision, api::safety::SafetyDecision::Slow);
        }
    }

    if let Some(drive) = fresh_body(inputs.drive, now_ns) {
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

fn emergency_stop_engaged(software_estop_engaged: bool, component_estops: &[bool]) -> bool {
    software_estop_engaged || component_estops.iter().any(|engaged| *engaged)
}

fn authorize(
    status: &api::safety::Status,
    _inputs: &SafetyInputs<'_>,
    now_ns: u64,
) -> api::safety::SafetyAuthorization {
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

fn source_stale<T>(required: bool, timed: Option<&Timed<T>>, now_ns: u64) -> bool {
    required && fresh_body(timed, now_ns).is_none()
}

fn fresh_body<T>(timed: Option<&Timed<T>>, now_ns: u64) -> Option<&T> {
    let timed = timed?;
    (now_ns.saturating_sub(timed.produced_at_ns) <= SOURCE_FRESH_NS).then_some(&timed.body)
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

fn source_stale_reason(source: &str) -> api::safety::SafetyReason {
    api::safety::SafetyReason {
        code: api::safety::SafetyReasonCode::SourceStale,
        detail: Some(format!("{source} missing or stale")),
    }
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

    use super::{
        RequiredSources, SOURCE_FRESH_NS, Safety, SafetyInputs, Timed, api, assess, authorize,
    };

    const NOW_NS: u64 = 1_000_000_000;
    const REQUIRED: RequiredSources = RequiredSources {
        battery: true,
        drive: true,
    };

    #[test]
    fn nominal_when_all_healthy() {
        let battery = timed(NOW_NS, battery(0.9));
        let drive = timed(NOW_NS, drive(None));
        let inputs = inputs(REQUIRED, Some(&battery), Some(&drive), false);
        let status = assess(&inputs, NOW_NS);

        assert_eq!(status.decision, api::safety::SafetyDecision::Allow);
        assert!(status.active_reasons.is_empty());
    }

    #[test]
    fn battery_low_slows() {
        let battery = timed(NOW_NS, battery(0.2));
        let drive = timed(NOW_NS, drive(None));
        let inputs = inputs(REQUIRED, Some(&battery), Some(&drive), false);
        let status = assess(&inputs, NOW_NS);

        assert_eq!(status.decision, api::safety::SafetyDecision::Slow);
        assert_eq!(
            status.active_reasons[0].code,
            api::safety::SafetyReasonCode::BatteryLow
        );
    }

    #[test]
    fn battery_critical_stops() {
        let battery = timed(NOW_NS, battery(0.05));
        let drive = timed(NOW_NS, drive(None));
        let inputs = inputs(REQUIRED, Some(&battery), Some(&drive), false);
        let status = assess(&inputs, NOW_NS);

        assert_eq!(status.decision, api::safety::SafetyDecision::Stop);
        assert_eq!(
            status.active_reasons[0].code,
            api::safety::SafetyReasonCode::BatteryCritical
        );
    }

    #[test]
    fn drive_fault_stops() {
        let battery = timed(NOW_NS, battery(0.9));
        let drive = timed(NOW_NS, drive(Some(api::drive::StopReason::Fault)));
        let inputs = inputs(REQUIRED, Some(&battery), Some(&drive), false);
        let status = assess(&inputs, NOW_NS);

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
        let battery = timed(NOW_NS, battery(0.9));
        let drive = timed(NOW_NS, drive(Some(api::drive::StopReason::EmergencyStop)));
        let inputs = inputs(REQUIRED, Some(&battery), Some(&drive), false);
        let status = assess(&inputs, NOW_NS);

        assert_eq!(status.decision, api::safety::SafetyDecision::EmergencyStop);
        assert!(
            status
                .active_reasons
                .iter()
                .any(|r| r.code == api::safety::SafetyReasonCode::EmergencyStopEngaged)
        );
        assert_zero_motion(&authorize(&status, &inputs, NOW_NS).approved_motion);
    }

    #[test]
    fn worst_decision_wins() {
        let battery = timed(NOW_NS, battery(0.2));
        let drive = timed(NOW_NS, drive(Some(api::drive::StopReason::Fault)));
        let inputs = inputs(REQUIRED, Some(&battery), Some(&drive), false);
        let status = assess(&inputs, NOW_NS);

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
    fn emergency_stop_input_wins_and_authorizes_zero() {
        let battery = timed(NOW_NS, battery(0.2));
        let drive = timed(NOW_NS, drive(Some(api::drive::StopReason::Fault)));
        let inputs = inputs(REQUIRED, Some(&battery), Some(&drive), true);
        let status = assess(&inputs, NOW_NS);
        let authorization = authorize(&status, &inputs, NOW_NS);

        assert_eq!(status.decision, api::safety::SafetyDecision::EmergencyStop);
        assert_eq!(status.active_reasons.len(), 1);
        assert_eq!(
            status.active_reasons[0].code,
            api::safety::SafetyReasonCode::EmergencyStopEngaged
        );
        assert_eq!(
            authorization.decision,
            api::safety::SafetyDecision::EmergencyStop
        );
        assert_zero_motion(&authorization.approved_motion);
    }

    #[test]
    fn emergency_stop_latch_composes_software_and_component_sources() {
        assert!(super::emergency_stop_engaged(true, &[false, false]));
        assert!(super::emergency_stop_engaged(false, &[false, true]));
        assert!(!super::emergency_stop_engaged(false, &[false, false]));
    }

    #[test]
    fn missing_required_inputs_fail_closed() {
        let inputs = inputs(REQUIRED, None, None, false);
        let status = assess(&inputs, NOW_NS);
        let authorization = authorize(&status, &inputs, NOW_NS);

        assert_eq!(status.decision, api::safety::SafetyDecision::Stop);
        assert!(
            status
                .active_reasons
                .iter()
                .any(|r| r.code == api::safety::SafetyReasonCode::SourceStale)
        );
        assert_zero_motion(&authorization.approved_motion);
    }

    #[test]
    fn stale_required_source_fails_closed() {
        let now_ns = SOURCE_FRESH_NS + 1;
        let battery = timed(0, battery(0.9));
        let drive = timed(now_ns, drive(None));
        let inputs = inputs(REQUIRED, Some(&battery), Some(&drive), false);
        let status = assess(&inputs, now_ns);
        let authorization = authorize(&status, &inputs, now_ns);

        assert_eq!(status.decision, api::safety::SafetyDecision::Stop);
        assert!(
            status
                .active_reasons
                .iter()
                .any(|r| r.code == api::safety::SafetyReasonCode::SourceStale)
        );
        assert_zero_motion(&authorization.approved_motion);
    }

    #[test]
    fn optional_missing_battery_does_not_block_nominal_drive() {
        let drive = timed(NOW_NS, drive(None));
        let inputs = inputs(
            RequiredSources {
                battery: false,
                drive: true,
            },
            None,
            Some(&drive),
            false,
        );
        let status = assess(&inputs, NOW_NS);

        assert_eq!(status.decision, api::safety::SafetyDecision::Allow);
        assert!(status.active_reasons.is_empty());
    }

    #[test]
    fn authorization_matches_status() {
        let battery = timed(1_000, battery(0.2));
        let drive = timed(1_000, drive(None));
        let inputs = inputs(REQUIRED, Some(&battery), Some(&drive), false);
        let status = assess(&inputs, 1_000);
        let authorization = authorize(&status, &inputs, 1_000);

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
        assert_contract::<api::safety::EmergencyStopRequest>(contracts, "subscribe");
        assert_contract::<api::component::emergency_stop::State>(contracts, "subscribe");
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

    fn inputs<'a>(
        required: RequiredSources,
        battery: Option<&'a Timed<api::battery::State>>,
        drive: Option<&'a Timed<api::drive::State>>,
        emergency_stop_engaged: bool,
    ) -> SafetyInputs<'a> {
        SafetyInputs {
            required,
            battery,
            drive,
            emergency_stop_engaged,
        }
    }

    fn timed<T>(produced_at_ns: u64, body: T) -> Timed<T> {
        Timed {
            body,
            produced_at_ns,
        }
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

    fn assert_zero_motion(approved_motion: &api::safety::MotionConstraint) {
        assert_eq!(approved_motion.linear_x_mps.min, 0.0);
        assert_eq!(approved_motion.linear_x_mps.max, 0.0);
        assert_eq!(approved_motion.angular_z_radps.min, 0.0);
        assert_eq!(approved_motion.angular_z_radps.max, 0.0);
    }
}
