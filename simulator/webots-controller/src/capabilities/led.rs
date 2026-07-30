//! LED capability: subscribes `component::led::Command` and drives the Webots
//! `LED` device. An LED is an actuator, so this capability is an input to the
//! controller rather than one of its outputs.

use anyhow::{Result, anyhow};
use phoxal::api;
use phoxal::model::component::CapabilityRef;

#[derive(Clone, Debug)]
pub(crate) struct LedSpec {
    pub(crate) reference: CapabilityRef,
}

pub(crate) struct NativeLed {
    led: webots_rs::device::led::Led,
}

impl NativeLed {
    pub(crate) fn new(webots: &webots_rs::Webots, spec: &LedSpec) -> Result<Self> {
        let led = webots
            .led(spec.reference.to_string())
            .map_err(|error| anyhow!(error))?;
        Ok(Self { led })
    }

    pub(crate) fn apply(&self, command: &api::component::led::Command) -> Result<()> {
        self.led
            .set(match command {
                api::component::led::Command::On => 1,
                api::component::led::Command::Off => 0,
            })
            .map_err(|error| anyhow!(error))
    }
}
