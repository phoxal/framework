//! `ddsm115` - a Waveshare DDSM115 wheel-motor component driver (D54).
//!
//! Drivers are first-class users of the framework's macro + runner. The runner
//! launches one participant **per**
//! `components.instances` entry, each with a distinct `participant_id` and its own
//! `component_instance` (D47/D53), read here via `ctx.component()`. This driver binds
//! its instance's per-component motor-command (subscribe) and encoder-sample
//! (publish) topics (dynamic keys, D17/D38), applies commands to the hardware, and
//! feeds back encoder samples. `Participant::shutdown` parks the motor before the bus closes.
//!
//! # Non-zero actuation requires a permit (#952 section H)
//!
//! Command reception and expiry live in a **managed task**, not in `Participant::step`.
//! That is the whole point: if logical time stops advancing, `Participant::step` stops
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
use phoxal::bus::{LEASE_TRACE_TARGET, Observed, ProducerFence, ProducerId};
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
    /// The component instance this motor belongs to, so its decisions are
    /// attributable on a robot with several wheels.
    instance: String,
    velocity_radps: f32,
    /// When the current non-zero velocity stops being permitted. `None` means
    /// the motor is already stopped and needs no permit.
    permitted_until: Option<LocalInstant>,
    /// Which commanding process holds actuation authority. A superseded
    /// authority must not be able to renew a permit after a replacement has
    /// been accepted (#952 section G), and a replayed or reordered sample must
    /// not renew one either.
    fence: ProducerFence,
    /// Who granted the permit currently held, what they numbered it, and where
    /// it sat in this driver's own arrival order - so the lapse event names the
    /// same command the grant did.
    permitted_by: Option<(ProducerId, u64, u64)>,
    /// This driver's own arrival order for motor commands.
    observations: u64,
}

impl Motor {
    fn new(instance: String) -> Self {
        Motor {
            instance,
            ..Motor::default()
        }
    }

    /// Admit an observed command, granting a permit only if the producer still
    /// holds authority and its sequence advanced.
    ///
    /// `observed_at` is the receiver's own stamp - the one the bus took before
    /// decode - not the instant this task happened to drain the ring. Using the
    /// drain time would hand a fresh permit to a sample that sat in the ring
    /// across a scheduler stall or a host suspend.
    fn admit(&mut self, observed: Observed<api::component::motor::Command>) -> LeaseDecision {
        let producer = observed.metadata.producer;
        let sequence = observed.metadata.sequence;
        self.observations = self.observations.saturating_add(1);
        let observation = self.observations;
        let decision = self.fence.admit(producer, sequence);
        match decision {
            LeaseDecision::Renewed => {
                tracing::debug!(
                    target: LEASE_TRACE_TARGET,
                    input = "component/motor/command",
                    instance = self.instance,
                    producer = %producer,
                    sequence,
                    observation,
                    decision = "renewed",
                    "command renewed the actuator permit"
                );
                self.permitted_by = Some((producer, sequence, observation));
                self.apply(observed.body, observed.observed_at);
            }
            LeaseDecision::ProducerReplaced { superseded } => {
                tracing::info!(
                    target: LEASE_TRACE_TARGET,
                    input = "component/motor/command",
                    instance = self.instance,
                    producer = %producer,
                    sequence,
                    observation,
                    superseded = %superseded,
                    decision = "producer_replaced",
                    "a replacement producer took actuation authority"
                );
                self.permitted_by = Some((producer, sequence, observation));
                self.apply(observed.body, observed.observed_at);
            }
            LeaseDecision::Rejected(rejection) => {
                tracing::info!(
                    target: LEASE_TRACE_TARGET,
                    input = "component/motor/command",
                    instance = self.instance,
                    producer = %producer,
                    sequence,
                    observation,
                    decision = "rejected",
                    reason = %rejection,
                    "motor command rejected; the permit was not renewed"
                );
            }
        }
        decision
    }

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

