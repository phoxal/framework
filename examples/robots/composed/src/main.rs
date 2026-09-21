use phoxal::runtime::{InitContext, Runtime, StepContext};

#[allow(dead_code)]
type SharedCounterState = counter_contract::CounterState;

struct Brain;

#[phoxal::runtime(period_ms = 100, timeout_ms = 50, init_timeout_ms = 1_000)]
impl Runtime for Brain {
    type Config = ();
    type State = ();
    type Inputs = ();
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
impl Brain {}

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run(Brain)
}
