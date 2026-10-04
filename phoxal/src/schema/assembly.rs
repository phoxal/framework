//! Standard descriptor assembly from retained schema records.
//!
//! One assembly implementation turns the compiler-resolved records embedded
//! in native artifacts into a deterministic `FileDescriptorSet`: one
//! synthesized file per Protobuf package, definitions sorted by name,
//! oneof variant tables joined into their owning messages, and proto3
//! optional presence represented with its synthetic oneof. Nothing here
//! reads authored source or executes a target program.

use std::collections::BTreeMap;

use prost_types::{
    DescriptorProto, EnumDescriptorProto, EnumValueDescriptorProto, FieldDescriptorProto,
    FileDescriptorProto, FileDescriptorSet, OneofDescriptorProto, field_descriptor_proto::Label,
    field_descriptor_proto::Type,
};

use super::Label as FieldLabel;
use super::decoded::{DecodedField, DecodedFieldType, DecodedMessage, DecodedOneof, DecodedRecord};

/// Why a record set could not be assembled into standard descriptors.
#[derive(Debug)]
pub enum Error {
    /// Two records define one qualified identity differently.
    Conflict(String),
    /// A field or oneof references a definition the record set does not carry.
    Missing(String),
    /// A declared oneof has no matching marker field or is malformed.
    Orphan(String),
    /// A oneof marker field carries a declared name but no variant table.
    MissingOneof(String, String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict(identity) => {
                write!(
                    formatter,
                    "conflicting Rust-authored schema definitions for `{identity}`"
                )
            }
            Self::Missing(identity) => {
                write!(
                    formatter,
                    "Rust-authored schema references missing definition `{identity}`"
                )
            }
            Self::Orphan(identity) => {
                write!(
                    formatter,
                    "oneof `{identity}` does not match a oneof field on its message"
                )
            }
            Self::MissingOneof(message, oneof) => {
                write!(
                    formatter,
                    "message `{message}` declares oneof `{oneof}` without a variant table"
                )
            }
        }
    }
}

impl std::error::Error for Error {}

/// Deterministic synthesized file name for one assembled package.
fn file_name(package: &str) -> String {
    format!("{package}.rust-authored.proto")
}

/// Assembles one closed record set into a standard descriptor set.
///
/// The input must carry every definition the records reference; a dangling
/// reference is a composition error. Identical duplicate definitions
/// collapse; conflicting ones are rejected.
pub fn assemble_file_descriptors(records: &[DecodedRecord]) -> Result<FileDescriptorSet, Error> {
    let mut by_identity = BTreeMap::<String, DecodedRecord>::new();
    for record in records {
        let mut record = record.clone();
        // Declaration order does not change a Protobuf definition. Prepared
        // APIs and owner declarations can retain the same fields in different
        // orders; compare their canonical wire definitions.
        match &mut record {
            DecodedRecord::Message(message) => message.fields.sort_by_key(|field| field.number),
            DecodedRecord::Enum(enumeration) => {
                enumeration.values.sort_by_key(|value| value.number);
            }
            DecodedRecord::Oneof(oneof) => oneof.variants.sort_by_key(|field| field.number),
        }
        let identity = record.identity();
        match by_identity.get(&identity) {
            Some(existing) if existing == &record => {}
            Some(_) => return Err(Error::Conflict(identity)),
            None => {
                by_identity.insert(identity, record.clone());
            }
        }
    }

    // Oneof tables are indexed by their containing message identity.
    let mut oneofs = BTreeMap::<String, Vec<&DecodedOneof>>::new();
    for record in by_identity.values() {
        if let DecodedRecord::Oneof(oneof) = record {
            let message = format!("{}.{}", oneof.package, oneof.message);
            oneofs.entry(message).or_default().push(oneof);
        }
    }
    let mut known = by_identity.keys().cloned().collect::<Vec<_>>();
    known.sort_unstable();

    // Group definitions by package; iteration is over sorted identities so
    // file order, definition order, and field order stay deterministic.
    let mut packages: BTreeMap<String, Vec<&DecodedRecord>> = BTreeMap::new();
    for record in by_identity.values() {
        if matches!(record, DecodedRecord::Oneof(_)) {
            continue;
        }
        packages
            .entry(record.package().to_owned())
            .or_default()
            .push(record);
    }

    let mut files = Vec::with_capacity(packages.len());
    for (package, definitions) in packages {
        let mut file = FileDescriptorProto {
            name: Some(file_name(&package)),
            package: Some(package.clone()),
            syntax: Some("proto3".to_owned()),
            ..Default::default()
        };
        let mut dependencies = Vec::<String>::new();
        for definition in definitions {
            match definition {
                DecodedRecord::Message(message) => {
                    let descriptor = message_descriptor(
                        message,
                        &oneofs,
                        &by_identity,
                        &mut dependencies,
                        &package,
                    )?;
                    file.message_type.push(descriptor);
                }
                DecodedRecord::Enum(enumeration) => {
                    file.enum_type.push(EnumDescriptorProto {
                        name: Some(enumeration.name.clone()),
                        value: enumeration
                            .values
                            .iter()
                            .map(|value| EnumValueDescriptorProto {
                                name: Some(value.name.clone()),
                                number: Some(value.number),
                                ..Default::default()
                            })
                            .collect(),
                        ..Default::default()
                    });
                }
                DecodedRecord::Oneof(_) => unreachable!("oneofs are indexed above"),
            }
        }
        dependencies.sort();
        dependencies.dedup();
        file.dependency = dependencies;
        files.push(file);
    }
    Ok(FileDescriptorSet { file: files })
}

