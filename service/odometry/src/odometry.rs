//! `odometry` - the official differential-drive wheel-odometry participant.
//!
//! This is the forward-kinematics counterpart to `drive`: it subscribes to each
//! wheel encoder on dynamic per-component topics, reconstructs the body twist,
//! integrates planar pose, and publishes `odometry/state`. It exercises the
//! SUBSCRIBE side of dynamic per-component topics on the new participant surface.
//!
//! Encoder bindings come from the robot model's differential kinematic config
//! (per-side encoder lists, wheel radius, wheel base). A non-differential model
//! is explicitly inactive; invalid required differential bindings still fail
//! setup. Missing or stale samples produce waiting state and no invented pose.

use std::f64::consts::PI;

use anyhow::{Result, bail};
use phoxal::api;
use phoxal::model::component::v0::CapabilityRef;
use phoxal::model::robot::v0::KinematicConfig;
use phoxal::model::v0::Robot;
use phoxal::prelude::*;

/// A wheel whose last encoder sample is older than this is dropped from the twist
/// estimate, so a dead encoder publisher stops contributing motion (and the pose
/// stops drifting) rather than integrating a frozen velocity forever. The drivers
/// publish encoder samples well above this rate (`ddsm115` at 100 Hz), so this only
/// trips on a genuinely silent encoder. Mirrors `drive`'s stale-target guard.
const ENCODER_STALE: std::time::Duration = std::time::Duration::from_millis(200);

/// One encoder binding resolved from the robot model.
#[derive(Clone)]
struct EncoderBinding {
    component_id: String,
    capability_id: String,
    direction_sign: i8,
}

impl EncoderBinding {
    fn resolve(robot: &Robot, refs: &[CapabilityRef], field: &str) -> Result<Vec<Self>> {
        if refs.is_empty() {
            bail!("robot.kinematic.{field} must list at least one encoder");
        }
        refs.iter()
            .map(|r| {
                let (_encoder, direction_sign) = robot.require_encoder(r)?;
                Ok(EncoderBinding {
                    component_id: r.component_id.clone(),
                    capability_id: r.capability_id.clone(),
                    direction_sign,
                })
            })
            .collect()
    }

    /// The dynamic per-instance encoder-sample topic for this binding. Odometry
    /// CONSUMES encoder samples (the encoder driver owns/publishes them), so this
    /// is the client `Subscribe` side from the public builder.
    fn topic(&self) -> phoxal::bus::Topic<phoxal::bus::Subscribe<api::component::encoder::Sample>> {
        api::topic::client()
            .component(&self.component_id)
            .encoder(&self.capability_id)
            .sample()
    }
}

/// Typed odometry config built from the robot model.
struct OdometryConfig {
    wheel_radius_m: f64,
    wheel_base_m: f64,
    left: Vec<EncoderBinding>,
    right: Vec<EncoderBinding>,
    active: bool,
}

impl OdometryConfig {
    fn from_robot(robot: &Robot) -> Result<Self> {
        let KinematicConfig::Differential {
            left_encoders,
            right_encoders,
            wheel_radius_m,
            wheel_base_m,
            ..
        } = &robot.manifest.robot.kinematic
        else {
            return Ok(Self {
                wheel_radius_m: 0.0,
                wheel_base_m: 0.0,
                left: Vec::new(),
                right: Vec::new(),
                active: false,
            });
        };
        if !(wheel_radius_m.is_finite() && *wheel_radius_m > 0.0) {
            bail!("wheel_radius_m must be finite and > 0");
        }
        if !(wheel_base_m.is_finite() && *wheel_base_m > 0.0) {
            bail!("wheel_base_m must be finite and > 0");
        }
        Ok(OdometryConfig {
            wheel_radius_m: *wheel_radius_m,
            wheel_base_m: *wheel_base_m,
            left: EncoderBinding::resolve(robot, left_encoders, "left_encoders")?,
            right: EncoderBinding::resolve(robot, right_encoders, "right_encoders")?,
            active: true,
        })
    }
}

pub struct Api {
    left_encoders: Vec<Subscriber<api::component::encoder::Sample>>,
    right_encoders: Vec<Subscriber<api::component::encoder::Sample>>,
    state: StatePublisher<api::odometry::State>,
}

pub struct OdometryState {
    // Runtime-private typed state (not handles).
    config: OdometryConfig,
    x_m: f64,
    y_m: f64,
    yaw_rad: f64,
    left_velocity_radps: Vec<f64>,
    right_velocity_radps: Vec<f64>,
    left_sample_at: Vec<Option<RobotInstant>>,
    right_sample_at: Vec<Option<RobotInstant>>,
}

#[phoxal::service(state = OdometryState, api = Api)]
pub struct Odometry;

impl Participant for Odometry {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let config = OdometryConfig::from_robot(ctx.robot()?)?;

