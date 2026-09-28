//! Host-side decoding of retained schema frames.
//!
//! Frames embedded by [`crate::message`] are decoded without executing the
//! target program; the decoded model feeds standard descriptor assembly.

use super::{
    FieldRecord, FieldType, Label, MAX_SCHEMA_RECORD_BYTES, SCHEMA_FRAME_MAGIC, SchemaRecord,
};

/// Why a retained schema section could not be decoded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchemaError {
    /// A frame was truncated or extended past its section.
    Malformed(&'static str),
    /// A payload used an unknown kind or label byte.
    Unsupported(u8),
    /// A payload exceeded the retained record bound.
    Oversized,
}

impl std::fmt::Display for SchemaError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(text) => write!(formatter, "malformed schema frame: {text}"),
            Self::Unsupported(byte) => {
                write!(formatter, "unsupported schema encoding byte {byte}")
            }
            Self::Oversized => write!(
                formatter,
                "schema record exceeds {MAX_SCHEMA_RECORD_BYTES} bytes"
            ),
        }
    }
}

impl std::error::Error for SchemaError {}

impl SchemaError {
    /// Returns the static diagnostic text.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Malformed(text) => text,
            Self::Unsupported(_) => "unsupported schema encoding byte",
            Self::Oversized => "schema record exceeds the retained byte bound",
        }
    }
}

struct SchemaCursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> SchemaCursor<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], SchemaError> {
        let end = self
            .position
            .checked_add(count)
            .ok_or(SchemaError::Malformed("length overflow"))?;
        if end > self.bytes.len() {
            return Err(SchemaError::Malformed("record extends past its frame"));
        }
        let slice = &self.bytes[self.position..end];
        self.position = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, SchemaError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<usize, SchemaError> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]) as usize)
    }

    fn u32(&mut self) -> Result<u32, SchemaError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn i32(&mut self) -> Result<i32, SchemaError> {
        Ok(self.u32()? as i32)
    }

    fn owned_str(&mut self) -> Result<String, SchemaError> {
        let length = self.u16()?;
        let bytes = self.take(length)?;
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| SchemaError::Malformed("non-UTF-8 schema string"))
    }

    fn optional_str(&mut self) -> Result<Option<String>, SchemaError> {
        let length = self.u16()?;
        if length == 0xFFFF {
            return Ok(None);
        }
        let bytes = self.take(length)?;
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map(Some)
            .map_err(|_| SchemaError::Malformed("non-UTF-8 schema string"))
    }
}

/// Host-side field shape as decoded from a retained frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedField {
    /// Declared stable field number.
    pub number: u32,
    /// Protobuf field name.
    pub name: String,
    /// Wire storage and reference identity.
    pub ty: DecodedFieldType,
    /// Cardinality.
    pub label: Label,
    /// The declared oneof this field belongs to, if any.
    pub oneof: Option<String>,
}

/// Host-side wire kind as decoded from a retained frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecodedFieldType {
    /// `double`
    Double,
    /// `float`
    Float,
    /// `int64`
    Int64,
    /// `uint64`
    Uint64,
    /// `int32`
    Int32,
    /// `uint32`
    Uint32,
    /// `bool`
    Bool,
    /// `string`
    String,
    /// `bytes`
    Bytes,
    /// A message reference by fully-qualified Protobuf name.
    Message(String),
    /// An enumeration reference by fully-qualified Protobuf name.
    Enum(String),
    /// A declared oneof; its variant fields live in the matching decoded
    /// oneof record.
    Oneof,
}

/// Host-side message definition as decoded from a retained frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedMessage {
    /// Owning Protobuf package.
    pub package: String,
    /// Protobuf message name.
    pub name: String,
    /// Fields in declaration order.
    pub fields: Vec<DecodedField>,
}

/// Host-side enumeration value as decoded from a retained frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedValue {
    /// Stable wire number.
    pub number: i32,
    /// Protobuf value name.
    pub name: String,
}

