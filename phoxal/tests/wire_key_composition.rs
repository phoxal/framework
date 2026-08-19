//! The composed form of a wire key, proven end to end.
//!
//! The api tree renders *relative* keys whose leading segment is the contract
//! family, and the bus session mounts everything under
//! `phoxal/<execution-id>`. Each half is tested by the module that owns it, and
//! neither can see the composed key a client actually speaks on, so a change to
//! either root convention would drift it silently. This test opens a real
//! in-process session and asserts the final key.

use phoxal::testing::TestBus;

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_family_topic_composes_under_the_execution_scoped_root() {
    let session = TestBus::for_participant("composition")
        .await
        .expect("open in-process bus");
    let execution = session.execution();
    let bus = session.handle();

    // The tree's half: relative, family-led, no revision segment.
    let bootstrap = phoxal::supervisor::api::topics().connect().client();
    assert_eq!(bootstrap.key(), "supervisor/connect");
    // The session's half: the execution is the whole root.
    assert_eq!(bus.root(), format!("phoxal/{execution}"));
    // Composed, this is the key a supervisor client actually speaks on.
    assert_eq!(
        bus.full_key(bootstrap.key()),
        format!("phoxal/{execution}/supervisor/connect")
    );

    session.close().await;
}
