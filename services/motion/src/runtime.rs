#[cfg(test)]
use crate::contract::motion_api::Inputs as MotionInputs;
use phoxal::contracts::robotics::OdometryState;

use crate::config::{MotionConfig, validate_motion_config};
use crate::contract::{
    ApplyEmergencyResponse, ArmRequest, ControlMode, EmergencyRefusalReason, EmergencyRefused,
    MotionIntent, MotionStatus, ReleaseEmergencyRequest,
};
#[cfg(test)]
use crate::contract::{Constraint, ConstraintReason};
use crate::contract::{MotionConstraints, Permission};
#[cfg(test)]
use crate::drive::setpoint_from_twist;
use crate::drive::{setpoint_from_intent, stopped_setpoint};
use crate::validation;
use phoxal::contracts::Empty;
use phoxal::contracts::component::actuator::ActuatorSetpoint;
#[cfg(test)]
use phoxal::runtime::StepContext;
#[cfg(test)]
use phoxal::runtime::input::{Latest, Setpoint};
use phoxal::runtime::{Context, ExecutionTime, Leased, Observation};

const INPUT_MAX_AGE_MS: u64 = 100;

#[cfg(test)]
const SETPOINT_VALID_FOR_MS: u64 = 100;

/// The private authority selected by one motion invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArmedMode {
    Manual,
    Autonomous,
}

/// Private state retained by the serialized motion owner.
#[derive(Clone, Debug)]
pub struct ArbiterState {
    config: MotionConfig,
    armed_mode: Option<ArmedMode>,
    emergency_latched: bool,
    last_engaged_invocation: Option<u64>,
    engaged_this_invocation: bool,
    protective_state_clear: bool,
    measurement_available: bool,
    selected_owner_id: Option<String>,
    selected_intent: Option<MotionIntent>,
    actuator_setpoint: ActuatorSetpoint,
}

impl ArbiterState {
    fn new(config: MotionConfig) -> Self {
        Self {
            actuator_setpoint: stopped_setpoint(&config),
            config,
            armed_mode: None,
            emergency_latched: false,
            last_engaged_invocation: None,
            engaged_this_invocation: false,
            protective_state_clear: false,
            measurement_available: false,
            selected_owner_id: None,
            selected_intent: None,
        }
    }

    fn disarm(&mut self) {
        self.armed_mode = None;
        self.selected_owner_id = None;
        self.selected_intent = None;
        self.actuator_setpoint = stopped_setpoint(&self.config);
    }

    fn arm(&mut self, mode: ArmedMode, owner_id: String) {
        self.armed_mode = Some(mode);
        self.selected_owner_id = Some(owner_id);
    }
}

/// The official motion service implementation.
pub struct Motion {
    arbiter: ArbiterState,
}

#[phoxal::runtime(contract = crate::contract::MotionApi, period_ms = 20)]
impl Motion {
    #[init]
    fn new(config: MotionConfig) -> phoxal::Result<Self> {
        validate_motion_config(&config)?;
        Ok(Self {
            arbiter: ArbiterState::new(config),
        })
    }

    /// Arms one authenticated caller at its merged dispatch position.
    #[handle(arm)]
    fn arm(
        &mut self,
        ctx: &mut Context<'_, Self>,
        request: ArmRequest,
    ) -> phoxal::Result<ApplyEmergencyResponse> {
        let facts = fresh_facts(ctx);
        Ok(apply_arm(
            &mut self.arbiter,
            &request,
            ctx.command_source().unwrap_or_default(),
            facts,
            &ctx.manual(),
            &ctx.autonomous(),
            ctx.now(),
        ))
    }

    /// Disarms the selected authority at its merged dispatch position.
    #[handle(disarm)]
    fn disarm(
        &mut self,
        _ctx: &mut Context<'_, Self>,
        _request: Empty,
    ) -> phoxal::Result<ApplyEmergencyResponse> {
        self.arbiter.disarm();
        Ok(accepted())
    }

    /// Latches the protective emergency stop at its merged dispatch
    /// position.
    #[handle(engage_emergency)]
    fn engage(
        &mut self,
        ctx: &mut Context<'_, Self>,
        _request: Empty,
    ) -> phoxal::Result<ApplyEmergencyResponse> {
        self.arbiter.emergency_latched = true;
        self.arbiter.last_engaged_invocation = Some(ctx.invocation_index());
        Ok(accepted())
    }

