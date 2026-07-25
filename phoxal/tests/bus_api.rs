//! Bus-ABI golden bindings against the train-selected API tree.
//!
//! The pure bus mechanics (encoding-string parsing, the codec fast-reject in
//! `decode_sample`, codec round-trips, namespace validation) live as unit tests
//! in the `phoxal-bus` crate, exercised there against a hand-written
//! `ContractBody`. These tests pin the *engine-level* binding: that the
//! `phoxal_api_tree!`-generated `ContractBody`/`ApiVersion` impls for `v0_1`
//! flow through the re-exported bus wire slots (encoding string + metadata)
//! exactly as published on the wire, and that `TOPIC` is version-qualified
//! (D1). The live publish/subscribe + query paths are covered end-to-end by
//! `interop.rs` / `interop_dynamic.rs`.

use phoxal::api;
use phoxal::bus::ContractBody;
use phoxal::bus::{
    BusMetadata, CodecId, ProducerId, RobotInstant, TimeWindow, TimelineId, encoding_string,
};

#[test]
fn encoding_string_carries_only_the_codec() {
    let enc = encoding_string(CodecId::MessagePack);
    assert_eq!(enc, "phoxal/v0;codec=1");
}

#[test]
fn contract_body_topic_is_version_qualified_on_the_real_tree() {
    // D1: the revision is folded into the wire key, so a real v0.1 contract
    // publishes on a key that cannot collide with a hypothetical v0.2
    // contract of the same leaf name.
    assert_eq!(
        <api::drive::Target as ContractBody>::TOPIC,
        "v0.1/drive/target"
    );
    assert_eq!(
        <api::asset::GetRequest as ContractBody>::TOPIC,
        "v0.1/asset/get"
    );
}

#[test]
fn bus_metadata_for_a_real_body_round_trips() {
    let timeline = TimelineId::mint();
    let meta = BusMetadata {
        codec: CodecId::MessagePack.as_u8(),
        producer: ProducerId::mint(),
        sequence: 9,
        produced_at: Some(TimeWindow::exact(RobotInstant::new(timeline, 42))),
        participant: "tester".to_string(),
    };
    assert_eq!(BusMetadata::decode(&meta.encode()).unwrap(), meta);

    // A command or diagnostic expresses no robot time, and that absence round
    // trips as absence rather than as a zero instant.
    let timeless = BusMetadata {
        produced_at: None,
        ..meta
    };
    let decoded = BusMetadata::decode(&timeless.encode()).unwrap();
    assert_eq!(decoded, timeless);
    assert_eq!(decoded.produced_exactly_at(), None);
}
