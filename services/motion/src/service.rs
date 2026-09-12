//! The official motion Runtime.
//!
//! Motion is the sole authority that converts a selected body-twist intent to
//! an actuator setpoint.  It starts disarmed, requires explicit arm evidence,
//! latches emergency engagement, and emits a fresh bounded setpoint on every
//! accepted invocation.  The driver remains responsible for enforcing the
//! setpoint expiry if this Runtime stops.

use phoxal::runtime::input::{Commands, Latest, Setpoint};
use phoxal::runtime::{ExecutionTime, InitContext, Runtime, StepContext};
use phoxal_motion::{
    ActuatorSetpoint, ActuatorTarget, ApplyEmergencyRequest, ApplyEmergencyResponse, Arm,
    ControlMode, EmergencyAccepted, EmergencyRefusalReason, EmergencyRefused, MotionIntent,
    MotionMeasurement, MotionStatus, SafetyState, actuator_target, apply_emergency_request,
    apply_emergency_response, ports,
};

const INPUT_MAX_AGE_MS: u64 = 100;
#[allow(
    dead_code,
    reason = "the constructor is shared by direct tests and future host adapters"
)]
const SETPOINT_VALID_FOR_MS: u64 = 100;
/// Admitted robot-specific motion limits and actuator identities.
#[derive(Clone, Debug, serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
pub struct MotionConfig {
    /// Maximum absolute forward body velocity in metres per second.
    pub max_linear_mps: f64,
    /// Maximum absolute yaw body velocity in radians per second.
    pub max_angular_radps: f64,
    /// Actuator identity receiving the left differential-drive command.
    pub left_actuator_id: String,
    /// Actuator identity receiving the right differential-drive command.
    pub right_actuator_id: String,
}

fn validate_motion_config(config: &MotionConfig) -> phoxal::Result<()> {
    if !config.max_linear_mps.is_finite() || config.max_linear_mps <= 0.0 {
        return Err(anyhow::anyhow!(
            "max_linear_mps must be finite and positive"
        ));
    }
    if !config.max_angular_radps.is_finite() || config.max_angular_radps <= 0.0 {
        return Err(anyhow::anyhow!(
            "max_angular_radps must be finite and positive"
        ));
    }
    if config.left_actuator_id.is_empty() || config.right_actuator_id.is_empty() {
        return Err(anyhow::anyhow!("actuator IDs must be non-empty"));
    }
    if config.left_actuator_id == config.right_actuator_id {
        return Err(anyhow::anyhow!("actuator IDs must be unique"));
    }
    Ok(())
}

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

/// One immutable input cut for Motion.
#[phoxal::runtime::inputs]
pub struct MotionInputs {
    /// Manual and autonomous control intents replace older values and expire
    /// independently from their publication timestamps.
    pub manual: Setpoint<MotionIntent>,
    pub autonomous: Setpoint<MotionIntent>,
    /// Safety is a current protective-state fact, never an authority lease.
    pub safety: Latest<SafetyState>,
    /// Measured evidence is required before arm and actuation.
    pub measurements: Latest<MotionMeasurement>,
    /// Emergency, arm, and disarm commands are processed in admission order.
    #[phoxal::runtime::input(
        port = ports::EMERGENCY,
        max_items = 32,
        max_bytes = 16_384
    )]
    pub emergency: Commands<ApplyEmergencyRequest, ApplyEmergencyResponse>,
}

/// Fresh per-invocation Motion products.
#[phoxal::runtime::outputs]
#[derive(Default)]
pub struct MotionOutputs {
    /// One processing reply for every admitted emergency or arm command.
    #[phoxal::runtime::outputs::reply(emergency, max_items = 32, max_bytes = 16_384)]
    pub emergency_replies: Vec<phoxal::runtime::Reply<ApplyEmergencyResponse>>,
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
            .and_then(|safety| {
                safety
                    .validate()
                    .ok()
                    .map(|_| safety.protective_state_clear)
            })
            .unwrap_or(false);
        state.measurement_available = fresh_measurement(inputs, ctx.now())
            .is_some_and(|measurement| measurement.validate().is_ok());
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

