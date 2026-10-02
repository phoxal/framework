use crate::config::Vl53l1xConfig;
use anyhow::Result;
use anyhow::anyhow;
use phoxal::runtime::Context;

const BACKEND_UNAVAILABLE: &str =
    "vl53l1x hardware backend unavailable: refusing to publish fabricated range measurements";

/// The VL53L1X hardware component driver.
pub struct Vl53l1x;

/// The Vl53l1x endpoint contract: the derived standard surface of
/// its declared capabilities.
#[phoxal::endpoints]
pub struct Vl53l1xApi {}

/// The hardware backend is intentionally unavailable until a real I2C
/// transport can publish measured ranges and stop safely.
#[phoxal::runtime(contract = Vl53l1xApi, period_ms = 50, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Vl53l1x {
    #[init]
    fn new(_config: Vl53l1xConfig) -> Result<Self> {
        Err(anyhow!(BACKEND_UNAVAILABLE))
    }

    #[step]
    fn unavailable(&mut self, _ctx: &mut Context<'_, Self>) -> Result<()> {
        Err(anyhow!(BACKEND_UNAVAILABLE))
    }
}

#[cfg(test)]
mod tests {
    use super::{BACKEND_UNAVAILABLE, Vl53l1xConfig};
    use phoxal::contracts::MethodShape;
    use phoxal::contracts::component::range;
    use phoxal::runtime::Runtime;
    use phoxal::runtime::outputs::OutputSet;
    use phoxal::runtime::{ExecutionTime, initialize};
    use phoxal::schema::MessageSchema;

    #[test]
    fn derived_range_method_retains_the_standard_schema() {
        assert_eq!(
            crate::api::service_methods::u0::RANGE.signature().endpoint,
            "range"
        );
        assert_eq!(
            crate::api::service_methods::u0::RANGE.signature().service,
            "phoxal.robotics.v1.RangeSample"
        );
        assert_eq!(
            crate::api::service_methods::u0::RANGE.signature().shape,
            MethodShape::Observation
        );
        assert!(range::RangeSample::retain_schema() > 0);
        assert_eq!(
            <super::phoxal_runtime_vl53l1x::Adapter as Runtime>::Outputs::FIELDS[0].name,
            "range"
        );
        assert_eq!(
            <super::phoxal_runtime_vl53l1x::Adapter as Runtime>::Outputs::FIELDS[0].port,
            Some("range")
        );
        assert!(
            <super::phoxal_runtime_vl53l1x::Adapter as Runtime>::Outputs::FIELDS[0]
                .port_signature
                .is_some()
        );
    }

    #[test]
    fn initialization_fails_before_publishing_without_hardware() {
        let result = initialize(
            &super::phoxal_runtime_vl53l1x::Adapter::new(),
            ExecutionTime::default(),
            Vl53l1xConfig::default(),
        );
        let error = match result {
            Ok(_) => panic!("setup must reject an unavailable hardware backend"),
            Err(error) => error,
        };
        assert_eq!(error.to_string(), BACKEND_UNAVAILABLE);
    }
}
