use phoxal::contract::CallMethod;
use phoxal::runtime::{
    CallCompletion, Completions, ExecutionDuration, ExecutionTime, Outputs, StepContext,
};

#[derive(Clone, PartialEq, prost::Message)]
struct Request {}

#[derive(Clone, PartialEq, prost::Message)]
struct Expected {
    #[prost(uint32, tag = "1")]
    value: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
struct Wrong {
    #[prost(string, tag = "1")]
    value: String,
}

const CALL: CallMethod<Request, Expected> = CallMethod::new(
    "example.Service",
    "Call",
    "call",
    ".example.Request",
    ".example.Expected",
    None,
    &[],
);

fn main() {
    let context = StepContext::first(
        ExecutionTime::default(),
        ExecutionDuration::from_millis(1),
    );
    let mut outputs = Outputs::default();
    let ticket = outputs.send(&context, CALL.bind("service", Request {})).unwrap();
    let completions = Completions::default();
    let _: Option<CallCompletion<Wrong>> = completions.get(&ticket);
}