/// Host-side enumeration as decoded from a retained frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedEnum {
    /// Owning Protobuf package.
    pub package: String,
    /// Protobuf enumeration name.
    pub name: String,
    /// Wire numbers with their Protobuf value names, in declaration order.
    pub values: Vec<DecodedValue>,
}

/// Host-side oneof table as decoded from a retained frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedOneof {
    /// Owning Protobuf package of the containing message.
    pub package: String,
    /// Protobuf name of the containing message.
    pub message: String,
    /// The oneof field name inside the message.
    pub field: String,
    /// Variants in declaration order.
    pub variants: Vec<DecodedField>,
}

/// Host-side schema definition as decoded from a retained frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecodedRecord {
    /// A message definition.
    Message(DecodedMessage),
    /// An enumeration definition.
    Enum(DecodedEnum),
    /// A oneof variant table.
    Oneof(DecodedOneof),
}

impl DecodedRecord {
    /// Returns the owning Protobuf package.
    #[must_use]
    pub fn package(&self) -> &str {
        match self {
            Self::Message(record) => &record.package,
            Self::Enum(record) => &record.package,
            Self::Oneof(record) => &record.package,
        }
    }

    /// Returns the fully-qualified identity of the decoded definition.
    #[must_use]
    pub fn identity(&self) -> String {
        match self {
            Self::Message(record) => format!("{}.{}", record.package, record.name),
            Self::Enum(record) => format!("{}.{}", record.package, record.name),
            Self::Oneof(record) => {
                format!("{}.{}.{}", record.package, record.message, record.field)
            }
        }
    }
}

impl SchemaRecord<'_> {
    /// Converts the compile-time record into its host-side decoded form.
    #[must_use]
    pub fn to_decoded(&self) -> DecodedRecord {
        fn field(field: &FieldRecord<'_>) -> DecodedField {
            DecodedField {
                number: field.number,
                name: field.name.to_owned(),
                ty: match field.ty {
                    FieldType::Double => DecodedFieldType::Double,
                    FieldType::Float => DecodedFieldType::Float,
                    FieldType::Int64 => DecodedFieldType::Int64,
                    FieldType::Uint64 => DecodedFieldType::Uint64,
                    FieldType::Int32 => DecodedFieldType::Int32,
                    FieldType::Uint32 => DecodedFieldType::Uint32,
                    FieldType::Bool => DecodedFieldType::Bool,
                    FieldType::String => DecodedFieldType::String,
                    FieldType::Bytes => DecodedFieldType::Bytes,
                    FieldType::Message(name) => DecodedFieldType::Message(name.to_owned()),
                    FieldType::Enum(name) => DecodedFieldType::Enum(name.to_owned()),
                    FieldType::Oneof => DecodedFieldType::Oneof,
                },
                label: field.label,
                oneof: field.oneof.map(str::to_owned),
            }
        }
        match self {
            Self::Message(record) => DecodedRecord::Message(DecodedMessage {
                package: record.package.to_owned(),
                name: record.name.to_owned(),
                fields: record.fields.iter().map(field).collect(),
            }),
            Self::Enum(record) => DecodedRecord::Enum(DecodedEnum {
                package: record.package.to_owned(),
                name: record.name.to_owned(),
                values: record
                    .values
                    .iter()
                    .map(|value| DecodedValue {
                        number: value.number,
                        name: value.name.to_owned(),
                    })
                    .collect(),
            }),
            Self::Oneof(record) => DecodedRecord::Oneof(DecodedOneof {
                package: record.package.to_owned(),
                message: record.message.to_owned(),
                field: record.field.to_owned(),
                variants: record.variants.iter().map(field).collect(),
            }),
        }
    }
}

