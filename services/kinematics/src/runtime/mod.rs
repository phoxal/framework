mod measurements;

use crate::config::{KinematicsConfig, validate_config};
use crate::contract::{
    FrameTransform, FrameTree, KinematicsStatus, LookupFrameRequest, LookupFrameResponse,
    UnavailableReason,
};
use crate::validation;
#[cfg(test)]
use phoxal::contracts::component::encoder::EncoderSample;
use phoxal::contracts::robotics::OdometryState;
use phoxal::runtime::Context;
#[cfg(test)]
use phoxal::runtime::Sample;
#[cfg(test)]
use phoxal::runtime::input::Samples;
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
    unavailable_reasons: Vec<UnavailableReason>,
    frame_history: VecDeque<FrameTree>,
    applied_invocation: u64,
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
            unavailable_reasons: vec![UnavailableReason::Encoder],
            frame_history: VecDeque::new(),
            applied_invocation: u64::MAX,
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
pub struct Kinematics {
    state: KinematicsState,
}

#[phoxal::runtime(contract = crate::contract::KinematicsApi, period_ms = 20)]
impl Kinematics {
    #[init]
    fn new(config: KinematicsConfig) -> phoxal::Result<Self> {
        validate_config(&config)?;
        Ok(Self {
            state: KinematicsState::new(config),
        })
    }

    /// Answers one frame lookup from the current integrated state.
    ///
    /// The invocation's encoder integration runs first when this handler
    /// is the first to observe the invocation, exactly as the unified
    /// step integrated before serving lookups.
    #[handle(lookup_frame)]
    fn lookup(
        &mut self,
        ctx: &mut Context<'_, Self>,
        request: LookupFrameRequest,
    ) -> phoxal::Result<LookupFrameResponse> {
        self.integrate(ctx)?;
        let response = lookup_frame(&self.state, &request);
        validation::lookup_response(&response).map_err(|error| anyhow::anyhow!(error))?;
        Ok(response)
    }

    #[step]
    fn advance(&mut self, ctx: &mut Context<'_, Self>) -> phoxal::Result<()> {
        self.integrate(ctx)?;
        validation::odometry(&self.state.odometry()).map_err(|error| anyhow::anyhow!(error))?;
        validation::frame_tree(&self.state.frames()).map_err(|error| anyhow::anyhow!(error))?;
        validation::status(&self.state.status()).map_err(|error| anyhow::anyhow!(error))?;
        Ok(())
    }

    /// Projects the integrated wheel odometry.
    #[publish(odometry)]
    fn odometry(&self) -> OdometryState {
        self.state.odometry()
    }

    /// Projects the current frame tree.
    #[publish(frames)]
    fn frames(&self) -> FrameTree {
        self.state.frames()
    }

    /// Projects availability separately from the odometry payload.
    #[publish(status)]
    fn status(&self) -> KinematicsStatus {
        self.state.status()
    }

    /// Integrates this invocation's admitted encoder cut exactly once, so
    /// a lookup dispatched before the periodic step observes the same
    /// freshly integrated state the unified step produced.
    fn integrate(&mut self, ctx: &mut Context<'_, Self>) -> phoxal::Result<()> {
        if self.state.applied_invocation == ctx.invocation_index() {
            return Ok(());
        }
        self.state.applied_invocation = ctx.invocation_index();
        match measurements::collect(
            &self.state.config,
            &mut self.state.encoders,
            ctx.encoders(),
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
                let heading = self.state.yaw_rad + half;
                let x = self.state.x_m + distance * heading.cos();
                let y = self.state.y_m + distance * heading.sin();
                if !x.is_finite() || !y.is_finite() || !delta.is_finite() {
                    return Err(anyhow::anyhow!("odometry integration overflow"));
                }
                self.state.x_m = x;
                self.state.y_m = y;
                self.state.yaw_rad = normalize_yaw(self.state.yaw_rad + delta);
                self.state.linear_x_mps = cut.linear_mps;
                self.state.angular_z_radps = cut.angular_radps;
                self.state.oldest_capture_time_nanos = Some(cut.oldest_capture_time_nanos);
                self.state.revision = self
                    .state
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("odometry revision overflow"))?;
                self.state.available = true;
                self.state.unavailable_reasons.clear();
                let frames = self.state.frames();
                self.state.retain_frames(frames);
                for joint in cut.joints {
                    ctx.emit_joints(joint)?;
                }
            }
            Err(reason) => {
                self.state.available = false;
                self.state.linear_x_mps = 0.0;
                self.state.angular_z_radps = 0.0;
                self.state.unavailable_reasons = vec![reason];
            }
        }
        Ok(())
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