fn message_descriptor(
    message: &DecodedMessage,
    oneofs: &BTreeMap<String, Vec<&DecodedOneof>>,
    by_identity: &BTreeMap<String, DecodedRecord>,
    dependencies: &mut Vec<String>,
    own_package: &str,
) -> Result<DescriptorProto, Error> {
    let identity = format!("{}.{}", message.package, message.name);
    let mut declared = Vec::new();
    for oneof in oneofs.get(&identity).into_iter().flatten() {
        declared.push(*oneof);
    }
    declared.sort_by(|left, right| {
        left.variants
            .first()
            .map_or(0, |field| field.number as i64)
            .cmp(
                &right
                    .variants
                    .first()
                    .map_or(0, |field| field.number as i64),
            )
    });

    // Fields are emitted in ascending declared number, with oneof marker
    // fields replaced by their variant tables.
    let mut fields = Vec::new();
    let mut synthetic_oneofs = Vec::new();
    for field in &message.fields {
        if matches!(field.ty, DecodedFieldType::Oneof) {
            let oneof_name = field
                .oneof
                .clone()
                .ok_or_else(|| Error::Orphan(identity.clone()))?;
            let (index, table) = declared
                .iter()
                .enumerate()
                .find(|(_, oneof)| oneof.field == oneof_name)
                .ok_or_else(|| Error::MissingOneof(identity.clone(), oneof_name.clone()))?;
            for variant in &table.variants {
                fields.push(field_descriptor(
                    variant,
                    Some(index as i32),
                    by_identity,
                    dependencies,
                    own_package,
                )?);
            }
            continue;
        }
        let mut descriptor = field_descriptor(field, None, by_identity, dependencies, own_package)?;
        // Message fields carry presence naturally; only scalar, string,
        // bytes, and enumeration fields need the proto3 synthetic oneof.
        if field.label == FieldLabel::Optional && !matches!(field.ty, DecodedFieldType::Message(_))
        {
            let synthetic_index = declared.len() + synthetic_oneofs.len();
            descriptor.proto3_optional = Some(true);
            descriptor.oneof_index = Some(synthetic_index as i32);
            synthetic_oneofs.push(format!("_{}", field.name));
        }
        fields.push(descriptor);
    }
    fields.sort_by_key(|field| field.number());

    let mut descriptor = DescriptorProto {
        name: Some(message.name.clone()),
        field: fields,
        ..Default::default()
    };
    for oneof in &declared {
        descriptor.oneof_decl.push(OneofDescriptorProto {
            name: Some(oneof.field.clone()),
            ..Default::default()
        });
    }
    for name in synthetic_oneofs {
        descriptor.oneof_decl.push(OneofDescriptorProto {
            name: Some(name),
            ..Default::default()
        });
    }
    Ok(descriptor)
}

