//! Bus-ABI golden bindings against the train-selected API tree.
//!
//! The pure bus mechanics (encoding-string parsing, the codec fast-reject in
//! `decode_sample`, codec round-trips, key-root validation) are unit tested in
//! the `phoxal-bus` crate against a hand-written `ContractBody`. These pin the
//! *engine-level* binding instead: that the `phoxal_api_tree!` generated
//! `ContractBody`/`ApiVersion` impls for the train-selected revision flow through `phoxal::bus`
//! exactly as published, and that `TOPIC` is version-qualified. That makes them
//! integration tests against the real API tree, not bus unit tests.

use phoxal::api;
use phoxal::bus::{
    BusMetadata, CodecId, ContractBody, ParticipantSourceIdentity, ProducerId, RobotInstant,
    SourceAttribution, TimeWindow, TimelineId,
};

#[test]
fn encoding_string_carries_only_the_codec() {
    assert_eq!(CodecId::MessagePack.encoding_string(), "phoxal/v0;codec=1");
}

/// The revision is folded into the wire key, so the current v0.2 contract
/// publishes on a key that cannot collide with the immutable v0.1 contract.
#[test]
fn contract_body_topic_is_version_qualified_on_the_real_tree() {
    assert_eq!(
        <api::drive::Target as ContractBody>::TOPIC,
        "v0.2/drive/target"
    );
    assert_eq!(
        <api::supervisor::asset::GetRequest as ContractBody>::TOPIC,
        "v0.2/supervisor/asset/get"
    );
}

#[test]
fn bus_metadata_for_a_real_body_round_trips() {
    let timeline = TimelineId::mint();
    let meta = BusMetadata {
        codec: CodecId::MessagePack.as_u8(),
        sequence: 9,
        produced_at: Some(TimeWindow::exact(RobotInstant::new(timeline, 42))),
        source: SourceAttribution::Participant(ParticipantSourceIdentity::new(
            phoxal_bus::ParticipantId::new("tester").expect("test participant"),
            ProducerId::try_from((1_u128 << 124) | 1).expect("a test producer is canonical"),
        )),
    };
    assert_eq!(BusMetadata::decode(&meta.encode().unwrap()).unwrap(), meta);

    // A command or diagnostic expresses no robot time, and that absence round
    // trips as absence rather than as a zero instant.
    let timeless = BusMetadata {
        produced_at: None,
        ..meta
    };
    let decoded = BusMetadata::decode(&timeless.encode().unwrap()).unwrap();
    assert_eq!(decoded, timeless);
    assert_eq!(decoded.produced_exactly_at(), None);
}
