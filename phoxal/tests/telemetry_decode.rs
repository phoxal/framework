#![cfg(feature = "preview-v2")]

//! A malformed `v2::telemetry::Process` payload, pushed through a live bus
//! subscription, must be surfaced as a decode error (counted, not silently
//! accepted) - and a well-formed one on the same key must still decode.
//!
//! `phoxal-api`'s round-trip tests only prove encode+decode are self-consistent
//! (both could drift together and still pass); this exercises the real
//! subscriber decode path (`spawn_subscription` -> `decode_sample` ->
//! `health().decode_errors`) with a hand-injected bad payload, the guarantee the
//! CLI telemetry consumer relies on. `phoxal-bus` cannot host this test: it must
//! not depend on `phoxal-api` (that would be circular), so the v2 body is
//! only nameable from the engine crate up.

use std::sync::atomic::Ordering;
use std::time::Duration;

use phoxal::raw::{
    Bus, BusConfig, BusMetadata, Codec, LogicalTime, MessagePack, OwnerCap, Publisher, Source,
    Subscriber, encoding_string,
};
use phoxal_api::{v1, v2};
use serial_test::serial;
use zenoh::bytes::{Encoding, ZBytes};

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn v2_process_subscription_counts_a_malformed_payload_as_a_decode_error() {
    let bus = Bus::open(BusConfig::in_process("test/telemetry-decode", "robot"))
        .await
        .expect("bus should open");

    // Client-side (subscribe) topic, exactly as the CLI telemetry consumer builds it.
    let sub_topic = v2::topic::new().telemetry().process();
    let subscriber = Subscriber::<v2::telemetry::Process>::new(&bus, &sub_topic, 8)
        .await
        .expect("subscription should declare");

    // A well-formed sample on the same key must decode and be delivered - proves
    // the subscription is live, so the decode-error assertion below can't be a
    // false pass from a dead subscription.
    let owner_topic = v2::topic::internal::new(OwnerCap::__mint())
        .telemetry()
        .process();
    let publisher = Publisher::<v2::telemetry::Process>::new(bus.clone(), &owner_topic)
        .expect("owner publisher should attach");
    let good = v2::telemetry::Process {
        cpu_pct: 4.25,
        rss_bytes: 33_554_432,
        window_ns: 3_000_000_000,
    };
    publisher
        .publish_at(LogicalTime::new(0, 1), good.clone())
        .await
        .expect("good sample should publish");

    let received = tokio::time::timeout(Duration::from_secs(5), subscriber.recv())
        .await
        .expect("a well-formed sample should arrive before the deadline")
        .expect("recv should not error");
    assert_eq!(received.body, good);
    assert_eq!(
        bus.health().decode_errors.load(Ordering::Relaxed),
        0,
        "a well-formed sample must not be counted as a decode error"
    );

    // Now inject a corrupt body payload on the SAME key. The envelope (codec +
    // metadata attachment) is valid so it passes the fast-reject; only the body
    // fails to decode - the exact drift a round-trip test cannot catch. 0xc1 is
    // MessagePack's never-used marker byte.
    let full_key = bus.full_key(sub_topic.key());
    let metadata = BusMetadata {
        codec: MessagePack::ID.as_u8(),
        produced_at_ns: 2,
        epoch: 0,
        source: Source {
            participant: "malformed-injector".to_string(),
            incarnation: 0,
            sequence: 0,
        },
    };
    bus.session()
        .put(full_key.as_str(), ZBytes::from(vec![0xc1u8, 0xc1, 0xc1]))
        .encoding(Encoding::from(encoding_string(MessagePack::ID)))
        .attachment(metadata.encode())
        .await
        .expect("raw malformed put should enqueue");

    // The decode error is surfaced asynchronously by the subscription task.
    // Bounded wait (never an open-ended loop): poll the counter, fail on the
    // deadline rather than hang.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if bus.health().decode_errors.load(Ordering::Relaxed) >= 1 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the malformed payload was not counted as a decode error within the deadline"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // The malformed sample must NOT have been delivered as a body.
    assert!(
        subscriber.try_recv().is_none(),
        "a payload that fails to decode must never be delivered to the subscriber"
    );

    bus.close().await.expect("bus should close");
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn v1_behavior_event_subscription_counts_a_malformed_payload() {
    let bus = Bus::open(BusConfig::in_process("test/behavior-decode", "robot"))
        .await
        .expect("bus should open");
    let topic = v1::topic::new().behavior().event();
    let subscriber = Subscriber::<v1::behavior::Event>::new(&bus, &topic, 8)
        .await
        .expect("subscription should declare");
    let owner = v1::topic::internal::new(OwnerCap::__mint())
        .behavior()
        .event();
    let publisher = Publisher::<v1::behavior::Event>::new(bus.clone(), &owner)
        .expect("owner publisher should attach");
    let good = v1::behavior::Event {
        sequence: 1,
        execution_id: Some("execution-1".to_string()),
        request_id: Some(v1::behavior::RequestId {
            value: "request-1".to_string(),
        }),
        behavior_id: Some("system.root".to_string()),
        content_hash: Some("sha256:test".to_string()),
        node_path: None,
        kind: v1::behavior::EventKind::RequestAccepted,
        failure: None,
        participant_id: "behavior".to_string(),
        logical_time_ns: 1,
    };
    publisher
        .publish_at(LogicalTime::new(0, 1), good.clone())
        .await
        .expect("good sample should publish");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), subscriber.recv())
            .await
            .expect("good sample timeout")
            .expect("good sample receive")
            .body,
        good
    );

    let full_key = bus.full_key(topic.key());
    let metadata = BusMetadata {
        codec: MessagePack::ID.as_u8(),
        produced_at_ns: 2,
        epoch: 0,
        source: Source {
            participant: "malformed-injector".to_string(),
            incarnation: 0,
            sequence: 0,
        },
    };
    bus.session()
        .put(full_key.as_str(), ZBytes::from(vec![0xc1u8, 0xc1, 0xc1]))
        .encoding(Encoding::from(encoding_string(MessagePack::ID)))
        .attachment(metadata.encode())
        .await
        .expect("raw malformed put should enqueue");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while bus.health().decode_errors.load(Ordering::Relaxed) == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "malformed behavior event was not counted before the deadline"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(subscriber.try_recv().is_none());
    bus.close().await.expect("bus should close");
}
