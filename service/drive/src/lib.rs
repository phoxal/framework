//! `drive` - the official differential-drive participant.
//!
//! A scheduled participant that closes the body-twist to wheel-velocity loop.
//! It subscribes to `drive/target`, clamps it to the configured linear and
//! angular limits, mixes the limited twist into per-wheel angular speeds via
//! differential inverse kinematics, and commands each wheel motor on its dynamic
//! `component/<id>/motor/<cap>/command` topic.
//! It also publishes `drive/state` (the raw requested target, the limited target,
//! the actuator authority, and any stop reason).
//! The per-side motor bindings and wheel geometry are built from the robot model
//! (D33). Unsupported kinematics make the service explicitly inactive rather
//! than failing static startup.
//! `drive/target` is internal actuation, not an observation: it carries no
//! production timestamp. Its liveness is therefore a **receiver-owned lease**
//! (#952 section F) - `drive` stamps its own observation, rejects a
//! non-increasing sequence from the accepted producer, and applies the held
//! command at a logical step only while both expiry conditions hold: a
//! host-monotonic silence deadline and a logical hold horizon. Either one
//! elapsing stops the wheels rather than carrying the last command. On shutdown
//! it makes a best-effort pass to park every wheel before the bus closes.

use std::time::Duration;

use anyhow::{Result, bail};
use phoxal::api;
use phoxal::model::component::v0::CapabilityRef;
use phoxal::model::robot::v0::{KinematicConfig, MotionLimits};
use phoxal::model::v0::Robot;
use phoxal::prelude::*;

/// How long `drive` tolerates silence from the accepted target producer. This
/// is human/network liveness and runs on the host clock, so an accelerated
/// simulation does not stretch it.
const TARGET_SILENCE: Duration = Duration::from_millis(500);

/// How far the robot may travel on one held target. This is bounded travel and
/// runs on robot time, so a decelerated simulation does not shrink it.
const TARGET_HOLD: Duration = Duration::from_millis(500);

/// Differential-drive inverse kinematics: body twist → wheel angular speeds.
#[derive(Clone, Copy)]
struct DifferentialDrive {
    wheel_radius_m: f64,
    wheel_base_m: f64,
}

impl DifferentialDrive {
    /// (left, right) wheel angular speed (rad/s) for a body twist.
    fn invert(&self, linear_mps: f64, angular_radps: f64) -> (f64, f64) {
        let half_track = self.wheel_base_m / 2.0;
        let v_left = linear_mps - angular_radps * half_track;
        let v_right = linear_mps + angular_radps * half_track;
        (v_left / self.wheel_radius_m, v_right / self.wheel_radius_m)
    }
}

/// One actuator binding resolved from the robot model.
#[derive(Clone)]
struct MotorBinding {
    component_id: String,
    capability_id: String,
    direction_sign: i8,
}

impl MotorBinding {
    fn resolve(robot: &Robot, refs: &[CapabilityRef], field: &str) -> Result<Vec<Self>> {
        if refs.is_empty() {
            bail!("robot.kinematic.{field} must list at least one actuator");
        }
        refs.iter()
            .map(|r| {
                let (_motor, direction_sign) = robot.require_motor(r)?;
                Ok(MotorBinding {
                    component_id: r.component_id.clone(),
                    capability_id: r.capability_id.clone(),
                    direction_sign,
                })
            })
            .collect()
    }

    /// The dynamic per-instance motor-command topic for this binding. Drive
    /// CLIENT-publishes motor commands (the motor driver owns/subscribes them), so
    /// this is the `Publish` side from the public builder.
    fn topic(&self) -> phoxal::bus::Topic<phoxal::bus::Publish<api::component::motor::Command>> {
        api::topic::new()
            .component(&self.component_id)
            .motor(&self.capability_id)
            .command()
    }
}

/// Typed drive config built from the robot model (D33).
struct DriveConfig {
    kinematics: Option<DifferentialDrive>,
    left: Vec<MotorBinding>,
    right: Vec<MotorBinding>,
    limits: MotionLimits,
}

impl DriveConfig {
    fn from_robot(robot: &Robot) -> Result<Self> {
        let limits = robot.manifest.robot.motion_limits.validate()?;
        let KinematicConfig::Differential {
            left_actuators,
            right_actuators,
            wheel_radius_m,
            wheel_base_m,
            ..
        } = &robot.manifest.robot.kinematic
        else {
            return Ok(Self {
                kinematics: None,
                left: Vec::new(),
                right: Vec::new(),
                limits,
            });
        };
        if !(wheel_radius_m.is_finite() && *wheel_radius_m > 0.0) {
            bail!("wheel_radius_m must be finite and > 0");
        }
        if !(wheel_base_m.is_finite() && *wheel_base_m > 0.0) {
            bail!("wheel_base_m must be finite and > 0");
        }
        Ok(DriveConfig {
            kinematics: Some(DifferentialDrive {
                wheel_radius_m: *wheel_radius_m,
                wheel_base_m: *wheel_base_m,
            }),
            left: MotorBinding::resolve(robot, left_actuators, "left_actuators")?,
            right: MotorBinding::resolve(robot, right_actuators, "right_actuators")?,
            limits,
        })
    }
}

