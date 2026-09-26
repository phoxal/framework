//! Disarmed default brain for the internal qualification robot, instructed
//! by `robot.yaml`: the endpoint surface is generated; this file owns only
//! the disarmed intent.

use phoxal::runtime::{InitContext, Runtime, StepContext};

phoxal::api!();

use crate::api::types::phoxal::motion::v1::MotionIntent;

#[derive(Clone, Copy, Debug, Default)]
struct Brain;

impl crate::api::projections::Projections for Brain {
    type State = ();

    /// The normal robot starts stopped.
    /// The local scenario substitutes the controller's manual input without
    /// turning the qualification program into deployed robot behavior.
    fn manual(&self, _state: &()) -> Option<MotionIntent> {
        None
    }
}

#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for Brain {
    type Config = ();
    type State = ();

    fn init(&self, _ctx: &InitContext, _config: ()) -> phoxal::Result<()> {
        Ok(())
    }

    fn step(
        &self,
        _ctx: &StepContext,
        state: (),
        _inputs: &Self::Inputs,
    ) -> phoxal::Result<((), Self::Outputs)> {
        Ok((state, Self::Outputs::default()))
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run(Brain)
}