    /// Stop the motor if its permit has lapsed, or if the host clock cannot be
    /// read at all - a permit nobody can age is not a permit.
    fn enforce_now(&mut self) -> f32 {
        match LocalInstant::try_now() {
            Some(now) => self.enforce(now),
            None => {
                let granted = self.permitted_by;
                tracing::error!(
                    target: LEASE_TRACE_TARGET,
                    input = "component/motor/command",
                    instance = self.instance,
                    producer = granted.map(|(producer, _, _)| producer.to_string()),
                    sequence = granted.map(|(_, sequence, _)| sequence),
                    observation = granted.map(|(_, _, observation)| observation),
                    decision = "permit_unmeasurable",
                    "the host boot clock could not be read; stopping the motor"
                );
                self.stop();
                self.velocity_radps
            }
        }
    }

    /// Stop the motor if its permit has lapsed. Returns the effective velocity.
    fn enforce(&mut self, now: LocalInstant) -> f32 {
        match self.permitted_until {
            Some(deadline) if now.reached(deadline) => {
                let granted = self.permitted_by;
                tracing::info!(
                    target: LEASE_TRACE_TARGET,
                    input = "component/motor/command",
                    instance = self.instance,
                    producer = granted.map(|(producer, _, _)| producer.to_string()),
                    sequence = granted.map(|(_, sequence, _)| sequence),
                    observation = granted.map(|(_, _, observation)| observation),
                    decision = "permit_lapsed",
                    "the actuator permit lapsed; stopping the motor"
                );
                self.stop();
            }
            None => self.stop(),
            Some(_) => {}
        }
        self.velocity_radps
    }

    fn stop(&mut self) {
        self.velocity_radps = 0.0;
        self.permitted_until = None;
        self.permitted_by = None;
    }
}

/// Build an observed command for the permit path. Framework-internal shape used
/// by the driver's own tests; the live path receives these from the bus.
#[cfg(test)]
fn observed_command(
    producer: ProducerId,
    sequence: u64,
    observed_at: LocalInstant,
    body: api::component::motor::Command,
) -> Observed<api::component::motor::Command> {
    Observed {
        body,
        metadata: phoxal::bus::BusMetadata {
            codec: phoxal::bus::CodecId::MessagePack.as_u8(),
            producer,
            sequence,
            produced_at: None,
            participant: "test".to_string(),
        },
        observed_at,
    }
}

pub struct Api {
    // The command subscription is owned by the managed permit task.
    encoder: MeasurementPublisher<api::component::encoder::Sample>,
}

pub struct Ddsm115State {
    // Driver-private hardware state.
    instance: String,
    position_rad: f64,
    motor: Arc<Mutex<Motor>>,
}

#[phoxal::driver(state = Ddsm115State, api = Api)]
pub struct Ddsm115;

impl Participant for Ddsm115 {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let instance = ctx.component()?.to_string();
        // Prove the instance exists in the robot model (binds this driver to it).
        let _ = ctx.robot()?.component_instance(&instance)?;

        let command = ctx
            .subscriber(
                api::topic::owner()
                    .component(&instance)
                    .motor(MOTOR_CAPABILITY)
                    .command(),
                32,
            )
            .await?;
        let encoder = ctx
            .measurement_publisher(
                api::topic::owner()
                    .component(&instance)
                    .encoder(ENCODER_CAPABILITY)
                    .sample(),
            )
            .await?;

        let motor = Arc::new(Mutex::new(Motor::new(instance.clone())));
        let permit_commands = command.clone();
        let permit_motor = Arc::clone(&motor);
        ctx.spawn_managed("motor-permit", async move {
            hold_permit_forever(permit_commands, permit_motor).await;
        });

