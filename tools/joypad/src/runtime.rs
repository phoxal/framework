use anyhow::Result;
use phoxal::api::{
    motion::{self, ManualCommand},
    safety::{self, EmergencyStopRequest},
    topic,
};
use phoxal::runtime::clock::Step;
use phoxal::runtime::{EmptyArgs, Io, RobotRuntimeArgs, Runtime, RuntimeInputs, TopicPublisher};
use tracing::info;

use crate::backend::{Backend, Sample, SelectedDevice};
use crate::mapping::{map_command, map_emergency_stop_request};

pub struct Config {
    pub selected_device: SelectedDevice,
    pub scheme: crate::mapping::ControlScheme,
}

pub enum Input {}

pub struct JoypadRuntime {
    backend: Backend,
    selected_device: SelectedDevice,
    scheme: crate::mapping::ControlScheme,
    connected: bool,
    manual_pub: TopicPublisher<ManualCommand>,
    emergency_stop_pub: TopicPublisher<EmergencyStopRequest>,
}

#[async_trait::async_trait]
impl Runtime for JoypadRuntime {
    const RUNTIME_ID: &'static str = "joypad";

    type Args = EmptyArgs;
    type Config = Config;
    type Input = Input;

    fn config(_args: &Self::Args, _common: &RobotRuntimeArgs) -> Result<Self::Config> {
        anyhow::bail!("joypad resolves config from its tool-specific CLI")
    }

    fn clock_period(_config: &Self::Config) -> std::time::Duration {
        std::time::Duration::from_millis(20)
    }

    async fn new(io: &mut Io<Self::Input>, config: Self::Config) -> Result<Self> {
        let manual_pub = io.publisher_topic(topic::new().motion().manual()).await?;
        let emergency_stop_pub = io
            .publisher_topic(topic::new().safety().emergency_stop_request())
            .await?;
        Ok(Self {
            backend: Backend::new()?,
            selected_device: config.selected_device,
            scheme: config.scheme,
            connected: false,
            manual_pub,
            emergency_stop_pub,
        })
    }

    async fn step(&mut self, step: Step, _inputs: RuntimeInputs<Self::Input>) -> Result<()> {
        let (command, emergency_stop_request) =
            match self.backend.sample_selected(&self.selected_device) {
                Sample::Connected(state) => {
                    if !self.connected {
                        self.connected = true;
                        info!(
                            device_name = %self.selected_device.name,
                            device_uuid = %self.selected_device.uuid_hyphenated(),
                            "Joypad connected"
                        );
                    }
                    (
                        map_command(self.scheme, &state),
                        map_emergency_stop_request(&state),
                    )
                }
                Sample::Unavailable => {
                    if self.connected {
                        self.connected = false;
                        info!(
                            device_name = %self.selected_device.name,
                            "Joypad disconnected; commanding stop until it returns"
                        );
                    }
                    (
                        motion::v1::ManualCommand {
                            linear_x_mps: 0.0,
                            angular_z_radps: 0.0,
                        },
                        safety::v1::EmergencyStopRequest { engaged: false },
                    )
                }
            };

        let at_ns = step.tick.time_ns();
        self.emergency_stop_pub
            .put(at_ns, &EmergencyStopRequest::V1(emergency_stop_request))
            .await?;
        self.manual_pub
            .put(at_ns, &ManualCommand::V1(command))
            .await
    }
}
