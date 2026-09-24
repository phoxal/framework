use crate::api::__contracts::phoxal::kinematics::v1::OdometryState;
#[cfg(test)]
use crate::api::motion::v1::actuator_target;
use crate::api::motion::v1::{
    ActuatorSetpoint, ApplyEmergencyResponse, ArmRequest, ControlMode, EmergencyAccepted,
    EmergencyRefusalReason, EmergencyRefused, MotionIntent, MotionStatus, ReleaseEmergencyRequest,
    apply_emergency_response, motion,
};
#[cfg(test)]
use crate::api::motion::v1::{Constraint, ConstraintReason};
use crate::api::motion::v1::{MotionConstraints, Permission};
use crate::config::{MotionConfig, validate_motion_config};
#[cfg(test)]
use crate::drive::setpoint_from_twist;
use crate::drive::{setpoint_from_intent, stopped_setpoint};
use crate::inputs::MotionInputs;
use crate::outputs::MotionOutputs;
use crate::validation;
use phoxal::contract::Empty;
#[cfg(test)]
use phoxal::runtime::input::{Latest, Setpoint};
use phoxal::runtime::{ExecutionTime, InitContext, Runtime, StepContext};

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
#[derive(Clone, Copy, Debug, Default)]
pub struct Motion;

#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for Motion {
    type Config = MotionConfig;
    type State = ArbiterState;
    type Inputs = MotionInputs;
    type Outputs = MotionOutputs;

    fn validate_config(config: &Self::Config) -> phoxal::Result<()> {
        validate_motion_config(config)
    }

    fn init(&self, _ctx: &InitContext, config: Self::Config) -> phoxal::Result<Self::State> {
        Ok(ArbiterState::new(config))
    }

    fn step(
        &self,
        ctx: &StepContext,
        mut state: Self::State,
        inputs: &Self::Inputs,
    ) -> phoxal::Result<(Self::State, Self::Outputs)> {
        inputs
            .arm
            .validate_order()
            .map_err(|error| anyhow::anyhow!(error))?;
        inputs
            .disarm
            .validate_order()
            .map_err(|error| anyhow::anyhow!(error))?;
        inputs
            .engage_emergency
            .validate_order()
            .map_err(|error| anyhow::anyhow!(error))?;
        inputs
            .release_emergency
            .validate_order()
            .map_err(|error| anyhow::anyhow!(error))?;

        state.engaged_this_invocation = false;
        state.protective_state_clear = fresh_safety(inputs, ctx.now())
            .is_some_and(|safety| safety_is_clear(safety, ctx.now()));
        state.measurement_available = fresh_measurement(inputs, ctx.now())
            .is_some_and(|measurement| measurement.available && valid_measurement(measurement));
        let mut outputs = MotionOutputs::default();
        let protective_state_clear = state.protective_state_clear;
        let measurement_available = state.measurement_available;

        let mut calls = Vec::new();
        calls.extend(inputs.arm.items().iter().map(MotionCall::Arm));
        calls.extend(inputs.disarm.items().iter().map(MotionCall::Disarm));
        calls.extend(
            inputs
                .engage_emergency
                .items()
                .iter()
                .map(MotionCall::EngageEmergency),
        );
        calls.extend(
            inputs
                .release_emergency
                .items()
                .iter()
                .map(MotionCall::ReleaseEmergency),
        );
        calls.sort_by_key(MotionCall::order);
        for call in calls {
            match call {
                MotionCall::Arm(command) => {
                    let response = apply_arm(
                        &mut state,
                        command.request(),
                        command.source(),
                        protective_state_clear,
                        measurement_available,
                        inputs,
                        ctx.now(),
                    );
                    outputs.arm_replies.push(command.reply(response));
                }
                MotionCall::Disarm(command) => {
                    state.disarm();
                    outputs.disarm_replies.push(command.reply(accepted()));
                }
                MotionCall::EngageEmergency(command) => {
                    state.emergency_latched = true;
                    state.engaged_this_invocation = true;
                    outputs
                        .engage_emergency_replies
                        .push(command.reply(accepted()));
                }
                MotionCall::ReleaseEmergency(command) => {
                    let response = apply_release(
                        &mut state,
                        command.request(),
                        protective_state_clear,
                        measurement_available,
                    );
                    outputs
                        .release_emergency_replies
                        .push(command.reply(response));
                }
            }
        }

        if state.engaged_this_invocation || state.emergency_latched {
            state.disarm();
        } else {
            select_and_limit_intent(&mut state, inputs, ctx.now());
        }

        validation::actuator_setpoint(
            &state.actuator_setpoint,
            state
                .config
                .left_wheels
                .iter()
                .chain(&state.config.right_wheels)
                .map(|wheel| wheel.actuator_id.as_str()),
        )
        .map_err(|error| anyhow::anyhow!(error))?;

        Ok((state, outputs))
    }
}

