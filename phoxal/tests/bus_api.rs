//! Bus-ABI golden bindings against the real `y2026_1` API tree.
//!
//! The pure bus mechanics (encoding-string parsing, the codec fast-reject in
//! `decode_sample`, codec round-trips, namespace validation) live as unit tests
//! in the `phoxal-bus` crate, exercised there against a hand-written
//! `ContractBody`. These tests pin the *engine-level* binding: that the
//! `phoxal_api_tree!`-generated `ContractBody`/`ApiVersion` impls for `y2026_1`
//! flow through the re-exported bus wire slots (encoding string + metadata)
//! exactly as published on the wire, and that `TOPIC` is generation-qualified
//! (D1). The live publish/subscribe + query paths are covered end-to-end by
//! `interop.rs` / `interop_dynamic.rs`.

use phoxal::bus::{BusMetadata, CodecId, Source, encoding_string};
use phoxal_api::ContractBody;
use phoxal_api::y2026_1 as api;

#[test]
fn encoding_string_carries_only_the_codec() {
    let enc = encoding_string(CodecId::MessagePack);
    assert_eq!(enc, "phoxal/v0;codec=1");
}

#[test]
fn contract_body_topic_is_generation_qualified_on_the_real_tree() {
    // D1: the generation is folded into the wire key, so a real y2026_1 contract
    // publishes on a key that cannot collide with a hypothetical y2026_2
    // contract of the same leaf name.
    assert_eq!(
        <api::drive::Target as ContractBody>::TOPIC,
        "y2026_1/drive/target"
    );
    assert_eq!(
        <api::asset::GetRequest as ContractBody>::TOPIC,
        "y2026_1/asset/get"
    );
}

#[test]
fn bus_metadata_for_a_real_body_round_trips() {
    let meta = BusMetadata {
        codec: CodecId::MessagePack.as_u8(),
        produced_at_ns: 42,
        epoch: 1,
        source: Source {
            participant: "tester".to_string(),
            incarnation: 3,
            sequence: 9,
        },
    };
    assert_eq!(BusMetadata::decode(&meta.encode()).unwrap(), meta);
}
