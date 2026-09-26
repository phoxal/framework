//! Producer A: the standard encoder contract from its own independent
//! package identity, with observably different values from producer B.

phoxal::api!();

use phoxal::robotics::EncoderSample;
use phoxal::runtime::{InitContext, Runtime, StepContext};

#[derive(Clone, Debug, Default, serde::Deserialize, phoxal::Config)]
struct EncoderConfig {
    /// Offset distinguishing this producer's measurements.
    #[serde(default = "default_base_position")]
    base_position_rad: f64,
}

fn default_base_position() -> f64 {
    0.0
}

#[derive(Debug)]
struct EncoderState {
    base_position_rad: f64,
    step: u64,
    sample: EncoderSample,
}

struct EncoderA;

fn sample(base: f64, step: u64) -> EncoderSample {
    EncoderSample {
        position_rad: Some(base + f64::from(u32::try_from(step).unwrap_or(u32::MAX)) * 0.001),
        velocity_radps: Some(1.0),
    }
}

#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for EncoderA {
    type Config = EncoderConfig;
    type State = EncoderState;

    fn init(&self, _ctx: &InitContext, config: Self::Config) -> phoxal::Result<Self::State> {
        Ok(EncoderState {
            base_position_rad: config.base_position_rad,
            step: 0,
            sample: sample(config.base_position_rad, 0),
        })
    }

    fn step(
        &self,
        _ctx: &StepContext,
        mut state: Self::State,
        inputs: &Self::Inputs,
    ) -> phoxal::Result<(Self::State, Self::Outputs)> {
        state.step = state.step.saturating_add(1);
        state.sample = sample(state.base_position_rad, state.step);
        let mut outputs = Self::Outputs::default();
        outputs.encoder(state.sample)?;
        outputs.ticks(vec![state.sample])?;
        for request in inputs.read_encoder.items() {
            // A side-effect-free read of the current measurement.
            outputs.read_encoder_reply(request.reply(state.sample))?;
        }
        Ok((state, outputs))
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run(EncoderA)
}
