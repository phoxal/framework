use crate::config::ZedF9pConfig;
use anyhow::Result;
use anyhow::anyhow;
use phoxal::runtime::Context;

const BACKEND_UNAVAILABLE: &str =
    "zed_f9p hardware backend unavailable: refusing to publish fabricated GNSS measurements";

/// The ZED-F9P hardware component driver.
pub struct ZedF9p;

/// The ZedF9p endpoint contract: the derived standard surface of
/// its declared capabilities.
#[phoxal::endpoints]
pub struct ZedF9pApi {}

/// The hardware backend is intentionally unavailable until a real receiver
/// transport can publish measured fixes and stop safely.
#[phoxal::runtime(contract = ZedF9pApi, period_ms = 100, timeout_ms = 200, init_timeout_ms = 1_000)]
impl ZedF9p {
    #[init]
    fn new(_config: ZedF9pConfig) -> Result<Self> {
        Err(anyhow!(BACKEND_UNAVAILABLE))
    }

    #[step]
    fn unavailable(&mut self, _ctx: &mut Context<'_, Self>) -> Result<()> {
        Err(anyhow!(BACKEND_UNAVAILABLE))
    }
}

#[cfg(test)]
mod tests {
    use super::{BACKEND_UNAVAILABLE, ZedF9pConfig};
    use phoxal::contracts::MethodShape;
    use phoxal::contracts::component::gnss;
    use phoxal::runtime::Runtime;
    use phoxal::runtime::outputs::OutputSet;
    use phoxal::runtime::{ExecutionTime, initialize};
    use phoxal::schema::MessageSchema;

    #[test]
    fn derived_gnss_method_retains_the_standard_schema() {
        assert_eq!(
            crate::api::service_methods::u0::GNSS.signature().endpoint,
            "gnss"
        );
        assert_eq!(
            crate::api::service_methods::u0::GNSS.signature().service,
            "phoxal.component.gnss.v1.GnssSample"
        );
        assert_eq!(
            crate::api::service_methods::u0::GNSS.signature().shape,
            MethodShape::Observation
        );
        assert!(gnss::GnssSample::retain_schema() > 0);
        assert_eq!(
            <super::phoxal_runtime_zed_f9p::Adapter as Runtime>::Outputs::FIELDS[0].name,
            "gnss"
        );
        assert_eq!(
            <super::phoxal_runtime_zed_f9p::Adapter as Runtime>::Outputs::FIELDS[0].port,
            Some("gnss")
        );
        assert!(
            <super::phoxal_runtime_zed_f9p::Adapter as Runtime>::Outputs::FIELDS[0]
                .port_signature
                .is_some()
        );
    }

    #[test]
    fn initialization_fails_before_publishing_without_hardware() {
        let result = initialize(
            &super::phoxal_runtime_zed_f9p::Adapter::new(),
            ExecutionTime::default(),
            ZedF9pConfig::default(),
        );
        let error = match result {
            Ok(_) => panic!("setup must reject an unavailable hardware backend"),
            Err(error) => error,
        };
        assert_eq!(error.to_string(), BACKEND_UNAVAILABLE);
    }
}
