use crate::config::Ddsm115Config;
use anyhow::Result;
use anyhow::anyhow;
use phoxal::runtime::InitContext;
use phoxal::runtime::Runtime;
use phoxal::runtime::StepContext;

const BACKEND_UNAVAILABLE: &str = "ddsm115 hardware backend unavailable: refusing to model motor or publish fabricated encoder measurements";

/// Driver state retained by the Runtime owner.
#[derive(Debug)]
pub struct Ddsm115State;

/// The DDSM115 hardware component driver. The endpoint surface is
/// generated from the component's declared capabilities: a
/// velocity-commanded motor implies the leased actuator setpoint input and
/// the encoder implies the queued encoder sample output.
pub struct Ddsm115;

#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for Ddsm115 {
    fn step(
        &self,
        _ctx: &StepContext,
        _state: Self::State,
        _inputs: &Self::Inputs,
    ) -> Result<(Self::State, Self::Outputs)> {
        Err(anyhow!(BACKEND_UNAVAILABLE))
    }

    type State = Ddsm115State;

    fn init(&self, _ctx: &InitContext, config: Self::Config) -> Result<Self::State> {
        Err(anyhow!("{BACKEND_UNAVAILABLE} (motor ID {})", config.id))
    }

    type Config = Ddsm115Config;
}

#[cfg(test)]
mod tests {
    use super::{BACKEND_UNAVAILABLE, Ddsm115, Ddsm115Config};
    use phoxal::contracts::MethodShape;
    use phoxal::runtime::Runtime;
    use phoxal::runtime::input::InputSet;
    use phoxal::runtime::outputs::OutputSet;
    use phoxal::runtime::{ExecutionTime, initialize};

    #[test]
    fn capabilities_determine_the_standard_endpoints() {
        let encoder = crate::api::service_methods::u0::ENCODER.signature();
        assert_eq!(encoder.endpoint, "encoder");
        assert_eq!(encoder.service, "phoxal.robotics.v1.EncoderSample");
        assert_eq!(encoder.shape, MethodShape::Observation);
        let actuator = crate::api::service_methods::u0::ACTUATOR.signature();
        assert_eq!(actuator.endpoint, "actuator");
        assert_eq!(
            actuator.request,
            "phoxal.component.actuator.v1.ActuatorSetpoint"
        );
        assert_eq!(actuator.lease.map(|lease| lease.valid_for_ms()), Some(100));
        let inputs = &<Ddsm115 as Runtime>::Inputs::FIELDS;
        assert_eq!(inputs[0].name, "actuator");
        assert_eq!(
            <Ddsm115 as Runtime>::Outputs::FIELDS[0].port,
            Some("encoder")
        );
        // Standard types carry no compiled descriptor set; the vocabulary's
        // schema frames are retained instead, exactly like a Rust contract.
        assert!(crate::api::service_methods::u0::retain_standard_schemas() > 0);
        let error = initialize(
            &Ddsm115,
            ExecutionTime::from_nanos(0),
            Ddsm115Config { id: 3 },
        )
        .expect_err("the hardware backend is unavailable");
        assert!(error.to_string().contains(BACKEND_UNAVAILABLE));
    }
}
