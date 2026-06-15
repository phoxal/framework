use std::time::Duration;

use anyhow::{Result, bail};
use phoxal::api::frame::v1::FrameId;
use phoxal::api::joint::{
    self as joint_contract,
    v1::{JointId, JointState, Quantity},
};
use phoxal::api::odometry::{
    self as odometry_contract,
    v1::{
        Covariance, Integration, IntegrationStep, OdometryEstimate, PoseEstimate, Residuals,
        SourceHealth, SourceId, SourceReason, SourceStatus, Status, StatusMode, StatusReason,
        VelocityEstimate,
    },
};
use phoxal::api::topic;
use phoxal::bus::typed::Received;
use phoxal::model::robot::v1::KinematicConfig;
use phoxal::model::v1::Robot;
use phoxal::runtime::clock::Step;
use phoxal::runtime::stale_timeout_ns;
use phoxal::runtime::{EmptyArgs, RobotRuntimeArgs};
use phoxal::runtime::{Io, Runtime, RuntimeInputs, TopicPublisher};
use tracing::warn;

const CLOCK_PERIOD: Duration = Duration::from_millis(20);
const ODOM_FRAME_ID: &str = "odom";
const BASE_FRAME_ID: &str = "base_footprint";
const VAR_PER_STEP_TRACKING_M2: f64 = 1.0e-6;
const VAR_PER_STEP_YAW_TRACKING_RAD2: f64 = 1.0e-7;
const VAR_PER_STEP_DEGRADED_M2: f64 = 1.0e-3;
const VAR_PER_STEP_YAW_DEGRADED_RAD2: f64 = 1.0e-4;

#[derive(Clone)]
pub struct Config {
    left_joint_id: JointId,
    right_joint_id: JointId,
    wheel_radius_m: f64,
    wheel_base_m: f64,
    stale_timeout_ns: u64,
    clock_period: Duration,
}

impl Config {
    pub fn from_robot(robot: &Robot) -> Result<Self> {
        let KinematicConfig::Differential {
            left_encoders,
            right_encoders,
            wheel_radius_m,
            wheel_base_m,
            ..
        } = &robot.manifest.motion.kinematic
        else {
            bail!(
                "odometry runtime only supports differential drive kinematics, found {}",
                robot.manifest.motion.kinematic.variant_label()
            );
        };

        validate_positive_f64(*wheel_radius_m, "wheel_radius_m")?;
        validate_positive_f64(*wheel_base_m, "wheel_base_m")?;

        let Some(left_encoder_ref) = left_encoders.first() else {
            bail!("motion.kinematic.left_encoders must list at least one encoder");
        };
        let Some(right_encoder_ref) = right_encoders.first() else {
            bail!("motion.kinematic.right_encoders must list at least one encoder");
        };

        let (left_encoder, _left_direction_sign) = robot.require_encoder(left_encoder_ref)?;
        let (right_encoder, _right_direction_sign) = robot.require_encoder(right_encoder_ref)?;
        validate_positive_f64(left_encoder.publish_rate_hz, "left publish_rate_hz")?;
        validate_positive_f64(right_encoder.publish_rate_hz, "right publish_rate_hz")?;

        let left_joint = robot.require_joint(left_encoder_ref)?;
        let right_joint = robot.require_joint(right_encoder_ref)?;
        let joint_publish_hz = left_encoder
            .publish_rate_hz
            .min(right_encoder.publish_rate_hz);

        Ok(Self {
            left_joint_id: JointId::new(&left_joint.name),
            right_joint_id: JointId::new(&right_joint.name),
            wheel_radius_m: *wheel_radius_m,
            wheel_base_m: *wheel_base_m,
            stale_timeout_ns: stale_timeout_ns(joint_publish_hz),
            clock_period: CLOCK_PERIOD,
        })
    }

    pub const fn clock_period(&self) -> Duration {
        self.clock_period
    }
}

fn validate_positive_f64(value: f64, field_name: &str) -> Result<()> {
    if !value.is_finite() || value <= 0.0 {
        bail!("{field_name} must be finite and > 0");
    }
    Ok(())
}

