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
//! # Non-zero actuation requires a permit (#952 section H)
//!
//! Command reception and expiry live in a **managed task**, not in `#[step]`.
//! That is the whole point: if logical time stops advancing, `#[step]` stops
//! running, and a driver that only expired commands inside its step would hold
//! its last velocity forever. The managed task runs independently of the step
//! loop and ages the permit on its own [`LocalInstant`] - the host's
//! suspend-aware monotonic boot clock - so the motor stops when the commanding
//! service goes silent, when logical time freezes, when the publisher dies, and
//! after a host suspend. `Stop` is always callable and needs no permit.
//!
//! Because this driver models the motor rather than driving it, that is not
//! hardware or backend proof; it is the framework-side floor every official
//! physical actuator driver inherits.
//!
//! This crate lives in `component/ddsm115` in the framework repository and is built
//! from git source by `phoxal-cli` at check/deploy time.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use phoxal::api;
use phoxal::prelude::*;

/// The motor / encoder capability names on a ddsm115 component instance
/// (matching `component.yaml`).
const MOTOR_CAPABILITY: &str = "motor";
const ENCODER_CAPABILITY: &str = "encoder";

/// How long a commanded velocity remains permitted without renewal. The
/// commanding service publishes far faster than this; the window only has to
/// outlast ordinary jitter, not a stalled producer.
const PERMIT: Duration = Duration::from_millis(200);

/// How often the permit task re-checks the deadline while no command arrives.
/// This bounds how long a stale velocity can survive past its deadline.
const PERMIT_TICK: Duration = Duration::from_millis(20);

/// The effective commanded velocity plus the permit that keeps it alive.
///
/// Shared between the managed permit task and the step loop. Only a command
/// grants a permit; both sides enforce it.
#[derive(Debug, Default)]
struct Motor {
    velocity_radps: f32,
    /// When the current non-zero velocity stops being permitted. `None` means
    /// the motor is already stopped and needs no permit.
    permitted_until: Option<LocalInstant>,
}

impl Motor {
    /// Apply a command, granting a fresh permit for a non-zero velocity.
    fn apply(&mut self, command: api::component::motor::Command, now: LocalInstant) {
        match command {
            api::component::motor::Command::Velocity(velocity) if velocity != 0.0 => {
                self.velocity_radps = velocity;
                self.permitted_until = Some(now.saturating_add(PERMIT));
            }
            // Stop is always callable and grants no permit. `Torque` is not
            // modeled by this driver, so it neither commands nor renews - which
            // means an unmodeled command can never keep the wheel turning.
            api::component::motor::Command::Velocity(_)
            | api::component::motor::Command::Stop
            | api::component::motor::Command::Torque(_) => self.stop(),
        }
    }

    /// Stop the motor if its permit has lapsed. Returns the effective velocity.
    fn enforce(&mut self, now: LocalInstant) -> f32 {
        match self.permitted_until {
            Some(deadline) if now.reached(deadline) => self.stop(),
            None => self.stop(),
            Some(_) => {}
        }
        self.velocity_radps
    }

    fn stop(&mut self) {
        self.velocity_radps = 0.0;
        self.permitted_until = None;
    }
}

#[derive(phoxal::Api)]
pub struct Api {
    // Handles on this instance's dynamic per-component topics. The command
    // subscription is drained by the managed permit task, never by `#[step]`.
    command: Subscriber<api::component::motor::Command>,
    encoder: MeasurementPublisher<api::component::encoder::Sample>,
}

#[phoxal::driver(id = "ddsm115", config = ())]
pub struct Ddsm115 {
    // Driver-private hardware state.
    instance: String,
    position_rad: f64,
    motor: Arc<Mutex<Motor>>,
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
            .measurement_publisher(
                api::topic::internal::new(cap)
                    .component(&instance)
                    .encoder(ENCODER_CAPABILITY)
                    .sample(),
            )
            .await?;

        let motor = Arc::new(Mutex::new(Motor::default()));
        let permit_commands = command.clone();
        let permit_motor = Arc::clone(&motor);
        ctx.spawn_managed("motor-permit", async move {
            hold_permit_forever(permit_commands, permit_motor).await;
        });

        Ok((
            Self {
                instance,
                position_rad: 0.0,
                motor,
            },
            Self::Api { command, encoder },
        ))
    }

    #[step(hz = 100)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        // Read the effective velocity, enforcing the permit here too so a step
        // can never integrate a velocity the permit task would have stopped.
        let velocity_radps = lock(&self.motor).enforce(LocalInstant::now());

        // Integrate the wheel position from the effective velocity.
        self.position_rad = integrate(self.position_rad, velocity_radps, step.dt().as_secs_f64());

        // This driver models the motor rather than reading a device clock, so
        // the encoder capture coincides exactly with this step.
        api.encoder.publish(
            CaptureStamp::exact(step.now()),
            api::component::encoder::Sample {
                position_rad: self.position_rad,
                velocity_radps,
            },
        )?;
        Ok(())
    }

    #[shutdown]
    async fn shutdown(&mut self, _ctx: ShutdownContext) -> Result<()> {
        // Park: command the motor to a stop. A real driver flushes the bus/CAN here
        // before the session closes.
        lock(&self.motor).stop();
        let _ = &self.instance;
        Ok(())
    }
}