    /// Releases a latched emergency under fresh protective evidence at its
    /// merged dispatch position.
    #[handle(release_emergency)]
    fn release(
        &mut self,
        ctx: &mut Context<'_, Self>,
        request: ReleaseEmergencyRequest,
    ) -> phoxal::Result<ApplyEmergencyResponse> {
        let facts = fresh_facts(ctx);
        Ok(apply_release(&mut self.arbiter, &request, facts))
    }

    #[step]
    fn arbitrate(&mut self, ctx: &mut Context<'_, Self>) -> phoxal::Result<()> {
        let facts = fresh_facts(ctx);
        self.arbiter.protective_state_clear = facts.protective_state_clear;
        self.arbiter.measurement_available = facts.measurement_available;
        let engaged_this_invocation =
            self.arbiter.last_engaged_invocation == Some(ctx.invocation_index());
        if engaged_this_invocation || self.arbiter.emergency_latched {
            self.arbiter.disarm();
        } else {
            select_and_limit_intent(
                &mut self.arbiter,
                &ctx.safety(),
                &ctx.manual(),
                &ctx.autonomous(),
                ctx.now(),
            );
        }

        validation::actuator_setpoint(
            &self.arbiter.actuator_setpoint,
            self.arbiter
                .config
                .left_wheels
                .iter()
                .chain(&self.arbiter.config.right_wheels)
                .map(|wheel| wheel.actuator_id.as_str()),
        )
        .map_err(|error| anyhow::anyhow!(error))?;

        // Materialize this invocation's engagement for the status
        // projection: handlers recorded the invocation that engaged, and
        // the projection encodes after this step.
        self.arbiter.engaged_this_invocation = engaged_this_invocation;
        Ok(())
    }

    /// Projects the final actuator intent with an independent validity bound.
    #[publish(actuators)]
    fn actuators(&self) -> Option<ActuatorSetpoint> {
        Some(self.arbiter.actuator_setpoint.clone())
    }

    /// Renews authority and protective status at each invocation so Safety
    /// can apply its freshness bound even while the robot remains disarmed.
    #[publish(status)]
    fn status(&self) -> MotionStatus {
        MotionStatus {
            mode: self
                .arbiter
                .armed_mode
                .map_or(ControlMode::Disarmed, |mode| match mode {
                    ArmedMode::Manual => ControlMode::Manual,
                    ArmedMode::Autonomous => ControlMode::Autonomous,
                }),
            emergency_latched: self.arbiter.emergency_latched,
            selected_owner_id: self.arbiter.selected_owner_id.clone(),
            protective_state_clear: self.arbiter.protective_state_clear,
            stopped: self.arbiter.actuator_setpoint == stopped_setpoint(&self.arbiter.config)
                || self.arbiter.engaged_this_invocation,
        }
    }
}

/// The input-derived facts one motion handler or step observes from the
/// same frozen cut the unified step refreshed before dispatch.
struct MotionFacts {
    invocation_index: u64,
    protective_state_clear: bool,
    measurement_available: bool,
}

fn fresh_facts(ctx: &Context<'_, Motion>) -> MotionFacts {
    let now = ctx.now();
    MotionFacts {
        invocation_index: ctx.invocation_index(),
        protective_state_clear: fresh_safety(&ctx.safety(), now)
            .is_some_and(|safety| safety_is_clear(safety, now)),
        measurement_available: fresh_measurement(&ctx.measurements(), now)
            .is_some_and(|measurement| measurement.available && valid_measurement(measurement)),
    }
}

fn fresh_safety<'a>(
    safety: &Observation<'a, MotionConstraints>,
    now: ExecutionTime,
) -> Option<&'a MotionConstraints> {
    safety.fresh_within(INPUT_MAX_AGE_MS).filter(|safety| {
        validation::constraints(safety).is_ok()
            && safety.valid_from_nanos <= now.as_nanos()
            && safety.expires_at_nanos > now.as_nanos()
            && fresh_capture(safety.oldest_capture_time_nanos, now)
    })
}

fn safety_is_clear(safety: &MotionConstraints, now: ExecutionTime) -> bool {
    validation::constraints(safety).is_ok()
        && safety.permission == Permission::Clear
        && safety.valid_from_nanos <= now.as_nanos()
        && safety.expires_at_nanos > now.as_nanos()
        && fresh_capture(safety.oldest_capture_time_nanos, now)
}

