//! Wire and descriptor interop for Rust-authored messages.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;

use prost::Message as _;
use prost_types::FileDescriptorSet;

use phoxal_schema_proof_fixture::{Command as AuthoredCommand, Control as AuthoredControl};
use phoxal_schema_proof_fixture::{
    Mode as AuthoredMode, Reason as AuthoredReason, Target as AuthoredTarget,
};

// Independently generated Prost types compiled from `reference.proto`,
// placed in the package module tree the generated cross-package references
// expect. The generated copy of the imported robotics vocabulary sits in
// the local `phoxal::robotics::v1` module below.
pub mod proof {
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/proof.v1.rs"));
    }
}
// proof.v1 references the imported vocabulary as a crate-root `phoxal`
// module, which shadows the SDK crate inside this file; SDK paths use a
// leading `::` below.
pub mod phoxal {
    pub mod robotics {
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/phoxal.robotics.v1.rs"));
        }
    }
    pub mod geometry {
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/phoxal.geometry.v1.rs"));
        }
    }
    pub mod component {
        pub mod battery {
            pub mod v1 {
                include!(concat!(env!("OUT_DIR"), "/phoxal.component.battery.v1.rs"));
            }
        }
        pub mod lidar {
            pub mod v1 {
                include!(concat!(env!("OUT_DIR"), "/phoxal.component.lidar.v1.rs"));
            }
        }
    }
}
pub use proof::v1::Control;
pub use proof::v1::control;
pub use proof::v1::parent;
pub use proof::v1::{Command, Gains, Mode, Numbers, Parent, Reason, Target, Telemetry};

fn sample_command() -> (AuthoredCommand, Command) {
    let authored = AuthoredCommand {
        mode: AuthoredMode::Manual,
        owner: Some("proof".to_owned()),
        targets: vec![AuthoredTarget {
            actuator_id: "wheel".to_owned(),
            control: Some(AuthoredControl::TorqueNm(0.75)),
        }],
        payload: vec![1, 2, 3],
        count: 7,
        depth_mm: vec![10, 20, 30],
        encoder: Some(::phoxal::contracts::component::encoder::EncoderSample {
            position_rad: Some(0.25),
            velocity_radps: None,
        }),
        reasons: vec![AuthoredReason::Unavailable, AuthoredReason::Degraded],
    };
    let reference = Command {
        mode: Mode::Manual as i32,
        owner: Some("proof".to_owned()),
        targets: vec![Target {
            actuator_id: "wheel".to_owned(),
            control: Some(Control {
                phoxal_envelope: Some(control::PhoxalEnvelope::TorqueNm(0.75)),
            }),
        }],
        payload: vec![1, 2, 3],
        count: 7,
        depth_mm: vec![10, 20, 30],
        encoder: Some(phoxal::robotics::v1::EncoderSample {
            position_rad: Some(0.25),
            velocity_radps: None,
        }),
        reasons: vec![Reason::Unavailable as i32, Reason::Degraded as i32],
    };
    (authored, reference)
}

#[test]
fn authored_bytes_decode_through_the_independent_generator() {
    let (authored, _) = sample_command();
    let bytes = authored.encode_to_vec();
    let decoded = Command::decode(bytes.as_slice()).expect("independent decode");
    assert_eq!(decoded.mode(), Mode::Manual);
    assert_eq!(decoded.owner.as_deref(), Some("proof"));
    let target = decoded.targets.first().expect("one target");
    assert!(matches!(
        &target.control,
        Some(Control {
            phoxal_envelope: Some(control::PhoxalEnvelope::TorqueNm(0.75))
        })
    ));
    assert_eq!(decoded.payload, vec![1, 2, 3]);
    assert_eq!(decoded.count, 7);
    assert_eq!(decoded.depth_mm, vec![10, 20, 30]);
    assert_eq!(
        decoded.encoder.expect("imported field").position_rad,
        Some(0.25)
    );
    assert_eq!(
        decoded.reasons().collect::<Vec<_>>(),
        vec![Reason::Unavailable, Reason::Degraded],
        "repeated enum values decode with their enum identity"
    );
}

