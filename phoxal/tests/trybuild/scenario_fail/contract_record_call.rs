use phoxal::contract::CallMethod;
use phoxal::scenario::{CapturePolicy, Simulation};

#[derive(Clone, PartialEq, prost::Message)]
struct Request {}

#[derive(Clone, PartialEq, prost::Message)]
struct Response {}

const CALL: CallMethod<Request, Response> = CallMethod::new(
    "example.Service",
    "Call",
    "call",
    ".example.Request",
    ".example.Response",
    None,
    &[],
);

fn main() -> phoxal::Result<()> {
    let mut sim = Simulation::from_context("record-call")?;
    let mut plan = sim.plan();
    let _ = plan.record(
        CALL.bind("service", Request {}),
        CapturePolicy::required_history(1)?,
    )?;
    Ok(())
}