        let mut left_encoders = Vec::with_capacity(config.left.len());
        for binding in &config.left {
            left_encoders.push(ctx.subscriber(binding.topic(), 32).await?);
        }
        let mut right_encoders = Vec::with_capacity(config.right.len());
        for binding in &config.right {
            right_encoders.push(ctx.subscriber(binding.topic(), 32).await?);
        }
        let state = ctx
            .state_publisher(api::topic::owner().odometry().state())
            .await?;

        Ok((
            OdometryState {
                left_velocity_radps: vec![0.0; config.left.len()],
                right_velocity_radps: vec![0.0; config.right.len()],
                left_sample_at: vec![None; config.left.len()],
                right_sample_at: vec![None; config.right.len()],
                config,
                x_m: 0.0,
                y_m: 0.0,
                yaw_rad: 0.0,
            },
            Api {
                left_encoders,
                right_encoders,
                state,
            },
        ))
    }

    async fn reset(
        &self,
        _ctx: ResetContext,
        _api: &Self::Api,
        state: &mut Self::State,
    ) -> Result<()> {
        state.x_m = 0.0;
        state.y_m = 0.0;
        state.yaw_rad = 0.0;
        state.left_velocity_radps.fill(0.0);
        state.right_velocity_radps.fill(0.0);
        state.left_sample_at.fill(None);
        state.right_sample_at.fill(None);
        Ok(())
    }

    #[phoxal::step(hz = 50)]
    async fn step(
        &self,
        api: &Self::Api,
        step: StepContext,
        state: &mut Self::State,
    ) -> Result<()> {
        if !state.config.active {
            return Ok(());
        }
        drain_encoders(
            &api.left_encoders,
            &state.config.left,
            &mut state.left_velocity_radps,
            &mut state.left_sample_at,
        );
        drain_encoders(
            &api.right_encoders,
            &state.config.right,
            &mut state.right_velocity_radps,
            &mut state.right_sample_at,
        );

        let now = step.now();
        let left_radps = average_side(&state.left_velocity_radps, &state.left_sample_at, now);
        let right_radps = average_side(&state.right_velocity_radps, &state.right_sample_at, now);
        let (Some(left_radps), Some(right_radps)) = (left_radps, right_radps) else {
            return Ok(());
        };
        let (linear_x_mps, angular_z_radps) = forward(
            left_radps,
            right_radps,
            state.config.wheel_radius_m,
            state.config.wheel_base_m,
        );

        let (x_m, y_m, yaw_rad) = integrate_pose(
            state.x_m,
            state.y_m,
            state.yaw_rad,
            linear_x_mps,
            angular_z_radps,
            step.dt().as_secs_f64(),
        );
        state.x_m = x_m;
        state.y_m = y_m;
        state.yaw_rad = yaw_rad;

        api.state.publish(
            step.token(),
            api::odometry::State {
                x_m: state.x_m,
                y_m: state.y_m,
                yaw_rad: state.yaw_rad,
                linear_x_mps: linear_x_mps as f32,
                angular_z_radps: angular_z_radps as f32,
            },
        )?;
        Ok(())
    }
}

fn drain_encoders(
    subscribers: &[Subscriber<api::component::encoder::Sample>],
    bindings: &[EncoderBinding],
    velocities: &mut [f64],
    sample_at: &mut [Option<RobotInstant>],
) {
    for (((subscriber, binding), velocity), seen_at) in subscribers
        .iter()
        .zip(bindings)
        .zip(velocities.iter_mut())
        .zip(sample_at.iter_mut())
    {
        while let Some(sample) = subscriber.try_recv() {
            *velocity = f64::from(sample.body.velocity_radps) * f64::from(binding.direction_sign);
            *seen_at = sample.metadata.produced_exactly_at();
        }
    }
}

/// Mean angular velocity of the wheels on one side, counting only wheels with a
/// fresh sample (seen at least once and not older than [`ENCODER_STALE`]). A
/// side with no fresh wheel reads as stationary, so a dead encoder cannot keep
/// the pose drifting on a frozen velocity.
fn average_side(
    velocities: &[f64],
    sample_at: &[Option<RobotInstant>],
    now: RobotInstant,
) -> Option<f64> {
    let mut sum = 0.0;
    let mut fresh = 0u32;
    for (velocity, seen_at) in velocities.iter().zip(sample_at) {
        if seen_at.is_some_and(|at| {
            TimeWindow::exact(at)
                .possibly_fresh_within(now, ENCODER_STALE)
                .unwrap_or(false)
        }) && velocity.is_finite()
        {
            sum += *velocity;
            fresh += 1;
        }
    }
    if fresh == 0 {
        None
    } else {
        Some(sum / f64::from(fresh))
    }
}

/// Differential-drive forward kinematics: wheel angular speeds → body twist.
fn forward(v_left_radps: f64, v_right_radps: f64, radius: f64, base: f64) -> (f64, f64) {
    let v_left = v_left_radps * radius;
    let v_right = v_right_radps * radius;
    ((v_left + v_right) / 2.0, (v_right - v_left) / base)
}

