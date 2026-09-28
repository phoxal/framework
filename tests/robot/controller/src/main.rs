//! Robot-local controller for the internal qualification fixture: the
//! endpoint surface and payloads are declared once here in Rust; this file
//! owns the wheel math, validation, and the actuator projection.

use phoxal::contracts::component::actuator::{ActuatorSetpoint, ActuatorTarget, Control};
use phoxal::contracts::component::encoder::EncoderSample;
use phoxal::contracts::{Latest, Queue};
use phoxal::runtime::{InitContext, Runtime, StepContext};

const WHEEL_RADIUS_M: f64 = 0.11;
const WHEEL_BASE_M: f64 = 0.52;
const MAX_LINEAR_MPS: f64 = 0.6;
const MAX_ANGULAR_RADPS: f64 = 2.0;
const ACTUATORS: [(&str, f64, f64); 4] = [
    ("front_left_drive.motor", -1.0, 1.0),
    ("rear_left_drive.motor", -1.0, 1.0),
    ("front_right_drive.motor", 1.0, -1.0),
    ("rear_right_drive.motor", 1.0, -1.0),
];

/// A motion command in the body frame.
///
/// This is the controller's typed expectation of the brain's manual
/// output; the brain authors its own copy of the same wire identity.
#[phoxal::message(package = "phoxal.motion.v1")]
pub struct MotionIntent {
    /// Forward velocity along the body x axis.
    #[phoxal(tag = 1)]
    pub linear_x_mps: f64,
    /// Counter-clockwise yaw rate around the body z axis.
    #[phoxal(tag = 2)]
    pub angular_z_radps: f64,
}

/// The controller's endpoint contract: a leased manual intent, queued
/// encoder history, and a leased actuator projection from state.
#[phoxal::endpoints]
pub struct ControllerApi {
    #[phoxal::input(lease_ms = 100, max_bytes = 4096)]
    manual: Latest<MotionIntent>,

    #[phoxal::input(max_items = 32, max_bytes = 262_144)]
    encoders: Queue<EncoderSample>,

    #[phoxal::output(projection = state, lease_ms = 100, max_bytes = 1024)]
    actuators: Latest<ActuatorSetpoint>,
}

#[derive(Clone, Copy, Debug, Default)]
struct Controller;

fn setpoint(linear_mps: f64, angular_radps: f64) -> ActuatorSetpoint {
    let linear = linear_mps.clamp(-MAX_LINEAR_MPS, MAX_LINEAR_MPS);
    let angular = angular_radps.clamp(-MAX_ANGULAR_RADPS, MAX_ANGULAR_RADPS);
    let targets = ACTUATORS
        .into_iter()
        .map(|(actuator_id, side, direction)| {
            let velocity =
                (linear + side * angular * WHEEL_BASE_M / 2.0) / WHEEL_RADIUS_M * direction;
            ActuatorTarget {
                actuator_id: actuator_id.to_owned(),
                control: Some(Control::VelocityRadps(velocity)),
            }
        })
        .collect();
    ActuatorSetpoint { targets }
}

impl controller_api::projections::Projections for Controller {
    type State = ActuatorSetpoint;

    fn actuators(&self, state: &ActuatorSetpoint) -> Option<ActuatorSetpoint> {
        Some(state.clone())
    }
}

#[phoxal::runtime(contract = ControllerApi, period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for Controller {
    type Config = ();
    type State = ActuatorSetpoint;

    fn init(&self, _ctx: &InitContext, _config: ()) -> phoxal::Result<Self::State> {
        Ok(setpoint(0.0, 0.0))
    }

    fn step(
        &self,
        ctx: &StepContext,
        _state: Self::State,
        inputs: &Self::Inputs,
    ) -> phoxal::Result<(Self::State, Self::Outputs)> {
        for sample in inputs.encoders.items() {
            sample
                .payload()
                .validate()
                .map_err(|error| phoxal::anyhow!(error))?;
        }

        let next = match inputs
            .manual
            .is_valid_at(ctx.now())
            .then(|| inputs.manual.value())
            .flatten()
        {
            Some(intent) => setpoint(intent.linear_x_mps, intent.angular_z_radps),
            None => setpoint(0.0, 0.0),
        };
        let outputs = Self::Outputs::default();
        Ok((next, outputs))
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run(Controller)
}
