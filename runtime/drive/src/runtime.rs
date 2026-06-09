use std::time::Duration;

use crate::core::DifferentialDrive;
use anyhow::{Result, bail};
use phoxal::api::component::v1::capability::motor;
use phoxal::api::drive::v1::{
    ActuatorAuthority, State, StopReason, Target, state as drive_state, target as drive_target,
};
use phoxal::bus::pubsub::Stamped;
use phoxal::model::component::v1::CapabilityRef;
use phoxal::model::robot::v1::KinematicConfig;
use phoxal::model::v1::Robot;
use phoxal::runtime::clock::Step;
use phoxal::runtime::{EmptyArgs, RobotRuntimeArgs};
use phoxal::runtime::{InputPolicy, Io, Publisher, Runtime, RuntimeInputs};

const CLOCK_PERIOD: Duration = Duration::from_millis(20);
const TARGET_STALE_TIMEOUT_NS: u64 = 500_000_000;
const MAX_LINEAR_SPEED_MPS: f64 = 0.6;
const MAX_ANGULAR_SPEED_RADPS: f64 = 2.0;

#[derive(Clone)]
pub struct Config {
    left_motors: Vec<MotorBinding>,
    right_motors: Vec<MotorBinding>,
    wheel_radius_m: f64,
    wheel_base_m: f64,
    max_linear_speed_mps: f64,
    max_angular_speed_radps: f64,
    clock_period: Duration,
}

#[derive(Clone)]
struct MotorBinding {
    component_id: String,
    capability_id: String,
    direction_sign: i8,
}

impl Config {
    pub fn from_robot(robot: &Robot) -> Result<Self> {
        let KinematicConfig::Differential {
            left_actuators,
            right_actuators,
            wheel_radius_m,
            wheel_base_m,
            ..
        } = &robot.manifest.motion.kinematic
        else {
            bail!(
                "drive runtime only supports differential drive kinematics, found {}",
                robot.manifest.motion.kinematic.variant_label()
            );
        };

        validate_positive_f64(*wheel_radius_m, "wheel_radius_m")?;
        validate_positive_f64(*wheel_base_m, "wheel_base_m")?;

        let left_motors = resolve_motor_bindings(robot, left_actuators, "left_actuators")?;
        let right_motors = resolve_motor_bindings(robot, right_actuators, "right_actuators")?;

        Ok(Self {
            left_motors,
            right_motors,
            wheel_radius_m: *wheel_radius_m,
            wheel_base_m: *wheel_base_m,
            max_linear_speed_mps: MAX_LINEAR_SPEED_MPS,
            max_angular_speed_radps: MAX_ANGULAR_SPEED_RADPS,
            clock_period: CLOCK_PERIOD,
        })
    }

    pub const fn clock_period(&self) -> Duration {
        self.clock_period
    }
}

impl MotorBinding {
    fn new(reference: &CapabilityRef, direction_sign: i8) -> Self {
        Self {
            component_id: reference.component_id.clone(),
            capability_id: reference.capability_id.clone(),
            direction_sign,
        }
    }
}

fn resolve_motor_bindings(
    robot: &Robot,
    actuators: &[CapabilityRef],
    field: &str,
) -> Result<Vec<MotorBinding>> {
    if actuators.is_empty() {
        bail!("motion.kinematic.{field} must list at least one actuator");
    }

    actuators
        .iter()
        .map(|actuator| {
            let (_motor, direction_sign) = robot.require_motor(actuator)?;
            Ok(MotorBinding::new(actuator, direction_sign))
        })
        .collect()
}

async fn motor_publishers(
    io: &mut Io<Input>,
    motors: &[MotorBinding],
) -> Result<Vec<Publisher<Stamped<motor::Command>>>> {
    let mut publishers = Vec::with_capacity(motors.len());
    for motor in motors {
        publishers.push(
            io.publisher::<Stamped<motor::Command>>(&motor.topic())
                .await?,
        );
    }
    Ok(publishers)
}

fn validate_positive_f64(value: f64, field_name: &str) -> Result<()> {
    if !value.is_finite() || value <= 0.0 {
        bail!("{field_name} must be finite and > 0");
    }
    Ok(())
}