#[test]
fn independent_bytes_decode_through_the_authored_codec() {
    let (_, reference) = sample_command();
    let bytes = reference.encode_to_vec();
    let decoded = AuthoredCommand::decode(bytes.as_slice()).expect("authored decode");
    assert_eq!(decoded.mode, AuthoredMode::Manual);
    assert_eq!(
        decoded.targets.first().expect("one target").control,
        Some(AuthoredControl::TorqueNm(0.75))
    );
    assert_eq!(decoded.depth_mm, vec![10, 20, 30]);
    assert_eq!(
        decoded.reasons,
        vec![AuthoredReason::Unavailable, AuthoredReason::Degraded]
    );
}

#[test]
fn authored_and_independent_encodings_are_identical() {
    let (authored, reference) = sample_command();
    assert_eq!(authored.encode_to_vec(), reference.encode_to_vec());
}

#[test]
fn repeated_oneof_message_occurrences_merge_through_both_surfaces() {
    use phoxal_schema_proof_fixture::{Child as AuthoredChild, Parent as AuthoredParent};

    // Two occurrences of the same message-valued variant: the first carries
    // a=1, the second b=2. Valid Protobuf input must keep both fields.
    let merged = &[10_u8, 2, 8, 1, 10, 2, 16, 2][..];
    let authored = <AuthoredParent as ::phoxal::contracts::ProstPayload>::decode_payload(merged)
        .expect("authored payload-enum merge decode");
    let reference = Parent::decode(merged).expect("reference payload-enum merge decode");
    assert_eq!(
        authored,
        AuthoredParent::Child(AuthoredChild { a: 1, b: 2 }),
        "repeated occurrences of one message-valued variant merge its fields"
    );
    assert_eq!(
        reference.phoxal_envelope.map(|choice| match choice {
            parent::PhoxalEnvelope::Child(child) => (child.a, child.b),
            parent::PhoxalEnvelope::Text(_) => {
                unreachable!("reference kept the child variant")
            }
        }),
        Some((1, 2)),
        "the independent reference decoder agrees"
    );

    // A different variant replaces the selection with a fresh payload.
    let switched = &[10_u8, 2, 8, 1, 18, 3, b'a', b'b', b'c'][..];
    let authored = <AuthoredParent as ::phoxal::contracts::ProstPayload>::decode_payload(switched)
        .expect("authored variant switch decode");
    assert_eq!(authored, AuthoredParent::Text("abc".to_owned()));

    // Repeated occurrences of a scalar-valued variant keep the last value,
    // matching Prost's last-one-wins rule for non-message fields.
    let rewritten = &[18_u8, 3, b'a', b'b', b'c', 18, 2, b'x', b'y'][..];
    let authored = <AuthoredParent as ::phoxal::contracts::ProstPayload>::decode_payload(rewritten)
        .expect("authored string merge decode");
    assert_eq!(authored, AuthoredParent::Text("xy".to_owned()));
}

#[test]
fn payload_enums_reject_missing_and_unknown_variants() {
    use phoxal_schema_proof_fixture::Parent as AuthoredParent;

    // An empty envelope selects no variant: absence is not a valid value
    // of the public enum, so decoding fails instead of fabricating one.
    let error = <AuthoredParent as ::phoxal::contracts::ProstPayload>::decode_payload(&[])
        .expect_err("an envelope selecting no variant fails decoding");
    assert!(
        format!("{error:?}").contains("selected no variant"),
        "the failure names the invariant: {error:?}"
    );

    // A wire carrying only a variant tag the enum does not declare skips
    // it like Prost skips unknown fields, leaving no valid selection -
    // and a missing selection fails instead of fabricating a variant.
    let unknown_only = &[74_u8, 1, 0][..];
    let error = <AuthoredParent as ::phoxal::contracts::ProstPayload>::decode_payload(unknown_only)
        .expect_err("an unrecognized variant leaves no valid selection");
    assert!(
        format!("{error:?}").contains("selected no variant"),
        "the failure names the invariant: {error:?}"
    );

    // Round-tripping a payload enum keeps wire parity with the
    // independent reference: the authored and generated message shapes
    // carry the same envelope.
    let authored = AuthoredParent::Text("payload".to_owned());
    let bytes = <AuthoredParent as ::phoxal::contracts::ProstPayload>::encode_payload(&authored)
        .expect("payload enum encodes");
    assert_eq!(
        Parent::decode(bytes.as_slice()).expect("reference decodes"),
        Parent {
            phoxal_envelope: Some(parent::PhoxalEnvelope::Text("payload".to_owned()))
        },
        "authored and reference surfaces share one wire contract"
    );
}