fn field_descriptor(
    field: &DecodedField,
    oneof_index: Option<i32>,
    by_identity: &BTreeMap<String, DecodedRecord>,
    dependencies: &mut Vec<String>,
    own_package: &str,
) -> Result<FieldDescriptorProto, Error> {
    let label = match field.label {
        FieldLabel::Repeated => Label::Repeated,
        FieldLabel::Singular | FieldLabel::Optional => Label::Optional,
    };
    let (r#type, type_name) = match &field.ty {
        DecodedFieldType::Double => (Type::Double, None),
        DecodedFieldType::Float => (Type::Float, None),
        DecodedFieldType::Int64 => (Type::Int64, None),
        DecodedFieldType::Uint64 => (Type::Uint64, None),
        DecodedFieldType::Int32 => (Type::Int32, None),
        DecodedFieldType::Uint32 => (Type::Uint32, None),
        DecodedFieldType::Bool => (Type::Bool, None),
        DecodedFieldType::String => (Type::String, None),
        DecodedFieldType::Bytes => (Type::Bytes, None),
        DecodedFieldType::Message(name) => {
            let record = by_identity
                .get(name)
                .ok_or_else(|| Error::Missing(name.clone()))?;
            if !matches!(record, DecodedRecord::Message(_)) {
                return Err(Error::Conflict(name.clone()));
            }
            if package_of(name) != own_package {
                dependencies.push(file_name(&package_of(name)));
            }
            (Type::Message, Some(format!(".{name}")))
        }
        DecodedFieldType::Enum(name) => {
            let record = by_identity
                .get(name)
                .ok_or_else(|| Error::Missing(name.clone()))?;
            if !matches!(record, DecodedRecord::Enum(_)) {
                return Err(Error::Conflict(name.clone()));
            }
            if package_of(name) != own_package {
                dependencies.push(file_name(&package_of(name)));
            }
            (Type::Enum, Some(format!(".{name}")))
        }
        DecodedFieldType::Oneof => {
            return Err(Error::Orphan(field.name.clone()));
        }
    };
    Ok(FieldDescriptorProto {
        name: Some(field.name.clone()),
        number: Some(field.number as i32),
        label: Some(label.into()),
        r#type: Some(r#type.into()),
        type_name,
        oneof_index,
        ..Default::default()
    })
}

fn package_of(identity: &str) -> String {
    identity
        .rsplit_once('.')
        .map_or_else(String::new, |(head, _)| head.to_owned())
}

#[cfg(test)]
mod tests {
    use super::super::Label;
    use super::super::decoded::{
        DecodedEnum, DecodedField, DecodedFieldType, DecodedMessage, DecodedOneof, DecodedRecord,
        DecodedValue,
    };
    use super::assemble_file_descriptors;

    fn scalar(number: u32, name: &str, ty: DecodedFieldType, label: Label) -> DecodedField {
        DecodedField {
            number,
            name: name.to_owned(),
            ty,
            label,
            oneof: None,
        }
    }

    /// A message set covering optional presence, repeated messages, enum
    /// references, and a declared oneof, mirroring the motion vocabulary.
    fn records() -> Vec<DecodedRecord> {
        vec![
            DecodedRecord::Message(DecodedMessage {
                package: "proof.v1".to_owned(),
                name: "Target".to_owned(),
                fields: vec![
                    scalar(1, "actuator_id", DecodedFieldType::String, Label::Singular),
                    DecodedField {
                        number: 2,
                        name: "control".to_owned(),
                        ty: DecodedFieldType::Oneof,
                        label: Label::Singular,
                        oneof: Some("control".to_owned()),
                    },
                ],
            }),
            DecodedRecord::Oneof(DecodedOneof {
                package: "proof.v1".to_owned(),
                message: "Target".to_owned(),
                field: "control".to_owned(),
                variants: vec![
                    DecodedField {
                        number: 2,
                        name: "velocity_radps".to_owned(),
                        ty: DecodedFieldType::Double,
                        label: Label::Singular,
                        oneof: Some("control".to_owned()),
                    },
                    DecodedField {
                        number: 3,
                        name: "torque_nm".to_owned(),
                        ty: DecodedFieldType::Double,
                        label: Label::Singular,
                        oneof: Some("control".to_owned()),
                    },
                ],
            }),
            DecodedRecord::Enum(DecodedEnum {
                package: "proof.v1".to_owned(),
                name: "Mode".to_owned(),
                values: vec![
                    DecodedValue {
                        number: 0,
                        name: "MODE_UNSPECIFIED".to_owned(),
                    },
                    DecodedValue {
                        number: 2,
                        name: "MODE_MANUAL".to_owned(),
                    },
                ],
            }),
            DecodedRecord::Message(DecodedMessage {
                package: "proof.v1".to_owned(),
                name: "Command".to_owned(),
                fields: vec![
                    DecodedField {
                        number: 1,
                        name: "mode".to_owned(),
                        ty: DecodedFieldType::Enum("proof.v1.Mode".to_owned()),
                        label: Label::Singular,
                        oneof: None,
                    },
                    scalar(2, "owner", DecodedFieldType::String, Label::Optional),
                    DecodedField {
                        number: 3,
                        name: "targets".to_owned(),
                        ty: DecodedFieldType::Message("proof.v1.Target".to_owned()),
                        label: Label::Repeated,
                        oneof: None,
                    },
                ],
            }),
        ]
    }