fn decode_field(cursor: &mut SchemaCursor<'_>) -> Result<DecodedField, SchemaError> {
    let number = cursor.u32()?;
    let name = cursor.owned_str()?;
    let kind = cursor.u8()?;
    let label_byte = cursor.u8()?;
    let label = Label::decode(label_byte).ok_or(SchemaError::Unsupported(label_byte))?;
    let oneof = cursor.optional_str()?;
    let ty = match kind {
        1 => DecodedFieldType::Double,
        2 => DecodedFieldType::Float,
        3 => DecodedFieldType::Int64,
        4 => DecodedFieldType::Uint64,
        5 => DecodedFieldType::Int32,
        6 => DecodedFieldType::Uint32,
        7 => DecodedFieldType::Bool,
        8 => DecodedFieldType::String,
        9 => DecodedFieldType::Bytes,
        10 => DecodedFieldType::Message(cursor.owned_str()?),
        11 => DecodedFieldType::Enum(cursor.owned_str()?),
        12 => DecodedFieldType::Oneof,
        other => return Err(SchemaError::Unsupported(other)),
    };
    Ok(DecodedField {
        number,
        name,
        ty,
        label,
        oneof,
    })
}

fn decode_payload(payload: &[u8]) -> Result<DecodedRecord, SchemaError> {
    if payload.len() > MAX_SCHEMA_RECORD_BYTES {
        return Err(SchemaError::Oversized);
    }
    let mut cursor = SchemaCursor {
        bytes: payload,
        position: 0,
    };
    let kind = cursor.u8()?;
    match kind {
        1 => {
            let package = cursor.owned_str()?;
            let name = cursor.owned_str()?;
            let count = cursor.u16()?;
            let mut fields = Vec::with_capacity(count);
            for _ in 0..count {
                fields.push(decode_field(&mut cursor)?);
            }
            Ok(DecodedRecord::Message(DecodedMessage {
                package,
                name,
                fields,
            }))
        }
        2 => {
            let package = cursor.owned_str()?;
            let name = cursor.owned_str()?;
            let count = cursor.u16()?;
            let mut values = Vec::with_capacity(count);
            for _ in 0..count {
                let number = cursor.i32()?;
                let value_name = cursor.owned_str()?;
                values.push(DecodedValue {
                    number,
                    name: value_name,
                });
            }
            Ok(DecodedRecord::Enum(DecodedEnum {
                package,
                name,
                values,
            }))
        }
        3 => {
            let package = cursor.owned_str()?;
            let message = cursor.owned_str()?;
            let field = cursor.owned_str()?;
            let count = cursor.u16()?;
            let mut variants = Vec::with_capacity(count);
            for _ in 0..count {
                variants.push(decode_field(&mut cursor)?);
            }
            Ok(DecodedRecord::Oneof(DecodedOneof {
                package,
                message,
                field,
                variants,
            }))
        }
        other => Err(SchemaError::Unsupported(other)),
    }
}

/// Decodes every framed schema record retained in one native section.
///
/// Bytes between frames are ignored, matching the section-scanning contract
/// the artifact inspector uses for descriptor frames. Duplicate records are
/// collapsed onto their first occurrence. A frame whose header promises more
/// bytes than the section retains ends the scan; a structurally invalid
/// payload is rejected.
pub fn decode_section(section: &[u8]) -> Result<Vec<DecodedRecord>, SchemaError> {
    let mut records = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = section[cursor..]
        .windows(SCHEMA_FRAME_MAGIC.len())
        .position(|window| window == SCHEMA_FRAME_MAGIC)
    {
        let magic = cursor + relative;
        let length_start = magic + SCHEMA_FRAME_MAGIC.len();
        let payload_start = length_start + 4;
        if payload_start + 4 > section.len() {
            break;
        }
        let header = &section[length_start..payload_start];
        let length = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let Some(payload) = section.get(payload_start..payload_start + length) else {
            break;
        };
        let record = decode_payload(payload)?;
        if !records.contains(&record) {
            records.push(record);
        }
        cursor = payload_start + length;
    }
    Ok(records)
}
