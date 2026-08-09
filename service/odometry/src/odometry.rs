//! `odometry` - the official differential-drive wheel-odometry participant.
//!
//! This is the forward-kinematics counterpart to `drive`: it subscribes to each
//! wheel encoder on dynamic per-component topics, reconstructs the body twist,
//! integrates planar pose, and publishes `odometry/state`.
//!
//! Encoder bindings come from the robot model's differential kinematic config
//! (per-side encoder lists, wheel radius, wheel base). A non-differential model
//! is explicitly inactive; invalid required differential bindings still fail
//! setup. Missing or stale samples produce waiting state and no invented pose.

use anyhow::{Result, bail};
use phoxal::api;
use phoxal::geometry::normalize_angle;
use phoxal::model::Robot;
use phoxal::model::identity::CapabilityRef;
use phoxal::model::robot::{DifferentialDrive, DifferentialWheelSpeeds, KinematicConfig};
use phoxal::prelude::*;

/// One encoder binding resolved from the robot model.
struct EncoderBinding {
    reference: CapabilityRef,
    direction_sign: i8,
}

impl EncoderBinding {
    fn resolve(robot: &Robot, references: &[CapabilityRef], field: &str) -> Result<Vec<Self>> {
        if references.is_empty() {
            bail!("robot.kinematic.{field} must list at least one encoder");
        }
        references
            .iter()
            .map(|reference| {
                let (_encoder, direction_sign) = robot.require_encoder(reference)?;
                Ok(EncoderBinding {
                    reference: reference.clone(),
                    direction_sign,
                })
            })
            .collect()
    }

    /// The dynamic per-instance encoder-sample topic for this binding. Odometry
    /// CONSUMES encoder samples (the encoder driver owns/publishes them), so this
    /// is the client `Subscribe` side from the public builder.
    fn topic(
        &self,
    ) -> Result<
        phoxal::bus::Topic<
            phoxal::bus::Subscribe<api::endpoint::component::encoder::SampleEndpoint>,
        >,
    > {
        Ok(api::topic::client()
            .component(&self.reference.component_id)?
            .encoder(&self.reference.capability_id)?
            .sample())
    }
}

/// One wheel encoder the service reads: its model binding and the subscriber
/// that carries its samples, so a sample can never be read with another
/// wheel's direction sign.
struct BoundEncoder {
    binding: EncoderBinding,
    subscriber: SampleReceiver<api::endpoint::component::encoder::SampleEndpoint>,
}

/// Typed odometry config built from the robot model.
struct OdometryConfig {
    kinematics: Option<DifferentialDrive>,
    left: Vec<EncoderBinding>,
    right: Vec<EncoderBinding>,
}

impl OdometryConfig {
    fn from_robot(robot: &Robot) -> Result<Self> {
        let KinematicConfig::Differential {
            left_encoders,
            right_encoders,
            wheel_radius_m,
            wheel_base_m,
            ..
        } = robot.motion().kinematic()
        else {
            return Ok(Self {
                kinematics: None,
                left: Vec::new(),
                right: Vec::new(),
            });
        };
        Ok(OdometryConfig {
            kinematics: Some(DifferentialDrive::new(*wheel_radius_m, *wheel_base_m).validate()?),
            left: EncoderBinding::resolve(robot, left_encoders, "left_encoders")?,
            right: EncoderBinding::resolve(robot, right_encoders, "right_encoders")?,
        })
    }
}

/// The integrated planar pose, in the odometry frame.
#[derive(Clone, Copy, Default)]
struct Pose {
    x_m: f64,
    y_m: f64,
    yaw_rad: f64,
}

impl Pose {
    /// Advance the pose by `dt_s` of the given body twist.
    ///
    /// Both translation components are taken at the heading held *entering* the
    /// interval, so the heading update comes last.
    fn integrate(&mut self, linear_x_mps: f64, angular_z_radps: f64, dt_s: f64) {
        self.x_m += linear_x_mps * dt_s * self.yaw_rad.cos();
        self.y_m += linear_x_mps * dt_s * self.yaw_rad.sin();
        self.yaw_rad = normalize_angle(self.yaw_rad + angular_z_radps * dt_s);
    }
}

pub(crate) struct Api {
    left: Vec<BoundEncoder>,
    right: Vec<BoundEncoder>,
    state: StatePublisher<api::odometry::State>,
}

pub(crate) struct OdometryState {
    /// `None` when the robot is not a differential drive: the service then
    /// publishes nothing rather than inventing a pose.
    kinematics: Option<DifferentialDrive>,
    pose: Pose,
    /// The newest reading from each wheel on a side, positionally aligned with
    /// that side's [`BoundEncoder`]s - both are built from the same binding
    /// list, in order, and neither grows or shrinks afterwards.
    left: Vec<Option<Timed<f64>>>,
    right: Vec<Option<Timed<f64>>>,
}

