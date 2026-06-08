use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriverConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    pub connection: ConnectionConfig,
    #[serde(default = "default_runtime_clock_ms")]
    pub runtime_clock_ms: u64,
}

/// Connection configuration for executable drivers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GpioPinConfig {
    pub line: u16,
    pub direction: GpioDirection,
    #[serde(default)]
    pub active_low: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GpioDirection {
    Input,
    Output,
}

const fn default_runtime_clock_ms() -> u64 {
    100
}
