/// The component driver's authored configuration.
///
/// The hardware transport is not available in this framework release, so the
/// driver deliberately accepts no configuration beyond its component-owned
/// connection block.
#[derive(Debug, Default, serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
pub struct Bno085Config {}