pub enum Input {
    Left(Received<JointState>),
    Right(Received<JointState>),
}

pub struct OdometryRuntime {
    left_joint_id: JointId,
    right_joint_id: JointId,
    wheel_radius_m: f64,
    wheel_base_m: f64,
    stale_timeout_ns: u64,
    state: OdometryState,
    estimate_publisher: TopicPublisher<odometry_contract::OdometryEstimate>,
    status_publisher: TopicPublisher<odometry_contract::Status>,
    source_health_publisher: TopicPublisher<odometry_contract::SourceHealth>,
    residuals_publisher: TopicPublisher<odometry_contract::Residuals>,
    integration_publisher: TopicPublisher<odometry_contract::Integration>,
}

#[async_trait::async_trait]
impl Runtime for OdometryRuntime {
    const RUNTIME_ID: &'static str = "odometry";

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
        io.subscribe_topic(
            topic::new().joint(config.left_joint_id.to_string()).data(),
            |sample| {
                let Received { at_ns, value } = sample;
                let joint_contract::JointState::V1(value) = value;
                Input::Left(Received { at_ns, value })
            },
        )
        .await?;
        io.subscribe_topic(
            topic::new().joint(config.right_joint_id.to_string()).data(),
            |sample| {
                let Received { at_ns, value } = sample;
                let joint_contract::JointState::V1(value) = value;
                Input::Right(Received { at_ns, value })
            },
        )
        .await?;

        let estimate_publisher = io
            .publisher_topic(topic::new().odometry().estimate())
            .await?;
        let status_publisher = io.publisher_topic(topic::new().odometry().status()).await?;
        let source_health_publisher = io
            .publisher_topic(topic::new().odometry().source_health())
            .await?;
        let residuals_publisher = io
            .publisher_topic(topic::new().odometry().residuals())
            .await?;
        let integration_publisher = io
            .publisher_topic(topic::new().odometry().integration())
            .await?;

