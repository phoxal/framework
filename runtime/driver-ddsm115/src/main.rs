//! `ddsm115` — a component driver for a wheel motor (D54).
//!
//! Drivers are first-class users of the same macro + runner — there is no separate
//! `Driver` trait. The runner launches one participant **per** `components.instances`
//! entry, each with a distinct `participant_id` and its own `component_instance`
//! (D47/D53), read here via `ctx.component()`. This driver binds its instance's
//! per-component motor-command (subscribe) and encoder-sample (publish) topics
//! (dynamic keys, D17/D38), applies commands to the (simulated) hardware, and feeds
//! back encoder samples. `#[shutdown]` parks the motor before the bus closes.

use anyhow::Result;
use phoxal::api::y2026_1 as api;
use phoxal::prelude::*;

/// The motor / encoder capability names on a ddsm115 component instance.
const MOTOR_CAPABILITY: &str = "motor";
const ENCODER_CAPABILITY: &str = "encoder";

#[derive(phoxal::Runtime)]
#[phoxal(id = "driver-ddsm115", api = y2026_1)]
struct Ddsm115 {
    // Runtime-private hardware state.
    instance: String,
    position_rad: f64,
    velocity_radps: f32,
    // Handles on this instance's dynamic per-component topics.
    command: Subscriber<api::component::MotorCommand>,
    encoder: Publisher<api::component::EncoderSample>,
}

#[phoxal::runtime]
impl Ddsm115 {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        let instance = ctx.component()?.to_string();
        // Prove the instance exists in the robot model (binds this driver to it).
        let _ = ctx.robot()?.component_instance(&instance)?;

        let command = ctx
            .subscribe(
                api::topic::new()
                    .component()
                    .motor_command(&instance, MOTOR_CAPABILITY),
            )
            .subscriber()
            .await?;
        let encoder = ctx
            .publisher(
                api::topic::new()
                    .component()
                    .encoder_sample(&instance, ENCODER_CAPABILITY),
            )
            .await?;

        Ok(Self {
            instance,
            position_rad: 0.0,
            velocity_radps: 0.0,
            command,
            encoder,
        })
    }

    #[step(hz = 100)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        let now = step.time();

        // Apply the most recent command(s) to the (simulated) motor.
        while let Some(received) = self.command.try_recv() {
            self.velocity_radps = velocity_from(received.body, self.velocity_radps);
        }

        // Integrate the wheel position from the commanded velocity (sim hardware).
        self.position_rad = integrate(
            self.position_rad,
            self.velocity_radps,
            step.dt().as_secs_f64(),
        );

        self.encoder
            .publish_at(
                now,
                api::component::EncoderSample {
                    position_rad: self.position_rad,
                    velocity_radps: self.velocity_radps,
                },
            )
            .await?;
        Ok(())
    }

    #[shutdown]
    async fn shutdown(&mut self, _ctx: ShutdownContext) -> Result<()> {
        // Park: command the (simulated) motor to a stop. A real driver flushes the
        // bus/CAN here before the session closes.
        self.velocity_radps = 0.0;
        let _ = &self.instance;
        Ok(())
    }
}

/// The new commanded velocity for a motor command (`Stop` → 0; `Torque` is not
/// modeled by this sim driver, so it holds the last velocity).
fn velocity_from(command: api::component::MotorCommand, current: f32) -> f32 {
    match command {
        api::component::MotorCommand::Velocity(v) => v,
        api::component::MotorCommand::Stop => 0.0,
        api::component::MotorCommand::Torque(_) => current,
    }
}

/// Integrate wheel position from a commanded angular velocity over `dt`.
fn integrate(position_rad: f64, velocity_radps: f32, dt_s: f64) -> f64 {
    position_rad + f64::from(velocity_radps) * dt_s
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<Ddsm115>()
}

#[cfg(test)]
mod tests {
    use super::{integrate, velocity_from};
    use phoxal::api::ContractBody;
    use phoxal::api::y2026_1 as api;

    #[test]
    fn command_maps_to_velocity() {
        assert_eq!(
            velocity_from(api::component::MotorCommand::Velocity(2.5), 1.0),
            2.5
        );
        assert_eq!(velocity_from(api::component::MotorCommand::Stop, 3.0), 0.0);
        assert_eq!(
            velocity_from(api::component::MotorCommand::Torque(9.0), 1.5),
            1.5
        );
    }

    #[test]
    fn integration_advances_position() {
        let p = integrate(0.0, 10.0, 0.1);
        assert!((p - 1.0).abs() < 1e-9);
    }

    #[test]
    fn emit_apis_reports_per_component_contracts() {
        let json = phoxal::runtime::emit_apis_json::<super::Ddsm115>();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["artifact"]["id"], "driver-ddsm115");
        let contracts = value["required_contracts"].as_array().unwrap();
        assert!(contracts.iter().any(|c| {
            c["family"] == <api::component::MotorCommand as ContractBody>::FAMILY
                && c["direction"] == "subscribe"
        }));
        assert!(contracts.iter().any(|c| {
            c["family"] == <api::component::EncoderSample as ContractBody>::FAMILY
                && c["direction"] == "publish"
        }));
    }
}
