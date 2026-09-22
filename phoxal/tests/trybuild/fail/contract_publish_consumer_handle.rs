use phoxal::contract::ObservationMethod;

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
    let observation = STATUS.bind("service");
    let _ = observation.__state_port();
}
