//! `battery` - the official simulated battery-state monitor.
//!
//! A scheduled runtime that consumes nothing: it has no subscriptions and models
//! a pack discharging under a constant load entirely from internal state.
//! Each step it advances the charge ratio for the elapsed `dt`, derives a voltage
//! by interpolating between the empty and full pack voltage, and publishes
//! `battery/state` (voltage, current, charge ratio) at 1 Hz.
//! The pack starts full and the modelled current drops to zero once depleted.
//! The numbers are placeholders for a real fuel gauge: a fixed draw and capacity,
//! not a reading from any hardware sensor.

use anyhow::Result;
use phoxal::prelude::*;
use phoxal_api::y2026_1 as api;

const FULL_VOLTAGE_V: f64 = 16.8;
const EMPTY_VOLTAGE_V: f64 = 12.0;
const DRAW_CURRENT_A: f64 = 2.0;
const CAPACITY_AH: f64 = 10.0;

#[derive(phoxal::Runtime)]
#[phoxal(id = "battery", api = y2026_1)]
struct Battery {
    // Runtime-private simulated pack state.
    charge_ratio: f64,
    // Handles.
    state: Publisher<api::battery::State>,
}

#[phoxal::runtime]
impl Battery {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        let state = ctx
            .publisher(api::topic::internal::new().battery().state())
            .await?;

        Ok(Self {
            charge_ratio: 1.0,
            state,
        })
    }

    #[step(hz = 1)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        let now = step.time();

        self.charge_ratio = discharge(
            self.charge_ratio,
            DRAW_CURRENT_A,
            step.dt().as_secs_f64(),
            CAPACITY_AH,
        );
        let current_a = current_for(self.charge_ratio, DRAW_CURRENT_A);

        self.state
            .publish_at(
                now,
                api::battery::State {
                    voltage_v: voltage_for(self.charge_ratio, EMPTY_VOLTAGE_V, FULL_VOLTAGE_V)
                        as f32,
                    current_a: current_a as f32,
                    charge_ratio: self.charge_ratio as f32,
                },
            )
            .await?;
        Ok(())
    }
}

fn discharge(ratio: f64, current_a: f64, dt_s: f64, capacity_ah: f64) -> f64 {
    if ratio <= 0.0 || current_a <= 0.0 || dt_s <= 0.0 || capacity_ah <= 0.0 {
        return ratio.clamp(0.0, 1.0);
    }

    let dt_hours = dt_s / 3_600.0;
    (ratio.clamp(0.0, 1.0) - (current_a * dt_hours / capacity_ah)).clamp(0.0, 1.0)
}

fn voltage_for(ratio: f64, empty_v: f64, full_v: f64) -> f64 {
    empty_v + (full_v - empty_v) * ratio.clamp(0.0, 1.0)
}

fn current_for(ratio: f64, draw_current_a: f64) -> f64 {
    if ratio > 0.0 { draw_current_a } else { 0.0 }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<Battery>()
}

#[cfg(test)]
mod tests {
    use super::{
        Battery, CAPACITY_AH, DRAW_CURRENT_A, EMPTY_VOLTAGE_V, FULL_VOLTAGE_V, discharge,
        voltage_for,
    };
    use phoxal_api::ContractBody;
    use phoxal_api::y2026_1 as api;

    #[test]
    fn discharge_reduces_charge_over_time() {
        let ratio = discharge(1.0, DRAW_CURRENT_A, 3_600.0, CAPACITY_AH);
        assert_close(ratio, 0.8);

        let depleted = discharge(0.1, DRAW_CURRENT_A, 3_600.0, 1.0);
        assert_close(depleted, 0.0);
    }

    #[test]
    fn voltage_interpolates_with_charge() {
        assert_close(
            voltage_for(1.0, EMPTY_VOLTAGE_V, FULL_VOLTAGE_V),
            FULL_VOLTAGE_V,
        );
        assert_close(
            voltage_for(0.0, EMPTY_VOLTAGE_V, FULL_VOLTAGE_V),
            EMPTY_VOLTAGE_V,
        );
        assert_close(
            voltage_for(0.5, EMPTY_VOLTAGE_V, FULL_VOLTAGE_V),
            (EMPTY_VOLTAGE_V + FULL_VOLTAGE_V) / 2.0,
        );
    }

    #[test]
    fn emit_apis_reports_y2026_1_battery_contract() {
        let json = phoxal::runtime::emit_apis_json::<Battery>();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["artifact"]["id"], "battery");
        assert_eq!(value["api_version"], "y2026_1");

        let contracts = value["required_contracts"].as_array().unwrap();
        assert!(contracts.iter().any(|c| {
            c["family"] == <api::battery::State as ContractBody>::FAMILY
                && c["topic"] == <api::battery::State as ContractBody>::TOPIC
                && c["direction"] == "publish"
        }));
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected {expected}, got {actual}"
        );
    }
}
