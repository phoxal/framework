//! Bus-ABI golden bindings against the real `y2026_1` API tree.
//!
//! The pure bus mechanics (encoding-string parsing, the `decode_sample`
//! family/api/codec fast-reject, codec round-trips, namespace validation) live as
//! unit tests in the `phoxal-bus` crate, exercised there against a hand-written
//! `ContractBody`. These tests pin the *engine-level* binding: that the
//! `phoxal_api_tree!`-generated `ContractBody`/`ApiVersion` impls for `y2026_1`
//! flow through the re-exported bus `bus_abi` slots (encoding string + metadata)
//! exactly as published on the wire. The live publish/subscribe + query paths are
//! covered end-to-end by `interop.rs` / `interop_dynamic.rs`.

use phoxal::bus::{BUS_ABI, BusMetadata, CodecId, Source, encoding_string};
use phoxal_api::y2026_1 as api;
use phoxal_api::{ApiVersion, ContractBody};

#[test]
fn bus_abi_id_is_frozen() {
    assert_eq!(BUS_ABI.id(), "phoxal-bus/v0");
}

#[test]
fn encoding_string_mirrors_a_real_contract_body() {
    let enc = encoding_string(
        <api::drive::Target as ContractBody>::FAMILY,
        <<api::drive::Target as ContractBody>::Api as ApiVersion>::ID,
        <api::drive::Target as ContractBody>::SCHEMA_ID,
        CodecId::MessagePack,
    );
    assert_eq!(
        enc,
        format!(
            "phoxal/v0;family=drive::Target;api=y2026_1;schema_id={};codec=1",
            <api::drive::Target as ContractBody>::SCHEMA_ID
        )
    );
}

#[test]
fn encoding_string_mirrors_a_real_query_request_body() {
    let enc = encoding_string(
        <api::asset::GetRequest as ContractBody>::FAMILY,
        <<api::asset::GetRequest as ContractBody>::Api as ApiVersion>::ID,
        <api::asset::GetRequest as ContractBody>::SCHEMA_ID,
        CodecId::MessagePack,
    );
    assert_eq!(
        enc,
        format!(
            "phoxal/v0;family=asset::GetRequest;api=y2026_1;schema_id={};codec=1",
            <api::asset::GetRequest as ContractBody>::SCHEMA_ID
        )
    );
}

#[test]
fn bus_metadata_for_a_real_body_round_trips() {
    let meta = BusMetadata {
        api_version: <<api::drive::Target as ContractBody>::Api as ApiVersion>::ID.to_string(),
        schema_id: <api::drive::Target as ContractBody>::SCHEMA_ID.to_string(),
        family: <api::drive::Target as ContractBody>::FAMILY.to_string(),
        codec: CodecId::MessagePack.as_u8(),
        produced_at_ns: 42,
        epoch: 1,
        source: Source {
            participant: "tester".to_string(),
            incarnation: 3,
            sequence: 9,
        },
    };
    assert_eq!(meta.api_version, "y2026_1");
    assert_eq!(meta.schema_id.len(), 16);
    assert_eq!(meta.family, "drive::Target");
    assert_eq!(BusMetadata::decode(&meta.encode()).unwrap(), meta);
}