#[derive(phoxal::Api)]
pub struct Api {
    target: Subscriber<api::drive::Target>,
    state: StatePublisher<api::drive::State>,
    left_motors: Vec<CommandPublisher<api::component::motor::Command>>,
    right_motors: Vec<CommandPublisher<api::component::motor::Command>>,
}

#[phoxal::service(id = "drive", config = ())]
pub struct Drive {
    // Runtime-private typed state (not handles).
    config: DriveConfig,
    target: Lease<api::drive::Target>,
}

#[phoxal::behavior]
impl Drive {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        // Owner opt-in (plan #00 L2): the runner-minted capability that the
        // owner (`internal`) topic builder requires.
        let cap = ctx.owner_capability();
        let config = DriveConfig::from_robot(ctx.robot()?)?;

        // Drive OWNS the `drive` node: it reads its command input and publishes its
        // telemetry through the owner (`internal`) builder.
        let target = ctx
            .subscriber(api::topic::internal::new(cap).drive().target(), 32)
            .await?;
        let state = ctx
            .state_publisher(api::topic::internal::new(cap).drive().state())
            .await?;

        let mut left_motors = Vec::with_capacity(config.left.len());
        for binding in &config.left {
            left_motors.push(ctx.command_publisher(binding.topic()).await?);
        }
        let mut right_motors = Vec::with_capacity(config.right.len());
        for binding in &config.right {
            right_motors.push(ctx.command_publisher(binding.topic()).await?);
        }

        Ok((
            Self {
                config,
                target: Lease::new(TARGET_SILENCE, TARGET_HOLD),
            },
            Self::Api {
                target,
                state,
                left_motors,
                right_motors,
            },
        ))
    }

    #[reset]
    async fn reset(&mut self, _ctx: ResetContext) -> Result<()> {
        self.target.clear();
        Ok(())
    }

    #[step(hz = 50)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        let now = step.now();
        let host_now = LocalInstant::now();

        // Offer every inbound target to the lease. The lease - not the sender -
        // decides what is live: it fences a superseded producer, rejects a
        // replayed sequence, and stamps the receiver's own observation.
        while let Some(observed) = api.target.try_recv() {
            let decision = self.target.offer(
                observed.metadata.producer,
                observed.metadata.sequence,
                observed.observed_at,
                observed.body,
            );
            if let LeaseDecision::Rejected(rejection) = decision {
                tracing::warn!(target: "phoxal.drive", error = %rejection, "rejected drive target");
            }
        }

        let (target, mut limited_target, mut authority, mut stop_reason) = resolve_target(
            self.target.live(host_now, now),
            self.config.limits,
            self.config.kinematics.is_some(),
        );
        let wheel_targets = self
            .config
            .kinematics
            .and_then(|kinematics| wheel_targets(kinematics, &limited_target));
        let (left, right) = wheel_targets.unwrap_or((0.0, 0.0));
        if self.config.kinematics.is_some() && wheel_targets.is_none() {
            limited_target = stopped_target();
            authority = api::drive::ActuatorAuthority::Stopped;
            stop_reason = Some(api::drive::StopReason::ActuatorCommandNotFinite);
        }

        for (publisher, binding) in api.left_motors.iter().zip(&self.config.left) {
            publisher.send(command(left, binding.direction_sign))?;
        }
        for (publisher, binding) in api.right_motors.iter().zip(&self.config.right) {
            publisher.send(command(right, binding.direction_sign))?;
        }

        api.state.publish(
            step.token(),
            api::drive::State {
                target,
                limited_target,
                actuator_authority: authority,
                stop_reason: stop_reason.clone(),
            },
        )?;
        Ok(())
    }

    #[shutdown]
    async fn shutdown(&mut self, api: &mut Self::Api, _ctx: ShutdownContext) -> Result<()> {
        // Best-effort park: command every wheel to stop before the bus closes.
        // A motor command expresses no robot time, so this needs no instant -
        // which is exactly why `Stop` stays callable outside a step, as it must.
        for publisher in api.left_motors.iter().chain(&api.right_motors) {
            let _ = publisher.send(api::component::motor::Command::Stop);
        }
        Ok(())
    }
}