    /// Projects authority and protective state for observers.
    #[phoxal::runtime::outputs::state(
        port = ports::STATUS,
        max_bytes = 512,
        bootstrap,
        on_change
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

fn fresh_safety(inputs: &MotionInputs, now: ExecutionTime) -> Option<&SafetyState> {
    inputs
        .safety
        .is_fresh_at(now, Some(INPUT_MAX_AGE_MS))
        .then(|| inputs.safety.value())
        .flatten()
}

fn fresh_measurement(inputs: &MotionInputs, now: ExecutionTime) -> Option<&MotionMeasurement> {
    inputs
        .measurements
        .is_fresh_at(now, Some(INPUT_MAX_AGE_MS))
        .then(|| inputs.measurements.value())
        .flatten()
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
    if !state.protective_state_clear || !state.measurement_available {
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
    state.actuator_setpoint = setpoint_from_intent(intent, &state.config);
}

fn stopped_setpoint(config: &MotionConfig) -> ActuatorSetpoint {
    setpoint_from_twist(0.0, 0.0, config)
}

fn setpoint_from_intent(intent: &MotionIntent, config: &MotionConfig) -> ActuatorSetpoint {
    let linear = intent
        .linear_x_mps
        .clamp(-config.max_linear_mps, config.max_linear_mps);
    let angular = intent
        .angular_z_radps
        .clamp(-config.max_angular_radps, config.max_angular_radps);
    setpoint_from_twist(linear, angular, config)
}

fn setpoint_from_twist(linear: f64, angular: f64, config: &MotionConfig) -> ActuatorSetpoint {
    ActuatorSetpoint {
        targets: vec![
            ActuatorTarget {
                actuator_id: config.left_actuator_id.clone(),
                control: Some(actuator_target::Control::VelocityRadps(linear - angular)),
            },
            ActuatorTarget {
                actuator_id: config.right_actuator_id.clone(),
                control: Some(actuator_target::Control::VelocityRadps(linear + angular)),
            },
        ],
    }
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

/// Build a valid current manual intent for a direct Runtime test or adapter.
#[must_use]
#[allow(
    dead_code,
    reason = "constructor helpers are shared by direct tests and host adapters"
)]
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

/// Build a valid current autonomous intent for a direct Runtime test or
/// adapter.
#[must_use]
#[allow(
    dead_code,
    reason = "constructor helpers are shared by direct tests and host adapters"
)]
pub fn autonomous_intent(
    owner_id: impl Into<String>,
    linear_x_mps: f64,
    angular_z_radps: f64,
    issued_at: ExecutionTime,
) -> Setpoint<MotionIntent> {
    manual_intent(owner_id, linear_x_mps, angular_z_radps, issued_at)
}

/// Build a stamped safety fact for a direct Runtime test or adapter.
#[must_use]
#[allow(
    dead_code,
    reason = "constructor helpers are shared by direct tests and host adapters"
)]
pub fn safety_state(protective_state_clear: bool, at: ExecutionTime) -> Latest<SafetyState> {
    Latest::from_sample(phoxal::runtime::Sample::new(
        SafetyState {
            protective_state_clear,
            reasons: Vec::new(),
        },
        phoxal::runtime::ObservationStamp::new("safety", at, None),
    ))
}

/// Build a stamped measurement for a direct Runtime test or adapter.
#[must_use]
#[allow(
    dead_code,
    reason = "constructor helpers are shared by direct tests and host adapters"
)]
pub fn measurement(at: ExecutionTime) -> Latest<MotionMeasurement> {
    Latest::from_sample(phoxal::runtime::Sample::new(
        MotionMeasurement {
            linear_x_mps: 0.0,
            angular_z_radps: 0.0,
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
            max_linear_mps: 0.5,
            max_angular_radps: 1.5,
            left_actuator_id: "left".into(),
            right_actuator_id: "right".into(),
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
                phoxal_motion::EngageEmergency {},
            )),
        }
    }

    fn release() -> ApplyEmergencyRequest {
        ApplyEmergencyRequest {
            command: Some(apply_emergency_request::Command::Release(
                phoxal_motion::ReleaseEmergency {
                    reset_token: "physical-reset".into(),
                },
            )),
        }
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
        invalid.left_actuator_id = invalid.right_actuator_id.clone();
        assert!(phoxal::runtime::initialize(&Motion, at(0), invalid).is_err());

        invalid = config();
        invalid.left_actuator_id.clear();
        assert!(phoxal::runtime::initialize(&Motion, at(0), invalid).is_err());
    }

    #[test]
    fn config_controls_limits_and_actuator_membership() {
        let service = Motion;
        let custom = MotionConfig {
            max_linear_mps: 0.1,
            max_angular_radps: 0.2,
            left_actuator_id: "left-wheel".into(),
            right_actuator_id: "right-wheel".into(),
        };
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
        assert!((left + 0.1).abs() < f64::EPSILON);
        assert!((right - 0.3).abs() < f64::EPSILON);
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
