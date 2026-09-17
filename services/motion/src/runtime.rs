use crate::config::{MotionConfig, validate_motion_config};
#[cfg(test)]
use crate::drive::setpoint_from_twist;
use crate::drive::{setpoint_from_intent, stopped_setpoint};
use crate::inputs::MotionInputs;
use crate::outputs::MotionOutputs;
#[cfg(test)]
use phoxal::runtime::input::{Latest, Setpoint};
use phoxal::runtime::{ExecutionTime, InitContext, Runtime, StepContext};
use phoxal_service_kinematics::OdometryState;
#[cfg(test)]
use phoxal_service_motion::actuator_target;
use phoxal_service_motion::{
    ActuatorSetpoint, ApplyEmergencyRequest, ApplyEmergencyResponse, Arm, ControlMode,
    EmergencyAccepted, EmergencyRefusalReason, EmergencyRefused, MotionIntent, MotionStatus,
    apply_emergency_request, apply_emergency_response, ports,
};
#[cfg(test)]
use phoxal_service_motion::{Constraint, ConstraintReason};
use phoxal_service_motion::{MotionConstraints, Permission};

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
            .emergency
            .validate_order()
            .map_err(|error| anyhow::anyhow!(error))?;

        state.engaged_this_invocation = false;
        state.protective_state_clear = fresh_safety(inputs, ctx.now())
            .is_some_and(|safety| safety_is_clear(safety, ctx.now()));
        state.measurement_available = fresh_measurement(inputs, ctx.now())
            .is_some_and(|measurement| measurement.available && measurement.validate().is_ok());
        let mut outputs = MotionOutputs::default();
        let protective_state_clear = state.protective_state_clear;
        let measurement_available = state.measurement_available;

        for command in inputs.emergency.items() {
            let response = apply_emergency_command(
                &mut state,
                command.request(),
                protective_state_clear,
                measurement_available,
                inputs,
                ctx.now(),
            );
            outputs.emergency_replies.push(command.reply(response));
        }

        if state.engaged_this_invocation || state.emergency_latched {
            state.disarm();
        } else {
            select_and_limit_intent(&mut state, inputs, ctx.now());
        }

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
        port = ports::ACTUATORS,
        max_bytes = 1_024,
        valid_for_ms = 100
    )]
    fn actuators(&self, state: &ArbiterState) -> Option<ActuatorSetpoint> {
        Some(state.actuator_setpoint.clone())
    }

    /// Renews authority and protective status at each invocation so Safety
    /// can apply its freshness bound even while the robot remains disarmed.
    #[phoxal::runtime::outputs::state(
        port = ports::STATUS,
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
            safety.validate().is_ok()
                && safety.valid_from_nanos <= now.as_nanos()
                && safety.expires_at_nanos > now.as_nanos()
                && fresh_capture(safety.oldest_capture_time_nanos, now)
        })
}

