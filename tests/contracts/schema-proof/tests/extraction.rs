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
        "phoxal.robotics.v1.MotionSetpoint",
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

#[test]
fn protocol_descriptors_are_extractable_without_executing_the_binary() {
    let binary = std::fs::read(env!("CARGO_BIN_EXE_phoxal-schema-proof-fixture")).unwrap();
    let records = schema_sections(&binary)
        .iter()
        .flat_map(|section| phoxal::schema::decode_section(section).unwrap())
        .filter(|record| {
            [
                "phoxal.bootstrap.v1",
                "phoxal.session.v1",
                "phoxal.execution.v1",
                "phoxal.simulation.v1",
            ]
            .contains(&record.package())
        })
        .collect::<Vec<_>>();
    let extracted = phoxal::schema::assemble_file_descriptors(&records).unwrap();
    assert_eq!(
        extracted,
        phoxal::communication::file_descriptor_set().unwrap()
    );
}

#[test]
#[ignore = "requires explicitly supplied source-built runtime artifacts"]
fn external_runtime_frames_retain_complete_cross_crate_schema_closures() {
    let paths = std::env::var_os("PHOXAL_SCHEMA_PROOF_ARTIFACTS")
        .expect("provide runtime artifact paths with the platform path separator");
    let paths: Vec<_> = std::env::split_paths(&paths).collect();
    assert!(!paths.is_empty());
    for path in paths {
        let bytes = std::fs::read(&path).expect("external runtime artifact");
        let records: Vec<_> = schema_sections(&bytes)
            .iter()
            .flat_map(|section| phoxal::schema::decode_section(section).expect("schema frames"))
            .collect();
        assert!(
            !records.is_empty(),
            "{} has no schema records",
            path.display()
        );
        let descriptors = phoxal::schema::assemble_file_descriptors(&records)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let pool = prost_reflect::DescriptorPool::decode(descriptors.encode_to_vec().as_slice())
            .expect("complete standard descriptors");
        println!(
            "{}: {} records, {} descriptor files",
            path.display(),
            records.len(),
            pool.files().len()
        );
    }
}
