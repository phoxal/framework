//! The composed form of a wire key, proven end to end.
//!
//! `phoxal_api_tree!` emits *relative* keys whose leading segment is the
//! contract family, and the bus session mounts everything under
//! `phoxal/<execution-id>`. Each half is unit tested in its own crate, and
//! neither can see the composed key that the generated rustdoc advertises, so a
//! change to either root convention would drift it silently. This test opens a
//! real in-process session and asserts the final key.

use phoxal_bus::{BusConfig, BusOwner, EndpointDescriptor, ExecutionId};

type ConnectEndpoint = phoxal_api::supervisor::endpoint::connect::TopicEndpoint;

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_family_topic_composes_under_the_execution_scoped_root() {
    let config = BusConfig::for_participant(
        ExecutionId::mint(),
        phoxal_bus::ParticipantId::new("composition").expect("valid participant id"),
        Vec::new(),
    );
    let execution = config.execution();
    let (owner, bus) = BusOwner::open(config).await.expect("open in-process bus");

    // The macro's half: relative, family-led, no revision segment.
    assert_eq!(
        <ConnectEndpoint as EndpointDescriptor>::TOPIC,
        "supervisor/connect"
    );
    // The session's half: the execution is the whole root.
    assert_eq!(bus.root(), format!("phoxal/{execution}"));
    // Composed, this is the key a supervisor client actually speaks on.
    assert_eq!(
        bus.full_key(<ConnectEndpoint as EndpointDescriptor>::TOPIC),
        format!("phoxal/{execution}/supervisor/connect")
    );

    owner.close().await;
}