pub enum Input {
    DriveTarget(Stamped<Target>),
}

pub struct DriveRuntime {
    config: Config,
    latest_target: Option<Stamped<Target>>,
    left_motor_publishers: Vec<Publisher<Stamped<motor::Command>>>,
    right_motor_publishers: Vec<Publisher<Stamped<motor::Command>>>,
    state_publisher: Publisher<Stamped<State>>,
}

#[async_trait::async_trait]
impl Runtime for DriveRuntime {
    const RUNTIME_ID: &'static str = "drive";

    type Args = EmptyArgs;
    type Config = Config;
    type Input = Input;

    fn config(_args: &Self::Args, common: &RobotRuntimeArgs) -> Result<Self::Config> {
        Config::from_robot(&common.robot()?)
    }

    fn clock_period(config: &Self::Config) -> Duration {
        config.clock_period()
    }

    async fn new(io: &mut Io<Self::Input>, config: Self::Config) -> Result<Self> {
        io.subscribe_with::<Stamped<Target>, _>(
            &drive_target::path(),
            InputPolicy::latest(),
            Input::DriveTarget,
        )
        .await?;

        let left_motor_publishers = motor_publishers(io, &config.left_motors).await?;
        let right_motor_publishers = motor_publishers(io, &config.right_motors).await?;
        let state_publisher = io.publisher::<Stamped<State>>(&drive_state::path()).await?;

        Ok(Self {
            config,
            latest_target: None,
            left_motor_publishers,
            right_motor_publishers,
            state_publisher,
        })
    }

    async fn step(&mut self, step: Step, inputs: RuntimeInputs<Self::Input>) -> Result<()> {
        for input in inputs {
            match input {
                Input::DriveTarget(sample) => self.latest_target = Some(sample),
            }
        }

        let now_ns = step.tick.time_ns();
        let (target, authority, stop_reason) = self.resolve_target(now_ns);
        let motor_commands = motor_velocity_commands(&self.config, target);

        for (publisher, command) in self
            .left_motor_publishers
            .iter()
            .zip(motor_commands.left.iter())
        {
            publisher
                .put(&Stamped::new(now_ns, motor::Command::Velocity(*command)))
                .await?;
        }
        for (publisher, command) in self
            .right_motor_publishers
            .iter()
            .zip(motor_commands.right.iter())
        {
            publisher
                .put(&Stamped::new(now_ns, motor::Command::Velocity(*command)))
                .await?;
        }

        // MVP phase: safety/localize authority integration is deferred, so a
        // fresh target is treated as authorized and only hard clamped here.
        self.state_publisher
            .put(&Stamped::new(
                now_ns,
                State {
                    target,
                    limited_target: target,
                    actuator_authority: authority,
                    stop_reason,
                },
            ))
            .await?;

        Ok(())
    }

    fn scenarios() -> &'static [phoxal::runtime::ScenarioDescriptor] {
        crate::scenarios::SCENARIOS
    }

    async fn run_scenario(
        name: &str,
        _common: &RobotRuntimeArgs,
        _args: &Self::Args,
    ) -> Result<()> {
        crate::scenarios::run(name)
    }
}

impl DriveRuntime {
    fn resolve_target(&self, now_ns: u64) -> (Target, ActuatorAuthority, Option<StopReason>) {
        let Some(latest) = &self.latest_target else {
            return (
                Target {
                    linear_x_mps: 0.0,
                    angular_z_radps: 0.0,
                },
                ActuatorAuthority::Stopped,
                Some(StopReason::NoTarget),
            );
        };

        let age_ns = now_ns.saturating_sub(latest.timestamp_ns);
        if age_ns > TARGET_STALE_TIMEOUT_NS {
            return (
                Target {
                    linear_x_mps: 0.0,
                    angular_z_radps: 0.0,
                },
                ActuatorAuthority::Stopped,
                Some(StopReason::CommandTimedOut),
            );
        }

        let target = Target {
            linear_x_mps: latest.data.linear_x_mps.clamp(
                -self.config.max_linear_speed_mps,
                self.config.max_linear_speed_mps,
            ),
            angular_z_radps: latest.data.angular_z_radps.clamp(
                -self.config.max_angular_speed_radps,
                self.config.max_angular_speed_radps,
            ),
        };
        (target, ActuatorAuthority::Active, None)
    }
}

