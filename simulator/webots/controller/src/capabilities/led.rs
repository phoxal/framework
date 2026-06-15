use anyhow::{Result, anyhow};
use phoxal::api::component::capability::led::v1::Command;
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Config {}

pub struct Led {
    led: webots_rs::device::led::Led,
}

impl Led {
    pub fn new(led: webots_rs::device::led::Led, _: &Config) -> Self {
        Self { led }
    }

    pub fn apply(&self, command: &Command) -> Result<()> {
        let value = match command {
            Command::On => 1,
            Command::Off => 0,
        };

        self.led.set(value).map_err(|error| anyhow!(error))
    }
}
