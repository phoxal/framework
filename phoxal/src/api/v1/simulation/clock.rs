use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Clock {
    epoch: u64,
    step: u64,
    time_ns: u64,
    dt_ns: u64,
}

impl Clock {
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

#[cfg(test)]
mod tests {
    use serde::Serialize;

    use super::Clock;

    #[derive(Serialize)]
    struct SimulatorApiClockLayout {
        epoch: u64,
        step: u64,
        time_ns: u64,
        dt_ns: u64,
    }

    #[test]
    fn clock_contract_matches_simulator_wire_values() {
        let clock = Clock::new(7, 11, 13, 17);
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
