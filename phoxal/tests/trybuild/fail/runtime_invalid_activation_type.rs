use phoxal::runtime::input::Read;
use phoxal::runtime::{Activation, InitContext, Runtime, StepContext};

struct Service;

#[phoxal::runtime::inputs]
struct Inputs {
    #[phoxal::runtime::input(max_response_bytes = 32)]
    read: Read<u64, u32, u64>,
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
    #[phoxal::runtime::outputs::activate(read, timeout_ms = 10)]
    fn select(&self, _state: &()) -> Option<Activation<u64, u64>> {
        None
    }
}

fn main() {}
