//! Foreign-vocabulary producer: the same physical quantities as the standard
//! encoder contract under different names and wire numbers, plus a harmless
//! diagnostics field the receiver-side mapping deliberately omits.

use phoxal::contracts::Latest;
use phoxal::runtime::{InitContext, Runtime, StepContext};

#[phoxal::message(package = "example.foreign.v1")]
pub struct ForeignReading {
    #[phoxal(tag = 2)]
    pub shaft_rate_radps: Option<f64>,
    #[phoxal(tag = 5)]
    pub shaft_position_rad: Option<f64>,
    #[phoxal(tag = 9)]
    pub diagnostics: Option<String>,
}

/// The foreign reading contract.
#[phoxal::endpoints]
pub struct ForeignApi {
    #[phoxal::output(max_bytes = 1024)]
    reading: Latest<ForeignReading>,
}

#[derive(Debug)]
struct ForeignState {
    step: u64,
}

struct ForeignReadingService;

#[phoxal::runtime(contract = ForeignApi, period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for ForeignReadingService {
    type Config = ();
    type State = ForeignState;

    fn init(&self, _ctx: &InitContext, _config: Self::Config) -> phoxal::Result<Self::State> {
        Ok(ForeignState { step: 0 })
    }

    fn step(
        &self,
        _ctx: &StepContext,
        mut state: Self::State,
        _inputs: &Self::Inputs,
    ) -> phoxal::Result<(Self::State, Self::Outputs)> {
        state.step = state.step.saturating_add(1);
        let mut outputs = Self::Outputs::default();
        let mut reading = ForeignReading {
            shaft_rate_radps: Some(0.5),
            shaft_position_rad: Some(
                5.0 + f64::from(u32::try_from(state.step).unwrap_or(u32::MAX)) * 0.001,
            ),
            diagnostics: None,
        };
        // Absence stays absence: the rate is omitted for the first ten steps.
        if state.step < 10 {
            reading.shaft_rate_radps = None;
        }
        if state.step.is_multiple_of(100) {
            reading.diagnostics = Some("periodic".to_owned());
        }
        outputs.reading(reading)?;
        Ok((state, outputs))
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run(ForeignReadingService)
}
