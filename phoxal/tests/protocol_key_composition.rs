//! The composed form of a protocol key, proven end to end.
//!
//! `phoxal_api_tree!`'s protocol mode emits *relative* keys whose leading
//! segment is the protocol name, and the bus session mounts everything under
//! `phoxal/<execution-id>`. Each half is unit tested in its own crate, and
//! neither can see the composed key that the macro rustdoc advertises, so a
//! change to either root convention would drift it silently. This test opens a
//! real in-process session and asserts the final key.

use phoxal_bus::{BusConfig, BusOwner, ContractBody};
use phoxal_macros::phoxal_api_tree;

phoxal_api_tree! {
    protocol supervisor {
        connect {
            #[serde(tag = "schema")]
            enum Hello {
                #[serde(rename = "supervisor.hello/v0")]
                V0 { token: String },
            }

            topic hello: command Hello;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_protocol_topic_composes_under_the_execution_scoped_root() {
    let config = BusConfig::in_process(
        phoxal_bus::ParticipantId::new("composition").expect("valid participant id"),
    );
    let execution = config.execution;
    let (owner, bus) = BusOwner::open(config).await.expect("open in-process bus");

    // The macro's half: relative, protocol-led, no revision segment.
    assert_eq!(
        <supervisor::connect::Hello as ContractBody>::TOPIC,
        "supervisor/connect/hello"
    );
    // The session's half: the execution is the whole root.
    assert_eq!(bus.root(), format!("phoxal/{execution}"));
    // Composed, this is the key a supervisor protocol actually speaks on.
    assert_eq!(
        bus.full_key(<supervisor::connect::Hello as ContractBody>::TOPIC),
        format!("phoxal/{execution}/supervisor/connect/hello")
    );

    owner.close().await.expect("close in-process bus");
}