fn fresh_measurement<'a>(
    measurements: &Observation<'a, OdometryState>,
    now: ExecutionTime,
) -> Option<&'a OdometryState> {
    measurements
        .fresh_within(INPUT_MAX_AGE_MS)
        .filter(|measurement| fresh_capture(measurement.oldest_capture_time_nanos, now))
}

fn valid_measurement(measurement: &OdometryState) -> bool {
    (!measurement.available || measurement.oldest_capture_time_nanos.is_some())
        && measurement.x_m.is_finite()
        && measurement.y_m.is_finite()
        && measurement.yaw_rad.is_finite()
        && (-std::f64::consts::PI..=std::f64::consts::PI).contains(&measurement.yaw_rad)
        && measurement.linear_x_mps.is_finite()
        && measurement.angular_z_radps.is_finite()
}

fn fresh_capture(capture: Option<u64>, now: ExecutionTime) -> bool {
    capture
        .and_then(|capture| now.as_nanos().checked_sub(capture))
        .is_some_and(|age| age <= INPUT_MAX_AGE_MS.saturating_mul(1_000_000))
}

fn apply_arm(
    state: &mut ArbiterState,
    request: &ArmRequest,
    owner: &str,
    facts: MotionFacts,
    manual: &Leased<'_, MotionIntent>,
    autonomous: &Leased<'_, MotionIntent>,
    now: ExecutionTime,
) -> ApplyEmergencyResponse {
    if validation::arm_request(request).is_err() {
        state.emergency_latched = true;
        state.last_engaged_invocation = Some(facts.invocation_index);
        state.disarm();
        return refused(EmergencyRefusalReason::InvalidRequest);
    }
    let Some(mode) = armed_mode(request.mode) else {
        return refused(EmergencyRefusalReason::InvalidRequest);
    };
    if state.emergency_latched
        || !facts.protective_state_clear
        || !facts.measurement_available
        || !intent_matches(mode, owner, manual, autonomous, now)
    {
        return refused(EmergencyRefusalReason::ProtectiveState);
    }
    state.arm(mode, owner.to_owned());
    accepted()
}

fn apply_release(
    state: &mut ArbiterState,
    request: &ReleaseEmergencyRequest,
    facts: MotionFacts,
) -> ApplyEmergencyResponse {
    if validation::release_request(request).is_err() {
        return refused(EmergencyRefusalReason::InvalidRequest);
    }
    if !facts.protective_state_clear || !facts.measurement_available {
        return refused(EmergencyRefusalReason::ProtectiveState);
    }
    state.emergency_latched = false;
    state.disarm();
    accepted()
}

fn armed_mode(mode: ControlMode) -> Option<ArmedMode> {
    match mode {
        ControlMode::Manual => Some(ArmedMode::Manual),
        ControlMode::Autonomous => Some(ArmedMode::Autonomous),
        _ => None,
    }
}

fn intent_matches(
    mode: ArmedMode,
    owner_id: &str,
    manual: &Leased<'_, MotionIntent>,
    autonomous: &Leased<'_, MotionIntent>,
    _now: ExecutionTime,
) -> bool {
    let (intent, source, valid) = match mode {
        ArmedMode::Manual => (manual.value(), manual.source(), manual.valid()),
        ArmedMode::Autonomous => (autonomous.value(), autonomous.source(), autonomous.valid()),
    };
    intent
        .zip(source)
        .zip(valid)
        .is_some_and(|((_, source), _)| intent_owner_matches(owner_id, Some(source)))
}

fn intent_owner_matches(command_owner: &str, intent_source: Option<&str>) -> bool {
    // The supervisor publishes scenario setpoints as its virtual graph source.
    // Its external Commands ingress names that same authority supervisor.public.
    intent_source == Some(command_owner)
        || (command_owner == "supervisor.public" && intent_source == Some("supervisor"))
}

