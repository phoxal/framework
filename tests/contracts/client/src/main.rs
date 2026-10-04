//! Contract-only client: consumes the manifest-authored provider contract
//! without linking any provider implementation, runtime, or hardware facade.

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("Phoxal supports Linux and macOS only");

phoxal::api!();

use api::provider;
use phoxal::contracts::{Call, Empty, MethodShape, Observation};

fn main() {
    // Instance bindings carry the same contract identities without naming a
    // Rust provider type or linking a runtime.
    let status: Observation<api::provider::ClientStatus> = provider::status();
    let inspect: Call<Empty, api::provider::ClientStatus> = provider::inspect(Empty {});

    let status = status.signature();
    let inspect = inspect.signature();
    assert_eq!(status.shape, MethodShape::Observation);
    assert!(status.retained_latest);
    assert_eq!(
        status.service, "example.sdk_client.v0.ClientStatus",
        "data endpoints are keyed by their qualified message identity"
    );
    assert_eq!(inspect.shape, MethodShape::Call);
    assert_eq!(
        inspect.service, "example.sdk_client.v0.InspectClient",
        "operation identity comes from the declared contract"
    );
    println!("contract-only client resolved every generated binding");
}