#[test]
fn nested_geometry_and_added_vocabulary_share_the_wire_contract() {
    use ::phoxal::contracts::component::battery::BatterySample as AuthoredBattery;
    use ::phoxal::contracts::component::battery::ChargeState as AuthoredChargeState;
    use ::phoxal::contracts::component::lidar::LaserScan as AuthoredScan;
    use ::phoxal::contracts::geometry::{
        Point3, Pose as AuthoredPose, Quaternion, Twist as AuthoredTwist, Vector3,
    };
    use phoxal_schema_proof_fixture::Telemetry as AuthoredTelemetry;

    let authored = AuthoredTelemetry {
        pose: Some(AuthoredPose {
            position_m: Some(Point3 {
                x: 1.0,
                y: -2.0,
                z: 3.5,
            }),
            orientation: Some(Quaternion {
                w: 1.0,
                x: 0.0,
                y: 0.0,
                z: 0.0,
            }),
        }),
        twist: Some(AuthoredTwist {
            linear_mps: Some(Vector3 {
                x: 0.5,
                y: 0.0,
                z: 0.0,
            }),
            angular_radps: None,
        }),
        battery: Some(AuthoredBattery {
            present: true,
            voltage_v: Some(11.1),
            current_a: Some(-0.5),
            temperature_c: None,
            state_of_charge: Some(0.62),
            charge_state: AuthoredChargeState::Discharging,
        }),
        scan: Some(AuthoredScan {
            sensor_frame_id: "laser".to_owned(),
            angle_min_rad: -1.0,
            angle_increment_rad: 0.01,
            time_increment_s: 0.0001,
            min_range_m: 0.1,
            max_range_m: 10.0,
            ranges_m: vec![1.0, f64::INFINITY, f64::NAN],
            intensities: Vec::new(),
        }),
    };
    let reference = Telemetry {
        pose: Some(phoxal::geometry::v1::Pose {
            position_m: Some(phoxal::geometry::v1::Point3 {
                x: 1.0,
                y: -2.0,
                z: 3.5,
            }),
            orientation: Some(phoxal::geometry::v1::Quaternion {
                w: 1.0,
                x: 0.0,
                y: 0.0,
                z: 0.0,
            }),
        }),
        twist: Some(phoxal::geometry::v1::Twist {
            linear_mps: Some(phoxal::geometry::v1::Vector3 {
                x: 0.5,
                y: 0.0,
                z: 0.0,
            }),
            angular_radps: None,
        }),
        battery: Some(phoxal::component::battery::v1::BatterySample {
            present: true,
            voltage_v: Some(11.1),
            current_a: Some(-0.5),
            temperature_c: None,
            state_of_charge: Some(0.62),
            charge_state: phoxal::component::battery::v1::ChargeState::Discharging.into(),
        }),
        scan: Some(phoxal::component::lidar::v1::LaserScan {
            sensor_frame_id: "laser".to_owned(),
            angle_min_rad: -1.0,
            angle_increment_rad: 0.01,
            time_increment_s: 0.0001,
            min_range_m: 0.1,
            max_range_m: 10.0,
            ranges_m: vec![1.0, f64::INFINITY, f64::NAN],
            intensities: Vec::new(),
        }),
    };

    // Identical bytes on both surfaces, including the non-finite scan
    // sentinels and the battery's typed charge state.
    assert_eq!(authored.encode_to_vec(), reference.encode_to_vec());

    // Each surface decodes the other's bytes and re-encodes them to the
    // same values; equality goes through the bytes because the scan
    // deliberately carries a NaN, which never compares equal to itself.
    let decoded_reference = Telemetry::decode(authored.encode_to_vec().as_slice())
        .expect("reference decodes authored telemetry");
    assert_eq!(decoded_reference.encode_to_vec(), reference.encode_to_vec());
    let decoded_authored = AuthoredTelemetry::decode(reference.encode_to_vec().as_slice())
        .expect("authored telemetry decodes through the SDK vocabulary unchanged");
    assert_eq!(decoded_authored.encode_to_vec(), authored.encode_to_vec());
    assert_eq!(
        decoded_authored
            .battery
            .expect("battery present")
            .charge_state,
        AuthoredChargeState::Discharging,
        "the typed enum field keeps its identity across surfaces"
    );
    let scan = decoded_authored.scan.expect("scan present");
    assert!(scan.ranges_m[1].is_infinite() && scan.ranges_m[2].is_nan());
}

