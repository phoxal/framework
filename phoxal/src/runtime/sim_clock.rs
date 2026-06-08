//! Simulation clock wire contract.
//!
//! The engine owns this contract because the shared step clock is part of
//! runtime bootstrap, not a simulator-domain API.
//!
use crate::bus::pubsub::Stamped;
use crate::bus::zenoh_typed::{TypedPublisherBuilder, TypedSchema, TypedSubscriberBuilder};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct SimulationClock {
    epoch: u64,
    step: u64,
    time_ns: u64,
    dt_ns: u64,
}

impl SimulationClock {
    pub const fn new(epoch: u64, step: u64, time_ns: u64, dt_ns: u64) -> Self {
        Self {
            epoch,
            step,
            time_ns,
            dt_ns,
        }
    }

    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    pub const fn step(&self) -> u64 {
        self.step
    }

    pub const fn time_ns(&self) -> u64 {
        self.time_ns
    }

    pub const fn dt_ns(&self) -> u64 {
        self.dt_ns
    }
}

impl TypedSchema for SimulationClock {
    const SCHEMA_NAME: &'static str = "simulation/clock";
    const SCHEMA_VERSION: u32 = 1;
}

pub const TOPIC: &str = "simulation/clock";

pub fn topic(bus: &crate::bus::Bus) -> String {
    bus.topic(TOPIC)
}

pub fn publisher_builder(
    bus: &crate::bus::Bus,
) -> crate::bus::Result<TypedPublisherBuilder<'_, 'static, Stamped<SimulationClock>>> {
    crate::bus::pubsub::publisher_builder(bus, TOPIC)
}

pub fn publisher(
    bus: &crate::bus::Bus,
) -> crate::bus::Result<TypedPublisherBuilder<'_, 'static, Stamped<SimulationClock>>> {
    publisher_builder(bus)
}

pub fn subscriber_builder(
    bus: &crate::bus::Bus,
) -> TypedSubscriberBuilder<'_, 'static, Stamped<SimulationClock>> {
    crate::bus::pubsub::subscriber_builder(bus, TOPIC)
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh_typed::TypedSchema;
    use serde::Serialize;

    use super::{SimulationClock, TOPIC};

    #[derive(Serialize)]
    struct SimulatorApiClockLayout {
        epoch: u64,
        step: u64,
        time_ns: u64,
        dt_ns: u64,
    }

    #[test]
    fn simulation_clock_contract_matches_simulator_wire_values() {
        assert_eq!(SimulationClock::SCHEMA_NAME, "simulation/clock");
        assert_eq!(SimulationClock::SCHEMA_VERSION, 1);
        assert_eq!(TOPIC, "simulation/clock");

        let clock = SimulationClock::new(7, 11, 13, 17);
        assert_eq!(clock.epoch(), 7);
        assert_eq!(clock.step(), 11);
        assert_eq!(clock.time_ns(), 13);
        assert_eq!(clock.dt_ns(), 17);

        let reference = SimulatorApiClockLayout {
            epoch: 7,
            step: 11,
            time_ns: 13,
            dt_ns: 17,
        };
        assert_eq!(
            rmp_serde::to_vec_named(&clock).expect("clock encodes"),
            rmp_serde::to_vec_named(&reference).expect("reference encodes")
        );
    }
}