fn select_and_limit_intent(
    state: &mut ArbiterState,
    safety_view: &Observation<'_, MotionConstraints>,
    manual: &Leased<'_, MotionIntent>,
    autonomous: &Leased<'_, MotionIntent>,
    now: ExecutionTime,
) {
    let Some(safety) = fresh_safety(safety_view, now) else {
        state.disarm();
        return;
    };
    if !state.measurement_available
        || !matches!(safety.permission, Permission::Clear | Permission::Limited)
    {
        state.disarm();
        return;
    }
    let Some(mode) = state.armed_mode else {
        state.disarm();
        return;
    };
    let (intent, owner) = match mode {
        ArmedMode::Manual => (manual.valid(), manual.source()),
        ArmedMode::Autonomous => (autonomous.valid(), autonomous.source()),
    };
    let Some(intent) = intent
        .copied()
        .filter(|intent| validation::intent(intent).is_ok())
    else {
        state.disarm();
        return;
    };
    let Some(owner) = owner else {
        state.disarm();
        return;
    };
    if state
        .selected_owner_id
        .as_deref()
        .is_some_and(|selected_owner| !intent_owner_matches(selected_owner, Some(owner)))
    {
        state.disarm();
        return;
    }
    state.selected_owner_id = Some(owner.to_owned());
    state.selected_intent = Some(intent);
    let mut limited = intent;
    for constraint in &safety.constraints {
        if let Some(maximum) = constraint.max_linear_speed_mps {
            limited.linear_x_mps = limited.linear_x_mps.clamp(-maximum, maximum);
        }
        if let Some(maximum) = constraint.max_angular_speed_radps {
            limited.angular_z_radps = limited.angular_z_radps.clamp(-maximum, maximum);
        }
    }
    state.actuator_setpoint = setpoint_from_intent(&limited, &state.config);
}

fn accepted() -> ApplyEmergencyResponse {
    ApplyEmergencyResponse::Accepted
}

fn refused(reason: EmergencyRefusalReason) -> ApplyEmergencyResponse {
    ApplyEmergencyResponse::Refused(EmergencyRefused { reason })
}

#[cfg(test)]
/// Build a valid current manual intent for a direct Runtime test or adapter.
#[must_use]
pub fn manual_intent(
    owner_id: impl Into<String>,
    linear_x_mps: f64,
    angular_z_radps: f64,
    issued_at: ExecutionTime,
) -> Setpoint<MotionIntent> {
    Setpoint::from_source(
        MotionIntent {
            linear_x_mps,
            angular_z_radps,
        },
        owner_id,
        issued_at,
        SETPOINT_VALID_FOR_MS,
    )
}

#[cfg(test)]
/// Build a valid current autonomous intent for a direct Runtime test or
/// adapter.
#[must_use]
pub fn autonomous_intent(
    owner_id: impl Into<String>,
    linear_x_mps: f64,
    angular_z_radps: f64,
    issued_at: ExecutionTime,
) -> Setpoint<MotionIntent> {
    manual_intent(owner_id, linear_x_mps, angular_z_radps, issued_at)
}

#[cfg(test)]
/// Build a stamped safety constraints product for a direct Runtime test or
/// adapter.
#[must_use]
pub fn safety_state(protective_state_clear: bool, at: ExecutionTime) -> Latest<MotionConstraints> {
    let (permission, constraints) = if protective_state_clear {
        (Permission::Clear, Vec::new())
    } else {
        (
            Permission::Stopped,
            vec![Constraint {
                reason: ConstraintReason::WorldUnavailable,
                max_linear_speed_mps: None,
                max_angular_speed_radps: None,
                observed_value: Some(0.0),
            }],
        )
    };
    Latest::from_sample(phoxal::runtime::Sample::new(
        MotionConstraints {
            sequence: 1,
            permission,
            constraints,
            oldest_capture_time_nanos: Some(at.as_nanos()),
            valid_from_nanos: at.as_nanos(),
            expires_at_nanos: at.as_nanos().saturating_add(100_000_000),
        },
        phoxal::runtime::ObservationStamp::new("safety", at, None),
    ))
}

#[cfg(test)]
/// Build a stamped measurement for a direct Runtime test or adapter.
#[must_use]
pub fn measurement(at: ExecutionTime) -> Latest<OdometryState> {
    Latest::from_sample(phoxal::runtime::Sample::new(
        OdometryState {
            available: true,
            oldest_capture_time_nanos: Some(at.as_nanos()),
            ..Default::default()
        },
        phoxal::runtime::ObservationStamp::new("encoders", at, None),
    ))
}

#[cfg(test)]
mod tests {
    use phoxal::runtime::{Command, CommandId, CommandOrder, Commands, ExecutionDuration, invoke};

    use super::phoxal_runtime_motion;
    use super::*;

    fn new_arbiter(config: MotionConfig) -> Motion {
        Motion::new(config).expect("valid motion configuration")
    }

    fn step(
        service: Motion,
        context: &StepContext,
        inputs: &MotionInputs,
    ) -> (Motion, crate::contract::motion_api::Outputs) {
        invoke(
            &phoxal_runtime_motion::Adapter::new(),
            context,
            service,
            inputs,
        )
        .expect("the motion step accepts the inputs")
    }