        Ok(Self {
            left_joint_id: config.left_joint_id,
            right_joint_id: config.right_joint_id,
            wheel_radius_m: config.wheel_radius_m,
            wheel_base_m: config.wheel_base_m,
            stale_timeout_ns: config.stale_timeout_ns,
            state: OdometryState::default(),
            estimate_publisher,
            status_publisher,
            source_health_publisher,
            residuals_publisher,
            integration_publisher,
        })
    }

    async fn step(&mut self, step: Step, inputs: RuntimeInputs<Self::Input>) -> Result<()> {
        self.state.handle_epoch(step.tick.epoch());

        for input in inputs {
            match input {
                Input::Left(sample) => {
                    if let Some(value) = joint_angle_rad(&sample.value, &self.left_joint_id) {
                        self.state.left_rad = Some(value);
                        self.state.last_left_sample_ns = sample.at_ns;
                    }
                }
                Input::Right(sample) => {
                    if let Some(value) = joint_angle_rad(&sample.value, &self.right_joint_id) {
                        self.state.right_rad = Some(value);
                        self.state.last_right_sample_ns = sample.at_ns;
                    }
                }
            }
        }

        let time_ns = step.tick.time_ns();
        let left_health = classify_wheel(
            time_ns,
            self.state.last_left_sample_ns,
            self.stale_timeout_ns,
        );
        let right_health = classify_wheel(
            time_ns,
            self.state.last_right_sample_ns,
            self.stale_timeout_ns,
        );
        let current_status = status_from_wheel_health(left_health, right_health);
        let (velocity, integration) = self.state.update_pose(
            current_status.mode,
            &self.left_joint_id,
            &self.right_joint_id,
            self.wheel_radius_m,
            self.wheel_base_m,
        );
        self.state.covariance.grow_for_mode(current_status.mode);

        let estimate = OdometryEstimate {
            pose: pose_estimate(self.state.pose),
            velocity,
            covariance: Some(self.state.covariance.to_payload()),
            status: current_status.clone(),
        };

        let source_health = self.source_health(left_health, right_health);
        let residuals = Residuals {
            residuals: Vec::new(),
        };

        self.estimate_publisher
            .put(time_ns, &odometry_contract::OdometryEstimate::V1(estimate))
            .await?;
        self.status_publisher
            .put(time_ns, &odometry_contract::Status::V1(current_status))
            .await?;
        self.source_health_publisher
            .put(time_ns, &odometry_contract::SourceHealth::V1(source_health))
            .await?;
        self.residuals_publisher
            .put(time_ns, &odometry_contract::Residuals::V1(residuals))
            .await?;
        self.integration_publisher
            .put(time_ns, &odometry_contract::Integration::V1(integration))
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

#[derive(Debug, Clone, PartialEq)]
struct OdometryState {
    pose: PlanarPose,
    last_left_rad: Option<f64>,
    last_right_rad: Option<f64>,
    left_rad: Option<f64>,
    right_rad: Option<f64>,
    last_left_sample_ns: Option<u64>,
    last_right_sample_ns: Option<u64>,
    status_mode: StatusMode,
    covariance: Covariance3,
    last_epoch: Option<u64>,
}

impl Default for OdometryState {
    fn default() -> Self {
        Self {
            pose: PlanarPose::default(),
            last_left_rad: None,
            last_right_rad: None,
            left_rad: None,
            right_rad: None,
            last_left_sample_ns: None,
            last_right_sample_ns: None,
            status_mode: StatusMode::Initializing,
            covariance: Covariance3::default(),
            last_epoch: None,
        }
    }
}

impl OdometryState {
    fn handle_epoch(&mut self, current_epoch: u64) {
        if self
            .last_epoch
            .is_some_and(|previous_epoch| previous_epoch != current_epoch)
        {
            self.reset_baselines();
        }
        self.last_epoch = Some(current_epoch);
    }

    fn reset_baselines(&mut self) {
        *self = Self::default();
    }

    fn update_pose(
        &mut self,
        current_mode: StatusMode,
        left_joint_id: &JointId,
        right_joint_id: &JointId,
        wheel_radius_m: f64,
        wheel_base_m: f64,
    ) -> (VelocityEstimate, Integration) {
        let previous_mode = self.status_mode;
        self.status_mode = current_mode;

        let Some(left_now) = self.left_rad else {
            return (zero_velocity(), Integration { steps: Vec::new() });
        };
        let Some(right_now) = self.right_rad else {
            return (zero_velocity(), Integration { steps: Vec::new() });
        };

        let should_integrate =
            previous_mode == StatusMode::Tracking && current_mode == StatusMode::Tracking;
        let (velocity, integration) = if should_integrate {
            match (self.last_left_rad, self.last_right_rad) {
                (Some(left_prev), Some(right_prev)) => {
                    let motion = integrate_step(
                        self.pose,
                        WheelJointIds {
                            left: left_joint_id,
                            right: right_joint_id,
                        },
                        WheelPositions {
                            left_prev_rad: left_prev,
                            right_prev_rad: right_prev,
                            left_now_rad: left_now,
                            right_now_rad: right_now,
                        },
                        wheel_radius_m,
                        wheel_base_m,
                    );
                    self.pose = motion.pose;
                    let velocity = velocity_from_motion(&motion);
                    let integration = Integration {
                        steps: vec![motion.left_contribution, motion.right_contribution],
                    };
                    (velocity, integration)
                }
                _ => (zero_velocity(), Integration { steps: Vec::new() }),
            }
        } else {
            (zero_velocity(), Integration { steps: Vec::new() })
        };

        self.last_left_rad = Some(left_now);
        self.last_right_rad = Some(right_now);
        (velocity, integration)
    }
}

impl OdometryRuntime {
    fn source_health(&self, left_health: WheelHealth, right_health: WheelHealth) -> SourceHealth {
        SourceHealth {
            sources: vec![
                source_status(&self.left_joint_id, left_health),
                source_status(&self.right_joint_id, right_health),
            ],
        }
    }
}

fn joint_angle_rad(state: &JointState, joint_id: &JointId) -> Option<f64> {
    match state.quantity {
        Quantity::AngleRad => Some(state.value),
        Quantity::LinearM => {
            warn!(%joint_id, "odometry runtime skipped non-angular joint sample");
            None
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct PlanarPose {
    x_m: f64,
    y_m: f64,
    yaw_rad: f64,
}

/// Planar-profile covariance keeps only the [x, y, yaw] diagonal; off-diagonals are zero.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Covariance3 {
    xx: f64,
    yy: f64,
    yaw_yaw: f64,
}

impl Covariance3 {
    fn grow_for_mode(&mut self, mode: StatusMode) {
        match mode {
            StatusMode::Initializing => {}
            StatusMode::Tracking => {
                self.xx += VAR_PER_STEP_TRACKING_M2;
                self.yy += VAR_PER_STEP_TRACKING_M2;
                self.yaw_yaw += VAR_PER_STEP_YAW_TRACKING_RAD2;
            }
            StatusMode::Degraded | StatusMode::Stale => {
                self.xx += VAR_PER_STEP_DEGRADED_M2;
                self.yy += VAR_PER_STEP_DEGRADED_M2;
                self.yaw_yaw += VAR_PER_STEP_YAW_DEGRADED_RAD2;
            }
        }
    }

    fn to_payload(self) -> Covariance {
        Covariance {
            // Planar differential-drive odometry publishes only the [x, y, yaw] diagonal.
            values: vec![self.xx, 0.0, 0.0, 0.0, self.yy, 0.0, 0.0, 0.0, self.yaw_yaw],
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct IntegrationOutcome {
    pose: PlanarPose,
    d_center_m: f64,
    d_yaw_rad: f64,
    left_contribution: IntegrationStep,
    right_contribution: IntegrationStep,
}

#[derive(Debug, Clone, Copy)]
struct WheelJointIds<'a> {
    left: &'a JointId,
    right: &'a JointId,
}

#[derive(Debug, Clone, Copy)]
struct WheelPositions {
    left_prev_rad: f64,
    right_prev_rad: f64,
    left_now_rad: f64,
    right_now_rad: f64,
}

fn integrate_step(
    previous: PlanarPose,
    joints: WheelJointIds<'_>,
    positions: WheelPositions,
    wheel_radius_m: f64,
    wheel_base_m: f64,
) -> IntegrationOutcome {
    let delta_left_rad = positions.left_now_rad - positions.left_prev_rad;
    let delta_right_rad = positions.right_now_rad - positions.right_prev_rad;
    let d_left_m = delta_left_rad * wheel_radius_m;
    let d_right_m = delta_right_rad * wheel_radius_m;
    let d_center_m = (d_left_m + d_right_m) / 2.0;
    let d_yaw_rad = (d_right_m - d_left_m) / wheel_base_m;
    let yaw_mid = previous.yaw_rad + d_yaw_rad / 2.0;

    IntegrationOutcome {
        pose: PlanarPose {
            x_m: previous.x_m + d_center_m * yaw_mid.cos(),
            y_m: previous.y_m + d_center_m * yaw_mid.sin(),
            yaw_rad: previous.yaw_rad + d_yaw_rad,
        },
        d_center_m,
        d_yaw_rad,
        left_contribution: IntegrationStep {
            source_id: SourceId::Joint(joints.left.clone()),
            delta_pose_m: [d_left_m, 0.0, 0.0],
            delta_yaw_rad: -d_left_m / wheel_base_m,
        },
        right_contribution: IntegrationStep {
            source_id: SourceId::Joint(joints.right.clone()),
            delta_pose_m: [d_right_m, 0.0, 0.0],
            delta_yaw_rad: d_right_m / wheel_base_m,
        },
    }
}

fn pose_estimate(pose: PlanarPose) -> PoseEstimate {
    let half_yaw = pose.yaw_rad / 2.0;
    PoseEstimate {
        frame_id: FrameId::new(ODOM_FRAME_ID),
        child_frame_id: FrameId::new(BASE_FRAME_ID),
        translation_m: [pose.x_m, pose.y_m, 0.0],
        rotation_xyzw: [0.0, 0.0, half_yaw.sin(), half_yaw.cos()],
    }
}

fn velocity_from_motion(motion: &IntegrationOutcome) -> VelocityEstimate {
    let dt_s = CLOCK_PERIOD.as_secs_f64();
    VelocityEstimate {
        frame_id: FrameId::new(BASE_FRAME_ID),
        linear_mps: [motion.d_center_m / dt_s, 0.0, 0.0],
        angular_radps: [0.0, 0.0, motion.d_yaw_rad / dt_s],
    }
}

fn zero_velocity() -> VelocityEstimate {
    VelocityEstimate {
        frame_id: FrameId::new(BASE_FRAME_ID),
        linear_mps: [0.0, 0.0, 0.0],
        angular_radps: [0.0, 0.0, 0.0],
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WheelHealth {
    Missing,
    Fresh,
    Stale,
}

fn classify_wheel(now_ns: u64, last_sample_ns: Option<u64>, stale_after_ns: u64) -> WheelHealth {
    let Some(last_sample_ns) = last_sample_ns else {
        return WheelHealth::Missing;
    };
    if now_ns.saturating_sub(last_sample_ns) > stale_after_ns {
        WheelHealth::Stale
    } else {
        WheelHealth::Fresh
    }
}

fn status_from_wheel_health(left: WheelHealth, right: WheelHealth) -> Status {
    if left == WheelHealth::Missing || right == WheelHealth::Missing {
        return Status {
            mode: StatusMode::Initializing,
            reasons: Vec::new(),
        };
    }

    match (left, right) {
        (WheelHealth::Fresh, WheelHealth::Fresh) => Status {
            mode: StatusMode::Tracking,
            reasons: Vec::new(),
        },
        (WheelHealth::Stale, WheelHealth::Stale) => Status {
            mode: StatusMode::Stale,
            reasons: vec![StatusReason::JointStale],
        },
        (WheelHealth::Stale, WheelHealth::Fresh) | (WheelHealth::Fresh, WheelHealth::Stale) => {
            Status {
                mode: StatusMode::Degraded,
                reasons: vec![StatusReason::JointStale],
            }
        }
        (WheelHealth::Missing, _) | (_, WheelHealth::Missing) => Status {
            mode: StatusMode::Initializing,
            reasons: Vec::new(),
        },
    }
}

fn source_status(joint_id: &JointId, health: WheelHealth) -> SourceStatus {
    SourceStatus {
        source_id: SourceId::Joint(joint_id.clone()),
        healthy: health == WheelHealth::Fresh,
        reason: match health {
            WheelHealth::Missing => Some(SourceReason::Missing),
            WheelHealth::Fresh => None,
            WheelHealth::Stale => Some(SourceReason::Stale),
        },
    }
}

#[cfg(test)]
mod tests {
    use phoxal::api::joint::v1::JointId;
    use phoxal::api::odometry::v1::{SourceId, StatusMode, StatusReason};

    use super::{
        Covariance3, IntegrationOutcome, OdometryState, PlanarPose, VAR_PER_STEP_DEGRADED_M2,
        VAR_PER_STEP_TRACKING_M2, VAR_PER_STEP_YAW_DEGRADED_RAD2, VAR_PER_STEP_YAW_TRACKING_RAD2,
        WheelHealth, WheelJointIds, WheelPositions, integrate_step, status_from_wheel_health,
    };

    const EPSILON: f64 = 1e-12;

    #[test]
    fn straight_line_increases_position_without_yaw_change() {
        let motion = integrate_for_test(PlanarPose::default(), 0.0, 0.0, 2.0, 2.0, 0.25, 0.5);

        assert_close(motion.pose.x_m, 0.5);
        assert_close(motion.pose.y_m, 0.0);
        assert_close(motion.pose.yaw_rad, 0.0);
    }

    #[test]
    fn in_place_rotation_changes_yaw_without_translation() {
        let motion = integrate_for_test(PlanarPose::default(), 0.0, 0.0, -1.0, 1.0, 0.2, 0.5);

        assert_close(motion.pose.x_m, 0.0);
        assert_close(motion.pose.y_m, 0.0);
        assert_close(motion.pose.yaw_rad, 0.8);
    }

    #[test]
    fn arc_uses_midpoint_yaw_formula() {
        let previous = PlanarPose {
            x_m: 1.0,
            y_m: 2.0,
            yaw_rad: 0.3,
        };

        let motion = integrate_for_test(previous, 0.0, 0.0, 1.0, 3.0, 0.1, 0.5);
        let d_center_m = 0.2;
        let d_yaw_rad = 0.4;
        let yaw_mid = previous.yaw_rad + d_yaw_rad / 2.0;

        assert_close(motion.pose.x_m, previous.x_m + d_center_m * yaw_mid.cos());
        assert_close(motion.pose.y_m, previous.y_m + d_center_m * yaw_mid.sin());
        assert_close(motion.pose.yaw_rad, previous.yaw_rad + d_yaw_rad);
    }

    #[test]
    fn tracking_covariance_grows_by_tracking_variance_per_step() {
        let mut covariance = Covariance3::default();

        for _ in 0..7 {
            covariance.grow_for_mode(StatusMode::Tracking);
        }

        assert_close(covariance.xx, 7.0 * VAR_PER_STEP_TRACKING_M2);
        assert_close(covariance.yy, 7.0 * VAR_PER_STEP_TRACKING_M2);
        assert_close(covariance.yaw_yaw, 7.0 * VAR_PER_STEP_YAW_TRACKING_RAD2);
        assert_eq!(covariance.to_payload().values.len(), 9);
    }

    #[test]
    fn degraded_covariance_grows_faster_from_previous_value() {
        let mut covariance = Covariance3::default();
        for _ in 0..4 {
            covariance.grow_for_mode(StatusMode::Tracking);
        }
        let previous = covariance;

        for _ in 0..3 {
            covariance.grow_for_mode(StatusMode::Degraded);
        }

        const {
            assert!(VAR_PER_STEP_DEGRADED_M2 > VAR_PER_STEP_TRACKING_M2);
            assert!(VAR_PER_STEP_YAW_DEGRADED_RAD2 > VAR_PER_STEP_YAW_TRACKING_RAD2);
        }
        assert_close(covariance.xx - previous.xx, 3.0 * VAR_PER_STEP_DEGRADED_M2);
        assert_close(covariance.yy - previous.yy, 3.0 * VAR_PER_STEP_DEGRADED_M2);
        assert_close(
            covariance.yaw_yaw - previous.yaw_yaw,
            3.0 * VAR_PER_STEP_YAW_DEGRADED_RAD2,
        );
    }

    #[test]
    fn initializing_keeps_covariance_at_zero() {
        let mut covariance = Covariance3::default();

        for _ in 0..5 {
            covariance.grow_for_mode(StatusMode::Initializing);
        }

        assert_eq!(covariance, Covariance3::default());
        assert_eq!(
            covariance.to_payload().values,
            vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]
        );
    }

    #[test]
    fn epoch_change_clears_local_baselines_and_covariance() {
        let mut state = OdometryState {
            pose: PlanarPose {
                x_m: 1.0,
                y_m: 2.0,
                yaw_rad: 0.3,
            },
            last_left_rad: Some(1.0),
            last_right_rad: Some(2.0),
            left_rad: Some(3.0),
            right_rad: Some(4.0),
            last_left_sample_ns: Some(100),
            last_right_sample_ns: Some(200),
            status_mode: StatusMode::Tracking,
            covariance: Covariance3 {
                xx: 1.0,
                yy: 1.0,
                yaw_yaw: 1.0,
            },
            last_epoch: Some(7),
        };

        state.handle_epoch(8);

        let expected = OdometryState {
            last_epoch: Some(8),
            ..OdometryState::default()
        };
        assert_eq!(state, expected);
    }

    #[test]
    fn straight_line_integration_steps_have_canceling_yaw_contributions() {
        let motion = integrate_for_test(PlanarPose::default(), 0.0, 0.0, 2.0, 2.0, 0.25, 0.5);

        let yaw_sum =
            motion.left_contribution.delta_yaw_rad + motion.right_contribution.delta_yaw_rad;
        assert_close(yaw_sum, 0.0);
        assert_close(
            motion.left_contribution.delta_yaw_rad.abs(),
            motion.right_contribution.delta_yaw_rad.abs(),
        );
    }

    #[test]
    fn rotation_integration_steps_sum_to_differential_yaw() {
        let wheel_radius_m = 0.2;
        let wheel_base_m = 0.5;
        let left_now_rad = -1.0;
        let right_now_rad = 1.0;

        let motion = integrate_for_test(
            PlanarPose::default(),
            0.0,
            0.0,
            left_now_rad,
            right_now_rad,
            wheel_radius_m,
            wheel_base_m,
        );

        let d_left_m = left_now_rad * wheel_radius_m;
        let d_right_m = right_now_rad * wheel_radius_m;
        let expected_yaw = (d_right_m - d_left_m) / wheel_base_m;
        let actual_yaw =
            motion.left_contribution.delta_yaw_rad + motion.right_contribution.delta_yaw_rad;
        assert_close(actual_yaw, expected_yaw);
        assert_close(actual_yaw, motion.d_yaw_rad);
    }

    #[test]
    fn integration_steps_identify_their_joint_sources() {
        let left_joint = left_joint_id();
        let right_joint = right_joint_id();

        let motion = integrate_step(
            PlanarPose::default(),
            WheelJointIds {
                left: &left_joint,
                right: &right_joint,
            },
            WheelPositions {
                left_prev_rad: 0.0,
                right_prev_rad: 0.0,
                left_now_rad: 1.0,
                right_now_rad: 2.0,
            },
            0.25,
            0.5,
        );

        assert_eq!(
            motion.left_contribution.source_id,
            SourceId::Joint(left_joint)
        );
        assert_eq!(
            motion.right_contribution.source_id,
            SourceId::Joint(right_joint)
        );
    }

    #[test]
    fn initializing_transitions_to_tracking_after_both_wheels_publish() {
        let initializing = status_from_wheel_health(WheelHealth::Fresh, WheelHealth::Missing);
        assert_eq!(initializing.mode, StatusMode::Initializing);

        let tracking = status_from_wheel_health(WheelHealth::Fresh, WheelHealth::Fresh);
        assert_eq!(tracking.mode, StatusMode::Tracking);
        assert!(tracking.reasons.is_empty());
    }

    #[test]
    fn tracking_degrades_when_left_wheel_is_stale() {
        let status = status_from_wheel_health(WheelHealth::Stale, WheelHealth::Fresh);

        assert_eq!(status.mode, StatusMode::Degraded);
        assert_eq!(status.reasons, vec![StatusReason::JointStale]);
    }

    #[test]
    fn tracking_becomes_stale_when_both_wheels_are_stale() {
        let status = status_from_wheel_health(WheelHealth::Stale, WheelHealth::Stale);

        assert_eq!(status.mode, StatusMode::Stale);
        assert_eq!(status.reasons, vec![StatusReason::JointStale]);
    }

    #[test]
    fn degraded_and_stale_statuses_always_carry_typed_reasons() {
        let degraded = status_from_wheel_health(WheelHealth::Fresh, WheelHealth::Stale);
        let stale = status_from_wheel_health(WheelHealth::Stale, WheelHealth::Stale);

        assert_eq!(degraded.mode, StatusMode::Degraded);
        assert!(!degraded.reasons.is_empty());
        assert_eq!(stale.mode, StatusMode::Stale);
        assert!(!stale.reasons.is_empty());
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= EPSILON,
            "actual {actual} did not equal expected {expected}"
        );
    }

    fn integrate_for_test(
        previous: PlanarPose,
        left_prev_rad: f64,
        right_prev_rad: f64,
        left_now_rad: f64,
        right_now_rad: f64,
        wheel_radius_m: f64,
        wheel_base_m: f64,
    ) -> IntegrationOutcome {
        let left_joint = left_joint_id();
        let right_joint = right_joint_id();
        integrate_step(
            previous,
            WheelJointIds {
                left: &left_joint,
                right: &right_joint,
            },
            WheelPositions {
                left_prev_rad,
                right_prev_rad,
                left_now_rad,
                right_now_rad,
            },
            wheel_radius_m,
            wheel_base_m,
        )
    }

    fn left_joint_id() -> JointId {
        JointId::new("left_wheel_joint")
    }

    fn right_joint_id() -> JointId {
        JointId::new("right_wheel_joint")
    }
}