impl OdometryState {
    /// Take every encoder sample that has arrived, keeping the newest per wheel.
    ///
    /// A sample whose producer cannot name an exact production instant leaves
    /// the wheel with no reading at all: a velocity that cannot be aged must
    /// not be integrated.
    fn drain(&mut self, api: &Api) {
        for (encoders, wheels) in [(&api.left, &mut self.left), (&api.right, &mut self.right)] {
            for (encoder, wheel) in encoders.iter().zip(wheels.iter_mut()) {
                while let Some(sample) = encoder.subscriber.try_recv() {
                    let velocity = f64::from(sample.body.velocity_radps)
                        * f64::from(encoder.binding.direction_sign);
                    *wheel = sample
                        .metadata
                        .produced_exactly_at()
                        .map(|at| Timed::new(velocity, at));
                }
            }
        }
    }

    /// The body twist the wheels report at `now`, or `None` when the robot is
    /// not a differential drive or either side has no fresh wheel.
    fn twist(&self, now: RobotInstant) -> Option<(f64, f64)> {
        let kinematics = self.kinematics?;
        let left_radps = Self::side_average(&self.left, now)?;
        let right_radps = Self::side_average(&self.right, now)?;
        let twist = kinematics.body_twist(DifferentialWheelSpeeds {
            left_radps,
            right_radps,
        });
        Some((twist.linear_x_mps, twist.angular_z_radps))
    }

    /// Mean angular velocity of the wheels on one side, counting only wheels
    /// with a finite reading no older than the encoder contract's staleness
    /// horizon. A side with no fresh wheel has no reading at all, so a dead
    /// encoder cannot keep the pose drifting on a frozen velocity.
    fn side_average(wheels: &[Option<Timed<f64>>], now: RobotInstant) -> Option<f64> {
        let mut sum = 0.0;
        let mut fresh = 0u32;
        for wheel in wheels.iter().flatten() {
            if wheel.fresh_within(now, api::component::encoder::Sample::STALE_AFTER)
                && wheel.body.is_finite()
            {
                sum += wheel.body;
                fresh += 1;
            }
        }
        if fresh == 0 {
            None
        } else {
            Some(sum / f64::from(fresh))
        }
    }
}

#[phoxal::service(state = OdometryState, api = Api)]
pub(crate) struct Odometry;

impl Participant for Odometry {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let config = OdometryConfig::from_robot(ctx.robot()?)?;

        let mut left = Vec::with_capacity(config.left.len());
        for binding in config.left {
            let subscriber = ctx.sample_receiver(binding.topic()?).await?;
            left.push(BoundEncoder {
                binding,
                subscriber,
            });
        }
        let mut right = Vec::with_capacity(config.right.len());
        for binding in config.right {
            let subscriber = ctx.sample_receiver(binding.topic()?).await?;
            right.push(BoundEncoder {
                binding,
                subscriber,
            });
        }
        let state = ctx.state_publisher(api::topic::owner().odometry().state())?;