    #[test]
    fn assembles_one_deterministic_file_per_package() {
        let first = assemble_file_descriptors(&records()).expect("records assemble");
        let second = assemble_file_descriptors(&records()).expect("records assemble again");
        assert_eq!(first.file.len(), 1);
        assert_eq!(
            first.file[0].name.as_deref(),
            Some("proof.v1.rust-authored.proto")
        );
        assert_eq!(first, second, "assembly is deterministic");

        let command = first.file[0]
            .message_type
            .iter()
            .find(|message| message.name() == "Command")
            .expect("command message");
        let mode = command
            .field
            .iter()
            .find(|field| field.name() == "mode")
            .expect("mode field");
        assert_eq!(mode.type_name.as_deref(), Some(".proof.v1.Mode"));
        let owner = command
            .field
            .iter()
            .find(|field| field.name() == "owner")
            .expect("owner field");
        assert_eq!(owner.proto3_optional, Some(true));
        let synthetic = &command.oneof_decl[owner.oneof_index.unwrap() as usize];
        assert_eq!(synthetic.name.as_deref(), Some("_owner"));

        let target = first.file[0]
            .message_type
            .iter()
            .find(|message| message.name() == "Target")
            .expect("target message");
        assert_eq!(target.oneof_decl.len(), 1);
        assert_eq!(target.oneof_decl[0].name.as_deref(), Some("control"));
        let velocity = target
            .field
            .iter()
            .find(|field| field.name() == "velocity_radps")
            .expect("velocity variant");
        assert_eq!(velocity.oneof_index, Some(0));
        assert_ne!(velocity.proto3_optional, Some(true));
    }

    #[test]
    fn identical_duplicates_collapse_and_conflicts_are_rejected() {
        let mut doubled = records();
        let duplicate = doubled[0].clone();
        doubled.push(duplicate);
        assemble_file_descriptors(&doubled).expect("identical duplicates collapse");
        let mut reordered = records();
        for mut record in records() {
            match &mut record {
                DecodedRecord::Message(message) => message.fields.reverse(),
                DecodedRecord::Enum(enumeration) => enumeration.values.reverse(),
                DecodedRecord::Oneof(oneof) => oneof.variants.reverse(),
            }
            reordered.push(record);
        }
        assert_eq!(
            assemble_file_descriptors(&reordered).expect("declaration order is immaterial"),
            assemble_file_descriptors(&records()).unwrap()
        );

        let mut conflicting = records();
        let mut mutated = conflicting[0].clone();
        if let DecodedRecord::Message(message) = &mut mutated {
            message.fields[0].name = "renamed".to_owned();
        }
        conflicting.push(mutated);
        let error =
            assemble_file_descriptors(&conflicting).expect_err("conflicting definitions must fail");
        assert!(error.to_string().contains("proof.v1.Target"));
    }

    #[test]
    fn dangling_references_are_composition_errors() {
        let mut partial = records();
        // Drop the enum definition referenced by Command.mode.
        partial.retain(|record| !matches!(record, DecodedRecord::Enum(_)));
        let error = assemble_file_descriptors(&partial)
            .expect_err("a missing referenced definition must fail");
        assert!(error.to_string().contains("proof.v1.Mode"));
    }

    #[test]
    fn cross_package_references_become_dependencies() {
        let records = vec![
            DecodedRecord::Message(DecodedMessage {
                package: "proof.consumer.v1".to_owned(),
                name: "Status".to_owned(),
                fields: vec![DecodedField {
                    number: 1,
                    name: "sample".to_owned(),
                    ty: DecodedFieldType::Message("phoxal.robotics.v1.EncoderSample".to_owned()),
                    label: Label::Singular,
                    oneof: None,
                }],
            }),
            DecodedRecord::Message(DecodedMessage {
                package: "phoxal.robotics.v1".to_owned(),
                name: "EncoderSample".to_owned(),
                fields: vec![scalar(
                    1,
                    "position_rad",
                    DecodedFieldType::Double,
                    Label::Optional,
                )],
            }),
        ];
        let assembled = assemble_file_descriptors(&records).expect("records assemble");
        let consumer = assembled
            .file
            .iter()
            .find(|file| file.package() == "proof.consumer.v1")
            .expect("consumer file");
        assert_eq!(
            consumer.dependency,
            vec!["phoxal.robotics.v1.rust-authored.proto".to_owned()]
        );
    }
}
