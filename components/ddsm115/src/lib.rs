//! DDSM115 component contract and hardware driver.

use anyhow::{Result, anyhow};
use phoxal::runtime::input::Setpoint;
use phoxal::runtime::{InitContext, Runtime, Sample, StepContext};

include!(concat!(env!("OUT_DIR"), "/phoxal.component.ddsm115.v1.rs"));

/// Public typed ports owned by the DDSM115 component contract.
pub use ddsm115::ports;

/// The original descriptor closure retained for independent contract
/// inspection and native artifact extraction.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/phoxal-descriptors.bin"));

pub use phoxal_robotics::EncoderSample;

const BACKEND_UNAVAILABLE: &str = "ddsm115 hardware backend unavailable: refusing to model motor or publish fabricated encoder measurements";

/// The motor's address on its shared RS-485 bus.
#[derive(Debug, serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
pub struct Ddsm115Config {
    /// The id configured on the motor itself.
    pub id: u8,
}

/// Driver state retained by the Runtime owner.
#[derive(Debug)]
pub struct Ddsm115State;

/// DDSM115 inputs and observations admitted at one Runtime boundary.
#[phoxal::runtime::inputs]
pub struct Ddsm115Inputs {
    /// The final motion authority's expiring intent for this actuator.
    pub actuator: Setpoint<phoxal_motion::ActuatorSetpoint>,
}

#[phoxal::runtime::outputs]
#[derive(Default)]
pub struct Ddsm115Outputs {
    /// Measured position and velocity from the motor's integrated encoder.
    #[phoxal::runtime::outputs::sample(
        port = ports::ENCODER,
        max_items = 16,
        max_bytes = 8_192
    )]
    pub encoder: Vec<Sample<EncoderSample>>,
}

/// The DDSM115 hardware component driver.
#[phoxal::driver(
    config = Ddsm115Config,
    state = Ddsm115State,
    connection = serial
)]
pub struct Ddsm115;

#[phoxal::runtime::outputs]
impl Ddsm115 {}

/// The hardware backend is intentionally unavailable until a real RS-485
/// transport can deliver commands, measure encoder state, and stop safely.
#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for Ddsm115 {
    type Config = Ddsm115Config;
    type State = Ddsm115State;
    type Inputs = Ddsm115Inputs;
    type Outputs = Ddsm115Outputs;

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
        BACKEND_UNAVAILABLE, Ddsm115, Ddsm115Config, Ddsm115Inputs, Ddsm115Outputs,
        FILE_DESCRIPTOR_SET, ports,
    };
    use phoxal::runtime::input::InputSet;
    use phoxal::runtime::outputs::OutputSet;
    use phoxal::runtime::{ExecutionTime, initialize};
    use phoxal_port::{PortDescriptor, PortKind};

    #[test]
    fn generated_encoder_port_uses_the_shared_robotics_payload() {
        assert_eq!(ports::ENCODER.name(), "encoder");
        assert_eq!(
            ports::ENCODER.signature().service,
            "phoxal.component.ddsm115.v1.Ddsm115"
        );
        assert_eq!(ports::ENCODER.signature().kind, PortKind::Sample);
        assert!(!FILE_DESCRIPTOR_SET.is_empty());
        assert!(!ports::ENCODER.signature().descriptor_set().is_empty());
        let actuator = &Ddsm115Inputs::FIELDS[0];
        assert_eq!(actuator.name, "actuator");
        assert_eq!(
            <phoxal_port::Setpoint<phoxal_motion::ActuatorSetpoint> as PortDescriptor>::KIND,
            PortKind::Setpoint
        );
        assert_eq!(
            Ddsm115Outputs::FIELDS
                .iter()
                .map(|field| field.name)
                .collect::<Vec<_>>(),
            ["encoder"]
        );
        assert_eq!(Ddsm115Outputs::FIELDS[0].port, Some("encoder"));
        assert!(Ddsm115Outputs::FIELDS[0].port_signature.is_some());
    }

    #[test]
    fn initialization_fails_before_modeling_or_publishing_without_hardware() {
        let result = initialize(&Ddsm115, ExecutionTime::default(), Ddsm115Config { id: 1 });
        let error = result.expect_err("setup must reject an unavailable hardware backend");
        assert_eq!(error.to_string(), BACKEND_UNAVAILABLE);
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
