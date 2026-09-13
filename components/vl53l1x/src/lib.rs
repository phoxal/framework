//! VL53L1X component contract and hardware driver.

use anyhow::{Result, anyhow};
use phoxal::runtime::{InitContext, Runtime, Sample, StepContext};

include!(concat!(env!("OUT_DIR"), "/phoxal.component.vl53l1x.v1.rs"));

/// Public typed ports owned by the VL53L1X component contract.
pub use vl53l1x::ports;

/// The original descriptor closure retained for independent contract
/// inspection and native artifact extraction.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/phoxal-descriptors.bin"));

const BACKEND_UNAVAILABLE: &str =
    "vl53l1x hardware backend unavailable: refusing to publish fabricated range measurements";

/// The component driver's authored configuration.
#[derive(Debug, Default, serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
pub struct Vl53l1xConfig {}

/// Driver state retained by the Runtime owner.
#[derive(Debug)]
pub struct Vl53l1xState;

/// VL53L1X observations admitted at one Runtime boundary.
#[phoxal::runtime::outputs]
#[derive(Default)]
pub struct Vl53l1xOutputs {
    /// Measured time-of-flight range observations.
    #[phoxal::runtime::outputs::sample(
        port = ports::RANGE,
        max_items = 16,
        max_bytes = 8_192
    )]
    pub range: Vec<Sample<RangeSample>>,
}

/// The VL53L1X hardware component driver.
pub struct Vl53l1x;

#[phoxal::runtime::outputs]
impl Vl53l1x {}

/// The hardware backend is intentionally unavailable until a real I2C
/// transport can publish measured ranges and stop safely.
#[phoxal::runtime(period_ms = 50, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for Vl53l1x {
    type Config = Vl53l1xConfig;
    type State = Vl53l1xState;
    type Inputs = ();
    type Outputs = Vl53l1xOutputs;

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

#[cfg(test)]
mod tests {
    use super::{
        BACKEND_UNAVAILABLE, FILE_DESCRIPTOR_SET, Vl53l1x, Vl53l1xConfig, Vl53l1xOutputs, ports,
    };
    use phoxal::runtime::outputs::OutputSet;
    use phoxal::runtime::{ExecutionTime, initialize};
    use phoxal_port::PortKind;

    #[test]
    fn generated_range_port_has_a_retained_descriptor() {
        assert_eq!(ports::RANGE.name(), "range");
        assert_eq!(
            ports::RANGE.signature().service,
            "phoxal.component.vl53l1x.v1.Vl53l1x"
        );
        assert_eq!(ports::RANGE.signature().kind, PortKind::Sample);
        assert!(!FILE_DESCRIPTOR_SET.is_empty());
        assert!(!ports::RANGE.signature().descriptor_set().is_empty());
        assert_eq!(Vl53l1xOutputs::FIELDS[0].name, "range");
        assert_eq!(Vl53l1xOutputs::FIELDS[0].port, Some("range"));
        assert!(Vl53l1xOutputs::FIELDS[0].port_signature.is_some());
    }

    #[test]
    fn initialization_fails_before_publishing_without_hardware() {
        let result = initialize(&Vl53l1x, ExecutionTime::default(), Vl53l1xConfig::default());
        let error = result.expect_err("setup must reject an unavailable hardware backend");
        assert_eq!(error.to_string(), BACKEND_UNAVAILABLE);
    }
}