    fn at(nanos: u64) -> ExecutionTime {
        ExecutionTime::from_nanos(nanos)
    }

    fn config() -> MotionConfig {
        MotionConfig {
            wheel_radius_m: 0.11,
            wheel_base_m: 0.6,
            left_wheels: vec![crate::config::WheelActuator {
                actuator_id: "left".into(),
                direction_sign: 1,
                gear_ratio: 1.0,
            }],
            right_wheels: vec![crate::config::WheelActuator {
                actuator_id: "right".into(),
                direction_sign: -1,
                gear_ratio: 1.0,
            }],
            max_linear_mps: 0.5,
            max_angular_radps: 1.5,
        }
    }

    fn inputs(
        manual: Setpoint<MotionIntent>,
        autonomous: Setpoint<MotionIntent>,
        calls: Vec<TestCall>,
    ) -> MotionInputs {
        let mut arm = Vec::new();
        let mut disarm = Vec::new();
        let mut engage_emergency = Vec::new();
        let mut release_emergency = Vec::new();
        for call in calls {
            match call {
                TestCall::Arm(command) => arm.push(command),
                TestCall::Disarm(command) => disarm.push(command),
                TestCall::EngageEmergency(command) => engage_emergency.push(command),
                TestCall::ReleaseEmergency(command) => release_emergency.push(command),
            }
        }
        MotionInputs {
            manual,
            autonomous,
            safety: safety_state(true, at(0)),
            measurements: measurement(at(0)),
            arm: Commands::new(arm),
            disarm: Commands::new(disarm),
            engage_emergency: Commands::new(engage_emergency),
            release_emergency: Commands::new(release_emergency),
        }
    }

    enum TestCall {
        Arm(Command<ArmRequest, ApplyEmergencyResponse>),
        Disarm(Command<Empty, ApplyEmergencyResponse>),
        EngageEmergency(Command<Empty, ApplyEmergencyResponse>),
        ReleaseEmergency(Command<ReleaseEmergencyRequest, ApplyEmergencyResponse>),
    }

    fn context(index: u64, nanos: u64) -> StepContext {
        StepContext::new(
            at(nanos),
            ExecutionDuration::from_millis(20),
            ExecutionDuration::from_millis(20),
            0,
            index,
        )
    }

    fn arm(id: u64, mode: ControlMode, owner_id: &str) -> TestCall {
        TestCall::Arm(Command::with_source_order(
            CommandOrder::new(0, 0, CommandId::new(id)),
            owner_id,
            ArmRequest { mode },
        ))
    }

    fn engage(order: CommandOrder) -> TestCall {
        TestCall::EngageEmergency(Command::with_order(order, Empty {}))
    }

    fn disarm(order: CommandOrder) -> TestCall {
        TestCall::Disarm(Command::with_order(order, Empty {}))
    }

    fn release(order: CommandOrder) -> TestCall {
        TestCall::ReleaseEmergency(Command::with_order(
            order,
            ReleaseEmergencyRequest {
                reset_token: "physical-reset".into(),
            },
        ))
    }

    #[test]
    fn four_wheel_actuation_is_complete_and_calibrated_in_both_directions() {
        let mut cfg = config();
        cfg.left_wheels.push(crate::config::WheelActuator {
            actuator_id: "left-rear".into(),
            direction_sign: -1,
            gear_ratio: 2.0,
        });
        cfg.right_wheels.push(crate::config::WheelActuator {
            actuator_id: "right-rear".into(),
            direction_sign: 1,
            gear_ratio: 3.0,
        });
        validate_motion_config(&cfg).unwrap();
        for direction in [-1.0, 1.0] {
            let output = setpoint_from_twist(0.22 * direction, 0.0, &cfg);
            validation::actuator_setpoint(&output, ["left", "left-rear", "right", "right-rear"])
                .unwrap();
            let expected = [2.0, -4.0, -2.0, 6.0];
            for (target, expected) in output.targets.iter().zip(expected) {
                assert_eq!(
                    target.control,
                    Some(
                        phoxal::contracts::component::actuator::Control::VelocityRadps(
                            expected * direction
                        )
                    )
                );
            }
        }
        assert!(
            stopped_setpoint(&cfg)
                .targets
                .iter()
                .all(|target| target.control
                    == Some(phoxal::contracts::component::actuator::Control::VelocityRadps(0.0)))
        );
        cfg.left_wheels.clear();
        assert!(validate_motion_config(&cfg).is_err());
    }

