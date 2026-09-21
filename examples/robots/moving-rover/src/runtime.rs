use crate::{config::Config, inputs::Inputs, outputs::Outputs};
use phoxal::runtime::{InitContext, Runtime, StepContext};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Brain;

#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for Brain {
    type Config = Config;
    type State = ();
    type Inputs = Inputs;
    type Outputs = Outputs;

    fn init(&self, _ctx: &InitContext, _config: Config) -> phoxal::Result<()> {
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
#[allow(dead_code, reason = "generated output projections")]
impl Brain {
    /// No operator or autonomous mission is selected at startup.
    #[phoxal::runtime::outputs::setpoint(port = motion::ports::MANUAL, max_bytes = 256, valid_for_ms = 100)]
    fn manual(&self, _state: &()) -> Option<motion::MotionIntent> {
        None
    }

    #[phoxal::runtime::outputs::setpoint(port = motion::ports::AUTONOMOUS, max_bytes = 256, valid_for_ms = 100)]
    fn autonomous(&self, _state: &()) -> Option<motion::MotionIntent> {
        None
    }
}
