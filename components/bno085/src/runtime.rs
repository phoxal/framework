use crate::config::Bno085Config;
use anyhow::Result;
use anyhow::anyhow;
use phoxal::runtime::InitContext;
use phoxal::runtime::Runtime;
use phoxal::runtime::StepContext;

const BACKEND_UNAVAILABLE: &str =
    "bno085 hardware backend unavailable: refusing to publish fabricated IMU measurements";

/// Driver state retained by the Runtime owner.
#[derive(Debug)]
pub struct Bno085State;

/// The BNO085 hardware component driver.
pub struct Bno085;

/// The hardware backend is intentionally unavailable until a real transport
/// can provide measured values and an owned stop path.
#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for Bno085 {
    type Config = Bno085Config;
    type State = Bno085State;

    fn init(&self, _ctx: &InitContext, _config: Self::Config) -> Result<Self::State> {
        Err(anyhow!(BACKEND_UNAVAILABLE))
    }

    fn step(
        &self,
        _ctx: &StepContext,
        _state: Self::State,
        _inputs: &Self::Inputs,
    ) -> Result<(Self::State, Self::Outputs)> {
        Err(anyhow!(BACKEND_UNAVAILABLE))
    }
}

/// Prove the unavailable hardware boundary without starting a supervisor,
/// transport, or simulator.
#[cfg(test)]
mod tests {
    use super::{BACKEND_UNAVAILABLE, Bno085, Bno085Config};
    use phoxal::contract::{MethodDescriptor, MethodShape};
    use phoxal::runtime::Runtime;
    use phoxal::runtime::outputs::OutputSet;
    use phoxal::runtime::{ExecutionTime, initialize};

    #[test]
    fn generated_methods_cover_every_declared_capability() {
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
            "phoxal.component.bno085.v1.ImuSample"
        );
        assert_eq!(
            crate::api::service_methods::u0::IMU.signature().shape,
            MethodShape::Observation
        );
        assert!(
            !crate::api::service_methods::u0::IMU
                .signature()
                .descriptor_set()
                .is_empty()
        );
        assert_eq!(
            <Bno085 as Runtime>::Outputs::FIELDS
                .iter()
                .map(|field| field.port)
                .collect::<Vec<_>>(),
            [Some("accelerometer"), Some("gyroscope"), Some("imu")]
        );
        assert!(
            <Bno085 as Runtime>::Outputs::FIELDS
                .iter()
                .all(|field| field.port_signature.is_some())
        );
    }

    #[test]
    fn initialization_fails_before_publishing_without_hardware() {
        let result = initialize(&Bno085, ExecutionTime::default(), Bno085Config::default());
        let error = result.expect_err("setup must reject an unavailable hardware backend");
        assert_eq!(error.to_string(), BACKEND_UNAVAILABLE);
    }
}