    #[test]
    fn body_velocity_uses_physical_wheel_radius() {
        let setpoint = setpoint_from_twist(0.22, 0.0, &config());
        assert_eq!(
            setpoint.targets[0].control,
            Some(phoxal::contracts::component::actuator::Control::VelocityRadps(2.0))
        );
    }

    #[test]
    fn yaw_signs_and_gearing_apply_once_at_the_motor_shaft() {
        let mut config = config();
        config.left_wheels[0].gear_ratio = 2.0;
        let setpoint = setpoint_from_twist(0.0, 1.0, &config);
        assert_eq!(
            setpoint.targets[0].control,
            Some(phoxal::contracts::component::actuator::Control::VelocityRadps(-0.3 / 0.11 * 2.0))
        );
        assert_eq!(
            setpoint.targets[1].control,
            Some(phoxal::contracts::component::actuator::Control::VelocityRadps(-0.3 / 0.11))
        );
        config.wheel_radius_m = 0.0;
        assert!(validate_motion_config(&config).is_err());
        config.wheel_radius_m = 0.11;
        config.left_wheels[0].direction_sign = 0;
        assert!(validate_motion_config(&config).is_err());
    }

    #[test]
    fn fresh_publications_cannot_rejuvenate_stale_capture_evidence_for_arming() {
        for stale_safety in [false, true] {
            for capture in [None, Some(0), Some(200_000_001)] {
                let now = at(200_000_000);
                let mut input = inputs(
                    manual_intent("operator", 0.2, 0.0, now),
                    Setpoint::withdrawn(),
                    vec![arm(1, ControlMode::Manual, "operator")],
                );
                input.safety = safety_state(true, now);
                input.measurements = measurement(now);
                let stamp = phoxal::runtime::ObservationStamp::new("derived", now, None);
                if stale_safety {
                    let mut value = input.safety.value().unwrap().clone();
                    value.oldest_capture_time_nanos = capture;
                    input.safety = Latest::from_sample(phoxal::runtime::Sample::new(value, stamp));
                } else {
                    let mut value = *input.measurements.value().unwrap();
                    value.oldest_capture_time_nanos = capture;
                    input.measurements =
                        Latest::from_sample(phoxal::runtime::Sample::new(value, stamp));
                }
                let initial = new_arbiter(config());
                let (state, _) = step(initial, &context(0, now.as_nanos()), &input);
                assert_eq!(state.status().mode, ControlMode::Disarmed);
                assert!(state.status().stopped);
            }
        }
        assert!(fresh_capture(Some(0), at(100_000_000)));
        assert!(!fresh_capture(Some(0), at(100_000_001)));
    }

    #[test]
    fn unavailable_odometry_cannot_arm_even_when_its_velocity_is_zero() {
        let initial = new_arbiter(config());
        let mut input = inputs(
            manual_intent("operator", 0.2, 0.0, at(0)),
            Setpoint::withdrawn(),
            vec![arm(1, ControlMode::Manual, "operator")],
        );
        input.measurements = Latest::from_sample(phoxal::runtime::Sample::new(
            OdometryState {
                available: false,
                ..Default::default()
            },
            phoxal::runtime::ObservationStamp::new("kinematics", at(0), None),
        ));
        let (state, _) = step(initial, &context(0, 0), &input);
        assert_eq!(state.status().mode, ControlMode::Disarmed);
    }

    #[test]
    fn protective_speed_limits_apply_to_armed_motion_but_do_not_authorize_arming() {
        let mut input = inputs(
            manual_intent("operator", 0.4, 0.0, at(0)),
            Setpoint::withdrawn(),
            vec![arm(1, ControlMode::Manual, "operator")],
        );
        let initial = new_arbiter(config());
        let (armed, _) = step(initial, &context(0, 0), &input);
        assert_eq!(armed.status().mode, ControlMode::Manual);
        let mut safety = input.safety.value().unwrap().clone();
        safety.permission = Permission::Limited;
        safety.constraints = vec![Constraint {
            reason: ConstraintReason::ObstacleProximity,
            max_linear_speed_mps: Some(0.15),
            max_angular_speed_radps: None,
            observed_value: Some(0.4),
        }];
        input.safety = Latest::from_sample(phoxal::runtime::Sample::new(
            safety,
            phoxal::runtime::ObservationStamp::new("safety", at(20_000_000), None),
        ));
        let fresh = new_arbiter(config());
        let (refused, _) = step(fresh, &context(0, 20_000_000), &input);
        assert_eq!(refused.status().mode, ControlMode::Disarmed);
        input.arm = Commands::default();
        let (limited, _) = step(armed, &context(1, 20_000_000), &input);
        assert_eq!(limited.status().mode, ControlMode::Manual);
        assert!(!limited.status().protective_state_clear);
        assert_eq!(
            limited.arbiter.actuator_setpoint.targets[0].control,
            Some(phoxal::contracts::component::actuator::Control::VelocityRadps(0.15 / 0.11))
        );
        input.safety = safety_state(false, at(40_000_000));
        let (stopped, _) = step(limited, &context(2, 40_000_000), &input);
        assert_eq!(stopped.status().mode, ControlMode::Disarmed);
        assert!(stopped.status().stopped);
    }

