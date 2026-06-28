//! `safety` — the official battery + drive safety monitor.
//!
//! This official runtime targets the inheriting API version `y2026_2`. It
//! consumes `battery`, a family that exists only from `y2026_2` on, plus
//! `drive/state`, and publishes the robot's aggregate `safety/state` posture.

use phoxal::api::y2026_2 as api;
use phoxal::prelude::*;

const BATTERY_CRITICAL_RATIO: f32 = 0.10;
const BATTERY_LOW_RATIO: f32 = 0.25;

#[derive(phoxal::Runtime)]
#[phoxal(id = "safety", api = y2026_2)]
struct Safety {
    // Runtime-private typed state (not handles).
    last_battery: Option<(api::battery::State, u64)>,
    last_drive: Option<(api::drive::State, u64)>,
    // Handles.
    battery: Subscriber<api::battery::State>,
    drive: Subscriber<api::drive::State>,
    state: Publisher<api::safety::Status>,
}

#[phoxal::runtime]
impl Safety {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        let battery = ctx
            .subscribe(api::topic::new().battery().state())
            .subscriber()
            .await?;
        let drive = ctx
            .subscribe(api::topic::new().drive().state())
            .subscriber()
            .await?;
        let state = ctx.publisher(api::topic::new().safety().state()).await?;

        Ok(Self {
            last_battery: None,
            last_drive: None,
            battery,
            drive,
            state,
        })
    }

    #[step(hz = 10)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        while let Some(received) = self.battery.try_recv() {
            self.last_battery = Some((received.body, received.metadata.produced_at_ns));
        }
        while let Some(received) = self.drive.try_recv() {
            self.last_drive = Some((received.body, received.metadata.produced_at_ns));
        }

        let status = assess(
            self.last_battery.as_ref().map(|(body, _)| body),
            self.last_drive.as_ref().map(|(body, _)| body),
        );
        self.state.publish_at(step.time(), status).await?;
        Ok(())
    }
}

/// Missing inputs contribute no concern in this first version; the monitor
/// fails open until it has an explicit battery or drive posture to evaluate.
fn assess(
    battery: Option<&api::battery::State>,
    drive: Option<&api::drive::State>,
) -> api::safety::Status {
    let mut concerns = Vec::new();
    let mut level = api::safety::Level::Nominal;

    if let Some(battery) = battery {
        if battery.charge_ratio < BATTERY_CRITICAL_RATIO {
            concerns.push(api::safety::Concern::BatteryCritical);
            level = worst_level(level, api::safety::Level::EmergencyStop);
        } else if battery.charge_ratio < BATTERY_LOW_RATIO {
            concerns.push(api::safety::Concern::BatteryLow);
            level = worst_level(level, api::safety::Level::Warning);
        }
    }

    if let Some(drive) = drive {
        if matches!(
            drive.stop_reason,
            Some(api::drive::StopReason::Fault | api::drive::StopReason::EmergencyStop)
        ) {
            concerns.push(api::safety::Concern::DriveFault);
            level = worst_level(level, api::safety::Level::EmergencyStop);
        }
    }

    api::safety::Status { level, concerns }
}

fn worst_level(current: api::safety::Level, candidate: api::safety::Level) -> api::safety::Level {
    use api::safety::Level;

    match (current, candidate) {
        (Level::EmergencyStop, _) | (_, Level::EmergencyStop) => Level::EmergencyStop,
        (Level::Warning, _) | (_, Level::Warning) => Level::Warning,
        (Level::Nominal, Level::Nominal) => Level::Nominal,
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<Safety>()
}

#[cfg(test)]
mod tests {
    use phoxal::api::ContractBody;

    use super::{Safety, api, assess};

    #[test]
    fn nominal_when_all_healthy() {
        let status = assess(Some(&battery(0.9)), Some(&drive(None)));

        assert_eq!(status.level, api::safety::Level::Nominal);
        assert!(status.concerns.is_empty());
    }

    #[test]
    fn battery_low_warns() {
        let status = assess(Some(&battery(0.2)), None);

        assert_eq!(status.level, api::safety::Level::Warning);
        assert_eq!(status.concerns, vec![api::safety::Concern::BatteryLow]);
    }

    #[test]
    fn battery_critical_emergency_stops() {
        let status = assess(Some(&battery(0.05)), None);

        assert_eq!(status.level, api::safety::Level::EmergencyStop);
        assert_eq!(status.concerns, vec![api::safety::Concern::BatteryCritical]);
    }

    #[test]
    fn drive_fault_emergency_stops() {
        let status = assess(None, Some(&drive(Some(api::drive::StopReason::Fault))));

        assert_eq!(status.level, api::safety::Level::EmergencyStop);
        assert!(status.concerns.contains(&api::safety::Concern::DriveFault));
    }

    #[test]
    fn worst_concern_wins() {
        let status = assess(
            Some(&battery(0.2)),
            Some(&drive(Some(api::drive::StopReason::Fault))),
        );

        assert_eq!(status.level, api::safety::Level::EmergencyStop);
        assert!(status.concerns.contains(&api::safety::Concern::BatteryLow));
        assert!(status.concerns.contains(&api::safety::Concern::DriveFault));
    }

    #[test]
    fn missing_inputs_are_nominal() {
        let status = assess(None, None);

        assert_eq!(status.level, api::safety::Level::Nominal);
        assert!(status.concerns.is_empty());
    }

    #[test]
    fn emit_apis_reports_y2026_2_safety_contracts() {
        let json = phoxal::runtime::emit_apis_json::<Safety>();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["artifact"]["id"], "safety");
        assert_eq!(value["api_version"], "y2026_2");

        let contracts = value["required_contracts"].as_array().unwrap();
        assert_contract::<api::battery::State>(contracts, "subscribe");
        assert_contract::<api::drive::State>(contracts, "subscribe");
        assert_contract::<api::safety::Status>(contracts, "publish");
    }

    fn assert_contract<B>(contracts: &[serde_json::Value], direction: &str)
    where
        B: ContractBody,
    {
        assert!(contracts.iter().any(|c| {
            c["family"] == B::FAMILY && c["topic"] == B::TOPIC && c["direction"] == direction
        }));
    }

    fn battery(charge_ratio: f32) -> api::battery::State {
        api::battery::State {
            voltage_v: 15.0,
            current_a: 1.0,
            charge_ratio,
        }
    }

    fn drive(stop_reason: Option<api::drive::StopReason>) -> api::drive::State {
        let target = api::drive::Target {
            linear_x_mps: 0.0,
            angular_z_radps: 0.0,
            curvature_limit_radpm: None,
        };

        api::drive::State {
            target: target.clone(),
            limited_target: target,
            actuator_authority: api::drive::ActuatorAuthority::Active,
            stop_reason,
        }
    }
}
