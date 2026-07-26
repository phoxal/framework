//! Battery capability: publishes `component::battery::State` from the Webots
//! robot battery sensor.
//!
//! The battery is the one capability Webots hangs off the robot rather than off
//! a device: `wb_robot_battery_sensor_get_value` returns "the present energy
//! level of the battery expressed in Joules", and `-1.0` when the world's
//! `Robot` node left its `battery` field empty. There is exactly one per robot,
//! so exactly one declared battery capability may bind it.
//!
//! Joules alone do not make a battery state. The pack's nominal voltage and
//! capacity come from the component manifest and turn that energy into the
//! reported charge ratio and current. Voltage is reported as the declared
//! nominal: Webots models pack energy, not terminal voltage, and a discharge
//! curve invented here would describe no simulated hardware.

use anyhow::{Result, anyhow};
use phoxal::api;
use phoxal::model::component::v0::CapabilityRef;

use super::is_due;

const SECONDS_PER_HOUR: f64 = 3_600.0;

#[derive(Clone, Debug)]
pub(crate) struct BatterySpec {
    pub(crate) reference: CapabilityRef,
    pub(crate) publish_every_steps: u64,
    pub(crate) sampling_period_ms: i32,
    pub(crate) voltage_v: f64,
    pub(crate) capacity_ah: f64,
}

impl BatterySpec {
    /// The energy a full pack holds, in Joules - the denominator Webots cannot
    /// supply, since reading the `Robot` node's `battery` field would take a
    /// supervisor this controller never opens.
    fn full_energy_j(&self) -> f64 {
        self.voltage_v * self.capacity_ah * SECONDS_PER_HOUR
    }
}

pub(crate) struct NativeBattery {
    sensor: webots_rs::device::battery_sensor::BatterySensor,
    spec: BatterySpec,
    last: Option<(f64, u64)>,
    reported_absent: bool,
}

impl NativeBattery {
    pub(crate) fn new(spec: &BatterySpec) -> Result<Self> {
        let sensor = webots_rs::device::battery_sensor::BatterySensor::new();
        sensor
            .enable(spec.sampling_period_ms)
            .map_err(|error| anyhow!(error))?;
        Ok(Self {
            sensor,
            spec: spec.clone(),
            last: None,
            reported_absent: false,
        })
    }

    pub(crate) fn read_if_due(
        &mut self,
        step_index: u64,
        time_ns: u64,
    ) -> Result<Option<api::component::battery::State>> {
        if !is_due(step_index, self.spec.publish_every_steps) {
            return Ok(None);
        }
        let energy_j = self.sensor.get_value().map_err(|error| anyhow!(error))?;
        // A world whose Robot node has no `battery` field models no battery.
        // Saying so once beats either a silent gap or a log line per step.
        if !energy_j.is_finite() || energy_j < 0.0 {
            if !self.reported_absent {
                self.reported_absent = true;
                tracing::warn!(
                    target: "simulator_webots_controller",
                    capability = %self.spec.reference,
                    "this world's Robot node declares no `battery` field, so the battery \
                     capability publishes nothing; add [energy, max_energy, recharge] to the \
                     Robot node to simulate it"
                );
            }
            return Ok(None);
        }
        let state = battery_state(&self.spec, energy_j, self.last, time_ns);
        self.last = Some((energy_j, time_ns));
        Ok(Some(state))
    }
}

fn battery_state(
    spec: &BatterySpec,
    energy_j: f64,
    previous: Option<(f64, u64)>,
    time_ns: u64,
) -> api::component::battery::State {
    let charge_ratio = (energy_j / spec.full_energy_j()).clamp(0.0, 1.0);
    // Current is what the pack is actually delivering: the energy it lost over
    // the elapsed window, at the pack's nominal voltage. Positive is discharge.
    let current_a = previous
        .and_then(|(previous_energy_j, previous_time_ns)| {
            let dt_ns = time_ns.saturating_sub(previous_time_ns);
            if dt_ns == 0 {
                return None;
            }
            let watts = (previous_energy_j - energy_j) * 1_000_000_000.0 / dt_ns as f64;
            Some(watts / spec.voltage_v)
        })
        .unwrap_or(0.0);
    api::component::battery::State {
        voltage_v: spec.voltage_v as f32,
        current_a: current_a as f32,
        charge_ratio: charge_ratio as f32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> BatterySpec {
        BatterySpec {
            reference: CapabilityRef::new("pack", "battery"),
            publish_every_steps: 1,
            sampling_period_ms: 100,
            voltage_v: 16.0,
            capacity_ah: 10.0,
        }
    }

    #[test]
    fn charge_ratio_is_energy_over_a_full_pack() {
        let spec = spec();
        let full = spec.full_energy_j();
        assert_eq!(battery_state(&spec, full, None, 0).charge_ratio, 1.0);
        assert_eq!(battery_state(&spec, full / 2.0, None, 0).charge_ratio, 0.5);
        assert_eq!(battery_state(&spec, 0.0, None, 0).charge_ratio, 0.0);
    }

    /// A world may recharge past the declared capacity, or the author may
    /// declare a smaller pack than the world holds. Neither makes a battery
    /// more than full.
    #[test]
    fn a_pack_over_its_declared_capacity_still_reads_full() {
        let spec = spec();
        let over = spec.full_energy_j() * 2.0;
        assert_eq!(battery_state(&spec, over, None, 0).charge_ratio, 1.0);
    }

    #[test]
    fn current_is_the_energy_lost_over_the_window_at_nominal_voltage() {
        let spec = spec();
        // 160 J lost in one second at 16 V is 160 W, so 10 A.
        let state = battery_state(&spec, 1_000.0, Some((1_160.0, 0)), 1_000_000_000);
        assert!((state.current_a - 10.0).abs() < 1e-3);
        assert_eq!(state.voltage_v, 16.0);
    }

    #[test]
    fn a_charging_pack_reports_negative_current() {
        let spec = spec();
        let state = battery_state(&spec, 1_160.0, Some((1_000.0, 0)), 1_000_000_000);
        assert!(state.current_a < 0.0);
    }

    #[test]
    fn the_first_reading_has_no_window_to_differentiate() {
        assert_eq!(battery_state(&spec(), 1_000.0, None, 0).current_a, 0.0);
        assert_eq!(
            battery_state(&spec(), 1_000.0, Some((2_000.0, 5)), 5).current_a,
            0.0
        );
    }
}