/// Receive commands and enforce the permit, independently of `#[step]`.
///
/// The `select!` is what makes this a watchdog rather than a mailbox: the tick
/// arm fires even when no command ever arrives, so a lapsed permit stops the
/// motor whether the publisher went silent, the process died, or logical time
/// froze.
async fn hold_permit_forever(
    commands: Subscriber<api::component::motor::Command>,
    motor: Arc<Mutex<Motor>>,
) {
    let mut tick = tokio::time::interval(PERMIT_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            observed = commands.recv() => {
                let Ok(observed) = observed else { break };
                lock(&motor).apply(observed.body, LocalInstant::now());
            }
            _ = tick.tick() => {
                lock(&motor).enforce(LocalInstant::now());
            }
        }
    }
    // The subscription closed with the bus: stop rather than hold the last
    // command while nothing can renew it.
    lock(&motor).stop();
}

fn lock(motor: &Mutex<Motor>) -> std::sync::MutexGuard<'_, Motor> {
    motor
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Integrate wheel position from a commanded angular velocity over `dt`.
fn integrate(position_rad: f64, velocity_radps: f32, dt_s: f64) -> f64 {
    position_rad + f64::from(velocity_radps) * dt_s
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoxal::bus::ContractBody;
    use phoxal::participant::{ContractRole, Participant, ParticipantApi};

    fn start() -> LocalInstant {
        LocalInstant::from_boot_ns(1_000_000_000)
    }

    #[test]
    fn a_velocity_command_grants_a_permit_and_stop_revokes_it() {
        let mut motor = Motor::default();
        motor.apply(api::component::motor::Command::Velocity(2.5), start());
        assert_eq!(motor.velocity_radps, 2.5);
        assert!(motor.permitted_until.is_some());

        motor.apply(api::component::motor::Command::Stop, start());
        assert_eq!(motor.velocity_radps, 0.0);
        assert!(
            motor.permitted_until.is_none(),
            "stop is always callable and grants no permit"
        );
    }

    #[test]
    fn an_unmodeled_command_stops_rather_than_holding_the_last_velocity() {
        let mut motor = Motor::default();
        motor.apply(api::component::motor::Command::Velocity(2.5), start());
        motor.apply(api::component::motor::Command::Torque(9.0), start());
        assert_eq!(
            motor.velocity_radps, 0.0,
            "a command this driver cannot model must never keep the wheel turning"
        );
    }

    /// The core property: with the commanding service silent, the permit lapses
    /// on the driver's own monotonic clock and the effective velocity becomes
    /// zero - regardless of whether logical time is advancing at all.
    #[test]
    fn a_lapsed_permit_stops_the_motor_even_though_no_step_ran() {
        let mut motor = Motor::default();
        motor.apply(api::component::motor::Command::Velocity(3.0), start());

        let inside = start().saturating_add(PERMIT - Duration::from_millis(1));
        assert_eq!(motor.enforce(inside), 3.0);

        let past = start().saturating_add(PERMIT);
        assert_eq!(
            motor.enforce(past),
            0.0,
            "the machine stops when the commanding service goes silent"
        );
        assert!(motor.permitted_until.is_none());
    }

    /// The permit ages on the suspend-aware boot clock, so a host that
    /// suspended past the deadline must not resume treating the retained
    /// command as fresh.
    #[test]
    fn host_suspend_counts_toward_the_permit_deadline() {
        let mut motor = Motor::default();
        motor.apply(api::component::motor::Command::Velocity(3.0), start());
        // `LocalInstant` reads CLOCK_BOOTTIME / Darwin continuous time, so an
        // hour of suspend advances it by an hour.
        let after_suspend = start().saturating_add(Duration::from_secs(3600));
        assert_eq!(motor.enforce(after_suspend), 0.0);
    }

    #[test]
    fn a_renewal_extends_the_permit() {
        let mut motor = Motor::default();
        motor.apply(api::component::motor::Command::Velocity(3.0), start());
        let renewed_at = start().saturating_add(PERMIT - Duration::from_millis(1));
        motor.apply(api::component::motor::Command::Velocity(3.0), renewed_at);
        assert_eq!(motor.enforce(start().saturating_add(PERMIT)), 3.0);
        assert_eq!(motor.enforce(renewed_at.saturating_add(PERMIT)), 0.0);
    }

    #[test]
    fn integration_advances_position() {
        let p = integrate(0.0, 10.0, 0.1);
        assert!((p - 1.0).abs() < 1e-9);
    }

    /// Freezing logical time must not leave the actuator commanded: the permit
    /// task runs on the host clock, so the effective velocity reaches zero
    /// without any step ever running.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn halting_logical_time_cannot_leave_the_actuator_commanded() {
        let motor = Arc::new(Mutex::new(Motor::default()));
        lock(&motor).apply(
            api::component::motor::Command::Velocity(3.0),
            LocalInstant::now(),
        );

        let watchdog_motor = Arc::clone(&motor);
        let watchdog = tokio::spawn(async move {
            let mut tick = tokio::time::interval(PERMIT_TICK);
            loop {
                tick.tick().await;
                lock(&watchdog_motor).enforce(LocalInstant::now());
            }
        });

        // No steps run at all: only the host clock advances.
        let stopped = tokio::time::timeout(PERMIT * 4, async {
            loop {
                if lock(&motor).velocity_radps == 0.0 {
                    return;
                }
                tokio::time::sleep(PERMIT_TICK).await;
            }
        })
        .await;
        watchdog.abort();
        stopped.expect("the actuator must stop when logical time stops advancing");
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
