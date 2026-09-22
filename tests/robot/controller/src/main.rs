use motion::{ActuatorSetpoint, ActuatorTarget, MotionIntent, actuator_target};
use phoxal::robotics::EncoderSample;
use phoxal::runtime::input::{Samples, Setpoint};
use phoxal::runtime::{InitContext, Runtime, StepContext};
use phoxal_test_controller::controller;

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

#[derive(Clone, Copy, Debug, Default)]
struct Controller;

#[phoxal::runtime::inputs]
struct Inputs {
    #[phoxal::runtime::input(port = controller::methods::MANUAL.__setpoint_port())]
    manual: Setpoint<MotionIntent>,
    #[phoxal::runtime::input(max_items = 32, max_bytes = 262_144)]
    encoders: Samples<EncoderSample>,
}

#[phoxal::runtime::outputs]
#[derive(Default)]
struct Outputs {}

#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for Controller {
    type Config = ();
    type State = ActuatorSetpoint;
    type Inputs = Inputs;
    type Outputs = Outputs;

    fn init(&self, _ctx: &InitContext, _config: ()) -> phoxal::Result<Self::State> {
        Ok(setpoint(0.0, 0.0))
    }

    fn step(
        &self,
        ctx: &StepContext,
        _state: Self::State,
        inputs: &Inputs,
    ) -> phoxal::Result<(Self::State, Outputs)> {
        for sample in inputs.encoders.items() {
            sample
                .payload()
                .validate()
                .map_err(|error| anyhow::anyhow!(error))?;
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
        Ok((next, Outputs::default()))
    }
}

#[phoxal::runtime::outputs]
#[allow(dead_code, reason = "the transport runner invokes output projections")]
impl Controller {
    #[phoxal::runtime::outputs::setpoint(
        port = controller::methods::ACTUATORS.__setpoint_port(),
        max_bytes = 1_024,
        valid_for_ms = 100
    )]
    fn actuators(&self, state: &ActuatorSetpoint) -> Option<ActuatorSetpoint> {
        Some(state.clone())
    }
}

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
                control: Some(actuator_target::Control::VelocityRadps(velocity)),
            }
        })
        .collect();
    ActuatorSetpoint { targets }
}

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run(Controller)
}
