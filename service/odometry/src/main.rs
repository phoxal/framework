//! `odometry` - the official differential-drive wheel-odometry participant.
//!
//! This is the forward-kinematics counterpart to `drive`: it subscribes to each
//! wheel encoder on dynamic per-component topics, reconstructs the body twist,
//! integrates planar pose, and publishes `odometry/state`. It exercises the
//! SUBSCRIBE side of dynamic per-component topics on the new participant surface.
//!
//! Encoder bindings come from the robot model's differential kinematic config
//! (per-side encoder lists, wheel radius, wheel base); a non-differential model
//! or non-positive geometry is rejected at setup. Each side's body twist averages
//! only the wheels with a fresh sample, so a silent encoder reads as stationary
//! rather than integrating a frozen velocity (see [`ENCODER_STALE_NS`]).

use std::f64::consts::PI;

use anyhow::{Result, bail};
use phoxal::model::component::v0::CapabilityRef;
use phoxal::model::robot::v0::KinematicConfig;
use phoxal::model::v0::Robot;
use phoxal::prelude::*;
use phoxal_api::y2026_1 as api;

/// A wheel whose last encoder sample is older than this is dropped from the twist
/// estimate, so a dead encoder publisher stops contributing motion (and the pose
/// stops drifting) rather than integrating a frozen velocity forever. The drivers
/// publish encoder samples well above this rate (`ddsm115` at 100 Hz), so this only
/// trips on a genuinely silent encoder. Mirrors `drive`'s stale-target guard.
const ENCODER_STALE_NS: u64 = 200_000_000; // 0.2 s

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
        api::topic::new()
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
            bail!(
                "odometry supports differential kinematics, found {}",
                robot.manifest.robot.kinematic.variant_label()
            );
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
        })
    }
}

#[derive(phoxal::Service)]
#[phoxal(id = "odometry", api = y2026_1)]
struct Odometry {
    // Runtime-private typed state (not handles).
    config: OdometryConfig,
    x_m: f64,
    y_m: f64,
    yaw_rad: f64,
    left_velocity_radps: Vec<f64>,
    right_velocity_radps: Vec<f64>,
    // Production time (ns) of each wheel's last encoder sample; 0 = never seen.
    left_sample_ns: Vec<u64>,
    right_sample_ns: Vec<u64>,
    // Handles.
    left_encoders: Vec<Subscriber<api::component::encoder::Sample>>,
    right_encoders: Vec<Subscriber<api::component::encoder::Sample>>,
    state: Publisher<api::odometry::State>,
}

#[phoxal::behavior]
impl Odometry {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        // Owner opt-in (plan #00 L2): the runner-minted capability that the
        // owner (`internal`) topic builder requires.
        let cap = ctx.owner_capability();
        let config = OdometryConfig::from_robot(ctx.robot()?)?;

        let mut left_encoders = Vec::with_capacity(config.left.len());
        for binding in &config.left {
            left_encoders.push(ctx.subscribe(binding.topic()).subscriber().await?);
        }
        let mut right_encoders = Vec::with_capacity(config.right.len());
        for binding in &config.right {
            right_encoders.push(ctx.subscribe(binding.topic()).subscriber().await?);
        }
        let state = ctx
            .publisher(api::topic::internal::new(cap).odometry().state())
            .await?;

