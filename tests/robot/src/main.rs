//! Disarmed default brain for the internal qualification robot.

use phoxal::runtime::{InitContext, Runtime, StepContext};

phoxal::api!();

#[derive(Clone, Copy, Debug, Default)]
struct Brain;

#[phoxal::runtime::inputs]
struct Inputs {}

#[phoxal::runtime::outputs]
#[derive(Default)]
struct Outputs {}

#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for Brain {
    type Config = ();
    type State = ();
    type Inputs = Inputs;
    type Outputs = Outputs;

    fn init(&self, _ctx: &InitContext, _config: ()) -> phoxal::Result<()> {
        Ok(())
    }

    fn step(
        &self,
        _ctx: &StepContext,
        state: (),
        _inputs: &Inputs,
    ) -> phoxal::Result<((), Outputs)> {
        Ok((state, Outputs::default()))
    }
}

#[phoxal::runtime::outputs]
#[allow(dead_code, reason = "the transport runner invokes output projections")]
impl Brain {
    /// The normal robot starts stopped.
    /// The local scenario substitutes the controller's manual input without
    /// turning the qualification program into deployed robot behavior.
    #[phoxal::runtime::outputs::setpoint(
        port = crate::api::__contracts::phoxal::test_controller::v1::controller::methods::MANUAL.__setpoint_port(),
        max_bytes = 256,
        valid_for_ms = 100
    )]
    fn manual(
        &self,
        _state: &(),
    ) -> Option<crate::api::__contracts::phoxal::motion::v1::MotionIntent> {
        None
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run(Brain)
}
