//! Native schema extraction proof: the complete authored closure, including
//! imported SDK definitions, is readable from this binary's linked sections
//! without executing it.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;

use object::{Object as _, ObjectSection as _};
use prost::Message as _;

use phoxal::schema::MessageSchema;

const SCHEMA_SECTIONS: [&str; 2] = [".phoxal_schema", "__phoxal_schema"];

fn schema_sections(bytes: &[u8]) -> Vec<Vec<u8>> {
    let file = object::File::parse(bytes).expect("the fixture binary parses");
    let mut sections = Vec::new();
    for section in file.sections() {
        let name = section.name().unwrap_or_default();
        if SCHEMA_SECTIONS.contains(&name) {
            sections.push(section.data().expect("section bytes").to_vec());
        }
    }
    sections
}

#[test]
fn the_complete_schema_closure_is_extractable_without_execution() {
    let binary = std::fs::read(env!("CARGO_BIN_EXE_phoxal-schema-proof-fixture"))
        .expect("the fixture binary is built beside the tests");
    let sections = schema_sections(&binary);
    assert!(
        !sections.is_empty(),
        "the binary retains a schema section in this artifact format"
    );

    let mut identities = BTreeMap::new();
    for section in &sections {
        for record in phoxal::schema::decode_section(section).expect("frames decode") {
            identities.insert(record.identity(), record);
        }
    }
    for expected in [
        "proof.v1.Command",
        "proof.v1.Target",
        "proof.v1.Mode",
        "proof.v1.Reason",
        // A payload enum retains both its message envelope and its
        // variant table.
        "proof.v1.Control",
        "proof.v1.Control.phoxal_envelope",
        // The imported SDK vocabulary survives linking into the final
        // artifact through the referencing contract.
        "phoxal.robotics.v1.EncoderSample",
        "phoxal.robotics.v1.RangeSample",
        // Nested SDK geometry and the additional component vocabularies
        // link with their whole referencing chains.
        "phoxal.geometry.v1.Pose",
        "phoxal.geometry.v1.Twist",
        "phoxal.geometry.v1.Point3",
        "phoxal.geometry.v1.Quaternion",
        "phoxal.geometry.v1.Vector3",
        "phoxal.component.battery.v1.BatterySample",
        "phoxal.component.battery.v1.ChargeState",
        "phoxal.component.lidar.v1.LaserScan",
    ] {
        assert!(
            identities.contains_key(expected),
            "the retained closure must carry {expected}; it carries {identities:?}"
        );
    }

    // The extracted closure assembles into standard descriptors that decode
    // through the ordinary reflection library.
    let assembled =
        phoxal::schema::assemble_file_descriptors(&identities.into_values().collect::<Vec<_>>())
            .expect("the extracted closure assembles");
    let pool = prost_reflect::DescriptorPool::decode(assembled.encode_to_vec().as_slice())
        .expect("the assembled descriptors load through reflection");
    assert!(pool.get_message_by_name("proof.v1.Command").is_some());
    assert!(
        pool.get_message_by_name("phoxal.robotics.v1.EncoderSample")
            .is_some()
    );
}

#[test]
fn retained_records_match_the_compile_time_records() {
    use phoxal_schema_proof_fixture::{Command, Control, Mode, Reason, Target};
    let binary = std::fs::read(env!("CARGO_BIN_EXE_phoxal-schema-proof-fixture"))
        .expect("the fixture binary is built beside the tests");
    let mut identities = BTreeMap::new();
    for section in schema_sections(&binary) {
        for record in phoxal::schema::decode_section(&section).expect("frames decode") {
            identities.insert(record.identity(), record);
        }
    }
    for record in [
        Command::RECORD.to_decoded(),
        Target::RECORD.to_decoded(),
        Mode::RECORD.to_decoded(),
        Reason::RECORD.to_decoded(),
        <Control as ::phoxal::schema::MessageSchema>::RECORD.to_decoded(),
        <Control as ::phoxal::schema::OneofSchema>::RECORD.to_decoded(),
    ] {
        let identity = record.identity();
        assert_eq!(
            identities.get(&identity),
            Some(&record),
            "the retained {identity} must equal its compile-time record"
        );
    }
}