        Ok(Self {
            left_velocity_radps: vec![0.0; config.left.len()],
            right_velocity_radps: vec![0.0; config.right.len()],
            left_sample_ns: vec![0; config.left.len()],
            right_sample_ns: vec![0; config.right.len()],
            config,
            x_m: 0.0,
            y_m: 0.0,
            yaw_rad: 0.0,
            left_encoders,
            right_encoders,
            state,
        })
    }

    #[step(hz = 50)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        drain_encoders(
            &mut self.left_encoders,
            &self.config.left,
            &mut self.left_velocity_radps,
            &mut self.left_sample_ns,
        );
        drain_encoders(
            &mut self.right_encoders,
            &self.config.right,
            &mut self.right_velocity_radps,
            &mut self.right_sample_ns,
        );

        let now_ns = step.time().time_ns();
        let left_radps = average_side(&self.left_velocity_radps, &self.left_sample_ns, now_ns);
        let right_radps = average_side(&self.right_velocity_radps, &self.right_sample_ns, now_ns);
        let (linear_x_mps, angular_z_radps) = forward(
            left_radps,
            right_radps,
            self.config.wheel_radius_m,
            self.config.wheel_base_m,
        );

        let (x_m, y_m, yaw_rad) = integrate_pose(
            self.x_m,
            self.y_m,
            self.yaw_rad,
            linear_x_mps,
            angular_z_radps,
            step.dt().as_secs_f64(),
        );
        self.x_m = x_m;
        self.y_m = y_m;
        self.yaw_rad = yaw_rad;

        self.state
            .publish_at(
                step.time(),
                api::odometry::State {
                    x_m: self.x_m,
                    y_m: self.y_m,
                    yaw_rad: self.yaw_rad,
                    linear_x_mps: linear_x_mps as f32,
                    angular_z_radps: angular_z_radps as f32,
                },
            )
            .await?;
        Ok(())
    }
}

fn drain_encoders(
    subscribers: &mut [Subscriber<api::component::encoder::Sample>],
    bindings: &[EncoderBinding],
    velocities: &mut [f64],
    sample_ns: &mut [u64],
) {
    for (((subscriber, binding), velocity), seen_ns) in subscribers
        .iter_mut()
        .zip(bindings)
        .zip(velocities.iter_mut())
        .zip(sample_ns.iter_mut())
    {
        while let Some(sample) = subscriber.try_recv() {
            *velocity = f64::from(sample.body.velocity_radps) * f64::from(binding.direction_sign);
            *seen_ns = sample.metadata.produced_at_ns;
        }
    }
}

/// Mean angular velocity of the wheels on one side, counting only wheels with a
/// fresh sample (seen at least once and not older than [`ENCODER_STALE_NS`]). A
/// side with no fresh wheel reads as stationary, so a dead encoder cannot keep
/// the pose drifting on a frozen velocity.
fn average_side(velocities: &[f64], sample_ns: &[u64], now_ns: u64) -> f64 {
    let mut sum = 0.0;
    let mut fresh = 0u32;
    for (velocity, seen_ns) in velocities.iter().zip(sample_ns) {
        if *seen_ns != 0 && now_ns.saturating_sub(*seen_ns) <= ENCODER_STALE_NS {
            sum += *velocity;
            fresh += 1;
        }
    }
    if fresh == 0 {
        0.0
    } else {
        sum / f64::from(fresh)
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

fn main() -> phoxal::Result<()> {
    phoxal::run::<Odometry>()
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;
    use std::path::PathBuf;

    use phoxal_api::ContractBody;
    use phoxal_api::y2026_1 as api;

    use super::{
        ENCODER_STALE_NS, OdometryConfig, average_side, forward, integrate_pose, normalize_yaw,
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
        let now_ns = 10 * ENCODER_STALE_NS;
        // No wheel ever sampled → stationary.
        assert_close(average_side(&[3.0, 5.0], &[0, 0], now_ns), 0.0);
        // Both fresh → plain mean.
        let fresh = now_ns - 1;
        assert_close(average_side(&[3.0, 5.0], &[fresh, fresh], now_ns), 4.0);
        // One wheel went silent (stale) → average over the fresh wheel only.
        let stale = now_ns - ENCODER_STALE_NS - 1;
        assert_close(average_side(&[3.0, 5.0], &[fresh, stale], now_ns), 3.0);
        // Both stale → treated as stationary (no drift on a dead encoder).
        assert_close(average_side(&[3.0, 5.0], &[stale, stale], now_ns), 0.0);
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
            assert!(topic.key().starts_with("component/"));
            assert!(topic.key().ends_with("/sample"));
        }
    }

    #[test]
    fn emit_apis_reports_contracts() {
        let json = phoxal::participant::emit_apis_json::<super::Odometry>();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["artifact"]["id"], "odometry");
        let contracts = value["required_contracts"].as_array().unwrap();
        assert!(
            contracts
                .iter()
                .any(|c| c["family"] == <api::component::encoder::Sample as ContractBody>::FAMILY)
        );
        assert!(
            contracts
                .iter()
                .any(|c| c["family"] == <api::odometry::State as ContractBody>::FAMILY)
        );
    }
}
