//! Live input source for the installed Motion executable qualification.

phoxal::api!();

use crate::api::__contracts::phoxal::kinematics::v1::OdometryState;
use crate::api::__contracts::phoxal::motion::v1::{MotionConstraints, Permission};
use crate::api::__contracts::phoxal::test_inputs::v1::test_inputs;
use phoxal::runtime::{InitContext, Runtime, StepContext};

struct MotionInputs;

#[phoxal::runtime::inputs]
struct Inputs {}

#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for MotionInputs {
    type Config = ();
    type State = u64;
    type Inputs = Inputs;
    type Outputs = ();

    fn init(&self, _ctx: &InitContext, _config: ()) -> phoxal::Result<u64> {
        Ok(0)
    }

    fn step(&self, ctx: &StepContext, _state: u64, _inputs: &Inputs) -> phoxal::Result<(u64, ())> {
        Ok((ctx.now().as_nanos(), ()))
    }
}

#[phoxal::runtime::outputs]
impl MotionInputs {
    #[phoxal::runtime::outputs::state(
        port = test_inputs::methods::CONSTRAINTS.__state_port(),
        max_bytes = 4096,
        bootstrap
    )]
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

    #[phoxal::runtime::outputs::state(
        port = test_inputs::methods::ODOMETRY.__state_port(),
        max_bytes = 512,
        bootstrap
    )]
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

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run(MotionInputs)
}
