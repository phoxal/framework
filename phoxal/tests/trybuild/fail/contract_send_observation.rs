use phoxal::contract::ObservationMethod;
use phoxal::runtime::{ExecutionDuration, ExecutionTime, Outputs, StepContext};

#[derive(Clone, PartialEq, prost::Message)]
struct Status {
    #[prost(uint32, tag = "1")]
    value: u32,
}

const STATUS: ObservationMethod<Status> = ObservationMethod::new(
    "example.Service",
    "Status",
    "status",
    ".google.protobuf.Empty",
    ".example.Status",
    true,
    None,
    &[],
);

fn main() {
    let context = StepContext::first(
        ExecutionTime::default(),
        ExecutionDuration::from_millis(1),
    );
    let mut outputs = Outputs::default();
    let _ = outputs.send(&context, STATUS.bind("service"));
}
