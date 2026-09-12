use phoxal::runtime::input::Operation;
use phoxal::runtime::{Activation, InitContext, Runtime, StepContext};

struct Service;

#[phoxal::runtime::inputs]
struct Inputs {
    operation: Operation<u64, u32>,
}

#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 100)]
impl Runtime for Service {
    type Config = ();
    type State = ();
    type Inputs = Inputs;
    type Outputs = ();

    fn init(&self, _ctx: &InitContext, _config: Self::Config) -> phoxal::Result<Self::State> {
        Ok(())
    }

    fn step(
        &self,
        _ctx: &StepContext,
        state: Self::State,
        _inputs: &Self::Inputs,
    ) -> phoxal::Result<(Self::State, Self::Outputs)> {
        Ok((state, ()))
    }
}

#[phoxal::runtime::outputs]
impl Service {
    #[phoxal::runtime::outputs::activate(operation)]
    fn select(&self, _state: &()) -> Option<Activation<u64, u8>> {
        None
    }

    #[phoxal::runtime::outputs::operation(operation, timeout_ms = 10, cancel_grace_ms = 10)]
    fn worker(input: u8) -> phoxal::Result<u16> {
        Ok(u16::from(input))
    }
}

fn main() {}
