use crate::config::ZedF9pConfig;
use crate::outputs::ZedF9pOutputs;
use anyhow::Result;
use anyhow::anyhow;
use phoxal::runtime::InitContext;
use phoxal::runtime::Runtime;
use phoxal::runtime::StepContext;
#[cfg(test)]
use phoxal_component_zed_f9p::FILE_DESCRIPTOR_SET;
#[cfg(test)]
use phoxal_component_zed_f9p::ports;

const BACKEND_UNAVAILABLE: &str =
    "zed_f9p hardware backend unavailable: refusing to publish fabricated GNSS measurements";

/// Driver state retained by the Runtime owner.
#[derive(Debug)]
pub struct ZedF9pState;

/// The ZED-F9P hardware component driver.
pub struct ZedF9p;

#[phoxal::runtime::outputs]
impl ZedF9p {}

/// The hardware backend is intentionally unavailable until a real receiver
/// transport can publish measured fixes and stop safely.
#[phoxal::runtime(period_ms = 100, timeout_ms = 200, init_timeout_ms = 1_000)]
impl Runtime for ZedF9p {
    type Config = ZedF9pConfig;
    type State = ZedF9pState;
    type Inputs = ();
    type Outputs = ZedF9pOutputs;

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
        BACKEND_UNAVAILABLE, FILE_DESCRIPTOR_SET, ZedF9p, ZedF9pConfig, ZedF9pOutputs, ports,
    };
    use phoxal::port::PortKind;
    use phoxal::runtime::outputs::OutputSet;
    use phoxal::runtime::{ExecutionTime, initialize};

    #[test]
    fn generated_gnss_port_has_a_retained_descriptor() {
        assert_eq!(ports::GNSS.name(), "gnss");
        assert_eq!(
            ports::GNSS.signature().service,
            "phoxal.component.zed_f9p.v1.ZedF9p"
        );
        assert_eq!(ports::GNSS.signature().kind, PortKind::Sample);
        assert!(!FILE_DESCRIPTOR_SET.is_empty());
        assert!(!ports::GNSS.signature().descriptor_set().is_empty());
        assert_eq!(ZedF9pOutputs::FIELDS[0].name, "gnss");
        assert_eq!(ZedF9pOutputs::FIELDS[0].port, Some("gnss"));
        assert!(ZedF9pOutputs::FIELDS[0].port_signature.is_some());
    }

    #[test]
    fn initialization_fails_before_publishing_without_hardware() {
        let result = initialize(&ZedF9p, ExecutionTime::default(), ZedF9pConfig::default());
        let error = result.expect_err("setup must reject an unavailable hardware backend");
        assert_eq!(error.to_string(), BACKEND_UNAVAILABLE);
    }
}
