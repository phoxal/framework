use crate::config::Bno085Config;
use anyhow::Result;
use anyhow::anyhow;
use phoxal::runtime::Context;

const BACKEND_UNAVAILABLE: &str =
    "bno085 hardware backend unavailable: refusing to publish fabricated IMU measurements";

/// The Bno085 hardware component driver.
pub struct Bno085;

/// The Bno085's endpoint contract: the derived standard surface of its
/// declared capabilities.
#[phoxal::endpoints]
pub struct Bno085Api {}

/// The hardware backend is intentionally unavailable until a real transport
/// can provide measured values and an owned stop path.
#[phoxal::runtime(contract = Bno085Api, period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Bno085 {
    #[init]
    fn new(_config: Bno085Config) -> Result<Self> {
        Err(anyhow!(BACKEND_UNAVAILABLE))
    }

    #[step]
    fn unavailable(&mut self, _ctx: &mut Context<'_, Self>) -> Result<()> {
        Err(anyhow!(BACKEND_UNAVAILABLE))
    }
}

/// Prove the unavailable hardware boundary without starting a supervisor,
/// transport, or simulator.
#[cfg(test)]
mod tests {
    use super::{BACKEND_UNAVAILABLE, Bno085Config};
    use phoxal::contracts::MethodShape;
    use phoxal::contracts::component::imu;
    use phoxal::runtime::Runtime;
    use phoxal::runtime::outputs::OutputSet;
    use phoxal::runtime::{ExecutionTime, initialize};
    use phoxal::schema::MessageSchema;

    type FixtureRuntime = super::phoxal_runtime_bno085::Adapter;

    #[test]
    fn derived_methods_cover_every_declared_capability() {
        assert_eq!(
            crate::api::service_methods::u0::IMU.signature().endpoint,
            "imu"
        );
        assert_eq!(
            crate::api::service_methods::u0::ACCELEROMETER
                .signature()
                .endpoint,
            "accelerometer"
        );
        assert_eq!(
            crate::api::service_methods::u0::GYROSCOPE
                .signature()
                .endpoint,
            "gyroscope"
        );
        assert_eq!(
            crate::api::service_methods::u0::IMU.signature().service,
            "phoxal.component.imu.v1.ImuSample"
        );
        assert_eq!(
            crate::api::service_methods::u0::IMU.signature().shape,
            MethodShape::Observation
        );
        assert!(imu::ImuSample::retain_schema() > 0);
        assert_eq!(
            <FixtureRuntime as Runtime>::Outputs::FIELDS
                .iter()
                .map(|field| field.port)
                .collect::<Vec<_>>(),
            [Some("accelerometer"), Some("gyroscope"), Some("imu")]
        );
        assert!(
            <FixtureRuntime as Runtime>::Outputs::FIELDS
                .iter()
                .all(|field| field.port_signature.is_some())
        );
    }

    #[test]
    fn initialization_fails_before_publishing_without_hardware() {
        let result = initialize(
            &FixtureRuntime::new(),
            ExecutionTime::default(),
            Bno085Config::default(),
        );
        let error = match result {
            Ok(_) => panic!("setup must reject an unavailable hardware backend"),
            Err(error) => error,
        };
        assert_eq!(error.to_string(), BACKEND_UNAVAILABLE);
    }
}
