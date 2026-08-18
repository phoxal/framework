//! The hardware connection block a robot document attaches to a component
//! instance.
//!
//! This vocabulary sits beside the versioned document bodies rather than inside
//! one of them: it is the shape the compiler hands to build tooling as
//! [`crate::authoring::CompiledDriver::config`], so it belongs to the document family
//! rather than to one generation of its grammar. A generation that spells the
//! block differently converts into these types in its own normalizer, and the
//! compiler below the normalization boundary keeps seeing exactly one driver
//! shape.
//!
//! `robot.yaml` v0 re-exports these types, so an authored v0 document keeps
//! naming them at their established path.
//!
//! [`DriverConfig`] deliberately carries no doc comment: `schemars` renders one
//! into the published editor schema, and this move is a refactor that must
//! leave that schema byte-identical.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DriverConfig {
    pub connection: ConnectionConfig,
    #[serde(default = "default_runtime_clock_ms")]
    pub runtime_clock_ms: u64,
}

/// Connection configuration for executable drivers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConnectionConfig {
    /// CAN bus connection.
    Can { bus: u8, node_id: u8 },
    /// I2C connection.
    I2c { bus: u8, address: u16 },
    /// SPI connection.
    Spi { bus: u8, chip_select: u8 },
    /// Serial port connection (RS-232/RS-485).
    Serial { port: String, baud: u32 },
    /// UART connection (distinct from Serial for hardware-specific drivers).
    Uart { port: String, baud_rate: u32 },
    /// USB connection.
    Usb {
        vendor_id: Option<u16>,
        product_id: Option<u16>,
    },
    /// GPIO pins.
    Gpio {
        chip: String,
        pins: Vec<GpioPinConfig>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GpioPinConfig {
    pub line: u16,
    pub direction: GpioDirection,
    #[serde(default)]
    pub active_low: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GpioDirection {
    Input,
    Output,
}

const fn default_runtime_clock_ms() -> u64 {
    100
}