#[phoxal::runtime::outputs]
#[allow(
    dead_code,
    reason = "the collected projections are invoked by the transport runner"
)]
impl Motion {
    /// Projects the final actuator intent with an independent validity bound.
    #[phoxal::runtime::outputs::setpoint(
        port = motion::methods::ACTUATORS.__setpoint_port(),
        max_bytes = 1_024,
        valid_for_ms = 100
    )]
    fn actuators(&self, state: &ArbiterState) -> Option<ActuatorSetpoint> {
        Some(state.actuator_setpoint.clone())
    }

    /// Renews authority and protective status at each invocation so Safety
    /// can apply its freshness bound even while the robot remains disarmed.
    #[phoxal::runtime::outputs::state(
        port = motion::methods::STATUS.__state_port(),
        max_bytes = 512,
        bootstrap
    )]
    fn status(&self, state: &ArbiterState) -> MotionStatus {
        MotionStatus {
            mode: state
                .armed_mode
                .map_or(ControlMode::Disarmed, |mode| match mode {
                    ArmedMode::Manual => ControlMode::Manual,
                    ArmedMode::Autonomous => ControlMode::Autonomous,
                })
                .into(),
            emergency_latched: state.emergency_latched,
            selected_owner_id: state.selected_owner_id.clone(),
            protective_state_clear: state.protective_state_clear,
            stopped: state.actuator_setpoint == stopped_setpoint(&state.config)
                || state.engaged_this_invocation,
        }
    }
}

fn fresh_safety(inputs: &MotionInputs, now: ExecutionTime) -> Option<&MotionConstraints> {
    inputs
        .safety
        .is_fresh_at(now, Some(INPUT_MAX_AGE_MS))
        .then(|| inputs.safety.value())
        .flatten()
        .filter(|safety| {
            validation::constraints(safety).is_ok()
                && safety.valid_from_nanos <= now.as_nanos()
                && safety.expires_at_nanos > now.as_nanos()
                && fresh_capture(safety.oldest_capture_time_nanos, now)
        })
}

fn safety_is_clear(safety: &MotionConstraints, now: ExecutionTime) -> bool {
    validation::constraints(safety).is_ok()
        && Permission::try_from(safety.permission).ok() == Some(Permission::Clear)
        && safety.valid_from_nanos <= now.as_nanos()
        && safety.expires_at_nanos > now.as_nanos()
        && fresh_capture(safety.oldest_capture_time_nanos, now)
}

