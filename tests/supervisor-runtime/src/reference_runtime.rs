//! The reference runtime used by the supervisor process-boundary tests.

use phoxal::contracts::Empty;
use phoxal::contracts::component::encoder::EncoderSample;
use phoxal::runtime::{InitContext, Runtime, StepContext};

use crate::contract::{InspectionReadResponse, InspectionState};

/// A compiled, input-free runtime owning one generated call, one retained
/// inspection observation, and a standard encoder output.
pub struct ReferenceRuntime {
    /// Marker file written on lifecycle transitions, observed by tests.
    pub marker: std::path::PathBuf,
}

impl crate::contract::inspection_api::projections::Projections for ReferenceRuntime {
    type State = u64;

    fn status(&self, state: &u64) -> InspectionState {
        InspectionState {
            count: *state,
            active: true,
        }
    }
}

#[phoxal::runtime(contract = crate::contract::InspectionApi, period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for ReferenceRuntime {
    fn step(
        &self,
        _ctx: &StepContext,
        state: Self::State,
        inputs: &Self::Inputs,
    ) -> phoxal::Result<(Self::State, Self::Outputs)> {
        if state == 0 {
            std::fs::write(&self.marker, b"stepped")?;
        }
        let next = state.saturating_add(1);
        let mut outputs = Self::Outputs::default();
        // The derived standard encoder endpoint: one live sample per step,
        // derived from the step counter rather than fabricated constants.
        outputs.encoder(vec![EncoderSample {
            position_rad: Some(state as f64 * 0.001),
            velocity_radps: Some(0.05),
        }])?;
        for command in inputs.read.items() {
            outputs.read_reply(command.reply(InspectionReadResponse {
                state: Some(InspectionState {
                    count: next,
                    active: command.request().key == "status",
                }),
            }))?;
        }
        for command in inputs.calibrate.items() {
            outputs.calibrate_reply(command.reply(Empty {}))?;
        }
        Ok((next, outputs))
    }

    type State = u64;

    fn init(&self, _ctx: &InitContext, _config: Self::Config) -> phoxal::Result<Self::State> {
        std::fs::write(&self.marker, b"initialized")?;
        Ok(0)
    }

    type Config = ();
}
