//! The official kinematics Runtime.
//!
//! Kinematics is the one owner of measured joint state, differential-drive
//! odometry, and the small model-backed frame tree used by the first rewrite.
//! The implementation deliberately consumes an explicit bounded encoder batch
//! instead of discovering component topics or maintaining a second catalogue.
//! A missing or stale wheel is reported as unavailable and never turns a held
//! velocity into fresh motion.

use std::collections::{BTreeMap, VecDeque};

use phoxal::runtime::input::Samples;
use phoxal::runtime::{InitContext, ObservationStamp, Runtime, Sample, StepContext};
use phoxal_kinematics::{
    EncoderMeasurement, FrameTransform, FrameTree, JointState, KinematicsStatus,
    LookupFrameRequest, LookupFrameResponse, OdometryState, UnavailableReason, ports,
};

const DEFAULT_MAX_AGE_MS: u64 = 100;
const DEFAULT_HISTORY_CAPACITY: u32 = 64;

/// Typed, validated wheel and frame configuration for one kinematics instance.
#[derive(Clone, Debug, serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
pub struct KinematicsConfig {
    /// Encoder identity supplying the left wheel measurement.
    #[serde(default = "default_left_encoder_id")]
    pub left_encoder_id: String,
    /// Encoder identity supplying the right wheel measurement.
    #[serde(default = "default_right_encoder_id")]
    pub right_encoder_id: String,
    /// Joint identity emitted for the left wheel.
    #[serde(default = "default_left_joint_id")]
    pub left_joint_id: String,
    /// Joint identity emitted for the right wheel.
    #[serde(default = "default_right_joint_id")]
    pub right_joint_id: String,
    /// Mount direction sign for the left encoder.
    #[serde(default = "default_direction_sign")]
    pub left_direction_sign: i8,
    /// Mount direction sign for the right encoder.
    #[serde(default = "default_direction_sign")]
    pub right_direction_sign: i8,
    /// Encoder-to-joint ratio for the left wheel.
    #[serde(default = "default_gear_ratio")]
    pub left_gear_ratio: f64,
    /// Encoder-to-joint ratio for the right wheel.
    #[serde(default = "default_gear_ratio")]
    pub right_gear_ratio: f64,
    /// Wheel radius in metres.
    #[serde(default = "default_wheel_radius_m")]
    pub wheel_radius_m: f64,
    /// Distance between wheel contact lines in metres.
    #[serde(default = "default_wheel_base_m")]
    pub wheel_base_m: f64,
    /// Frame containing the integrated pose.
    #[serde(default = "default_odom_frame_id")]
    pub odom_frame_id: String,
    /// Frame fixed to the robot body.
    #[serde(default = "default_base_frame_id")]
    pub base_frame_id: String,
    /// Maximum accepted encoder age in logical milliseconds.
    #[serde(default = "default_max_age_ms")]
    pub max_age_ms: u64,
    /// Number of frame trees retained for bounded reads.
    #[serde(default = "default_history_capacity")]
    pub history_capacity: u32,
}

fn default_left_encoder_id() -> String {
    "left_encoder".to_owned()
}

fn default_right_encoder_id() -> String {
    "right_encoder".to_owned()
}

fn default_left_joint_id() -> String {
    "left_wheel".to_owned()
}

fn default_right_joint_id() -> String {
    "right_wheel".to_owned()
}

const fn default_direction_sign() -> i8 {
    1
}

const fn default_gear_ratio() -> f64 {
    1.0
}

const fn default_wheel_radius_m() -> f64 {
    0.1
}

const fn default_wheel_base_m() -> f64 {
    0.4
}

fn default_odom_frame_id() -> String {
    "odom".to_owned()
}

fn default_base_frame_id() -> String {
    "base_link".to_owned()
}

const fn default_max_age_ms() -> u64 {
    DEFAULT_MAX_AGE_MS
}

const fn default_history_capacity() -> u32 {
    DEFAULT_HISTORY_CAPACITY
}

