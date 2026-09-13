use crate::config::Bno085Config;
use crate::outputs::Bno085Outputs;
use anyhow::Result;
use anyhow::anyhow;
use phoxal::runtime::InitContext;
use phoxal::runtime::Runtime;
use phoxal::runtime::StepContext;
#[cfg(test)]
use phoxal_component_bno085::FILE_DESCRIPTOR_SET;
#[cfg(test)]
use phoxal_component_bno085::ports;

const BACKEND_UNAVAILABLE: &str =
    "bno085 hardware backend unavailable: refusing to publish fabricated IMU measurements";

/// Driver state retained by the Runtime owner.
#[derive(Debug)]
pub struct Bno085State;

/// The BNO085 hardware component driver.
pub struct Bno085;

#[phoxal::runtime::outputs]
impl Bno085 {}

/// The hardware backend is intentionally unavailable until a real transport
/// can provide measured values and an owned stop path.
#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for Bno085 {
    type Config = Bno085Config;
    type State = Bno085State;
    type Inputs = ();
    type Outputs = Bno085Outputs;

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
    use super::{
        BACKEND_UNAVAILABLE, Bno085, Bno085Config, Bno085Outputs, FILE_DESCRIPTOR_SET, ports,
    };
    use phoxal::runtime::outputs::OutputSet;
    use phoxal::runtime::{ExecutionTime, initialize};
    use phoxal_port::PortKind;

    #[test]
    fn generated_ports_cover_every_declared_capability() {
        assert_eq!(ports::IMU.name(), "imu");
        assert_eq!(ports::ACCELEROMETER.name(), "accelerometer");
        assert_eq!(ports::GYROSCOPE.name(), "gyroscope");
        assert_eq!(
            ports::IMU.signature().service,
            "phoxal.component.bno085.v1.Bno085"
        );
        assert_eq!(ports::IMU.signature().kind, PortKind::Sample);
        assert!(!FILE_DESCRIPTOR_SET.is_empty());
        assert!(!ports::IMU.signature().descriptor_set().is_empty());
        assert_eq!(
            Bno085Outputs::FIELDS
                .iter()
                .map(|field| field.port)
                .collect::<Vec<_>>(),
            [Some("imu"), Some("accelerometer"), Some("gyroscope")]
        );
        assert!(
            Bno085Outputs::FIELDS
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