#[test]
fn packed_runs_cannot_cross_their_declared_length() {
    use phoxal_schema_proof_fixture::{Gains as AuthoredGains, Numbers as AuthoredNumbers};

    // The run declares one byte, but the varint needs two: the element
    // crosses the declared boundary, which Prost's bounded merge loop
    // rejects with DelimitedLengthExceeded.
    let varint_overrun = &[10_u8, 1, 128, 0][..];
    for (surface, error) in [
        (
            "authored",
            format!("{:?}", AuthoredNumbers::decode(varint_overrun).unwrap_err()),
        ),
        (
            "reference",
            format!("{:?}", Numbers::decode(varint_overrun).unwrap_err()),
        ),
    ] {
        assert!(
            error.contains("DelimitedLengthExceeded"),
            "{surface} rejects the overrun like Prost: {error}"
        );
    }

    // Fixed-width elements overrun the same way: the run declares one byte,
    // the float needs four.
    let fixed_overrun = &[10_u8, 1, 0, 0, 128, 63][..];
    for (surface, error) in [
        (
            "authored",
            format!("{:?}", AuthoredGains::decode(fixed_overrun).unwrap_err()),
        ),
        (
            "reference",
            format!("{:?}", Gains::decode(fixed_overrun).unwrap_err()),
        ),
    ] {
        assert!(
            error.contains("DelimitedLengthExceeded"),
            "{surface} rejects the fixed overrun like Prost: {error}"
        );
    }

    // A run that ends exactly at its declared length still decodes on both
    // surfaces.
    let bounded = &[10_u8, 3, 1, 2, 3][..];
    assert_eq!(
        AuthoredNumbers::decode(bounded)
            .expect("authored bounded packed run decodes")
            .values,
        vec![1, 2, 3]
    );
    assert_eq!(
        Numbers::decode(bounded)
            .expect("reference bounded packed run decodes")
            .values,
        vec![1, 2, 3]
    );
}

/// One structural projection of a definition, covering exactly the
/// wire-relevant identity: field numbers, kinds, cardinality, presence,
/// oneof membership, and enum values.
fn shape_of(pool: &prost_reflect::DescriptorPool, name: &str) -> String {
    if let Some(enumeration) = pool.get_enum_by_name(name) {
        let mut shape = format!("enum {name} {{");
        for value in enumeration.values() {
            shape.push_str(&format!(" {}={};", value.name(), value.number()));
        }
        return shape + " }";
    }
    let message = pool
        .get_message_by_name(name)
        .unwrap_or_else(|| panic!("{name} exists in the pool"));
    let oneof_names: Vec<&str> = message
        .descriptor_proto()
        .oneof_decl
        .iter()
        .map(|oneof| oneof.name())
        .collect();
    let mut shape = format!("message {name} {{");
    for field in message.fields() {
        let descriptor = field.field_descriptor_proto();
        let oneof = descriptor
            .oneof_index
            .map(|index| oneof_names[index as usize].to_owned());
        let optional = descriptor.proto3_optional().then_some("proto3-optional");
        shape.push_str(&format!(
            " {}:{}:{:?}:{:?}:{}:{};",
            field.name(),
            field.number(),
            descriptor.r#type(),
            descriptor.label(),
            oneof.unwrap_or_default(),
            optional.unwrap_or_default(),
        ));
    }
    shape + " }"
}