    #[test]
    fn restart_starts_disarmed_and_requires_explicit_arm() {
        let initial = new_arbiter(config());
        assert_eq!(initial.status().mode, ControlMode::Disarmed);

        let (state, outputs) = step(
            initial,
            &context(0, 0),
            &inputs(
                manual_intent("operator", 0.2, 0.0, at(0)),
                Setpoint::withdrawn(),
                vec![arm(1, ControlMode::Manual, "operator")],
            ),
        );
        assert_eq!(outputs.arm_replies.len(), 1);
        assert_eq!(state.status().mode, ControlMode::Manual);
    }

    #[test]
    fn invalid_arm_then_release_and_arm_keeps_the_current_invocation_stopped() {
        let initial = new_arbiter(config());
        let (state, outputs) = step(
            initial,
            &context(0, 0),
            &inputs(
                manual_intent("operator", 0.2, 0.0, at(0)),
                Setpoint::withdrawn(),
                vec![
                    arm(1, ControlMode::Unspecified, "operator"),
                    release(CommandOrder::new(0, 0, CommandId::new(2))),
                    arm(3, ControlMode::Manual, "operator"),
                ],
            ),
        );
        assert_eq!(outputs.arm_replies.len(), 2);
        assert_eq!(outputs.release_emergency_replies.len(), 1);
        assert_eq!(state.status().mode, ControlMode::Disarmed);
        assert!(state.status().stopped);
        assert_eq!(state.arbiter.actuator_setpoint, stopped_setpoint(&config()));
    }

    #[test]
    fn engage_then_release_in_one_batch_stops_and_does_not_rearm() {
        let initial = new_arbiter(config());
        let (state, _) = step(
            initial,
            &context(0, 0),
            &inputs(
                manual_intent("operator", 0.2, 0.0, at(0)),
                Setpoint::withdrawn(),
                vec![arm(1, ControlMode::Manual, "operator")],
            ),
        );
        let (state, outputs) = step(
            state,
            &context(1, 20_000_000),
            &inputs(
                manual_intent("operator", 0.2, 0.0, at(20_000_000)),
                Setpoint::withdrawn(),
                vec![
                    engage(CommandOrder::new(1, 0, CommandId::new(2))),
                    release(CommandOrder::new(1, 0, CommandId::new(3))),
                ],
            ),
        );
        assert_eq!(outputs.engage_emergency_replies.len(), 1);
        assert_eq!(outputs.release_emergency_replies.len(), 1);
        let status = state.status();
        assert_eq!(status.mode, ControlMode::Disarmed);
        assert!(!status.emergency_latched);
        assert!(status.stopped);
    }

    #[test]
    fn disarm_is_ordered_with_other_service_calls() {
        let initial = new_arbiter(config());
        let input = inputs(
            manual_intent("operator", 0.2, 0.0, at(0)),
            Setpoint::withdrawn(),
            vec![
                arm(1, ControlMode::Manual, "operator"),
                disarm(CommandOrder::new(1, 0, CommandId::new(2))),
            ],
        );

        let (state, outputs) = step(initial, &context(0, 0), &input);

        assert_eq!(outputs.arm_replies.len(), 1);
        assert_eq!(outputs.disarm_replies.len(), 1);
        assert_eq!(state.status().mode, ControlMode::Disarmed);
        assert!(state.status().stopped);
    }

