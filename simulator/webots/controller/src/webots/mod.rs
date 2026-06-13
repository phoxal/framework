pub(crate) mod controller;
pub(crate) mod output;
pub(crate) mod registry;

use phoxal::api::v1::component::capability::led::Command as LedCommand;
use phoxal::api::v1::component::capability::motor::Command as MotorCommand;
use phoxal::model::component::v1::CapabilityRef;

#[derive(Clone, Debug)]
pub enum Command {
    Motor {
        capability: CapabilityRef,
        payload: MotorCommand,
    },
    Led {
        capability: CapabilityRef,
        payload: LedCommand,
    },
}

impl Command {
    pub fn motor(capability: CapabilityRef, payload: MotorCommand) -> Self {
        Self::Motor {
            capability,
            payload,
        }
    }

    pub fn led(capability: CapabilityRef, payload: LedCommand) -> Self {
        Self::Led {
            capability,
            payload,
        }
    }

    pub fn capability(&self) -> &CapabilityRef {
        match self {
            Self::Motor { capability, .. } | Self::Led { capability, .. } => capability,
        }
    }
}
