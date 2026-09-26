//! Robot brain for the contract-evaluation composition, authored from
//! `service.yaml`: the endpoint surface (cross-service `inspect` requirement
//! and the served `report` operation) is generated; this file owns only the
//! probe tally and pacing.

phoxal::api!();

use crate::api::types::example::probe::v1::ProbeReport;
use phoxal::contract::Empty;
use phoxal::runtime::{CallTicket, InitContext, Runtime, StepContext};

use crate::api::consumer::ConsumerStatus;

#[derive(Default)]
struct BrainState {
    pending: Option<CallTicket<ConsumerStatus>>,
    completions: u64,
    failures: u64,
    last_phase: String,
    step: u64,
}

/// Probe cadence: one generated operation every 25 periods (500 ms).  A
/// runtime caller paces itself against the receiver's declared ingress
/// bound; per-step calling can outrun a slower receiver under controlled
/// scheduling.
const PROBE_EVERY_STEPS: u64 = 25;
const PROBE_WARMUP_STEPS: u64 = 50;

struct Brain;

fn report(state: &BrainState) -> ProbeReport {
    ProbeReport {
        inspect_completions: state.completions,
        inspect_failures: state.failures,
        last_phase: state.last_phase.clone(),
    }
}

#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for Brain {
    type Config = ();
    type State = BrainState;

    fn init(&self, _ctx: &InitContext, _config: ()) -> phoxal::Result<Self::State> {
        Ok(BrainState::default())
    }

    fn step(
        &self,
        ctx: &StepContext,
        mut state: Self::State,
        inputs: &Self::Inputs,
    ) -> phoxal::Result<(Self::State, Self::Outputs)> {
        if let Some(ticket) = state.pending.take() {
            match inputs.inspect.get(&ticket) {
                Some(completion) => match completion.response() {
                    Ok(status) => {
                        state.completions = state.completions.saturating_add(1);
                        state.last_phase = status.phase.clone();
                    }
                    Err(_) => state.failures = state.failures.saturating_add(1),
                },
                None => state.pending = Some(ticket),
            }
        }
        let mut outputs = Self::Outputs::default();
        for request in inputs.report.items() {
            outputs.report_reply(request.reply(report(&state)))?;
        }
        state.step = state.step.saturating_add(1);
        // One paced robot-brain-initiated operation through the shared
        // generated robot API.
        if state.pending.is_none()
            && state.step >= PROBE_WARMUP_STEPS
            && (state.step - PROBE_WARMUP_STEPS).is_multiple_of(PROBE_EVERY_STEPS)
        {
            let ticket = outputs.send(ctx, crate::api::calls::inspect(Empty {}))?;
            state.pending = Some(ticket);
        }
        Ok((state, outputs))
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run(Brain)
}
