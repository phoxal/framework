//! Private identities and package-scoped module declarations.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use prost::Message as _;
use prost::Name as _;

use phoxal::schema::MessageSchema;
use phoxal_schema_proof_fixture::v1::{Snapshot, SnapshotValue, State};
use phoxal_schema_proof_fixture::{MapState, nested};

#[test]
fn private_inputs_derive_owner_qualified_identities() {
    assert_eq!(
        MapState::WIRE_NAME,
        "phoxal.private.phoxal_2dschema_2dproof_2dfixture.phoxal_5fschema_5fproof_5ffixture.MapState",
        "a private input at the crate root carries the package- and target-qualified identity"
    );
    assert_eq!(
        nested::MapState::WIRE_NAME,
        "phoxal.private.phoxal_2dschema_2dproof_2dfixture.phoxal_5fschema_5fproof_5ffixture.nested.MapState",
        "the same type name in another module derives a distinct identity"
    );
    assert_ne!(MapState::WIRE_NAME, nested::MapState::WIRE_NAME);
    assert_eq!(
        MapState::PACKAGE,
        "phoxal.private.phoxal_2dschema_2dproof_2dfixture.phoxal_5fschema_5fproof_5ffixture"
    );
}

#[test]
fn private_inputs_roundtrip_through_prost_and_retain_their_schemas() {
    let value = MapState {
        revision: 7,
        available: true,
        oldest_capture_time_nanos: Some(1234),
    };
    let bytes = value.encode_to_vec();
    let decoded = MapState::decode(bytes.as_slice()).expect("private input decodes");
    assert_eq!(decoded, value);
    assert!(MapState::retain_schema() > 0);
    assert!(nested::MapState::retain_schema() > 0);
}

#[test]
fn a_module_declares_one_package_for_its_messages() {
    assert_eq!(State::WIRE_NAME, "proof.module.v1.State");
    assert_eq!(Snapshot::WIRE_NAME, "proof.module.v1.Snapshot");
    let value = Snapshot {
        state: State::Ready,
        prior: Some(State::Unspecified),
        history: vec![State::Ready, State::Unspecified],
        value: Some(SnapshotValue::Reading(2.5)),
    };
    let bytes = value.encode_to_vec();
    let decoded = Snapshot::decode(bytes.as_slice()).expect("module message decodes");
    assert_eq!(decoded, value);
    assert!(Snapshot::retain_schema() > 0);
    assert!(State::retain_schema() > 0);
}

#[test]
fn qualified_and_renamed_imports_keep_the_authored_identity() {
    // Importing through a qualified module path and under a different
    // local name changes nothing about the authored wire identity.
    use phoxal_schema_proof_fixture::v1::Snapshot as RenamedSnapshot;
    use phoxal_schema_proof_fixture::v1::State as ModuleState;

    assert_eq!(RenamedSnapshot::WIRE_NAME, "proof.module.v1.Snapshot");
    assert_eq!(ModuleState::WIRE_NAME, "proof.module.v1.State");
    let value = RenamedSnapshot {
        state: ModuleState::Ready,
        prior: None,
        history: Vec::new(),
        value: None,
    };
    assert_eq!(
        RenamedSnapshot::decode(value.encode_to_vec().as_slice()).expect("renamed import decodes"),
        value
    );
}

#[test]
fn module_items_keep_their_standalone_attribute_options() {
    use phoxal_schema_proof_fixture::v1::{Empty, Reading};

    // A schema-name override and an inherited package compose: the wire
    // name is the module's package plus the renamed message name.
    assert_eq!(Reading::WIRE_NAME, "proof.module.v1.WireReading");
    let reading = Reading { value: 2.5 };
    assert_eq!(
        Reading::decode(reading.encode_to_vec().as_slice()).expect("renamed reading decodes"),
        reading
    );
    assert!(Reading::retain_schema() > 0);

    // An empty argument list inside the module still receives the package.
    assert_eq!(Empty::WIRE_NAME, "proof.module.v1.Empty");
    let empty = Empty { flag: true };
    assert_eq!(
        Empty::decode(empty.encode_to_vec().as_slice()).expect("empty-attribute item decodes"),
        empty
    );
    assert!(Empty::retain_schema() > 0);
}

#[test]
fn unknown_enum_values_fail_decoding_instead_of_defaulting() {
    // Field 1 (state), wire type varint, value 42: no declared variant.
    let mut bytes = Vec::new();
    prost::encoding::encode_key(1_u32, prost::encoding::WireType::Varint, &mut bytes);
    prost::encoding::encode_varint(42_u64, &mut bytes);
    let error =
        Snapshot::decode(bytes.as_slice()).expect_err("an unknown value must fail decoding");
    assert!(
        error.to_string().contains("unknown"),
        "the failure names the rejected value: {error}"
    );
}