/// Resolve the effective (limited) target from whatever the lease says is
/// live. `live` is `None` when nothing has been accepted, when the producer has
/// gone silent past the host deadline, or when the held command has been
/// applied for longer than the logical hold horizon - all three stop the wheels
/// rather than carrying the last command.
fn resolve_target(
    live: Option<&api::drive::Target>,
    limits: MotionLimits,
    has_kinematics: bool,
) -> (
    api::drive::Target,
    api::drive::Target,
    api::drive::ActuatorAuthority,
    Option<api::drive::StopReason>,
) {
    let stopped = stopped_target();
    if !has_kinematics {
        return (
            stopped.clone(),
            stopped,
            api::drive::ActuatorAuthority::Stopped,
            Some(api::drive::StopReason::Inactive),
        );
    }
    let Some(target) = live else {
        return (
            stopped.clone(),
            stopped,
            api::drive::ActuatorAuthority::Stopped,
            Some(api::drive::StopReason::TargetStale),
        );
    };
    if !(target.linear_x_mps.is_finite()
        && target.angular_z_radps.is_finite()
        && target.curvature_limit_radpm.is_none_or(f32::is_finite))
    {
        return (
            target.clone(),
            stopped,
            api::drive::ActuatorAuthority::Stopped,
            Some(api::drive::StopReason::TargetNotFinite),
        );
    }
    let limited = api::drive::Target {
        linear_x_mps: clamp_f32(target.linear_x_mps, limits.max_linear_speed_mps),
        angular_z_radps: clamp_f32(target.angular_z_radps, limits.max_angular_speed_radps),
        curvature_limit_radpm: target.curvature_limit_radpm,
    };
    (
        target.clone(),
        limited,
        api::drive::ActuatorAuthority::Active,
        None,
    )
}

fn stopped_target() -> api::drive::Target {
    api::drive::Target {
        linear_x_mps: 0.0,
        angular_z_radps: 0.0,
        curvature_limit_radpm: None,
    }
}

fn command(wheel_radps: f64, direction_sign: i8) -> api::component::motor::Command {
    api::component::motor::Command::Velocity((wheel_radps * f64::from(direction_sign)) as f32)
}

fn wheel_targets(kinematics: DifferentialDrive, target: &api::drive::Target) -> Option<(f64, f64)> {
    let (left, right) = kinematics.invert(
        f64::from(target.linear_x_mps),
        f64::from(target.angular_z_radps),
    );
    (left.is_finite()
        && right.is_finite()
        && left.abs() <= f64::from(f32::MAX)
        && right.abs() <= f64::from(f32::MAX))
    .then_some((left, right))
}

fn clamp_f32(value: f32, limit: f64) -> f32 {
    value.clamp(-limit as f32, limit as f32)
}

#[cfg(test)]
mod tests {
    use phoxal::api;
    use phoxal::bus::ContractBody;
    use phoxal::bus::{Lease, LeaseDecision, LocalInstant, ProducerId, RobotInstant, TimelineId};
    use phoxal::participant::{ContractRole, Participant, ParticipantApi};

