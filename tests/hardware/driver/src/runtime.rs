use crate::config::HardwareFixtureConfig;
use crate::contract::FixtureObservation;
use anyhow::Result;
use phoxal::contracts::component::actuator::Control;
use phoxal::contracts::component::encoder::EncoderSample;
use phoxal::runtime::{InitContext, Runtime, StepContext};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// State retained by the fixture Runtime.
#[derive(Debug, Default)]
pub struct HardwareFixtureState {
    accepted_steps: u64,
    applied_velocity_radps: f64,
}

#[derive(Default)]
struct RuntimeControl {
    stalled: AtomicBool,
    stop_requested: AtomicBool,
}

/// A Runtime driver backed by the acceptance fixture's injected I/O.
///
/// The default value is used by the standalone binary.  Acceptance tests
/// clone the driver and use the private control handle to inject a stalled
/// computation or request a terminal stop; no production hardware behavior is
/// implied by those controls.
#[derive(Clone, Default)]
pub struct HardwareFixtureDriver {
    control: Arc<RuntimeControl>,
}

impl HardwareFixtureDriver {
    /// Creates a fixture driver with no injected fault.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[phoxal::runtime(contract = crate::contract::DriverApi, period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for HardwareFixtureDriver {
    type Config = HardwareFixtureConfig;
    type State = HardwareFixtureState;

    fn init(&self, _ctx: &InitContext, config: Self::Config) -> Result<Self::State> {
        if config.device_id.trim().is_empty() {
            anyhow::bail!("fixture device_id must not be empty");
        }
        Ok(HardwareFixtureState::default())
    }

    fn step(
        &self,
        ctx: &StepContext,
        mut state: Self::State,
        inputs: &Self::Inputs,
    ) -> Result<(Self::State, Self::Outputs)> {
        while self.control.stalled.load(Ordering::Acquire) {
            if self.control.stop_requested.load(Ordering::Acquire) {
                anyhow::bail!("fixture computation stopped while stalled");
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        if self.control.stop_requested.load(Ordering::Acquire) {
            anyhow::bail!("fixture computation stop requested");
        }

        state.accepted_steps = state.accepted_steps.saturating_add(1);
        state.applied_velocity_radps = inputs
            .actuator
            .value()
            .filter(|_| inputs.actuator.is_valid_at(ctx.now()))
            .and_then(|setpoint| {
                setpoint
                    .targets
                    .iter()
                    .find(|target| target.actuator_id == "fixture_motor")
                    .and_then(|target| match target.control.as_ref() {
                        Some(Control::VelocityRadps(value)) => Some(*value),
                        _ => None,
                    })
            })
            .unwrap_or(0.0);
        let measurements: Vec<FixtureObservation> = inputs
            .acquired
            .items()
            .iter()
            .map(|sample| *sample.payload())
            .collect();
        let mut outputs = Self::Outputs::default();
        outputs.encoder(
            measurements
                .iter()
                .map(|sample| EncoderSample {
                    position_rad: Some(sample.position_rad),
                    velocity_radps: None,
                })
                .collect(),
        )?;
        outputs.observations(measurements)?;
        Ok((state, outputs))
    }
}

#[cfg(test)]
mod tests;
