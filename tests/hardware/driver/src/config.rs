/// Configuration for one selected fixture device.
#[derive(Debug, serde::Deserialize, serde::Serialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
pub struct HardwareFixtureConfig {
    /// Stable identity of the injected fixture device.
    pub device_id: String,
}