fn safety_is_clear(safety: &MotionConstraints, now: ExecutionTime) -> bool {
    safety.validate().is_ok()
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

fn fresh_capture(capture: Option<u64>, now: ExecutionTime) -> bool {
    capture
        .and_then(|capture| now.as_nanos().checked_sub(capture))
        .is_some_and(|age| age <= INPUT_MAX_AGE_MS.saturating_mul(1_000_000))
}

fn apply_emergency_command(
    state: &mut ArbiterState,
    request: &ApplyEmergencyRequest,
    protective_state_clear: bool,
    measurement_available: bool,
    inputs: &MotionInputs,
    now: ExecutionTime,
) -> ApplyEmergencyResponse {
    if request.validate().is_err() {
        state.emergency_latched = true;
        state.engaged_this_invocation = true;
        state.disarm();
        return refused(EmergencyRefusalReason::InvalidRequest);
    }
    match request.command.as_ref() {
        Some(apply_emergency_request::Command::Engage(_)) => {
            state.emergency_latched = true;
            state.engaged_this_invocation = true;
            accepted()
        }
        Some(apply_emergency_request::Command::Release(_)) => {
            if !protective_state_clear || !measurement_available {
                return refused(EmergencyRefusalReason::ProtectiveState);
            }
            state.emergency_latched = false;
            state.disarm();
            accepted()
        }
        Some(apply_emergency_request::Command::Arm(Arm { mode, owner_id })) => {
            let Some(mode) = armed_mode(*mode) else {
                return refused(EmergencyRefusalReason::InvalidRequest);
            };
            if state.emergency_latched
                || !protective_state_clear
                || !measurement_available
                || !intent_matches(mode, owner_id, inputs, now)
            {
                return refused(EmergencyRefusalReason::ProtectiveState);
            }
            state.arm(mode, owner_id.clone());
            accepted()
        }
        Some(apply_emergency_request::Command::Disarm(_)) => {
            state.disarm();
            accepted()
        }
        None => refused(EmergencyRefusalReason::InvalidRequest),
    }
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
        .filter(|intent| intent.owner_id == owner_id)
        .is_some_and(|_intent| match mode {
            ArmedMode::Manual => inputs.manual.is_valid_at(now),
            ArmedMode::Autonomous => inputs.autonomous.is_valid_at(now),
        })
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
    let Some(intent) = intent.filter(|intent| intent.validate().is_ok()) else {
        state.disarm();
        return;
    };
    if state
        .selected_owner_id
        .as_deref()
        .is_some_and(|selected_owner| selected_owner != intent.owner_id)
    {
        state.disarm();
        return;
    }
    let owner_id = intent.owner_id.clone();
    state.selected_owner_id = Some(owner_id);
    state.selected_intent = Some(intent.clone());
    let mut limited = intent.clone();
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
    Setpoint::new(
        MotionIntent {
            owner_id: owner_id.into(),
            linear_x_mps,
            angular_z_radps,
        },
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
        commands: Vec<Command<ApplyEmergencyRequest, ApplyEmergencyResponse>>,
    ) -> MotionInputs {
        MotionInputs {
            manual,
            autonomous,
            safety: safety_state(true, at(0)),
            measurements: measurement(at(0)),
            emergency: Commands::new(commands),
        }
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

    fn arm(mode: ControlMode, owner_id: &str) -> ApplyEmergencyRequest {
        ApplyEmergencyRequest {
            command: Some(apply_emergency_request::Command::Arm(Arm {
                mode: mode.into(),
                owner_id: owner_id.to_owned(),
            })),
        }
    }

    fn engage() -> ApplyEmergencyRequest {
        ApplyEmergencyRequest {
            command: Some(apply_emergency_request::Command::Engage(
                phoxal_service_motion::EngageEmergency {},
            )),
        }
    }

    fn release() -> ApplyEmergencyRequest {
        ApplyEmergencyRequest {
            command: Some(apply_emergency_request::Command::Release(
                phoxal_service_motion::ReleaseEmergency {
                    reset_token: "physical-reset".into(),
                },
            )),
        }
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
            output
                .validate_for(["left", "left-rear", "right", "right-rear"])
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
                    vec![Command::new(
                        CommandId::new(1),
                        arm(ControlMode::Manual, "operator"),
                    )],
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
            vec![Command::new(
                CommandId::new(1),
                arm(ControlMode::Manual, "operator"),
            )],
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
            vec![Command::new(
                CommandId::new(1),
                arm(ControlMode::Manual, "operator"),
            )],
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
        input.emergency = Commands::default();
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
                    vec![Command::new(
                        CommandId::new(1),
                        arm(ControlMode::Manual, "operator"),
                    )],
                ),
            )
            .expect("arm command");
        assert_eq!(outputs.emergency_replies.len(), 1);
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
                    vec![Command::new(
                        CommandId::new(1),
                        arm(ControlMode::Manual, "operator"),
                    )],
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
                        Command::with_order(CommandOrder::new(1, 0, CommandId::new(2)), engage()),
                        Command::with_order(CommandOrder::new(1, 0, CommandId::new(3)), release()),
                    ],
                ),
            )
            .expect("engage and release batch");
        assert_eq!(outputs.emergency_replies.len(), 2);
        let status = service.status(&state);
        assert_eq!(status.mode, ControlMode::Disarmed as i32);
        assert!(!status.emergency_latched);
        assert!(status.stopped);
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
                    vec![Command::new(
                        CommandId::new(1),
                        arm(ControlMode::Manual, "operator"),
                    )],
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
                    vec![Command::new(
                        CommandId::new(1),
                        arm(ControlMode::Manual, "operator-a"),
                    )],
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
                    vec![Command::new(
                        CommandId::new(1),
                        arm(ControlMode::Manual, "operator"),
                    )],
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
        let invalid = ApplyEmergencyRequest { command: None };
        let (state, outputs) = service
            .step(
                &context(0, 0),
                initial,
                &inputs(
                    manual_intent("operator", 0.2, 0.0, at(0)),
                    Setpoint::withdrawn(),
                    vec![Command::new(CommandId::new(1), invalid)],
                ),
            )
            .expect("invalid command returns typed refusal");
        assert!(matches!(
            outputs.emergency_replies[0].response().decision,
            Some(apply_emergency_response::Decision::Refused(_))
        ));
        let status = service.status(&state);
        assert!(status.emergency_latched);
        assert!(status.stopped);
    }
}
