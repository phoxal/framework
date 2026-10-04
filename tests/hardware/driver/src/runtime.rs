use crate::config::HardwareFixtureConfig;
use anyhow::Result;
use phoxal::contracts::component::actuator::Control;
use phoxal::contracts::component::encoder::EncoderSample;
use phoxal::runtime::Context;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

struct RuntimeControl {
    stalled: AtomicBool,
    stop_requested: AtomicBool,
}

impl RuntimeControl {
    const fn new() -> Self {
        Self {
            stalled: AtomicBool::new(false),
            stop_requested: AtomicBool::new(false),
        }
    }
}

/// Registry of live fixture controls: the authored runtime owns its state
/// privately inside the canonical runtime owner, so each initialization records
/// its control here and the test harness claims the one belonging to the
/// owner it just constructed.
static CONTROLS: std::sync::Mutex<Vec<std::sync::Arc<RuntimeControl>>> =
    std::sync::Mutex::new(Vec::new());

/// An authored runtime backed by the acceptance fixture's injected I/O.
///
/// The default value is used by the standalone binary.  Acceptance tests
/// use the private control handle to inject a stalled
/// computation or request a terminal stop; no production hardware behavior is
/// implied by those controls.
pub struct HardwareFixtureDriver {
    control: std::sync::Arc<RuntimeControl>,
    accepted_steps: u64,
    applied_velocity_radps: f64,
}

#[phoxal::runtime(contract = crate::contract::DriverApi, period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl HardwareFixtureDriver {
    #[init]
    fn start(config: HardwareFixtureConfig) -> Result<Self> {
        if config.device_id.trim().is_empty() {
            anyhow::bail!("fixture device_id must not be empty");
        }
        let control = std::sync::Arc::new(RuntimeControl::new());
        CONTROLS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(std::sync::Arc::clone(&control));
        Ok(Self {
            control,
            accepted_steps: 0,
            applied_velocity_radps: 0.0,
        })
    }

    #[step]
    fn advance(&mut self, ctx: &mut Context<'_, Self>) -> Result<()> {
        while self.control.stalled.load(Ordering::Acquire) {
            if self.control.stop_requested.load(Ordering::Acquire) {
                anyhow::bail!("fixture computation stopped while stalled");
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        if self.control.stop_requested.load(Ordering::Acquire) {
            anyhow::bail!("fixture computation stop requested");
        }

        self.accepted_steps = self.accepted_steps.saturating_add(1);
        self.applied_velocity_radps = ctx
            .actuator()
            .valid()
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
        for sample in ctx.acquired().items() {
            let observation = *sample.payload();
            ctx.emit_encoder(EncoderSample {
                position_rad: Some(observation.position_rad),
                velocity_radps: None,
            })?;
            ctx.emit_observations(observation)?;
        }
        Ok(())
    }
}

/// Claims the control of the most recently initialized fixture runtime.
#[cfg(test)]
fn latest_control() -> std::sync::Arc<RuntimeControl> {
    CONTROLS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .last()
        .cloned()
        .expect("a fixture runtime initialized first")
}

/// Serializes runner construction against the control claim so concurrent
/// tests always pair with their own runtime's control.
#[cfg(test)]
static HANDOFF: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests;
