use crate::config::Vl53l1xConfig;
use crate::outputs::Vl53l1xOutputs;
use anyhow::Result;
use anyhow::anyhow;
use phoxal::runtime::InitContext;
use phoxal::runtime::Runtime;
use phoxal::runtime::StepContext;
#[cfg(test)]
use phoxal_component_vl53l1x::FILE_DESCRIPTOR_SET;
#[cfg(test)]
use phoxal_component_vl53l1x::vl53l1x;

const BACKEND_UNAVAILABLE: &str =
    "vl53l1x hardware backend unavailable: refusing to publish fabricated range measurements";

/// Driver state retained by the Runtime owner.
#[derive(Debug)]
pub struct Vl53l1xState;

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
        BACKEND_UNAVAILABLE, FILE_DESCRIPTOR_SET, Vl53l1x, Vl53l1xConfig, Vl53l1xOutputs, vl53l1x,
    };
    use phoxal::contract::{MethodDescriptor, MethodShape};
    use phoxal::runtime::outputs::OutputSet;
    use phoxal::runtime::{ExecutionTime, initialize};

    #[test]
    fn generated_range_method_has_a_retained_descriptor() {
        assert_eq!(vl53l1x::methods::RANGE.signature().endpoint, "range");
        assert_eq!(
            vl53l1x::methods::RANGE.signature().service,
            "phoxal.component.vl53l1x.v1.Vl53l1x"
        );
        assert_eq!(
            vl53l1x::methods::RANGE.signature().shape,
            MethodShape::Observation
        );
        assert!(!FILE_DESCRIPTOR_SET.is_empty());
        assert!(
            !vl53l1x::methods::RANGE
                .signature()
                .descriptor_set()
                .is_empty()
        );
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
