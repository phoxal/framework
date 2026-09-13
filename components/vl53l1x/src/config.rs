/// The component driver's authored configuration.
#[derive(Debug, Default, serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
pub struct Vl53l1xConfig {}
