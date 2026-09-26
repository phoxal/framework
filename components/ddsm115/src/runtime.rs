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

/// The DDSM115 hardware component driver.
pub struct Ddsm115;

/// The hardware backend is intentionally unavailable until a real RS-485
/// transport can deliver commands, measure encoder state, and stop safely.
#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for Ddsm115 {
    type Config = Ddsm115Config;
    type State = Ddsm115State;

    fn init(&self, _ctx: &InitContext, config: Self::Config) -> Result<Self::State> {
        Err(anyhow!("{BACKEND_UNAVAILABLE} (motor ID {})", config.id))
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
    use super::{BACKEND_UNAVAILABLE, Ddsm115, Ddsm115Config, phoxal_provider::Inputs};
    use phoxal::contract::{MethodDescriptor, MethodShape};
    use phoxal::runtime::Runtime;
    use phoxal::runtime::input::InputSet;
    use phoxal::runtime::outputs::OutputSet;
    use phoxal::runtime::{ExecutionTime, initialize};

    #[test]
    fn generated_encoder_method_uses_the_shared_robotics_payload() {
        assert_eq!(
            crate::api::service_methods::u0::ENCODER
                .signature()
                .endpoint,
            "encoder"
        );
        assert_eq!(
            crate::api::service_methods::u0::ENCODER.signature().service,
            "phoxal.robotics.v1.EncoderSample"
        );
        assert_eq!(
            crate::api::service_methods::u0::ENCODER.signature().shape,
            MethodShape::Observation
        );
        assert!(
            !crate::api::service_methods::u0::ENCODER
                .signature()
                .descriptor_set()
                .is_empty()
        );
        let actuator = &Inputs::FIELDS[0];
        assert_eq!(actuator.name, "actuator");
        assert_eq!(
            <Ddsm115 as Runtime>::Outputs::FIELDS
                .iter()
                .map(|field| field.name)
                .collect::<Vec<_>>(),
            ["encoder"]
        );
        assert_eq!(
            <Ddsm115 as Runtime>::Outputs::FIELDS[0].port,
            Some("encoder")
        );
        assert!(
            <Ddsm115 as Runtime>::Outputs::FIELDS[0]
                .port_signature
                .is_some()
        );
    }

    #[test]
    fn initialization_fails_before_modeling_or_publishing_without_hardware() {
        let result = initialize(&Ddsm115, ExecutionTime::default(), Ddsm115Config { id: 1 });
        let error = result.expect_err("setup must reject an unavailable hardware backend");
        assert!(error.to_string().contains(BACKEND_UNAVAILABLE));
    }

    #[test]
    fn the_driver_config_is_the_motor_id_and_nothing_else() {
        let config: Ddsm115Config =
            serde_json::from_value(serde_json::json!({ "id": 3 })).expect("the motor id parses");
        assert_eq!(config.id, 3);
        assert!(
            serde_json::from_value::<Ddsm115Config>(serde_json::json!({
                "id": 3,
                "bus": 0
            }))
            .is_err(),
            "an undeclared key in the driver's own config must not parse"
        );
    }
}
