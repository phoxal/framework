mod measurements;

use crate::api::types::phoxal::kinematics::v1::{
    FrameTransform, FrameTree, KinematicsStatus, LookupFrameRequest, LookupFrameResponse,
    OdometryState, UnavailableReason,
};
use crate::config::{KinematicsConfig, validate_config};
use crate::validation;
#[cfg(test)]
use phoxal::robotics::EncoderSample;
#[cfg(test)]
use phoxal::runtime::Sample;
#[cfg(test)]
use phoxal::runtime::input::Samples;
use phoxal::runtime::{InitContext, Runtime, StepContext};
use std::collections::VecDeque;

/// Private state retained by the serialized kinematics owner.
pub struct KinematicsState {
    config: KinematicsConfig,
    encoders: measurements::RetainedEncoders,
    x_m: f64,
    y_m: f64,
    yaw_rad: f64,
    linear_x_mps: f64,
    angular_z_radps: f64,
    revision: u64,
    oldest_capture_time_nanos: Option<u64>,
    available: bool,
    unavailable_reasons: Vec<i32>,
    frame_history: VecDeque<FrameTree>,
}

impl KinematicsState {
    fn new(config: KinematicsConfig) -> Self {
        Self {
            config,
            encoders: measurements::RetainedEncoders::new(),
            x_m: 0.0,
            y_m: 0.0,
            yaw_rad: 0.0,
            linear_x_mps: 0.0,
            angular_z_radps: 0.0,
            revision: 0,
            oldest_capture_time_nanos: None,
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
            oldest_capture_time_nanos: self.oldest_capture_time_nanos,
        }
    }

    fn frames(&self) -> FrameTree {
        let config = &self.config;
        let mut transforms = vec![FrameTransform {
            parent_frame_id: config.odom_frame_id.clone(),
            child_frame_id: config.base_frame_id.clone(),
            x_m: self.x_m,
            y_m: self.y_m,
            yaw_rad: self.yaw_rad,
        }];
        for (wheels, side) in [(&config.left_wheels, 1.0), (&config.right_wheels, -1.0)] {
            transforms.extend(wheels.iter().map(|wheel| FrameTransform {
                parent_frame_id: config.base_frame_id.clone(),
                child_frame_id: wheel.joint_id.clone(),
                x_m: wheel.longitudinal_offset_m,
                y_m: side * config.wheel_base_m / 2.0,
                yaw_rad: 0.0,
            }));
        }
        FrameTree {
            transforms,
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

/// The official kinematics service implementation.
#[derive(Clone, Copy, Debug, Default)]
pub struct Kinematics;

impl crate::api::projections::Projections for Kinematics {
    type State = KinematicsState;

    fn odometry(&self, state: &KinematicsState) -> OdometryState {
        state.odometry()
    }

    fn frames(&self, state: &KinematicsState) -> FrameTree {
        state.frames()
    }

    fn status(&self, state: &KinematicsState) -> KinematicsStatus {
        state.status()
    }
}

#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for Kinematics {
    type Config = KinematicsConfig;
    type State = KinematicsState;

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
        inputs
            .lookup_frame
            .validate_order()
            .map_err(|error| anyhow::anyhow!(error))?;
        let mut outputs = Self::Outputs::default();
        match measurements::collect(
            &state.config,
            &mut state.encoders,
            &inputs.encoders,
            ctx.now(),
        ) {
            Ok(cut) => {
                let dt = ctx.elapsed().as_nanos() as f64 / 1_000_000_000.0;
                let delta = cut.angular_radps * dt;
                let half = delta / 2.0;
                let scale = if half.abs() < 1e-8 {
                    1.0 - half * half / 6.0
                } else {
                    half.sin() / half
                };
                let distance = cut.linear_mps * dt * scale;
                let heading = state.yaw_rad + half;
                let x = state.x_m + distance * heading.cos();
                let y = state.y_m + distance * heading.sin();
                if !x.is_finite() || !y.is_finite() || !delta.is_finite() {
                    return Err(anyhow::anyhow!("odometry integration overflow"));
                }
                state.x_m = x;
                state.y_m = y;
                state.yaw_rad = normalize_yaw(state.yaw_rad + delta);
                state.linear_x_mps = cut.linear_mps;
                state.angular_z_radps = cut.angular_radps;
                state.oldest_capture_time_nanos = Some(cut.oldest_capture_time_nanos);
                state.revision = state
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("odometry revision overflow"))?;
                state.available = true;
                state.unavailable_reasons.clear();
                state.retain_frames(state.frames());
                outputs.joints(cut.joints)?;
            }
            Err(reason) => {
                state.available = false;
                state.linear_x_mps = 0.0;
                state.angular_z_radps = 0.0;
                state.unavailable_reasons = vec![reason as i32];
            }
        }
        validation::odometry(&state.odometry()).map_err(|error| anyhow::anyhow!(error))?;
        validation::frame_tree(&state.frames()).map_err(|error| anyhow::anyhow!(error))?;
        validation::status(&state.status()).map_err(|error| anyhow::anyhow!(error))?;
        for command in inputs.lookup_frame.items() {
            let response = lookup_frame(&state, command.request());
            validation::lookup_response(&response).map_err(|error| anyhow::anyhow!(error))?;
            outputs.lookup_frame_reply(command.reply(response))?;
        }
        Ok((state, outputs))
    }
}

fn lookup_frame(state: &KinematicsState, request: &LookupFrameRequest) -> LookupFrameResponse {
    let current = state.frames();
    if validation::lookup_request(request).is_err() {
        return LookupFrameResponse {
            transform: None,
            revision: current.revision,
        };
    }
    let tree = if request.revision == 0 {
        &current
    } else if let Some(tree) = state
        .frame_history
        .iter()
        .find(|tree| tree.revision == request.revision)
    {
        tree
    } else {
        return LookupFrameResponse {
            transform: None,
            revision: current.revision,
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

fn normalize_yaw(yaw: f64) -> f64 {
    let two_pi = 2.0 * std::f64::consts::PI;
    (yaw + std::f64::consts::PI).rem_euclid(two_pi) - std::f64::consts::PI
}

#[cfg(test)]
mod tests;
