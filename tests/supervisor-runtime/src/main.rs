//! A compiled, input-free Runtime used by the supervisor process-boundary test.
//!
//! The binary owns one generated call, one retained inspection observation,
//! and the motion-input projections the public-session qualification feeds to
//! the installed Motion executable, so the process and public-session tests
//! exercise actual method admission.

use std::path::PathBuf;

use phoxal::runtime::{InitContext, Runtime, RuntimeLaunch, StepContext};
phoxal::api!();
use crate::api::types::example::inspection::v1::{InspectionReadResponse, InspectionState};
use crate::api::types::phoxal::kinematics::v1::OdometryState;
use crate::api::types::phoxal::motion::v1::{MotionConstraints, Permission};

struct ReferenceRuntime {
    marker: PathBuf,
}

impl crate::api::projections::Projections for ReferenceRuntime {
    type State = u64;

    fn status(&self, state: &u64) -> InspectionState {
        InspectionState {
            count: *state,
            active: true,
        }
    }

    fn constraints(&self, now: &u64) -> MotionConstraints {
        MotionConstraints {
            sequence: *now,
            permission: Permission::Clear as i32,
            constraints: Vec::new(),
            valid_from_nanos: *now,
            expires_at_nanos: now.saturating_add(1_000_000_000),
            oldest_capture_time_nanos: None,
        }
    }

    fn odometry(&self, now: &u64) -> OdometryState {
        OdometryState {
            x_m: 0.0,
            y_m: 0.0,
            yaw_rad: 0.0,
            linear_x_mps: 0.0,
            angular_z_radps: 0.0,
            revision: *now,
            available: true,
            oldest_capture_time_nanos: None,
        }
    }
}

#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for ReferenceRuntime {
    type Config = ();
    type State = u64;

    fn init(&self, _ctx: &InitContext, _config: Self::Config) -> phoxal::Result<Self::State> {
        std::fs::write(&self.marker, b"initialized")?;
        Ok(0)
    }

    fn step(
        &self,
        _ctx: &StepContext,
        state: Self::State,
        inputs: &Self::Inputs,
    ) -> phoxal::Result<(Self::State, Self::Outputs)> {
        if state == 0 {
            std::fs::write(&self.marker, b"stepped")?;
        }
        let next = state.saturating_add(1);
        let mut outputs = Self::Outputs::default();
        for command in inputs.read.items() {
            outputs.read_reply(command.reply(InspectionReadResponse {
                state: Some(InspectionState {
                    count: next,
                    active: command.request().key == "status",
                }),
            }))?;
        }
        Ok((next, outputs))
    }
}

fn main() -> phoxal::Result<()> {
    let bundle_root = RuntimeLaunch::parse()?.bundle_root;
    phoxal::runtime::run(ReferenceRuntime {
        marker: bundle_root.join("reference-runtime.marker"),
    })
}
