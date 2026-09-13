/// The motor's address on its shared RS-485 bus.
#[derive(Debug, serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
pub struct Ddsm115Config {
    /// The id configured on the motor itself.
    pub id: u8,
}
