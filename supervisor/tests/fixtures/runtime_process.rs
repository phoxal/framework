//! A compiled, input-free Runtime used by the supervisor process-boundary test.
//!
//! The binary owns one generated call and one retained observation so the
//! process and public-session tests exercise actual method admission.

use std::path::PathBuf;

use phoxal::runtime::input::Commands;
use phoxal::runtime::{InitContext, Runtime, RuntimeLaunch, StepContext};
phoxal::api!();
use crate::api::__contracts::example::inspection::v1::{
    InspectionReadRequest, InspectionReadResponse, InspectionState, inspection,
};

struct ReferenceRuntime {
    marker: PathBuf,
}

#[phoxal::runtime::inputs]
struct ReferenceInputs {
    #[phoxal::runtime::input(
        port = inspection::methods::READ.__commands_port(),
        max_items = 8,
        max_bytes = 4096
    )]
    reads: Commands<InspectionReadRequest, InspectionReadResponse>,
}

#[derive(Default)]
#[phoxal::runtime::outputs]
struct ReferenceOutputs {
    #[phoxal::runtime::outputs::reply(reads, max_items = 8, max_bytes = 4096)]
    read_replies: Vec<phoxal::runtime::Reply<InspectionReadResponse>>,
}

#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for ReferenceRuntime {
    type Config = ();
    type State = u64;
    type Inputs = ReferenceInputs;
    type Outputs = ReferenceOutputs;

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
        let mut outputs = ReferenceOutputs::default();
        for command in inputs.reads.items() {
            outputs
                .read_replies
                .push(command.reply(InspectionReadResponse {
                    state: Some(InspectionState {
                        count: next,
                        active: command.request().key == "status",
                    }),
                }));
        }
        Ok((next, outputs))
    }
}

#[phoxal::runtime::outputs]
impl ReferenceRuntime {
    #[phoxal::runtime::outputs::state(
        port = inspection::methods::STATUS.__state_port(),
        max_bytes = 4096,
        bootstrap,
        on_change
    )]
    fn status(&self, state: &u64) -> InspectionState {
        InspectionState {
            count: *state,
            active: true,
        }
    }
}

fn main() -> phoxal::Result<()> {
    let bundle_root = RuntimeLaunch::parse()?.bundle_root;
    phoxal::runtime::run(ReferenceRuntime {
        marker: bundle_root.join("reference-runtime.marker"),
    })
}