#[test]
fn assembled_descriptors_agree_with_the_protoc_reference() {
    use ::phoxal::schema::{MessageSchema, OneofSchema};
    use phoxal_schema_proof_fixture::Telemetry as AuthoredTelemetry;
    let mut records = BTreeMap::new();
    for record in [
        <AuthoredCommand as MessageSchema>::RECORD.to_decoded(),
        <AuthoredTarget as MessageSchema>::RECORD.to_decoded(),
        <AuthoredMode as MessageSchema>::RECORD.to_decoded(),
        <AuthoredReason as MessageSchema>::RECORD.to_decoded(),
        <AuthoredControl as MessageSchema>::RECORD.to_decoded(),
        <AuthoredControl as OneofSchema>::RECORD.to_decoded(),
        <AuthoredTelemetry as MessageSchema>::RECORD.to_decoded(),
        ::phoxal::contracts::component::encoder::EncoderSample::RECORD.to_decoded(),
        ::phoxal::contracts::component::range::RangeSample::RECORD.to_decoded(),
        ::phoxal::contracts::geometry::Point3::RECORD.to_decoded(),
        ::phoxal::contracts::geometry::Pose::RECORD.to_decoded(),
        ::phoxal::contracts::geometry::Quaternion::RECORD.to_decoded(),
        ::phoxal::contracts::geometry::Twist::RECORD.to_decoded(),
        ::phoxal::contracts::geometry::Vector3::RECORD.to_decoded(),
        ::phoxal::contracts::component::battery::BatterySample::RECORD.to_decoded(),
        <::phoxal::contracts::component::battery::ChargeState as MessageSchema>::RECORD
            .to_decoded(),
        ::phoxal::contracts::component::lidar::LaserScan::RECORD.to_decoded(),
    ] {
        records.insert(record.identity(), record);
    }
    let assembled =
        ::phoxal::schema::assemble_file_descriptors(&records.values().cloned().collect::<Vec<_>>())
            .expect("assembly");
    let assembled_pool = descriptor_pool_from(assembled);
    let reference: FileDescriptorSet = FileDescriptorSet::decode(
        include_bytes!(concat!(env!("OUT_DIR"), "/reference.bin")).as_slice(),
    )
    .expect("reference descriptors");
    let reference_pool = descriptor_pool_from(reference);

    for name in [
        "proof.v1.Command",
        "proof.v1.Target",
        "proof.v1.Mode",
        "proof.v1.Reason",
        "proof.v1.Telemetry",
        "phoxal.robotics.v1.EncoderSample",
        "phoxal.geometry.v1.Pose",
        "phoxal.geometry.v1.Twist",
        "phoxal.geometry.v1.Quaternion",
        "phoxal.component.battery.v1.BatterySample",
        "phoxal.component.battery.v1.ChargeState",
        "phoxal.component.lidar.v1.LaserScan",
    ] {
        assert_eq!(
            shape_of(&assembled_pool, name),
            shape_of(&reference_pool, name),
            "{name} must have the same wire shape in both descriptor sources"
        );
    }

    // The repeated enum must keep its enum identity in the assembled
    // descriptor, not degrade to a repeated integer schema.
    let command = assembled_pool
        .get_message_by_name("proof.v1.Command")
        .expect("assembled command");
    let reasons = command
        .get_field_by_name("reasons")
        .expect("assembled repeated enum field");
    let reason_type = reasons
        .field_descriptor_proto()
        .r#type
        .expect("explicit field type");
    assert_eq!(
        prost_types::field_descriptor_proto::Type::try_from(reason_type).expect("known type"),
        prost_types::field_descriptor_proto::Type::Enum,
        "a repeated enumeration assembles as a repeated enum field"
    );
    assert_eq!(
        shape_of(&assembled_pool, "proof.v1.Reason"),
        shape_of(&reference_pool, "proof.v1.Reason"),
        "the referenced enum assembles identically"
    );
}

fn descriptor_pool_from(set: FileDescriptorSet) -> prost_reflect::DescriptorPool {
    prost_reflect::DescriptorPool::decode(set.encode_to_vec().as_slice())
        .expect("descriptor pool decodes")
}
