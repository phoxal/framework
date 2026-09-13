/// Configuration for one counter instance.
#[derive(Debug, serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
pub struct CounterConfig {
    /// Value assigned during initialization.
    #[serde(default)]
    pub initial: u64,
}