impl Default for KinematicsConfig {
    fn default() -> Self {
        Self {
            left_encoder_id: "left_encoder".to_owned(),
            right_encoder_id: "right_encoder".to_owned(),
            left_joint_id: "left_wheel".to_owned(),
            right_joint_id: "right_wheel".to_owned(),
            left_direction_sign: 1,
            right_direction_sign: 1,
            left_gear_ratio: 1.0,
            right_gear_ratio: 1.0,
            wheel_radius_m: 0.1,
            wheel_base_m: 0.4,
            odom_frame_id: "odom".to_owned(),
            base_frame_id: "base_link".to_owned(),
            max_age_ms: DEFAULT_MAX_AGE_MS,
            history_capacity: DEFAULT_HISTORY_CAPACITY,
        }
    }
}

fn validate_config(config: &KinematicsConfig) -> phoxal::Result<()> {
    for (value, field) in [
        (&config.left_encoder_id, "left_encoder_id"),
        (&config.right_encoder_id, "right_encoder_id"),
        (&config.left_joint_id, "left_joint_id"),
        (&config.right_joint_id, "right_joint_id"),
        (&config.odom_frame_id, "odom_frame_id"),
        (&config.base_frame_id, "base_frame_id"),
    ] {
        if value.is_empty() || value.len() > phoxal_kinematics::MAX_ID_BYTES {
            return Err(anyhow::anyhow!(
                "{field} must contain 1 to {} UTF-8 bytes",
                phoxal_kinematics::MAX_ID_BYTES
            ));
        }
    }
    let ids = [
        &config.left_encoder_id,
        &config.right_encoder_id,
        &config.left_joint_id,
        &config.right_joint_id,
        &config.odom_frame_id,
        &config.base_frame_id,
    ];
    if ids
        .iter()
        .enumerate()
        .any(|(index, id)| ids[..index].contains(id))
    {
        return Err(anyhow::anyhow!(
            "kinematics encoder, joint, and frame identities must be distinct"
        ));
    }
    if !matches!(config.left_direction_sign, -1 | 1)
        || !matches!(config.right_direction_sign, -1 | 1)
    {
        return Err(anyhow::anyhow!(
            "encoder direction signs must be either -1 or 1"
        ));
    }
    for (value, field) in [
        (config.left_gear_ratio, "left_gear_ratio"),
        (config.right_gear_ratio, "right_gear_ratio"),
        (config.wheel_radius_m, "wheel_radius_m"),
        (config.wheel_base_m, "wheel_base_m"),
    ] {
        if !value.is_finite() || value <= 0.0 {
            return Err(anyhow::anyhow!("{field} must be finite and positive"));
        }
    }
    if config.max_age_ms == 0 || config.history_capacity == 0 {
        return Err(anyhow::anyhow!(
            "max_age_ms and history_capacity must be positive"
        ));
    }
    Ok(())
}

/// Private state retained by the serialized kinematics owner.
pub struct KinematicsState {
    config: KinematicsConfig,
    joints: BTreeMap<String, JointState>,
    x_m: f64,
    y_m: f64,
    yaw_rad: f64,
    linear_x_mps: f64,
    angular_z_radps: f64,
    revision: u64,
    available: bool,
    unavailable_reasons: Vec<i32>,
    frame_history: VecDeque<FrameTree>,
}

impl KinematicsState {
    fn new(config: KinematicsConfig) -> Self {
        Self {
            config,
            joints: BTreeMap::new(),
            x_m: 0.0,
            y_m: 0.0,
            yaw_rad: 0.0,
            linear_x_mps: 0.0,
            angular_z_radps: 0.0,
            revision: 0,
            available: false,
            unavailable_reasons: vec![UnavailableReason::Encoder as i32],
            frame_history: VecDeque::new(),
        }
    }

    fn odometry(&self) -> OdometryState {
        OdometryState {
            x_m: self.x_m,
            y_m: self.y_m,
            yaw_rad: self.yaw_rad,
            linear_x_mps: self.linear_x_mps,
            angular_z_radps: self.angular_z_radps,
            revision: self.revision,
            available: self.available,
        }
    }