impl MotorBinding {
    fn topic(&self) -> String {
        motor::command::path(self.component_id.as_str(), self.capability_id.as_str())
    }
}

struct MotorVelocityCommands {
    left: Vec<f32>,
    right: Vec<f32>,
}

fn motor_velocity_commands(config: &Config, target: Target) -> MotorVelocityCommands {
    let (left_omega_radps, right_omega_radps) = DifferentialDrive {
        wheel_radius_m: config.wheel_radius_m,
        wheel_base_m: config.wheel_base_m,
    }
    .invert(target.linear_x_mps, target.angular_z_radps);

    MotorVelocityCommands {
        left: side_velocity_commands(&config.left_motors, left_omega_radps),
        right: side_velocity_commands(&config.right_motors, right_omega_radps),
    }
}

fn side_velocity_commands(motors: &[MotorBinding], omega_radps: f64) -> Vec<f32> {
    motors
        .iter()
        .map(|motor| signed_velocity(omega_radps, motor.direction_sign))
        .collect()
}

fn signed_velocity(omega_radps: f64, direction_sign: i8) -> f32 {
    (omega_radps * f64::from(direction_sign)) as f32
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn config_from_fixture_resolves_side_motor_lists() {
        let robot = fixture_robot();
        let config = match Config::from_robot(&robot) {
            Ok(config) => config,
            Err(error) => panic!("failed to resolve drive config from fixture: {error:#}"),
        };

        assert_eq!(
            motor_summary(&config.left_motors),
            vec![
                ("front_left_drive", "motor", 1),
                ("rear_left_drive", "motor", 1)
            ]
        );
        assert_eq!(
            motor_summary(&config.right_motors),
            vec![
                ("front_right_drive", "motor", -1),
                ("rear_right_drive", "motor", -1)
            ]
        );
        assert_eq!(config.wheel_radius_m, 0.10);
        assert_eq!(config.wheel_base_m, 0.40);
    }

    #[test]
    fn four_motor_differential_fanout_keeps_each_side_in_lockstep() {
        let robot = fixture_robot();
        let config = match Config::from_robot(&robot) {
            Ok(config) => config,
            Err(error) => panic!("failed to resolve drive config from fixture: {error:#}"),
        };
        let commands = motor_velocity_commands(
            &config,
            Target {
                linear_x_mps: 0.5,
                angular_z_radps: 0.5,
            },
        );

        assert_eq!(commands.left, vec![4.0, 4.0]);
        assert_eq!(commands.right, vec![-6.0, -6.0]);
    }

    fn motor_summary(motors: &[MotorBinding]) -> Vec<(&str, &str, i8)> {
        motors
            .iter()
            .map(|motor| {
                (
                    motor.component_id.as_str(),
                    motor.capability_id.as_str(),
                    motor.direction_sign,
                )
            })
            .collect()
    }

    fn fixture_robot() -> Robot {
        let bundle_root = fixture_bundle_root();
        match Robot::read_from_dir(&bundle_root) {
            Ok(robot) => robot,
            Err(error) => panic!(
                "failed to read fixture robot from {}: {error:#}",
                bundle_root.display()
            ),
        }
    }

    fn fixture_bundle_root() -> PathBuf {
        let manifest_dir = match std::env::var("CARGO_MANIFEST_DIR") {
            Ok(value) => PathBuf::from(value),
            Err(error) => panic!("CARGO_MANIFEST_DIR is not set: {error}"),
        };
        let workspace_root = match manifest_dir.parent().and_then(|path| path.parent()) {
            Some(path) => path,
            None => panic!(
                "runtime/drive CARGO_MANIFEST_DIR must live two levels below the workspace root: {}",
                manifest_dir.display()
            ),
        };

        workspace_root
            .join("fixture")
            .join("robot")
            .join("rgbd-imu-diff-drive")
    }
}
