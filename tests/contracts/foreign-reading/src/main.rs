//! Foreign-vocabulary producer: publishes shaft readings whose names and
//! wire numbers differ from the standard encoder contract.

phoxal::api!();

use crate::api::types::example::foreign::v1::ForeignReading;
use phoxal::runtime::{InitContext, Runtime, StepContext};

#[derive(Debug, Default)]
struct ForeignState {
    step: u64,
    reading: ForeignReading,
}

struct ForeignSource;

#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for ForeignSource {
    type Config = ();
    type State = ForeignState;

    fn init(&self, _ctx: &InitContext, _config: ()) -> phoxal::Result<Self::State> {
        Ok(ForeignState::default())
    }

    fn step(
        &self,
        _ctx: &StepContext,
        mut state: Self::State,
        _inputs: &Self::Inputs,
    ) -> phoxal::Result<(Self::State, Self::Outputs)> {
        state.step = state.step.saturating_add(1);
        // Absence stays absence: the first ten readings omit the rate field.
        state.reading = ForeignReading {
            shaft_position_rad: Some(5.0 + state.step as f64 * 0.001),
            shaft_rate_radps: (state.step > 10).then_some(1.0),
            diagnostics: (state.step % 100 == 0).then(|| format!("step {}", state.step)),
        };
        let mut outputs = Self::Outputs::default();
        outputs.reading(state.reading.clone())?;
        Ok((state, outputs))
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run(ForeignSource)
}