    fn frames(&self) -> FrameTree {
        let config = &self.config;
        FrameTree {
            transforms: vec![
                FrameTransform {
                    parent_frame_id: config.odom_frame_id.clone(),
                    child_frame_id: config.base_frame_id.clone(),
                    x_m: self.x_m,
                    y_m: self.y_m,
                    yaw_rad: self.yaw_rad,
                },
                FrameTransform {
                    parent_frame_id: config.base_frame_id.clone(),
                    child_frame_id: config.left_joint_id.clone(),
                    x_m: 0.0,
                    y_m: config.wheel_base_m / 2.0,
                    yaw_rad: 0.0,
                },
                FrameTransform {
                    parent_frame_id: config.base_frame_id.clone(),
                    child_frame_id: config.right_joint_id.clone(),
                    x_m: 0.0,
                    y_m: -config.wheel_base_m / 2.0,
                    yaw_rad: 0.0,
                },
            ],
            revision: self.revision,
        }
    }

    fn status(&self) -> KinematicsStatus {
        KinematicsStatus {
            available: self.available,
            unavailable_reasons: self.unavailable_reasons.clone(),
            revision: self.revision,
        }
    }

    fn retain_frames(&mut self, frames: FrameTree) {
        if self.frame_history.len() == self.config.history_capacity as usize {
            self.frame_history.pop_front();
        }
        self.frame_history.push_back(frames);
    }
}

/// One immutable encoder input cut.
#[phoxal::runtime::inputs]
pub struct KinematicsInputs {
    /// Bounded measurements, retaining each producer's capture stamp.
    #[phoxal::runtime::input(max_items = 32, max_bytes = 16_384)]
    pub encoders: Samples<EncoderMeasurement>,
}

/// Fresh measured joint products.
#[phoxal::runtime::outputs]
#[derive(Default)]
pub struct KinematicsOutputs {
    /// One stamped sample for every valid encoder measurement in the cut.
    #[phoxal::runtime::outputs::sample(
        port = ports::JOINTS,
        max_items = 32,
        max_bytes = 16_384
    )]
    pub joints: Vec<Sample<JointState>>,
}

/// The official kinematics service implementation.
#[derive(Clone, Copy, Debug, Default)]
pub struct Kinematics;

#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for Kinematics {
    type Config = KinematicsConfig;
    type State = KinematicsState;
    type Inputs = KinematicsInputs;
    type Outputs = KinematicsOutputs;

    fn validate_config(config: &Self::Config) -> phoxal::Result<()> {
        validate_config(config)
    }

    fn init(&self, _ctx: &InitContext, config: Self::Config) -> phoxal::Result<Self::State> {
        Ok(KinematicsState::new(config))
    }

    fn step(
        &self,
        ctx: &StepContext,
        mut state: Self::State,
        inputs: &Self::Inputs,
    ) -> phoxal::Result<(Self::State, Self::Outputs)> {
        let mut latest = BTreeMap::<String, (EncoderMeasurement, ObservationStamp)>::new();
        let mut invalid_measurement = false;
        for sample in inputs.encoders.items() {
            let measurement = sample.payload();
            if measurement.validate().is_err() {
                invalid_measurement = true;
                continue;
            }
            let Some(age) = ctx
                .now()
                .checked_duration_since(sample.stamp().capture_time())
            else {
                invalid_measurement = true;
                continue;
            };
            if age.as_millis() > state.config.max_age_ms {
                continue;
            }
            let replace = latest
                .get(&measurement.encoder_id)
                .is_none_or(|(_, stamp)| stamp.capture_time() < sample.stamp().capture_time());
            if replace {
                latest.insert(
                    measurement.encoder_id.clone(),
                    (measurement.clone(), sample.stamp().clone()),
                );
            }
        }

        let mut outputs = KinematicsOutputs::default();
        for (encoder_id, (measurement, stamp)) in &latest {
            let (joint_id, direction_sign, gear_ratio) =
                if encoder_id == &state.config.left_encoder_id {
                    (
                        &state.config.left_joint_id,
                        state.config.left_direction_sign,
                        state.config.left_gear_ratio,
                    )
                } else if encoder_id == &state.config.right_encoder_id {
                    (
                        &state.config.right_joint_id,
                        state.config.right_direction_sign,
                        state.config.right_gear_ratio,
                    )
                } else {
                    continue;
                };
            let scale = f64::from(direction_sign) / gear_ratio;
            let joint = JointState {
                joint_id: joint_id.clone(),
                position_rad: measurement.position_rad * scale,
                velocity_radps: measurement.velocity_radps * scale,
                effort_nm: measurement.effort_nm.map(|effort| effort * scale),
            };
            state.joints.insert(joint_id.clone(), joint.clone());
            outputs.joints.push(Sample::new(joint, stamp.clone()));
        }

        let left = latest.get(&state.config.left_encoder_id);
        let right = latest.get(&state.config.right_encoder_id);
        if let (Some((left, _)), Some((right, _))) = (left, right) {
            let left_radps = left.velocity_radps * f64::from(state.config.left_direction_sign)
                / state.config.left_gear_ratio;
            let right_radps = right.velocity_radps * f64::from(state.config.right_direction_sign)
                / state.config.right_gear_ratio;
            let linear = state.config.wheel_radius_m * (left_radps + right_radps) / 2.0;
            let angular = state.config.wheel_radius_m * (right_radps - left_radps)
                / state.config.wheel_base_m;
            if linear.is_finite() && angular.is_finite() {
                let dt_s = ctx.elapsed().as_nanos() as f64 / 1_000_000_000.0;
                state.x_m += linear * dt_s * state.yaw_rad.cos();
                state.y_m += linear * dt_s * state.yaw_rad.sin();
                state.yaw_rad = normalize_yaw(state.yaw_rad + angular * dt_s);
                state.linear_x_mps = linear;
                state.angular_z_radps = angular;
                state.revision = state.revision.saturating_add(1);
                state.available = true;
                state.unavailable_reasons.clear();
                let frames = state.frames();
                state.retain_frames(frames);
            } else {
                invalid_measurement = true;
            }
        }

        if !state.available || left.is_none() || right.is_none() || invalid_measurement {
            state.available = false;
            state.linear_x_mps = 0.0;
            state.angular_z_radps = 0.0;
            state.unavailable_reasons = if invalid_measurement {
                vec![UnavailableReason::InvalidMeasurement as i32]
            } else {
                vec![UnavailableReason::Encoder as i32]
            };
        }
        Ok((state, outputs))
    }
}

