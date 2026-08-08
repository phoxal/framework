//! LED capability: applies `component::led::Command` to the Webots `LED`
//! device. An LED is an actuator, so this capability is an input to the
//! controller rather than one of its outputs.

use anyhow::Result;
use phoxal::api;
use phoxal::model::identity::CapabilityRef;

pub(crate) struct NativeLed {
    led: webots_rs::device::led::Led,
}

impl NativeLed {
    pub(crate) fn new(webots: &webots_rs::Webots, reference: &CapabilityRef) -> Result<Self> {
        Ok(Self {
            led: webots.led(reference.to_string())?,
        })
    }

    pub(crate) fn apply(&self, command: &api::component::led::Command) -> Result<()> {
        // Webots takes an LED's colour index; a single-colour LED is off at 0
        // and on at 1, which is the whole of what the contract expresses.
        self.led.set(match command {
            api::component::led::Command::On => 1,
            api::component::led::Command::Off => 0,
        })?;
        Ok(())
    }
}
