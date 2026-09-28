//! Electrical battery observations, independent of power-management policy.

#[phoxal::messages(package = "phoxal.component.battery.v1")]
mod v1 {
    /// Measured charge direction or a fully charged battery.
    pub enum ChargeState {
        Unknown = 0,
        Charging = 1,
        Discharging = 2,
        Idle = 3,
        Full = 4,
    }

    /// One battery observation. Missing measurements are unknown.
    /// Current is positive while charging and negative while discharging.
    /// State of charge is a fraction in [0, 1], not a percentage in [0, 100].
    pub struct BatterySample {
        #[phoxal(tag = 1)]
        pub present: bool,
        #[phoxal(tag = 2)]
        pub voltage_v: Option<f64>,
        #[phoxal(tag = 3)]
        pub current_a: Option<f64>,
        #[phoxal(tag = 4)]
        pub temperature_c: Option<f64>,
        #[phoxal(tag = 5)]
        pub state_of_charge: Option<f64>,
        #[phoxal(tag = 6)]
        pub charge_state: ChargeState,
    }
}

pub use v1::*;
