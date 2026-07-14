//! `ddsm115` - a Waveshare DDSM115 wheel-motor component driver (D54).
//!
//! Drivers are first-class users of the framework's macro + runner. The runner
//! launches one participant **per**
//! `components.instances` entry, each with a distinct `participant_id` and its own
//! `component_instance` (D47/D53), read here via `ctx.component()`. This driver binds
//! its instance's per-component motor-command (subscribe) and encoder-sample
//! (publish) topics (dynamic keys, D17/D38), applies commands to the hardware, and
//! feeds back encoder samples. `#[shutdown]` parks the motor before the bus closes.
//!
//! This crate lives in `component/ddsm115` in the framework repository and is built
//! from git source by `phoxal-cli` at check/deploy time.

use anyhow::Result;
use phoxal::prelude::*;
use phoxal_api::v1 as api;

/// The motor / encoder capability names on a ddsm115 component instance
/// (matching `component.yaml`).
const MOTOR_CAPABILITY: &str = "motor";
const ENCODER_CAPABILITY: &str = "encoder";

#[derive(phoxal::Api)]
struct Api {
    // Handles on this instance's dynamic per-component topics.
    command: Subscriber<api::component::motor::Command>,
    encoder: Publisher<api::component::encoder::Sample>,
}

#[phoxal::driver(id = "ddsm115", config = ())]
struct Ddsm115 {
    // Driver-private hardware state.
    instance: String,
    position_rad: f64,
    velocity_radps: f32,
}

#[phoxal::behavior]
impl Ddsm115 {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        // Owner opt-in (plan #00 L2): the runner-minted capability that the owner
        // (`internal`) topic builder requires. This driver OWNS its component node.
        let cap = ctx.owner_capability();
        let instance = ctx.component()?.to_string();
        // Prove the instance exists in the robot model (binds this driver to it).
        let _ = ctx.robot()?.component_instance(&instance)?;

        let command = ctx
            .subscriber(
                api::topic::internal::new(cap)
                    .component(&instance)
                    .motor(MOTOR_CAPABILITY)
                    .command(),
                32,
            )
            .await?;
        let encoder = ctx
            .publisher(
                api::topic::internal::new(cap)
                    .component(&instance)
                    .encoder(ENCODER_CAPABILITY)
                    .sample(),
            )
            .await?;

        Ok((
            Self {
                instance,
                position_rad: 0.0,
                velocity_radps: 0.0,
            },
            Self::Api { command, encoder },
        ))
    }

    #[step(hz = 100)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        let now = step.time();

        // Apply the most recent command(s) to the motor.
        while let Some(received) = api.command.try_recv() {
            self.velocity_radps = velocity_from(received.body, self.velocity_radps);
        }

        // Integrate the wheel position from the commanded velocity.
        self.position_rad = integrate(
            self.position_rad,
            self.velocity_radps,
            step.dt().as_secs_f64(),
        );

        api.encoder
            .publish_at(
                now,
                api::component::encoder::Sample {
                    position_rad: self.position_rad,
                    velocity_radps: self.velocity_radps,
                },
            )
            .await?;
        Ok(())
    }

    #[shutdown]
    async fn shutdown(&mut self, _ctx: ShutdownContext) -> Result<()> {
        // Park: command the motor to a stop. A real driver flushes the bus/CAN here
        // before the session closes.
        self.velocity_radps = 0.0;
        let _ = &self.instance;
        Ok(())
    }
}

/// The new commanded velocity for a motor command (`Stop` -> 0; `Torque` is not
/// modeled by this driver, so it holds the last velocity).
fn velocity_from(command: api::component::motor::Command, current: f32) -> f32 {
    match command {
        api::component::motor::Command::Velocity(v) => v,
        api::component::motor::Command::Stop => 0.0,
        api::component::motor::Command::Torque(_) => current,
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
    use super::{Ddsm115, integrate, velocity_from};
    use phoxal::participant::{ContractRole, Participant, ParticipantApi};
    use phoxal_api::ContractBody;
    use phoxal_api::v1 as api;

    #[test]
    fn command_maps_to_velocity() {
        assert_eq!(
            velocity_from(api::component::motor::Command::Velocity(2.5), 1.0),
            2.5
        );
        assert_eq!(
            velocity_from(api::component::motor::Command::Stop, 3.0),
            0.0
        );
        assert_eq!(
            velocity_from(api::component::motor::Command::Torque(9.0), 1.5),
            1.5
        );
    }

    #[test]
    fn integration_advances_position() {
        let p = integrate(0.0, 10.0, 0.1);
        assert!((p - 1.0).abs() < 1e-9);
    }

    #[test]
    fn api_reports_per_component_contracts() {
        assert_eq!(<Ddsm115 as Participant>::ID, "ddsm115");

        let contracts = <<Ddsm115 as Participant>::Api as ParticipantApi>::CONTRACTS;
        assert!(contracts.iter().any(|c| {
            c.topic == <api::component::motor::Command as ContractBody>::TOPIC
                && c.role == ContractRole::Subscribe
        }));
        assert!(contracts.iter().any(|c| {
            c.topic == <api::component::encoder::Sample as ContractBody>::TOPIC
                && c.role == ContractRole::Publish
        }));
    }
}
