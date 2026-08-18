//! Bus-ABI golden bindings against the real api tree.
//!
//! The pure bus mechanics (encoding-string parsing, codec fast-rejects,
//! codec round-trips, and key-root validation) are unit tested in the
//! `phoxal::bus` module. These integration tests pin the keys the tree renders
//! and prove that their family-rooted topics and metadata wire format reach the
//! participant facade unchanged.

use phoxal::api;
use phoxal::bus::{
    BusMetadata, CodecId, ParticipantSourceIdentity, ProducerId, RobotInstant, SourceAttribution,
    TimeWindow, TimelineId,
};
use phoxal::supervisor::api as supervisor;

#[test]
fn encoding_string_carries_only_the_codec() {
    assert_eq!(CodecId::MessagePack.encoding_string(), "phoxal/v0;codec=1");
}

/// Every tree roots its keys at its own semantic namespace, so contracts from
/// different families cannot collide on transport keys.
#[test]
fn endpoint_topic_is_family_rooted_on_the_real_tree() {
    assert_eq!(api::topics().drive().target().key(), "robot/drive/target");
    assert_eq!(
        supervisor::topics().bundle().get().key(),
        "supervisor/bundle/get"
    );
}

#[test]
fn bus_metadata_for_a_real_endpoint_round_trips() {
    let timeline = TimelineId::mint();
    let meta = BusMetadata {
        codec: CodecId::MessagePack.as_u8(),
        sequence: 9,
        stream_position: None,
        produced_at: Some(TimeWindow::exact(RobotInstant::new(timeline, 42))),
        source: SourceAttribution::Participant(ParticipantSourceIdentity::new(
            phoxal::bus::ParticipantId::new("tester").expect("test participant"),
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
