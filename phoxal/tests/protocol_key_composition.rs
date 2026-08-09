//! The composed form of a protocol key, proven end to end.
//!
//! `phoxal_api_tree!`'s protocol mode emits *relative* keys whose leading
//! segment is the protocol name, and the bus session mounts everything under
//! `phoxal/<execution-id>`. Each half is unit tested in its own crate, and
//! neither can see the composed key that the macro rustdoc advertises, so a
//! change to either root convention would drift it silently. This test opens a
//! real in-process session and asserts the final key.

use phoxal_bus::{BusConfig, BusOwner, EndpointDescriptor, ExecutionId};
use phoxal_macros::phoxal_protocol;

mod payload {
    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    #[serde(tag = "schema")]
    pub enum Hello {
        #[serde(rename = "supervisor.hello/v0")]
        V0 { token: String },
    }
}

phoxal_protocol! {
    protocol supervisor {
        connect {
            topic hello: command crate::payload::Hello;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_protocol_topic_composes_under_the_execution_scoped_root() {
    let config = BusConfig::for_participant(
        ExecutionId::mint(),
        phoxal_bus::ParticipantId::new("composition").expect("valid participant id"),
        Vec::new(),
    );
    let execution = config.execution();
    let (owner, bus) = BusOwner::open(config).await.expect("open in-process bus");

    // The macro's half: relative, protocol-led, no revision segment.
    assert_eq!(
        <supervisor::endpoint::connect::HelloEndpoint as EndpointDescriptor>::TOPIC,
        "supervisor/connect/hello"
    );
    // The session's half: the execution is the whole root.
    assert_eq!(bus.root(), format!("phoxal/{execution}"));
    // Composed, this is the key a supervisor protocol actually speaks on.
    assert_eq!(
        bus.full_key(<supervisor::endpoint::connect::HelloEndpoint as EndpointDescriptor>::TOPIC),
        format!("phoxal/{execution}/supervisor/connect/hello")
    );

    owner.close().await;
}