    use super::{
        DifferentialDrive, Drive, DriveConfig, Duration, MotionLimits, TARGET_HOLD, TARGET_SILENCE,
        resolve_target, stopped_target,
    };
    use std::path::PathBuf;

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixture/robot/rgbd-imu-diff-drive")
    }

    #[test]
    fn differential_inversion() {
        let d = DifferentialDrive {
            wheel_radius_m: 0.1,
            wheel_base_m: 0.4,
        };
        let (l, r) = d.invert(1.0, 0.0);
        assert!((l - 10.0).abs() < 1e-9 && (r - 10.0).abs() < 1e-9);
        let (l, r) = d.invert(0.0, 1.0);
        assert!((l + 2.0).abs() < 1e-9 && (r - 2.0).abs() < 1e-9);
    }

    #[test]
    fn wheel_command_overflow_fails_closed() {
        let kinematics = DifferentialDrive {
            wheel_radius_m: f64::MIN_POSITIVE,
            wheel_base_m: 0.4,
        };
        let target = api::drive::Target {
            linear_x_mps: 0.6,
            angular_z_radps: 0.0,
            curvature_limit_radpm: None,
        };
        assert!(super::wheel_targets(kinematics, &target).is_none());
    }

    #[test]
    fn config_from_robot_resolves_per_side_motors() {
        let robot = phoxal::model::v0::Robot::read_from_dir(fixture()).unwrap();
        let config = DriveConfig::from_robot(&robot).unwrap();
        // The fixture is a 4-wheel differential: 2 motors per side.
        assert_eq!(config.left.len(), 2);
        assert_eq!(config.right.len(), 2);
        assert!(config.kinematics.unwrap().wheel_radius_m > 0.0);
        // Each binding resolves to a concrete dynamic motor topic.
        let topic = config.left[0].topic();
        assert!(topic.key().starts_with("v0.1/component/"));
        assert!(topic.key().ends_with("/command"));
    }

    const LIMITS: MotionLimits = MotionLimits {
        max_linear_speed_mps: 0.6,
        max_angular_speed_radps: 2.0,
    };

    #[test]
    fn resolve_reports_raw_requested_and_limited_targets() {
        let requested = api::drive::Target {
            linear_x_mps: 5.0,
            angular_z_radps: -5.0,
            curvature_limit_radpm: Some(0.4),
        };

        let (target, limited_target, authority, stop_reason) =
            resolve_target(Some(&requested), LIMITS, true);

        assert_eq!(target, requested);
        assert_eq!(limited_target.linear_x_mps, 0.6);
        assert_eq!(limited_target.angular_z_radps, -2.0);
        assert_eq!(limited_target.curvature_limit_radpm, Some(0.4));
        assert_eq!(authority, api::drive::ActuatorAuthority::Active);
        assert_eq!(stop_reason, None);
    }

    #[test]
    fn nothing_live_stops_the_wheels_rather_than_carrying_the_last_command() {
        let (_, limited, authority, stop_reason) = resolve_target(None, LIMITS, true);
        assert_eq!(limited, stopped_target());
        assert_eq!(authority, api::drive::ActuatorAuthority::Stopped);
        assert_eq!(stop_reason, Some(api::drive::StopReason::TargetStale));
    }

    /// Either expiry condition alone stops the wheels. Host silence is checked
    /// on the host clock so an accelerated simulation cannot stretch it, and the
    /// hold horizon is checked on robot time so a decelerated one cannot shrink
    /// it (#952 section F).
    #[test]
    fn either_lease_expiry_condition_alone_stops_the_wheels() {
        let requested = api::drive::Target {
            linear_x_mps: 0.4,
            angular_z_radps: 0.0,
            curvature_limit_radpm: None,
        };
        let producer = ProducerId::mint();
        let line = TimelineId::mint();
        let host_start = LocalInstant::from_boot_ns(0);
        let robot_start = RobotInstant::new(line, 0);

        let mut silent = Lease::new(TARGET_SILENCE, TARGET_HOLD);
        silent.offer(producer, 1, host_start, requested.clone());
        assert!(silent.live(host_start, robot_start).is_some());
        let past_silence = host_start.saturating_add(TARGET_SILENCE + Duration::from_millis(1));
        assert!(
            silent.live(past_silence, robot_start).is_none(),
            "host silence expires the lease even while robot time stands still"
        );

        let mut held = Lease::new(TARGET_SILENCE, TARGET_HOLD);
        held.offer(producer, 1, host_start, requested);
        assert!(held.live(host_start, robot_start).is_some());
        let past_hold = robot_start.saturating_add(TARGET_HOLD + Duration::from_millis(1));
        assert!(
            held.live(host_start, past_hold).is_none(),
            "the logical horizon expires the lease even while host time stands still"
        );
    }

    /// A restarted publisher is a fresh producer whose sequence restarts at
    /// zero; that must take over rather than look like a replay, and the
    /// superseded producer must not be able to command afterwards.
    #[test]
    fn a_replacement_producer_takes_over_and_fences_the_previous_one() {
        let target = |linear_x_mps| api::drive::Target {
            linear_x_mps,
            angular_z_radps: 0.0,
            curvature_limit_radpm: None,
        };
        let first = ProducerId::mint();
        let second = ProducerId::mint();
        let host_now = LocalInstant::from_boot_ns(0);
        let now = RobotInstant::new(TimelineId::mint(), 0);

        let mut lease = Lease::new(TARGET_SILENCE, TARGET_HOLD);
        lease.offer(first, 9, host_now, target(0.1));
        assert!(matches!(
            lease.offer(second, 0, host_now, target(0.2)),
            LeaseDecision::ProducerReplaced { superseded } if superseded == first
        ));
        assert_eq!(lease.producer(), Some(second));
        assert_eq!(
            lease.live(host_now, now).map(|target| target.linear_x_mps),
            Some(0.2)
        );
        assert!(matches!(
            lease.offer(second, 0, host_now, target(0.3)),
            LeaseDecision::Rejected(_)
        ));
    }

    #[test]
    fn api_declares_the_drive_contracts() {
        assert_eq!(<Drive as Participant>::ID, "drive");

        let contracts = <<Drive as Participant>::Api as ParticipantApi>::CONTRACTS;
        assert!(contracts.iter().any(|c| {
            c.topic == <api::drive::Target as ContractBody>::TOPIC
                && c.role == ContractRole::Subscribe
        }));
        assert!(contracts.iter().any(|c| {
            c.topic == <api::drive::State as ContractBody>::TOPIC && c.role == ContractRole::Publish
        }));
        assert!(contracts.iter().any(|c| {
            c.topic == <api::component::motor::Command as ContractBody>::TOPIC
                && c.role == ContractRole::Publish
        }));
    }
}
