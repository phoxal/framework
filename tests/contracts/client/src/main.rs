//! Contract-only client: consumes the manifest-authored consumer contract
//! without linking any provider implementation, runtime, or hardware facade.

phoxal::api!();

use api::consumer;
use phoxal::contract::{Call, Empty, MethodShape, Observation};

fn main() {
    // Instance bindings carry the same contract identities without naming a
    // Rust provider type or linking a runtime.
    let status: Observation<api::consumer::ConsumerStatus> = consumer::status();
    let inspect: Call<Empty, api::consumer::ConsumerStatus> = consumer::inspect(Empty {});

    let status = status.signature();
    let inspect = inspect.signature();
    assert_eq!(status.shape, MethodShape::Observation);
    assert!(status.retained_latest);
    assert_eq!(
        status.service, "example.contract_evaluation.v1.ConsumerStatus",
        "data endpoints are keyed by their qualified message identity"
    );
    assert_eq!(inspect.shape, MethodShape::Call);
    assert_eq!(
        inspect.service, "example.contract_evaluation.v1.InspectConsumer",
        "operation identity comes from the declared contract"
    );
    println!("contract-only client resolved every generated binding");
}
