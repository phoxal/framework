//! Focused Motion contract runtime used by the packaged-contract qualification.

use phoxal::contract::Empty;
use phoxal::runtime::input::Commands;
use phoxal::runtime::{InitContext, Runtime, StepContext};
use phoxal_service_motion::{
    ApplyEmergencyResponse, ControlMode, EmergencyAccepted, MotionStatus, apply_emergency_response,
    motion,
};

#[derive(Clone, Copy, Debug, Default)]
struct MotionContractRuntime;

#[phoxal::runtime::inputs]
struct MotionInputs {
    #[phoxal::runtime::input(
        port = motion::methods::DISARM.__commands_port(),
        max_items = 8,
        max_bytes = 4096
    )]
    disarm: Commands<Empty, ApplyEmergencyResponse>,
}

#[derive(Default)]
#[phoxal::runtime::outputs]
struct MotionOutputs {
    #[phoxal::runtime::outputs::reply(disarm, max_items = 8, max_bytes = 4096)]
    disarm_replies: Vec<phoxal::runtime::Reply<ApplyEmergencyResponse>>,
}

#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for MotionContractRuntime {
    type Config = ();
    type State = u64;
    type Inputs = MotionInputs;
    type Outputs = MotionOutputs;

    fn init(&self, _ctx: &InitContext, _config: Self::Config) -> phoxal::Result<Self::State> {
        Ok(0)
    }

    fn step(
        &self,
        _ctx: &StepContext,
        state: Self::State,
        inputs: &Self::Inputs,
    ) -> phoxal::Result<(Self::State, Self::Outputs)> {
        let mut outputs = MotionOutputs::default();
        for command in inputs.disarm.items() {
            outputs
                .disarm_replies
                .push(command.reply(ApplyEmergencyResponse {
                    decision: Some(apply_emergency_response::Decision::Accepted(
                        EmergencyAccepted {},
                    )),
                }));
        }
        Ok((state.saturating_add(1), outputs))
    }
}

#[phoxal::runtime::outputs]
impl MotionContractRuntime {
    #[phoxal::runtime::outputs::state(
        port = motion::methods::STATUS.__state_port(),
        max_bytes = 4096,
        bootstrap
    )]
    fn status(&self, _state: &u64) -> MotionStatus {
        MotionStatus {
            mode: ControlMode::Disarmed.into(),
            emergency_latched: false,
            selected_owner_id: None,
            protective_state_clear: false,
            stopped: true,
        }
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run(MotionContractRuntime)
}