#[phoxal::runtime::outputs]
#[allow(
    dead_code,
    reason = "the collected projections are invoked by the transport runner"
)]
impl Kinematics {
    /// Projects continuous odometry state.
    #[phoxal::runtime::outputs::state(port = ports::ODOMETRY, max_bytes = 512, bootstrap, on_change)]
    fn odometry(&self, state: &KinematicsState) -> OdometryState {
        state.odometry()
    }

    /// Projects the current model-backed frame tree.
    #[phoxal::runtime::outputs::state(port = ports::FRAMES, max_bytes = 4_096, bootstrap, on_change)]
    fn frames(&self, state: &KinematicsState) -> FrameTree {
        state.frames()
    }

    /// Projects availability and missing-input reasons.
    #[phoxal::runtime::outputs::state(port = ports::STATUS, max_bytes = 512, bootstrap, on_change)]
    fn status(&self, state: &KinematicsState) -> KinematicsStatus {
        state.status()
    }

    fn read_view(&self, state: &KinematicsState) -> KinematicsReadView {
        KinematicsReadView {
            current: state.frames(),
            history: state.frame_history.iter().cloned().collect(),
        }
    }

    /// Looks up one exact frame edge from the current bounded history.
    #[phoxal::runtime::outputs::read(
        port = ports::LOOKUP_FRAME,
        project = Self::read_view,
        max_request_bytes = 256,
        max_response_bytes = 1_024
    )]
    fn lookup_frame(
        &self,
        view: &KinematicsReadView,
        request: &LookupFrameRequest,
    ) -> LookupFrameResponse {
        if request.validate().is_err() {
            return LookupFrameResponse {
                transform: None,
                revision: view.current.revision,
            };
        }
        let tree = if request.revision == 0 {
            &view.current
        } else if let Some(tree) = view
            .history
            .iter()
            .find(|tree| tree.revision == request.revision)
        {
            tree
        } else {
            return LookupFrameResponse {
                transform: None,
                revision: view.current.revision,
            };
        };
        LookupFrameResponse {
            transform: tree
                .transforms
                .iter()
                .find(|transform| {
                    transform.parent_frame_id == request.parent_frame_id
                        && transform.child_frame_id == request.child_frame_id
                })
                .cloned(),
            revision: tree.revision,
        }
    }
}

