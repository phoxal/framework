use derive_new::new;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, new)]
pub struct State {
    voltage_v: f64,
    current_a: f64,
    percentage: f32,
}

impl State {
    pub const fn voltage_v(&self) -> f64 {
        self.voltage_v
    }

    pub const fn current_a(&self) -> f64 {
        self.current_a
    }

    pub const fn percentage(&self) -> f32 {
        self.percentage
    }
}

pub const KIND: &str = "battery";