        Ok((
            Ddsm115State {
                instance,
                position_rad: 0.0,
                motor,
            },
            Api { encoder },
        ))
    }

    #[phoxal::step(hz = 100)]
    async fn step(
        &self,
        api: &Self::Api,
        step: StepContext,
        state: &mut Self::State,
    ) -> Result<()> {
        // Read the effective velocity, enforcing the permit here too so a step
        // can never integrate a velocity the permit task would have stopped.
        let velocity_radps = lock(&state.motor).enforce_now();

        // Integrate the wheel position from the effective velocity.
        state.position_rad = integrate(state.position_rad, velocity_radps, step.dt().as_secs_f64());

        // This driver models the motor rather than reading a device clock, so
        // the encoder capture coincides exactly with this step.
        api.encoder.publish(
            CaptureStamp::exact(step.now()),
            api::component::encoder::Sample {
                position_rad: state.position_rad,
                velocity_radps,
            },
        )?;
        Ok(())
    }

    async fn shutdown(
        &self,
        _ctx: ShutdownContext,
        _api: &Self::Api,
        state: &mut Self::State,
    ) -> Result<()> {
        // Park: command the motor to a stop. A real driver flushes the bus/CAN here
        // before the session closes.
        lock(&state.motor).stop();
        let _ = &state.instance;
        Ok(())
    }
}

/// Receive commands and enforce the permit, independently of `Participant::step`.
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
                lock(&motor).admit(observed);
            }
            _ = tick.tick() => {
                lock(&motor).enforce_now();
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

    /// The permit is renewed from the *observation* instant the bus stamped,
    /// not from the moment this task drained the ring. A sample that sat in the
    /// ring across a stall is already old when it is admitted, so it must not
    /// buy a full fresh window.
    #[test]
    fn a_stale_queued_command_does_not_buy_a_fresh_permit() {
        let producer = ProducerId::mint();
        let mut motor = Motor::default();
        // Observed at `start`, drained a full permit window later.
        motor.admit(observed_command(
            producer,
            1,
            start(),
            api::component::motor::Command::Velocity(3.0),
        ));
        assert_eq!(
            motor.enforce(start().saturating_add(PERMIT)),
            0.0,
            "the permit ages from when the sample was observed, not when it was drained"
        );
    }

    /// A superseded commanding process must not be able to renew the actuator
    /// after a replacement has been accepted, and a replayed sequence from the
    /// accepted producer must not renew it either (#952 section G).
    #[test]
    fn a_superseded_or_replayed_command_cannot_renew_the_permit() {
        let first = ProducerId::mint();
        let second = ProducerId::mint();
        let mut motor = Motor::default();

        motor.admit(observed_command(
            first,
            9,
            start(),
            api::component::motor::Command::Velocity(1.0),
        ));
        assert!(matches!(
            motor.admit(observed_command(
                second,
                0,
                start(),
                api::component::motor::Command::Velocity(2.0),
            )),
            LeaseDecision::ProducerReplaced { .. }
        ));
        assert_eq!(motor.velocity_radps, 2.0);

        // The replaced authority is fenced for good.
        assert!(matches!(
            motor.admit(observed_command(
                first,
                10,
                start(),
                api::component::motor::Command::Velocity(9.0),
            )),
            LeaseDecision::Rejected(_)
        ));
        assert_eq!(motor.velocity_radps, 2.0);

        // And a replay from the accepted producer renews nothing.
        assert!(matches!(
            motor.admit(observed_command(
                second,
                0,
                start().saturating_add(PERMIT),
                api::component::motor::Command::Velocity(9.0),
            )),
            LeaseDecision::Rejected(_)
        ));
        assert_eq!(
            motor.enforce(start().saturating_add(PERMIT)),
            0.0,
            "a replayed sample must not extend the permit"
        );
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
        let motor = Arc::new(Mutex::new(Motor::new("left".to_string())));
        lock(&motor).apply(
            api::component::motor::Command::Velocity(3.0),
            LocalInstant::try_now().expect("test host clock"),
        );

        let watchdog_motor = Arc::clone(&motor);
        let watchdog = tokio::spawn(async move {
            let mut tick = tokio::time::interval(PERMIT_TICK);
            loop {
                tick.tick().await;
                lock(&watchdog_motor).enforce_now();
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
}