struct KinematicsReadView {
    current: FrameTree,
    history: Vec<FrameTree>,
}

fn normalize_yaw(yaw: f64) -> f64 {
    let two_pi = 2.0 * std::f64::consts::PI;
    (yaw + std::f64::consts::PI).rem_euclid(two_pi) - std::f64::consts::PI
}

#[cfg(test)]
mod tests {
    use phoxal::runtime::{ExecutionDuration, ExecutionTime, ObservationStamp, StepContext};

    use super::*;

    fn config() -> KinematicsConfig {
        KinematicsConfig::default()
    }

    fn sample(
        encoder_id: &str,
        position_rad: f64,
        velocity_radps: f64,
        at_ms: u64,
    ) -> Sample<EncoderMeasurement> {
        Sample::new(
            EncoderMeasurement {
                encoder_id: encoder_id.to_owned(),
                position_rad,
                velocity_radps,
                effort_nm: None,
            },
            ObservationStamp::new(
                encoder_id,
                ExecutionTime::from_nanos(at_ms * 1_000_000),
                None,
            ),
        )
    }

    fn context(index: u64, now_ms: u64, previous_ms: Option<u64>) -> StepContext {
        StepContext::from_previous(
            ExecutionTime::from_nanos(now_ms * 1_000_000),
            ExecutionDuration::from_millis(20),
            previous_ms.map(|at| ExecutionTime::from_nanos(at * 1_000_000)),
            0,
            index,
        )
    }

    #[test]
    fn one_encoder_batch_produces_joints_odometry_and_frames() {
        let state = KinematicsState::new(config());
        let inputs = KinematicsInputs {
            encoders: Samples::new(vec![
                sample("left_encoder", 2.0, 4.0, 20),
                sample("right_encoder", 2.0, 4.0, 20),
            ]),
        };
        let (state, outputs) = Kinematics
            .step(&context(0, 20, None), state, &inputs)
            .expect("complete wheel batch");
        assert_eq!(outputs.joints.len(), 2);
        assert!(outputs.joints.iter().any(|sample| {
            sample.payload().joint_id == "left_wheel" && sample.payload().position_rad == 2.0
        }));
        assert!(state.available);
        assert_eq!(state.revision, 1);
        assert!(state.frames().validate().is_ok());
    }

    #[test]
    fn missing_wheel_is_explicitly_unavailable_and_does_not_integrate() {
        let state = KinematicsState::new(config());
        let (state, _) = Kinematics
            .step(
                &context(0, 20, None),
                state,
                &KinematicsInputs {
                    encoders: Samples::new(vec![
                        sample("left_encoder", 0.0, 4.0, 20),
                        sample("right_encoder", 0.0, 4.0, 20),
                    ]),
                },
            )
            .expect("complete wheel batch");
        let (state, _) = Kinematics
            .step(
                &context(1, 40, Some(20)),
                state,
                &KinematicsInputs {
                    encoders: Samples::new(vec![sample("left_encoder", 0.0, 4.0, 40)]),
                },
            )
            .expect("partial input is a valid transition");
        assert!(!state.available);
        assert_eq!(state.linear_x_mps, 0.0);
        assert_eq!(state.angular_z_radps, 0.0);
        assert_eq!(state.revision, 1);
    }

    #[test]
    fn stale_measurements_are_not_reused_as_fresh_motion() {
        let state = KinematicsState::new(config());
        let (state, outputs) = Kinematics
            .step(
                &context(0, 200, None),
                state,
                &KinematicsInputs {
                    encoders: Samples::new(vec![
                        sample("left_encoder", 0.0, 4.0, 20),
                        sample("right_encoder", 0.0, 4.0, 20),
                    ]),
                },
            )
            .expect("stale input is a valid transition");
        assert!(outputs.joints.is_empty());
        assert!(!state.available);
        assert_eq!(state.linear_x_mps, 0.0);
    }
}