    #[test]
    fn expired_intent_stops_without_falling_back_to_autonomy() {
        let initial = new_arbiter(config());
        let (state, _) = step(
            initial,
            &context(0, 0),
            &inputs(
                manual_intent("operator", 0.2, 0.0, at(0)),
                autonomous_intent("planner", 0.1, 0.0, at(0)),
                vec![arm(1, ControlMode::Manual, "operator")],
            ),
        );
        let (state, _) = step(
            state,
            &context(1, 200_000_000),
            &inputs(
                manual_intent("operator", 0.2, 0.0, at(0)),
                autonomous_intent("planner", 0.1, 0.0, at(0)),
                Vec::new(),
            ),
        );
        let status = state.status();
        assert_eq!(status.mode, ControlMode::Disarmed);
        assert!(status.stopped);
    }

    #[test]
    fn supervisor_scenario_setpoint_matches_authenticated_public_command() {
        assert!(super::intent_owner_matches(
            "supervisor.public",
            Some("supervisor")
        ));
        assert!(!super::intent_owner_matches(
            "operator-a",
            Some("supervisor")
        ));
        assert!(!super::intent_owner_matches(
            "supervisor.public",
            Some("operator-a")
        ));
    }

    #[test]
    fn owner_change_requires_a_new_explicit_arm() {
        let initial = new_arbiter(config());
        let (state, _) = step(
            initial,
            &context(0, 0),
            &inputs(
                manual_intent("operator-a", 0.2, 0.0, at(0)),
                Setpoint::withdrawn(),
                vec![arm(1, ControlMode::Manual, "operator-a")],
            ),
        );
        let (state, _) = step(
            state,
            &context(1, 20_000_000),
            &inputs(
                manual_intent("operator-b", 0.2, 0.0, at(20_000_000)),
                Setpoint::withdrawn(),
                Vec::new(),
            ),
        );
        let status = state.status();
        assert_eq!(status.mode, ControlMode::Disarmed);
        assert!(status.stopped);
    }

    #[test]
    fn config_validates_limits_and_unique_actuator_ids() {
        let mut invalid = config();
        invalid.max_linear_mps = 0.0;
        assert!(Motion::new(invalid).is_err());

        invalid = config();
        invalid.max_angular_radps = f64::NAN;
        assert!(Motion::new(invalid).is_err());

        invalid = config();
        invalid.left_wheels[0].actuator_id = invalid.right_wheels[0].actuator_id.clone();
        assert!(Motion::new(invalid).is_err());

        invalid = config();
        invalid.left_wheels[0].actuator_id.clear();
        assert!(Motion::new(invalid).is_err());
    }

    #[test]
    fn config_controls_limits_and_actuator_membership() {
        let mut custom = MotionConfig {
            max_linear_mps: 0.1,
            max_angular_radps: 0.2,
            ..config()
        };
        custom.left_wheels[0].actuator_id = "left-wheel".into();
        custom.right_wheels[0].actuator_id = "right-wheel".into();
        let initial = new_arbiter(custom);
        let (state, _) = step(
            initial,
            &context(0, 0),
            &inputs(
                manual_intent("operator", 0.5, 0.5, at(0)),
                Setpoint::withdrawn(),
                vec![arm(1, ControlMode::Manual, "operator")],
            ),
        );
        let setpoint = state.actuators().expect("setpoint projection");
        assert_eq!(setpoint.targets[0].actuator_id, "left-wheel");
        assert_eq!(setpoint.targets[1].actuator_id, "right-wheel");
        let left = match setpoint.targets[0].control.as_ref() {
            Some(phoxal::contracts::component::actuator::Control::VelocityRadps(value)) => *value,
            _ => panic!("left actuator must use velocity control"),
        };
        let right = match setpoint.targets[1].control.as_ref() {
            Some(phoxal::contracts::component::actuator::Control::VelocityRadps(value)) => *value,
            _ => panic!("right actuator must use velocity control"),
        };
        assert!((left - (0.1 - 0.2 * 0.3) / 0.11).abs() < 1e-12);
        assert!((right + (0.1 + 0.2 * 0.3) / 0.11).abs() < 1e-12);
    }

    #[test]
    fn malformed_emergency_input_latches_stop() {
        let initial = new_arbiter(config());
        let (state, outputs) = step(
            initial,
            &context(0, 0),
            &inputs(
                manual_intent("operator", 0.2, 0.0, at(0)),
                Setpoint::withdrawn(),
                vec![arm(1, ControlMode::Unspecified, "operator")],
            ),
        );
        assert!(matches!(
            outputs.arm_replies[0].response(),
            ApplyEmergencyResponse::Refused(_)
        ));
        let status = state.status();
        assert!(status.emergency_latched);
        assert!(status.stopped);
    }
}