fn integrate_pose(
    x_m: f64,
    y_m: f64,
    yaw_rad: f64,
    linear_x_mps: f64,
    angular_z_radps: f64,
    dt_s: f64,
) -> (f64, f64, f64) {
    (
        x_m + linear_x_mps * dt_s * yaw_rad.cos(),
        y_m + linear_x_mps * dt_s * yaw_rad.sin(),
        normalize_yaw(yaw_rad + angular_z_radps * dt_s),
    )
}

fn normalize_yaw(yaw_rad: f64) -> f64 {
    let two_pi = 2.0 * PI;
    let normalized = (yaw_rad + PI).rem_euclid(two_pi) - PI;
    if normalized <= -PI { PI } else { normalized }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;
    use std::path::PathBuf;

    use phoxal::bus::{RobotInstant, TimelineId};

    use super::{
        ENCODER_STALE, OdometryConfig, average_side, forward, integrate_pose, normalize_yaw,
    };

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixture/robot/rgbd-imu-diff-drive")
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn forward_kinematics_reconstructs_body_twist() {
        let (linear_x, angular_z) = forward(10.0, 10.0, 0.1, 0.4);
        assert_close(linear_x, 1.0);
        assert_close(angular_z, 0.0);

        let (linear_x, angular_z) = forward(-2.0, 2.0, 0.1, 0.4);
        assert_close(linear_x, 0.0);
        assert_close(angular_z, 1.0);
    }

    #[test]
    fn pose_integration_advances_forward_twist() {
        let mut x_m = 0.0;
        let mut y_m = 0.0;
        let mut yaw_rad = 0.0;
        for _ in 0..50 {
            (x_m, y_m, yaw_rad) = integrate_pose(x_m, y_m, yaw_rad, 0.5, 0.0, 0.02);
        }

        assert_close(x_m, 0.5);
        assert_close(y_m, 0.0);
        assert_close(yaw_rad, 0.0);
    }

    #[test]
    fn yaw_normalization_uses_negative_pi_exclusive_positive_pi_inclusive_range() {
        assert_close(normalize_yaw(PI), PI);
        assert_close(normalize_yaw(-PI), PI);
        assert_close(normalize_yaw(3.0 * PI), PI);
        assert_close(normalize_yaw(PI + 0.25), -PI + 0.25);
        assert_close(normalize_yaw(-PI - 0.25), PI - 0.25);
    }

    #[test]
    fn average_side_counts_only_fresh_wheels() {
        let line = TimelineId::mint();
        let stale_ns = u64::try_from(ENCODER_STALE.as_nanos()).unwrap();
        let now_ns = 10 * stale_ns;
        let now = RobotInstant::new(line, now_ns);
        // No wheel ever sampled → stationary.
        assert_eq!(average_side(&[3.0, 5.0], &[None, None], now), None);
        // Both fresh → plain mean.
        let fresh = Some(RobotInstant::new(line, now_ns - 1));
        assert_close(
            average_side(&[3.0, 5.0], &[fresh, fresh], now).unwrap(),
            4.0,
        );
        // One wheel went silent (stale) → average over the fresh wheel only.
        let stale = Some(RobotInstant::new(line, now_ns - stale_ns - 1));
        assert_close(
            average_side(&[3.0, 5.0], &[fresh, stale], now).unwrap(),
            3.0,
        );
        // Both stale → treated as stationary (no drift on a dead encoder).
        assert_eq!(average_side(&[3.0, 5.0], &[stale, stale], now), None);
        assert_eq!(
            average_side(&[f64::NAN, 5.0], &[fresh, fresh], now),
            Some(5.0)
        );
        assert_eq!(average_side(&[f64::INFINITY], &[fresh], now), None);
        // Samples from this step's future, and from a replaced world, must not
        // resurrect after a reset.
        assert_eq!(
            average_side(
                &[3.0, 5.0],
                &[
                    Some(RobotInstant::new(line, now_ns + 1)),
                    Some(RobotInstant::new(TimelineId::mint(), now_ns)),
                ],
                now,
            ),
            None
        );
    }

    #[test]
    fn config_from_robot_resolves_per_side_encoders() {
        let robot = phoxal::model::v0::Robot::read_from_dir(fixture()).unwrap();
        let config = OdometryConfig::from_robot(&robot).unwrap();
        // The fixture is a 4-wheel differential: 2 encoders per side.
        assert_eq!(config.left.len(), 2);
        assert_eq!(config.right.len(), 2);
        assert!(config.wheel_radius_m > 0.0);

        for binding in config.left.iter().chain(&config.right) {
            let topic = binding.topic();
            assert!(topic.key().starts_with("v0.1/component/"));
            assert!(topic.key().ends_with("/sample"));
        }
    }
}