        Ok((
            OdometryState {
                kinematics: config.kinematics,
                pose: Pose::default(),
                left: vec![None; left.len()],
                right: vec![None; right.len()],
            },
            Api { left, right, state },
        ))
    }

    fn reset(&self, _ctx: ResetContext, _api: &Self::Api, state: &mut Self::State) -> Result<()> {
        state.pose = Pose::default();
        state.left.fill(None);
        state.right.fill(None);
        Ok(())
    }

    #[phoxal::step(hz = 50)]
    fn step(&self, api: &Self::Api, step: StepContext, state: &mut Self::State) -> Result<()> {
        state.drain(api);

        let Some((linear_x_mps, angular_z_radps)) = state.twist(step.now()) else {
            return Ok(());
        };
        state
            .pose
            .integrate(linear_x_mps, angular_z_radps, step.dt.as_secs_f64());

        api.state.publish(
            &step.token,
            api::odometry::State {
                x_m: state.pose.x_m,
                y_m: state.pose.y_m,
                yaw_rad: state.pose.yaw_rad,
                linear_x_mps: linear_x_mps as f32,
                angular_z_radps: angular_z_radps as f32,
            },
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use phoxal::api;
    use phoxal::bus::{RobotInstant, Timed, TimelineId};
    use phoxal::model::RobotBuilder;
    use phoxal::model::builder::Kinematics;

    use super::{DifferentialDrive, DifferentialWheelSpeeds, OdometryConfig, OdometryState, Pose};

    const KINEMATICS: DifferentialDrive = DifferentialDrive::new(0.1, 0.4);

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn forward_kinematics_reconstructs_body_twist() {
        let twist = KINEMATICS.body_twist(DifferentialWheelSpeeds {
            left_radps: 10.0,
            right_radps: 10.0,
        });
        let (linear_x, angular_z) = (twist.linear_x_mps, twist.angular_z_radps);
        assert_close(linear_x, 1.0);
        assert_close(angular_z, 0.0);

        let twist = KINEMATICS.body_twist(DifferentialWheelSpeeds {
            left_radps: -2.0,
            right_radps: 2.0,
        });
        let (linear_x, angular_z) = (twist.linear_x_mps, twist.angular_z_radps);
        assert_close(linear_x, 0.0);
        assert_close(angular_z, 1.0);
    }

    #[test]
    fn pose_integration_advances_forward_twist() {
        let mut pose = Pose::default();
        for _ in 0..50 {
            pose.integrate(0.5, 0.0, 0.02);
        }

        assert_close(pose.x_m, 0.5);
        assert_close(pose.y_m, 0.0);
        assert_close(pose.yaw_rad, 0.0);
    }

    #[test]
    fn side_average_counts_only_fresh_wheels() {
        let line = TimelineId::mint();
        let stale_ns =
            u64::try_from(api::component::encoder::Sample::STALE_AFTER.as_nanos()).unwrap();
        let now_ns = 10 * stale_ns;
        let now = RobotInstant::new(line, now_ns);
        let wheel = |velocity, at| Some(Timed::new(velocity, at));
        let fresh_at = RobotInstant::new(line, now_ns - 1);
        let stale_at = RobotInstant::new(line, now_ns - stale_ns - 1);
        let average = |wheels: &[Option<Timed<f64>>]| OdometryState::side_average(wheels, now);

        // No wheel ever sampled -> stationary.
        assert_eq!(average(&[None, None]), None);
        // Both fresh -> plain mean.
        assert_close(
            average(&[wheel(3.0, fresh_at), wheel(5.0, fresh_at)]).unwrap(),
            4.0,
        );
        // One wheel went silent (stale) -> average over the fresh wheel only.
        assert_close(
            average(&[wheel(3.0, fresh_at), wheel(5.0, stale_at)]).unwrap(),
            3.0,
        );
        // Both stale -> treated as stationary (no drift on a dead encoder).
        assert_eq!(average(&[wheel(3.0, stale_at), wheel(5.0, stale_at)]), None);
        assert_eq!(
            average(&[wheel(f64::NAN, fresh_at), wheel(5.0, fresh_at)]),
            Some(5.0)
        );
        assert_eq!(average(&[wheel(f64::INFINITY, fresh_at)]), None);
        // Samples from this step's future, and from a replaced world, must not
        // resurrect after a reset.
        assert_eq!(
            average(&[
                wheel(3.0, RobotInstant::new(line, now_ns + 1)),
                wheel(5.0, RobotInstant::new(TimelineId::mint(), now_ns)),
            ]),
            None
        );
    }

    #[test]
    fn config_from_robot_resolves_per_side_encoders() {
        // A 4-wheel differential: 2 encoders per side, and the per-side lists
        // are what the config has to split on.
        let robot = RobotBuilder::new("rover")
            .component_type("drive_motor", |motor| {
                motor
                    .motor("motor", "motor_joint")
                    .encoder("encoder", "motor_joint")
            })
            .component("front_left_drive", "drive_motor")
            .component("front_right_drive", "drive_motor")
            .component("rear_left_drive", "drive_motor")
            .component("rear_right_drive", "drive_motor")
            .kinematics(Kinematics::Differential {
                left_actuators: &["front_left_drive.motor", "rear_left_drive.motor"],
                right_actuators: &["front_right_drive.motor", "rear_right_drive.motor"],
                left_encoders: &["front_left_drive.encoder", "rear_left_drive.encoder"],
                right_encoders: &["front_right_drive.encoder", "rear_right_drive.encoder"],
                wheel_radius_m: 0.1,
                wheel_base_m: 0.4,
            })
            .build()
            .expect("a valid robot");

        let config = OdometryConfig::from_robot(&robot).unwrap();

        assert_eq!(config.left.len(), 2);
        assert_eq!(config.right.len(), 2);
        assert_eq!(config.kinematics.unwrap().wheel_radius_m, 0.1);

        for binding in config.left.iter().chain(&config.right) {
            let topic = binding.topic().unwrap();
            assert!(topic.key().starts_with("v0.2/component/"));
            assert!(topic.key().ends_with("/sample"));
        }
    }
}
