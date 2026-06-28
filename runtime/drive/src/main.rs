//! `drive` — the official differential-drive runtime.
//!
//! A scheduled official runtime that closes the body-twist → wheel-velocity loop:
//! it reads `drive/target`, limits + mixes it into per-wheel angular speeds, and
//! commands each motor on its dynamic per-component topic. It exercises the full
//! Phase 4 surface — `ctx.robot()` (D33: build typed state from the model),
//! dynamic per-instance topic builders (D17/D38), `#[step]`, and `#[shutdown]`
//! (park the motors).

use anyhow::{Result, bail};
use phoxal::api::y2026_1 as api;
use phoxal::model::component::v1::CapabilityRef;
use phoxal::model::robot::v1::KinematicConfig;
use phoxal::model::v1::Robot;
use phoxal::prelude::*;

const MAX_LINEAR_MPS: f64 = 0.6;
const MAX_ANGULAR_RADPS: f64 = 2.0;
const TARGET_STALE_NS: u64 = 500_000_000; // 0.5 s

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
            bail!("motion.kinematic.{field} must list at least one actuator");
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

    /// The dynamic per-instance motor-command topic for this binding.
    fn topic(&self) -> phoxal::bus::Topic<phoxal::bus::PubSub<api::component::MotorCommand>> {
        api::topic::new()
            .component()
            .motor_command(&self.component_id, &self.capability_id)
    }
}

/// Typed drive config built from the robot model (D33).
struct DriveConfig {
    kinematics: DifferentialDrive,
    left: Vec<MotorBinding>,
    right: Vec<MotorBinding>,
}

impl DriveConfig {
    fn from_robot(robot: &Robot) -> Result<Self> {
        let KinematicConfig::Differential {
            left_actuators,
            right_actuators,
            wheel_radius_m,
            wheel_base_m,
            ..
        } = &robot.manifest.motion.kinematic
        else {
            bail!(
                "drive supports differential kinematics, found {}",
                robot.manifest.motion.kinematic.variant_label()
            );
        };
        if !(wheel_radius_m.is_finite() && *wheel_radius_m > 0.0) {
            bail!("wheel_radius_m must be finite and > 0");
        }
        if !(wheel_base_m.is_finite() && *wheel_base_m > 0.0) {
            bail!("wheel_base_m must be finite and > 0");
        }
        Ok(DriveConfig {
            kinematics: DifferentialDrive {
                wheel_radius_m: *wheel_radius_m,
                wheel_base_m: *wheel_base_m,
            },
            left: MotorBinding::resolve(robot, left_actuators, "left_actuators")?,
            right: MotorBinding::resolve(robot, right_actuators, "right_actuators")?,
        })
    }
}

#[derive(phoxal::Runtime)]
#[phoxal(id = "drive", api = y2026_1)]
struct Drive {
    // Runtime-private typed state (not handles).
    config: DriveConfig,
    last_target: Option<(api::drive::Target, u64)>,
    // Handles.
    target: Subscriber<api::drive::Target>,
    state: Publisher<api::drive::State>,
    left_motors: Vec<Publisher<api::component::MotorCommand>>,
    right_motors: Vec<Publisher<api::component::MotorCommand>>,
}

#[phoxal::runtime]
impl Drive {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        let config = DriveConfig::from_robot(ctx.robot()?)?;

        let target = ctx
            .subscribe(api::topic::new().drive().target())
            .subscriber()
            .await?;
        let state = ctx.publisher(api::topic::new().drive().state()).await?;

        let mut left_motors = Vec::with_capacity(config.left.len());
        for binding in &config.left {
            left_motors.push(ctx.publisher(binding.topic()).await?);
        }
        let mut right_motors = Vec::with_capacity(config.right.len());
        for binding in &config.right {
            right_motors.push(ctx.publisher(binding.topic()).await?);
        }

        Ok(Self {
            config,
            last_target: None,
            target,
            state,
            left_motors,
            right_motors,
        })
    }

    #[step(hz = 50)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        let now = step.time();

        // Drain inbound targets, keeping the latest + its production time.
        while let Some(received) = self.target.try_recv() {
            self.last_target = Some((received.body, received.metadata.produced_at_ns));
        }

        let (target, authority, stop_reason) = self.resolve(now.time_ns());
        let (left, right) = self.config.kinematics.invert(
            f64::from(target.linear_x_mps),
            f64::from(target.angular_z_radps),
        );

        for (publisher, binding) in self.left_motors.iter().zip(&self.config.left) {
            publisher
                .publish_at(now, command(left, binding.direction_sign))
                .await?;
        }
        for (publisher, binding) in self.right_motors.iter().zip(&self.config.right) {
            publisher
                .publish_at(now, command(right, binding.direction_sign))
                .await?;
        }

        self.state
            .publish_at(
                now,
                api::drive::State {
                    target: target.clone(),
                    limited_target: target,
                    actuator_authority: authority,
                    stop_reason,
                },
            )
            .await?;
        Ok(())
    }

    #[shutdown]
    async fn shutdown(&mut self, _ctx: ShutdownContext) -> Result<()> {
        // Best-effort park: command every wheel to stop before the bus closes.
        let now = LogicalTime::new(0, 0);
        for publisher in self.left_motors.iter().chain(&self.right_motors) {
            let _ = publisher
                .publish_at(now, api::component::MotorCommand::Stop)
                .await;
        }
        Ok(())
    }
}

impl Drive {
    /// Resolve the effective (limited) target, stopping on no/stale command.
    fn resolve(
        &self,
        now_ns: u64,
    ) -> (
        api::drive::Target,
        api::drive::ActuatorAuthority,
        Option<api::drive::StopReason>,
    ) {
        let stopped = api::drive::Target {
            linear_x_mps: 0.0,
            angular_z_radps: 0.0,
        };
        let Some((target, produced_at_ns)) = &self.last_target else {
            return (
                stopped,
                api::drive::ActuatorAuthority::Stopped,
                Some(api::drive::StopReason::NoTarget),
            );
        };
        if now_ns.saturating_sub(*produced_at_ns) > TARGET_STALE_NS {
            return (
                stopped,
                api::drive::ActuatorAuthority::Stopped,
                Some(api::drive::StopReason::Fault),
            );
        }
        let limited = api::drive::Target {
            linear_x_mps: clamp_f32(target.linear_x_mps, MAX_LINEAR_MPS),
            angular_z_radps: clamp_f32(target.angular_z_radps, MAX_ANGULAR_RADPS),
        };
        (limited, api::drive::ActuatorAuthority::Active, None)
    }
}

fn command(wheel_radps: f64, direction_sign: i8) -> api::component::MotorCommand {
    api::component::MotorCommand::Velocity((wheel_radps * f64::from(direction_sign)) as f32)
}

fn clamp_f32(value: f32, limit: f64) -> f32 {
    value.clamp(-limit as f32, limit as f32)
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<Drive>()
}

#[cfg(test)]
mod tests {
    use super::{DifferentialDrive, DriveConfig};
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
    fn config_from_robot_resolves_per_side_motors() {
        let robot = phoxal::model::v1::Robot::read_from_dir(fixture()).unwrap();
        let config = DriveConfig::from_robot(&robot).unwrap();
        // The fixture is a 4-wheel differential: 2 motors per side.
        assert_eq!(config.left.len(), 2);
        assert_eq!(config.right.len(), 2);
        assert!(config.kinematics.wheel_radius_m > 0.0);
        // Each binding resolves to a concrete dynamic motor topic.
        let topic = config.left[0].topic();
        assert!(topic.key().starts_with("component/"));
        assert!(topic.key().ends_with("/command"));
    }
}