fn fresh_measurement(inputs: &MotionInputs, now: ExecutionTime) -> Option<&OdometryState> {
    inputs
        .measurements
        .is_fresh_at(now, Some(INPUT_MAX_AGE_MS))
        .then(|| inputs.measurements.value())
        .flatten()
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

enum MotionCall<'a> {
    Arm(&'a phoxal::runtime::Command<ArmRequest, ApplyEmergencyResponse>),
    Disarm(&'a phoxal::runtime::Command<Empty, ApplyEmergencyResponse>),
    EngageEmergency(&'a phoxal::runtime::Command<Empty, ApplyEmergencyResponse>),
    ReleaseEmergency(&'a phoxal::runtime::Command<ReleaseEmergencyRequest, ApplyEmergencyResponse>),
}

impl MotionCall<'_> {
    fn order(&self) -> phoxal::runtime::CommandOrder {
        match self {
            Self::Arm(command) => command.order(),
            Self::Disarm(command) => command.order(),
            Self::EngageEmergency(command) => command.order(),
            Self::ReleaseEmergency(command) => command.order(),
        }
    }
}

fn apply_arm(
    state: &mut ArbiterState,
    request: &ArmRequest,
    owner: &str,
    protective_state_clear: bool,
    measurement_available: bool,
    inputs: &MotionInputs,
    now: ExecutionTime,
) -> ApplyEmergencyResponse {
    if validation::arm_request(request).is_err() {
        state.emergency_latched = true;
        state.engaged_this_invocation = true;
        state.disarm();
        return refused(EmergencyRefusalReason::InvalidRequest);
    }
    let Some(mode) = armed_mode(request.mode) else {
        return refused(EmergencyRefusalReason::InvalidRequest);
    };
    if state.emergency_latched
        || !protective_state_clear
        || !measurement_available
        || !intent_matches(mode, owner, inputs, now)
    {
        return refused(EmergencyRefusalReason::ProtectiveState);
    }
    state.arm(mode, owner.to_owned());
    accepted()
}

fn apply_release(
    state: &mut ArbiterState,
    request: &ReleaseEmergencyRequest,
    protective_state_clear: bool,
    measurement_available: bool,
) -> ApplyEmergencyResponse {
    if validation::release_request(request).is_err() {
        return refused(EmergencyRefusalReason::InvalidRequest);
    }
    if !protective_state_clear || !measurement_available {
        return refused(EmergencyRefusalReason::ProtectiveState);
    }
    state.emergency_latched = false;
    state.disarm();
    accepted()
}

fn armed_mode(mode: i32) -> Option<ArmedMode> {
    match ControlMode::try_from(mode).ok()? {
        ControlMode::Manual => Some(ArmedMode::Manual),
        ControlMode::Autonomous => Some(ArmedMode::Autonomous),
        _ => None,
    }
}

fn intent_matches(
    mode: ArmedMode,
    owner_id: &str,
    inputs: &MotionInputs,
    now: ExecutionTime,
) -> bool {
    let intent = match mode {
        ArmedMode::Manual => inputs.manual.value(),
        ArmedMode::Autonomous => inputs.autonomous.value(),
    };
    intent
        .filter(|_| match mode {
            ArmedMode::Manual => intent_owner_matches(owner_id, inputs.manual.source()),
            ArmedMode::Autonomous => intent_owner_matches(owner_id, inputs.autonomous.source()),
        })
        .is_some_and(|_| match mode {
            ArmedMode::Manual => inputs.manual.is_valid_at(now),
            ArmedMode::Autonomous => inputs.autonomous.is_valid_at(now),
        })
}

fn intent_owner_matches(command_owner: &str, intent_source: Option<&str>) -> bool {
    // The supervisor publishes scenario setpoints as its virtual graph source.
    // Its external Commands ingress names that same authority supervisor.public.
    intent_source == Some(command_owner)
        || (command_owner == "supervisor.public" && intent_source == Some("supervisor"))
}

fn select_and_limit_intent(state: &mut ArbiterState, inputs: &MotionInputs, now: ExecutionTime) {
    let Some(safety) = fresh_safety(inputs, now) else {
        state.disarm();
        return;
    };
    if !state.measurement_available
        || !matches!(
            Permission::try_from(safety.permission),
            Ok(Permission::Clear | Permission::Limited)
        )
    {
        state.disarm();
        return;
    }
    let Some(mode) = state.armed_mode else {
        state.disarm();
        return;
    };
    let intent = match mode {
        ArmedMode::Manual => inputs
            .manual
            .is_valid_at(now)
            .then(|| inputs.manual.value())
            .flatten(),
        ArmedMode::Autonomous => inputs
            .autonomous
            .is_valid_at(now)
            .then(|| inputs.autonomous.value())
            .flatten(),
    };
    let Some(intent) = intent.filter(|intent| validation::intent(intent).is_ok()) else {
        state.disarm();
        return;
    };
    let owner = match mode {
        ArmedMode::Manual => inputs.manual.source(),
        ArmedMode::Autonomous => inputs.autonomous.source(),
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
    state.selected_intent = Some(*intent);
    let mut limited = *intent;
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
    ApplyEmergencyResponse {
        decision: Some(apply_emergency_response::Decision::Accepted(
            EmergencyAccepted {},
        )),
    }
}

fn refused(reason: EmergencyRefusalReason) -> ApplyEmergencyResponse {
    ApplyEmergencyResponse {
        decision: Some(apply_emergency_response::Decision::Refused(
            EmergencyRefused {
                reason: reason.into(),
            },
        )),
    }
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
                reason: ConstraintReason::WorldUnavailable as i32,
                max_linear_speed_mps: None,
                max_angular_speed_radps: None,
                observed_value: Some(0.0),
            }],
        )
    };
    Latest::from_sample(phoxal::runtime::Sample::new(
        MotionConstraints {
            sequence: 1,
            permission: permission as i32,
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
    use phoxal::runtime::{Command, CommandId, CommandOrder, Commands, ExecutionDuration};

    use super::*;

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
            ArmRequest { mode: mode.into() },
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
                    Some(actuator_target::Control::VelocityRadps(
                        expected * direction
                    ))
                );
            }
        }
        assert!(
            stopped_setpoint(&cfg)
                .targets
                .iter()
                .all(|target| target.control == Some(actuator_target::Control::VelocityRadps(0.0)))
        );
        cfg.left_wheels.clear();
        assert!(validate_motion_config(&cfg).is_err());
    }

    #[test]
    fn body_velocity_uses_physical_wheel_radius() {
        let setpoint = setpoint_from_twist(0.22, 0.0, &config());
        assert_eq!(
            setpoint.targets[0].control,
            Some(actuator_target::Control::VelocityRadps(2.0))
        );
    }

    #[test]
    fn yaw_signs_and_gearing_apply_once_at_the_motor_shaft() {
        let mut config = config();
        config.left_wheels[0].gear_ratio = 2.0;
        let setpoint = setpoint_from_twist(0.0, 1.0, &config);
        assert_eq!(
            setpoint.targets[0].control,
            Some(actuator_target::Control::VelocityRadps(-0.3 / 0.11 * 2.0))
        );
        assert_eq!(
            setpoint.targets[1].control,
            Some(actuator_target::Control::VelocityRadps(-0.3 / 0.11))
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
                let initial = phoxal::runtime::initialize(&Motion, at(0), config()).unwrap();
                let (state, _) = Motion
                    .step(&context(0, now.as_nanos()), initial, &input)
                    .unwrap();
                assert_eq!(Motion.status(&state).mode, ControlMode::Disarmed as i32);
                assert!(Motion.status(&state).stopped);
            }
        }
        assert!(fresh_capture(Some(0), at(100_000_000)));
        assert!(!fresh_capture(Some(0), at(100_000_001)));
    }

    #[test]
    fn unavailable_odometry_cannot_arm_even_when_its_velocity_is_zero() {
        let service = Motion;
        let initial = phoxal::runtime::initialize(&service, at(0), config()).unwrap();
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
        let (state, _) = service.step(&context(0, 0), initial, &input).unwrap();
        assert_eq!(service.status(&state).mode, ControlMode::Disarmed as i32);
    }

    #[test]
    fn protective_speed_limits_apply_to_armed_motion_but_do_not_authorize_arming() {
        let mut input = inputs(
            manual_intent("operator", 0.4, 0.0, at(0)),
            Setpoint::withdrawn(),
            vec![arm(1, ControlMode::Manual, "operator")],
        );
        let initial = phoxal::runtime::initialize(&Motion, at(0), config()).unwrap();
        let (armed, _) = Motion.step(&context(0, 0), initial, &input).unwrap();
        assert_eq!(Motion.status(&armed).mode, ControlMode::Manual as i32);
        let mut safety = input.safety.value().unwrap().clone();
        safety.permission = Permission::Limited as i32;
        safety.constraints = vec![Constraint {
            reason: ConstraintReason::ObstacleProximity as i32,
            max_linear_speed_mps: Some(0.15),
            max_angular_speed_radps: None,
            observed_value: Some(0.4),
        }];
        input.safety = Latest::from_sample(phoxal::runtime::Sample::new(
            safety,
            phoxal::runtime::ObservationStamp::new("safety", at(20_000_000), None),
        ));
        let fresh = phoxal::runtime::initialize(&Motion, at(0), config()).unwrap();
        let (refused, _) = Motion.step(&context(0, 20_000_000), fresh, &input).unwrap();
        assert_eq!(Motion.status(&refused).mode, ControlMode::Disarmed as i32);
        input.arm = Commands::default();
        let (limited, _) = Motion.step(&context(1, 20_000_000), armed, &input).unwrap();
        assert_eq!(Motion.status(&limited).mode, ControlMode::Manual as i32);
        assert!(!Motion.status(&limited).protective_state_clear);
        assert_eq!(
            limited.actuator_setpoint.targets[0].control,
            Some(actuator_target::Control::VelocityRadps(0.15 / 0.11))
        );
        input.safety = safety_state(false, at(40_000_000));
        let (stopped, _) = Motion
            .step(&context(2, 40_000_000), limited, &input)
            .unwrap();
        assert_eq!(Motion.status(&stopped).mode, ControlMode::Disarmed as i32);
        assert!(Motion.status(&stopped).stopped);
    }

    #[test]
    fn restart_starts_disarmed_and_requires_explicit_arm() {
        let service = Motion;
        let initial =
            phoxal::runtime::initialize(&service, at(0), config()).expect("initialize motion");
        assert_eq!(service.status(&initial).mode, ControlMode::Disarmed as i32);

        let (state, outputs) = service
            .step(
                &context(0, 0),
                initial,
                &inputs(
                    manual_intent("operator", 0.2, 0.0, at(0)),
                    Setpoint::withdrawn(),
                    vec![arm(1, ControlMode::Manual, "operator")],
                ),
            )
            .expect("arm command");
        assert_eq!(outputs.arm_replies.len(), 1);
        assert_eq!(service.status(&state).mode, ControlMode::Manual as i32);
    }

    #[test]
    fn engage_then_release_in_one_batch_stops_and_does_not_rearm() {
        let service = Motion;
        let initial =
            phoxal::runtime::initialize(&service, at(0), config()).expect("initialize motion");
        let (state, _) = service
            .step(
                &context(0, 0),
                initial,
                &inputs(
                    manual_intent("operator", 0.2, 0.0, at(0)),
                    Setpoint::withdrawn(),
                    vec![arm(1, ControlMode::Manual, "operator")],
                ),
            )
            .expect("arm command");
        let (state, outputs) = service
            .step(
                &context(1, 20_000_000),
                state,
                &inputs(
                    manual_intent("operator", 0.2, 0.0, at(20_000_000)),
                    Setpoint::withdrawn(),
                    vec![
                        engage(CommandOrder::new(1, 0, CommandId::new(2))),
                        release(CommandOrder::new(1, 0, CommandId::new(3))),
                    ],
                ),
            )
            .expect("engage and release batch");
        assert_eq!(outputs.engage_emergency_replies.len(), 1);
        assert_eq!(outputs.release_emergency_replies.len(), 1);
        let status = service.status(&state);
        assert_eq!(status.mode, ControlMode::Disarmed as i32);
        assert!(!status.emergency_latched);
        assert!(status.stopped);
    }

    #[test]
    fn disarm_is_ordered_with_other_service_calls() {
        let service = Motion;
        let initial = phoxal::runtime::initialize(&service, at(0), config()).unwrap();
        let input = inputs(
            manual_intent("operator", 0.2, 0.0, at(0)),
            Setpoint::withdrawn(),
            vec![
                arm(1, ControlMode::Manual, "operator"),
                disarm(CommandOrder::new(1, 0, CommandId::new(2))),
            ],
        );

        let (state, outputs) = service.step(&context(0, 0), initial, &input).unwrap();

        assert_eq!(outputs.arm_replies.len(), 1);
        assert_eq!(outputs.disarm_replies.len(), 1);
        assert_eq!(service.status(&state).mode, ControlMode::Disarmed as i32);
        assert!(service.status(&state).stopped);
    }

    #[test]
    fn expired_intent_stops_without_falling_back_to_autonomy() {
        let service = Motion;
        let initial =
            phoxal::runtime::initialize(&service, at(0), config()).expect("initialize motion");
        let (state, _) = service
            .step(
                &context(0, 0),
                initial,
                &inputs(
                    manual_intent("operator", 0.2, 0.0, at(0)),
                    autonomous_intent("planner", 0.1, 0.0, at(0)),
                    vec![arm(1, ControlMode::Manual, "operator")],
                ),
            )
            .expect("arm manual");
        let (state, _) = service
            .step(
                &context(1, 200_000_000),
                state,
                &inputs(
                    manual_intent("operator", 0.2, 0.0, at(0)),
                    autonomous_intent("planner", 0.1, 0.0, at(0)),
                    Vec::new(),
                ),
            )
            .expect("expired intent is safe");
        let status = service.status(&state);
        assert_eq!(status.mode, ControlMode::Disarmed as i32);
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
        let service = Motion;
        let initial =
            phoxal::runtime::initialize(&service, at(0), config()).expect("initialize motion");
        let (state, _) = service
            .step(
                &context(0, 0),
                initial,
                &inputs(
                    manual_intent("operator-a", 0.2, 0.0, at(0)),
                    Setpoint::withdrawn(),
                    vec![arm(1, ControlMode::Manual, "operator-a")],
                ),
            )
            .expect("arm first operator");
        let (state, _) = service
            .step(
                &context(1, 20_000_000),
                state,
                &inputs(
                    manual_intent("operator-b", 0.2, 0.0, at(20_000_000)),
                    Setpoint::withdrawn(),
                    Vec::new(),
                ),
            )
            .expect("owner change is handled as a safe transition");
        let status = service.status(&state);
        assert_eq!(status.mode, ControlMode::Disarmed as i32);
        assert!(status.stopped);
    }

    #[test]
    fn config_validates_limits_and_unique_actuator_ids() {
        let mut invalid = config();
        invalid.max_linear_mps = 0.0;
        assert!(phoxal::runtime::initialize(&Motion, at(0), invalid).is_err());

        invalid = config();
        invalid.max_angular_radps = f64::NAN;
        assert!(phoxal::runtime::initialize(&Motion, at(0), invalid).is_err());

        invalid = config();
        invalid.left_wheels[0].actuator_id = invalid.right_wheels[0].actuator_id.clone();
        assert!(phoxal::runtime::initialize(&Motion, at(0), invalid).is_err());

        invalid = config();
        invalid.left_wheels[0].actuator_id.clear();
        assert!(phoxal::runtime::initialize(&Motion, at(0), invalid).is_err());
    }

    #[test]
    fn config_controls_limits_and_actuator_membership() {
        let service = Motion;
        let mut custom = MotionConfig {
            max_linear_mps: 0.1,
            max_angular_radps: 0.2,
            ..config()
        };
        custom.left_wheels[0].actuator_id = "left-wheel".into();
        custom.right_wheels[0].actuator_id = "right-wheel".into();
        let initial = phoxal::runtime::initialize(&service, at(0), custom)
            .expect("initialize configured motion");
        let (state, _) = service
            .step(
                &context(0, 0),
                initial,
                &inputs(
                    manual_intent("operator", 0.5, 0.5, at(0)),
                    Setpoint::withdrawn(),
                    vec![arm(1, ControlMode::Manual, "operator")],
                ),
            )
            .expect("configured motion step");
        let setpoint = service.actuators(&state).expect("setpoint projection");
        assert_eq!(setpoint.targets[0].actuator_id, "left-wheel");
        assert_eq!(setpoint.targets[1].actuator_id, "right-wheel");
        let left = match setpoint.targets[0].control.as_ref() {
            Some(actuator_target::Control::VelocityRadps(value)) => *value,
            _ => panic!("left actuator must use velocity control"),
        };
        let right = match setpoint.targets[1].control.as_ref() {
            Some(actuator_target::Control::VelocityRadps(value)) => *value,
            _ => panic!("right actuator must use velocity control"),
        };
        assert!((left - (0.1 - 0.2 * 0.3) / 0.11).abs() < 1e-12);
        assert!((right + (0.1 + 0.2 * 0.3) / 0.11).abs() < 1e-12);
    }

    #[test]
    fn malformed_emergency_input_latches_stop() {
        let service = Motion;
        let initial =
            phoxal::runtime::initialize(&service, at(0), config()).expect("initialize motion");
        let (state, outputs) = service
            .step(
                &context(0, 0),
                initial,
                &inputs(
                    manual_intent("operator", 0.2, 0.0, at(0)),
                    Setpoint::withdrawn(),
                    vec![arm(1, ControlMode::Unspecified, "operator")],
                ),
            )
            .expect("invalid command returns typed refusal");
        assert!(matches!(
            outputs.arm_replies[0].response().decision,
            Some(apply_emergency_response::Decision::Refused(_))
        ));
        let status = service.status(&state);
        assert!(status.emergency_latched);
        assert!(status.stopped);
    }
}
