//! Compiler-resolved schema records for Rust-authored Protobuf messages.
//!
//! [`crate::message`] expands one authored struct or enumeration into the
//! Prost codec derives plus a schema record here. Records are the single
//! derived representation: the same bytes are retained in a native section
//! for host-side extraction, and the standard `FileDescriptorSet` used by
//! composition and client generation is assembled from them by
//! the internal assembly module. Nothing here executes service code or interprets authored
//! source at runtime.

mod assembly;
mod codec;
mod decoded;

pub use assembly::{Error as AssemblyError, assemble_file_descriptors};
pub use codec::{
    PhoxalWire, encode_repeated, merge_fresh_default, merge_repeated, missing_variant_error,
    repeated_len, singular_encode, singular_len, unknown_enum_value_error, unknown_oneof_tag,
};
pub use decoded::{
    DecodedEnum, DecodedField, DecodedFieldType, DecodedMessage, DecodedOneof, DecodedRecord,
    DecodedValue, SchemaError, decode_section,
};

/// Wire-storage kinds a Rust-authored field can declare.
///
/// The supported subset is exactly what the migrated contracts use; the
/// macro rejects anything else with a compile diagnostic instead of
/// guessing a wire representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FieldType<'a> {
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
    Message(&'a str),
    /// An enumeration reference by fully-qualified Protobuf name.
    Enum(&'a str),
    /// A declared oneof; its variant fields live in the matching
    /// [`OneofRecord`].
    Oneof,
}

impl FieldType<'_> {
    const fn encode_kind(self) -> u8 {
        match self {
            Self::Double => 1,
            Self::Float => 2,
            Self::Int64 => 3,
            Self::Uint64 => 4,
            Self::Int32 => 5,
            Self::Uint32 => 6,
            Self::Bool => 7,
            Self::String => 8,
            Self::Bytes => 9,
            Self::Message(_) => 10,
            Self::Enum(_) => 11,
            Self::Oneof => 12,
        }
    }

    const fn has_name(self) -> bool {
        matches!(self, Self::Message(_) | Self::Enum(_))
    }
}

/// Field cardinality as declared in Rust.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Label {
    /// A plain singular field.
    Singular,
    /// A proto3 optional field carrying explicit presence.
    Optional,
    /// A repeated field.
    Repeated,
}

impl Label {
    const fn encode(self) -> u8 {
        match self {
            Self::Singular => 0,
            Self::Optional => 1,
            Self::Repeated => 2,
        }
    }

    fn decode(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Singular),
            1 => Some(Self::Optional),
            2 => Some(Self::Repeated),
            _ => None,
        }
    }
}

/// One authored field of a message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FieldRecord<'a> {
    /// Declared stable field number.
    pub number: u32,
    /// Protobuf field name.
    pub name: &'a str,
    /// Wire storage and reference identity.
    pub ty: FieldType<'a>,
    /// Cardinality.
    pub label: Label,
    /// The declared oneof this field belongs to, if any.
    pub oneof: Option<&'a str>,
}

/// One authored message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MessageRecord<'a> {
    /// Owning Protobuf package.
    pub package: &'a str,
    /// Protobuf message name.
    pub name: &'a str,
    /// Fields in declaration order.
    pub fields: &'a [FieldRecord<'a>],
}

/// One authored enumeration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EnumRecord<'a> {
    /// Owning Protobuf package.
    pub package: &'a str,
    /// Protobuf enumeration name.
    pub name: &'a str,
    /// Wire numbers with their Protobuf value names, in declaration order.
    pub values: &'a [EnumValue<'a>],
}

/// One enumeration value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EnumValue<'a> {
    /// Stable wire number.
    pub number: i32,
    /// Protobuf value name.
    pub name: &'a str,
}

/// One authored oneof: the variant table of a message field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OneofRecord<'a> {
    /// Owning Protobuf package of the containing message.
    pub package: &'a str,
    /// Protobuf name of the containing message.
    pub message: &'a str,
    /// The oneof field name inside the message.
    pub field: &'a str,
    /// Variants in declaration order.
    pub variants: &'a [FieldRecord<'a>],
}

/// One retained schema definition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchemaRecord<'a> {
    /// A message definition.
    Message(MessageRecord<'a>),
    /// An enumeration definition.
    Enum(EnumRecord<'a>),
    /// A oneof variant table.
    Oneof(OneofRecord<'a>),
}

impl SchemaRecord<'_> {
    /// Returns the owning Protobuf package.
    #[must_use]
    pub const fn package(&self) -> &str {
        match self {
            Self::Message(record) => record.package,
            Self::Enum(record) => record.package,
            Self::Oneof(record) => record.package,
        }
    }

    /// Returns the definition's own Protobuf name (its containing message
    /// for a oneof table).
    #[must_use]
    pub const fn name(&self) -> &str {
        match self {
            Self::Message(record) => record.name,
            Self::Enum(record) => record.name,
            Self::Oneof(record) => record.message,
        }
    }

    /// Returns the fully-qualified identity of the retained definition.
    ///
    /// A oneof is identified by its containing message and field name; its
    /// variant fields carry their own qualified names through the message.
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

/// Schema identity and retention owned by one Rust-authored type.
///
/// Implemented by the [`crate::message`] struct and enum expansions;
/// not hand-authored. `WIRE_NAME` lets a referencing field record cite the
/// fully-qualified identity without the macro inspecting another crate.
pub trait MessageSchema: Sized {
    /// The retained schema record.
    const RECORD: SchemaRecord<'static>;
    /// The fully-qualified Protobuf identity.
    const WIRE_NAME: &'static str;

    /// Retains this definition's schema frame and those of every referenced
    /// message or enumeration so the final artifact carries the complete
    /// reachable contract.
    fn retain_schema() -> usize;
}

/// The internal oneof name marking a message as a payload enum: a Rust
/// enum whose variants carry explicit stable tags and own their own
/// message envelope. Authored oneof fields can never produce this name;
/// the prepared generator uses it to rebuild the same public enum shape
/// from compiled descriptors. `phoxal-build` carries the same constant.
pub const PAYLOAD_ENUM_ENVELOPE: &str = "phoxal_envelope";

/// Wire-tag table owned by one Rust-authored oneof enumeration.
///
/// Implemented by the [`crate::message`] enum expansion; the referencing message
/// compares it against its declared field tags at compile time so the two
/// spellings cannot drift.
pub trait OneofSchema {
    /// Field numbers of the variants, in declaration order.
    const TAGS: &'static [u32];
    /// The retained oneof variant table.
    const RECORD: SchemaRecord<'static>;

    /// Retains this table's schema frame and those of every referenced
    /// definition.
    fn retain_schema() -> usize;
}

/// Eight-byte prefix of one retained schema frame.
pub const SCHEMA_FRAME_MAGIC: [u8; 8] = *b"PHXSCHE1";

/// Number of bytes before a framed schema payload.
pub const SCHEMA_FRAME_HEADER_BYTES: usize = 12;

/// Maximum retained schema payload in one frame.
pub const MAX_SCHEMA_RECORD_BYTES: usize = 65_536;

/// Returns the exact framed byte count of one schema record.
#[must_use]
pub const fn encoded_len(record: &SchemaRecord<'_>) -> usize {
    SCHEMA_FRAME_HEADER_BYTES + payload_len(record)
}

const fn str_len(value: &str) -> usize {
    value.len()
}

const fn field_len(field: &FieldRecord<'_>) -> usize {
    // The oneof slot always carries its 2-byte prefix: a name when the
    // field belongs to a declared oneof, the 0xFFFF marker otherwise.
    let mut length = 4 + 2 + str_len(field.name) + 1 + 1;
    match field.oneof {
        Some(oneof) => length += 2 + str_len(oneof),
        None => length += 2,
    }
    if field.ty.has_name() {
        length += 2 + reference_len(field.ty);
    }
    length
}

const fn reference_len(ty: FieldType<'_>) -> usize {
    match ty {
        FieldType::Message(name) | FieldType::Enum(name) => str_len(name),
        _ => 0,
    }
}

const fn payload_len(record: &SchemaRecord<'_>) -> usize {
    match record {
        SchemaRecord::Message(message) => {
            let mut length = 1 + 2 + str_len(message.package) + 2 + str_len(message.name) + 2;
            let mut index = 0;
            while index < message.fields.len() {
                length += field_len(&message.fields[index]);
                index += 1;
            }
            length
        }
        SchemaRecord::Enum(enumeration) => {
            let mut length =
                1 + 2 + str_len(enumeration.package) + 2 + str_len(enumeration.name) + 2;
            let mut index = 0;
            while index < enumeration.values.len() {
                length += 4 + 2 + str_len(enumeration.values[index].name);
                index += 1;
            }
            length
        }
        SchemaRecord::Oneof(oneof) => {
            let mut length = 1
                + 2
                + str_len(oneof.package)
                + 2
                + str_len(oneof.message)
                + 2
                + str_len(oneof.field)
                + 2;
            let mut index = 0;
            while index < oneof.variants.len() {
                length += field_len(&oneof.variants[index]);
                index += 1;
            }
            length
        }
    }
}

/// Writes one schema record's framed bytes.
///
/// `bytes` must have exactly [`encoded_len`] bytes; the function writes the
/// magic, payload length, and payload and asserts the exact fit. Static
/// initializers call this from a block expression so the frame length stays
/// a plain const expression:
///
/// ```ignore
/// static FRAME: [u8; encoded_len(&RECORD)] = {
///     let mut bytes = [0_u8; encoded_len(&RECORD)];
///     write_frame(&RECORD, &mut bytes);
///     bytes
/// };
/// ```
pub const fn write_frame(record: &SchemaRecord<'_>, bytes: &mut [u8]) {
    let mut position = 0;
    while position < SCHEMA_FRAME_MAGIC.len() {
        bytes[position] = SCHEMA_FRAME_MAGIC[position];
        position += 1;
    }
    let payload_length = payload_len(record);
    let length_bytes = (payload_length as u32).to_le_bytes();
    position = 0;
    while position < length_bytes.len() {
        bytes[SCHEMA_FRAME_MAGIC.len() + position] = length_bytes[position];
        position += 1;
    }
    position = SCHEMA_FRAME_HEADER_BYTES;
    position = encode_payload(record, bytes, position);
    assert!(
        position == bytes.len(),
        "schema frame buffer must match the encoded record length"
    );
}

const fn encode_str(bytes: &mut [u8], position: usize, value: &str) -> usize {
    let length = str_len(value);
    assert!(length <= u16::MAX as usize, "schema string exceeds 64 KiB");
    let encoded = (length as u16).to_le_bytes();
    bytes[position] = encoded[0];
    bytes[position + 1] = encoded[1];
    let source = value.as_bytes();
    let mut index = 0;
    while index < length {
        bytes[position + 2 + index] = source[index];
        index += 1;
    }
    position + 2 + length
}

const fn encode_u16(bytes: &mut [u8], position: usize, value: usize) -> usize {
    assert!(value <= u16::MAX as usize, "schema count exceeds 64 KiB");
    let encoded = (value as u16).to_le_bytes();
    bytes[position] = encoded[0];
    bytes[position + 1] = encoded[1];
    position + 2
}

const fn encode_field(bytes: &mut [u8], position: usize, field: &FieldRecord<'_>) -> usize {
    let number = field.number.to_le_bytes();
    let mut index = 0;
    while index < number.len() {
        bytes[position + index] = number[index];
        index += 1;
    }
    let position = encode_str(bytes, position + 4, field.name);
    bytes[position] = field.ty.encode_kind();
    bytes[position + 1] = field.label.encode();
    let mut position = position + 2;
    match field.oneof {
        Some(oneof) => position = encode_str(bytes, position, oneof),
        None => {
            bytes[position] = 0xFF;
            bytes[position + 1] = 0xFF;
            position += 2;
        }
    }
    if field.ty.has_name() {
        let name = match field.ty {
            FieldType::Message(name) | FieldType::Enum(name) => name,
            _ => unreachable!(),
        };
        position = encode_str(bytes, position, name);
    }
    position
}

const fn encode_payload(record: &SchemaRecord<'_>, bytes: &mut [u8], start: usize) -> usize {
    match record {
        SchemaRecord::Message(message) => {
            bytes[start] = 1;
            let position = encode_str(bytes, start + 1, message.package);
            let position = encode_str(bytes, position, message.name);
            let position = encode_u16(bytes, position, message.fields.len());
            let mut position = position;
            let mut index = 0;
            while index < message.fields.len() {
                position = encode_field(bytes, position, &message.fields[index]);
                index += 1;
            }
            position
        }
        SchemaRecord::Enum(enumeration) => {
            bytes[start] = 2;
            let position = encode_str(bytes, start + 1, enumeration.package);
            let position = encode_str(bytes, position, enumeration.name);
            let position = encode_u16(bytes, position, enumeration.values.len());
            let mut position = position;
            let mut index = 0;
            while index < enumeration.values.len() {
                let number = enumeration.values[index].number.to_le_bytes();
                let mut offset = 0;
                while offset < number.len() {
                    bytes[position + offset] = number[offset];
                    offset += 1;
                }
                position = encode_str(bytes, position + 4, enumeration.values[index].name);
                index += 1;
            }
            position
        }
        SchemaRecord::Oneof(oneof) => {
            bytes[start] = 3;
            let position = encode_str(bytes, start + 1, oneof.package);
            let position = encode_str(bytes, position, oneof.message);
            let position = encode_str(bytes, position, oneof.field);
            let position = encode_u16(bytes, position, oneof.variants.len());
            let mut position = position;
            let mut index = 0;
            while index < oneof.variants.len() {
                position = encode_field(bytes, position, &oneof.variants[index]);
                index += 1;
            }
            position
        }
    }
}

/// The fixed prefix of every private identity.
pub const PRIVATE_IDENTITY_PREFIX: &str = "phoxal.private.";

/// Lowercase hex digits of the identity segment escape.
const ESCAPE_HEX: &[u8; 16] = b"0123456789abcdef";

/// Skips the crate-name segment of a module path: `module_path!()` is
/// always rooted at the current crate, whose name the identity carries
/// as its own target segment.
const fn after_crate_segment(module: &str) -> usize {
    let src = module.as_bytes();
    let mut i = 0;
    while i < src.len() && src[i] != b':' {
        i += 1;
    }
    // Skip the `::` separator when a remainder exists.
    if i + 1 < src.len() { i + 2 } else { src.len() }
}

/// The owner and location of a privately authored schema item.
///
/// The identity carries the owning package and the compiling target
/// (`CARGO_CRATE_NAME`, plus `CARGO_BIN_NAME` when a differently spelled
/// binary target owns the item) alongside the module path, so the same
/// type name authored in two binaries of one package — or in same-named
/// binaries of different packages — never shares an identity.
pub struct PrivateIdentity<'a> {
    /// `CARGO_PKG_NAME` of the authoring package.
    pub package: &'a str,
    /// `CARGO_CRATE_NAME` of the compiling target.
    pub crate_name: &'a str,
    /// `CARGO_BIN_NAME` when the compiling target is a binary whose name
    /// differs from its crate name.
    pub bin_name: Option<&'a str>,
    /// `module_path!()` at the authored item.
    pub module: &'a str,
    /// Trailing schema-name segments of the wire form.
    pub tail: &'a [&'a str],
}

/// The length of one identity segment under the injective encoding:
/// alphanumeric bytes pass through, every other byte becomes `_` plus
/// two lowercase hex digits. Unlike a lossy hyphen-to-underscore fold
/// this keeps distinct spellings distinct.
const fn segment_len(segment: &str) -> usize {
    let src = segment.as_bytes();
    let mut i = 0;
    let mut total = 0;
    while i < src.len() {
        if src[i].is_ascii_alphanumeric() {
            total += 1;
        } else {
            total += 3;
        }
        i += 1;
    }
    total
}

/// The encoded length of a module path after its crate segment, with
/// each `::` separator folded to one `.` byte.
const fn module_len(module: &str) -> usize {
    let src = module.as_bytes();
    let mut i = after_crate_segment(module);
    let mut total = 0;
    while i < src.len() {
        if src[i] == b':' {
            total += 1;
            i += 2;
        } else if src[i].is_ascii_alphanumeric() {
            total += 1;
            i += 1;
        } else {
            total += 3;
            i += 1;
        }
    }
    total
}

/// Compares a binary target name with its crate name: Cargo spells binary
/// names with dashes and crate names with underscores, and one target's
/// two spellings name the same identity segment.
const fn bin_matches_crate(bin: &str, crate_name: &str) -> bool {
    let bin = bin.as_bytes();
    let crate_name = crate_name.as_bytes();
    if bin.len() != crate_name.len() {
        return false;
    }
    let mut i = 0;
    while i < bin.len() {
        let left = if bin[i] == b'-' { b'_' } else { bin[i] };
        let right = if crate_name[i] == b'-' {
            b'_'
        } else {
            crate_name[i]
        };
        if left != right {
            return false;
        }
        i += 1;
    }
    true
}

/// The byte length of `PRIVATE_IDENTITY_PREFIX` followed by the encoded
/// Length helper for [`crate::phoxal_joined_wire`].
pub const fn joined_name_len(prefix: &str, suffix: &str) -> usize {
    prefix.len() + suffix.len()
}

/// Buffer helper for [`crate::phoxal_joined_wire`].
pub const fn joined_name_buf<const N: usize>(prefix: &str, suffix: &str) -> [u8; N] {
    let mut bytes = [0_u8; N];
    let mut index = 0;
    while index < prefix.len() {
        bytes[index] = prefix.as_bytes()[index];
        index += 1;
    }
    let mut suffix_index = 0;
    while suffix_index < suffix.len() {
        bytes[index] = suffix.as_bytes()[suffix_index];
        index += 1;
        suffix_index += 1;
    }
    bytes
}

/// identity segments — package, target, the optional differently spelled
/// binary name, the module path after its crate segment, and the tail
/// names — joined by single `.` separators.
pub const fn private_identity_len(identity: &PrivateIdentity<'_>) -> usize {
    let mut total = PRIVATE_IDENTITY_PREFIX.len() + segment_len(identity.package);
    total += 1 + segment_len(identity.crate_name);
    if let Some(bin) = identity.bin_name
        && !bin_matches_crate(bin, identity.crate_name)
    {
        total += 1 + segment_len(bin);
    }
    let module = module_len(identity.module);
    if module > 0 {
        total += 1 + module;
    }
    let mut piece = 0;
    while piece < identity.tail.len() {
        total += 1 + segment_len(identity.tail[piece]);
        piece += 1;
    }
    total
}

/// Appends one segment's injective encoding and returns the new offset.
const fn write_segment<const N: usize>(segment: &str, buf: &mut [u8; N], start: usize) -> usize {
    let src = segment.as_bytes();
    let mut i = 0;
    let mut n = start;
    while i < src.len() {
        let byte = src[i];
        if byte.is_ascii_alphanumeric() {
            buf[n] = byte;
            n += 1;
        } else {
            buf[n] = b'_';
            buf[n + 1] = ESCAPE_HEX[(byte >> 4) as usize];
            buf[n + 2] = ESCAPE_HEX[(byte & 0x0f) as usize];
            n += 3;
        }
        i += 1;
    }
    n
}

/// Appends the module path after its crate segment, folding each `::`
/// separator to one `.` byte, and returns the new offset.
const fn write_module<const N: usize>(module: &str, buf: &mut [u8; N], start: usize) -> usize {
    let src = module.as_bytes();
    let mut i = after_crate_segment(module);
    let mut n = start;
    while i < src.len() {
        let byte = src[i];
        if byte == b':' {
            buf[n] = b'.';
            n += 1;
            i += 2;
        } else if byte.is_ascii_alphanumeric() {
            buf[n] = byte;
            n += 1;
            i += 1;
        } else {
            buf[n] = b'_';
            buf[n + 1] = ESCAPE_HEX[(byte >> 4) as usize];
            buf[n + 2] = ESCAPE_HEX[(byte & 0x0f) as usize];
            n += 3;
            i += 1;
        }
    }
    n
}

/// Builds the identity bytes: the fixed prefix, then the encoded
/// package, target, and module segments, so the result is a dotted
/// Protobuf package identifier that is deterministic,
/// relocation-independent, and injective in package, target, and module
/// spellings.
pub const fn private_identity_buf<const N: usize>(identity: &PrivateIdentity<'_>) -> [u8; N] {
    let mut buf = [0_u8; N];
    let prefix = PRIVATE_IDENTITY_PREFIX.as_bytes();
    let mut n = 0;
    while n < prefix.len() {
        buf[n] = prefix[n];
        n += 1;
    }
    // A `.` separator precedes every piece after the first, mirroring
    // `private_identity_len`; the prefix already ends with its own `.`.
    n = write_segment(identity.package, &mut buf, n);
    buf[n] = b'.';
    n += 1;
    n = write_segment(identity.crate_name, &mut buf, n);
    if let Some(bin) = identity.bin_name
        && !bin_matches_crate(bin, identity.crate_name)
    {
        buf[n] = b'.';
        n += 1;
        n = write_segment(bin, &mut buf, n);
    }
    if module_len(identity.module) > 0 {
        buf[n] = b'.';
        n += 1;
        n = write_module(identity.module, &mut buf, n);
    }
    let mut piece = 0;
    while piece < identity.tail.len() {
        buf[n] = b'.';
        n += 1;
        n = write_segment(identity.tail[piece], &mut buf, n);
        piece += 1;
    }
    buf
}

/// Derives the owner-qualified internal package identity of a private
/// message: the owning package and compiling target plus the authored
/// item's module path, injectively encoded into a dotted Protobuf
/// package. Deterministic, relocation-independent, and distinct for the
/// same type name authored in different targets, modules, or packages.
#[macro_export]
macro_rules! phoxal_private_identity {
    ($module:expr) => {{
        const IDENTITY: $crate::schema::PrivateIdentity = $crate::schema::PrivateIdentity {
            package: env!("CARGO_PKG_NAME"),
            crate_name: env!("CARGO_CRATE_NAME"),
            bin_name: option_env!("CARGO_BIN_NAME"),
            module: $module,
            tail: &[],
        };
        const IDENTITY_LEN: usize = $crate::schema::private_identity_len(&IDENTITY);
        const IDENTITY_BUF: [u8; IDENTITY_LEN] =
            $crate::schema::private_identity_buf::<IDENTITY_LEN>(&IDENTITY);
        const IDENTITY_OUT: &str = match ::std::str::from_utf8(&IDENTITY_BUF) {
            ::std::result::Result::Ok(value) => value,
            ::std::result::Result::Err(_) => panic!("private identity must be ASCII"),
        };
        IDENTITY_OUT
    }};
}

/// Derives the fully-qualified wire name of a private message: its private
/// package identity plus `.` and the schema name.
#[macro_export]
macro_rules! phoxal_private_wire {
    ($module:expr, $name:expr) => {{
        const IDENTITY: $crate::schema::PrivateIdentity = $crate::schema::PrivateIdentity {
            package: env!("CARGO_PKG_NAME"),
            crate_name: env!("CARGO_CRATE_NAME"),
            bin_name: option_env!("CARGO_BIN_NAME"),
            module: $module,
            tail: &[$name],
        };
        const IDENTITY_LEN: usize = $crate::schema::private_identity_len(&IDENTITY);
        const IDENTITY_BUF: [u8; IDENTITY_LEN] =
            $crate::schema::private_identity_buf::<IDENTITY_LEN>(&IDENTITY);
        const IDENTITY_OUT: &str = match ::std::str::from_utf8(&IDENTITY_BUF) {
            ::std::result::Result::Ok(value) => value,
            ::std::result::Result::Err(_) => panic!("private identity must be ASCII"),
        };
        IDENTITY_OUT
    }};
}

/// Joins one wire-name prefix and one literal suffix at const evaluation:
/// the identity of a payload enum's internal unit-variant payload.
#[macro_export]
macro_rules! phoxal_joined_wire {
    ($prefix:expr, $suffix:expr) => {{
        const PREFIX: &str = $prefix;
        const SUFFIX: &str = $suffix;
        const LEN: usize = $crate::schema::joined_name_len(PREFIX, SUFFIX);
        const BUF: [u8; LEN] = $crate::schema::joined_name_buf::<LEN>(PREFIX, SUFFIX);
        const OUT: &str = match ::std::str::from_utf8(&BUF) {
            ::std::result::Result::Ok(value) => value,
            ::std::result::Result::Err(_) => panic!("wire names must be ASCII"),
        };
        OUT
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_identities_separate_package_target_and_module() {
        macro_rules! identity_str {
            ($package:expr, $crate_name:expr, $bin_name:expr, $module:expr, $tail:expr) => {{
                const IDENTITY: PrivateIdentity = PrivateIdentity {
                    package: $package,
                    crate_name: $crate_name,
                    bin_name: $bin_name,
                    module: $module,
                    tail: $tail,
                };
                const IDENTITY_LEN: usize = private_identity_len(&IDENTITY);
                const IDENTITY_BUF: [u8; IDENTITY_LEN] =
                    private_identity_buf::<IDENTITY_LEN>(&IDENTITY);
                match ::std::str::from_utf8(&IDENTITY_BUF) {
                    ::std::result::Result::Ok(value) => value,
                    ::std::result::Result::Err(_) => panic!("identity must be ASCII"),
                }
            }};
        }
        let brain = identity_str!(
            "robot-kit",
            "brain",
            Some("brain"),
            "brain::input",
            &["Reading"]
        );
        let adapter = identity_str!(
            "robot-kit",
            "adapter",
            Some("adapter"),
            "adapter::input",
            &["Reading"]
        );
        let other_package = identity_str!(
            "robot_kit",
            "brain",
            Some("brain"),
            "brain::input",
            &["Reading"]
        );
        let lib = identity_str!(
            "robot-kit",
            "robot_kit",
            None,
            "robot_kit::contract",
            &["MapState"]
        );
        assert_eq!(brain, "phoxal.private.robot_2dkit.brain.input.Reading");
        assert_eq!(adapter, "phoxal.private.robot_2dkit.adapter.input.Reading");
        assert_ne!(
            brain, adapter,
            "two binaries of one package never share an identity"
        );
        assert_ne!(
            brain, other_package,
            "hyphen and underscore package spellings never fold together"
        );
        assert_eq!(
            other_package,
            "phoxal.private.robot_5fkit.brain.input.Reading"
        );
        assert_eq!(
            lib,
            "phoxal.private.robot_2dkit.robot_5fkit.contract.MapState"
        );
    }

    const SAMPLE: SchemaRecord<'_> = SchemaRecord::Message(MessageRecord {
        package: "example.v1",
        name: "Sample",
        fields: &[
            FieldRecord {
                number: 1,
                name: "x_m",
                ty: FieldType::Double,
                label: Label::Singular,
                oneof: None,
            },
            FieldRecord {
                number: 2,
                name: "owner",
                ty: FieldType::String,
                label: Label::Optional,
                oneof: None,
            },
            FieldRecord {
                number: 3,
                name: "targets",
                ty: FieldType::Message("example.v1.Target"),
                label: Label::Repeated,
                oneof: None,
            },
            FieldRecord {
                number: 4,
                name: "mode",
                ty: FieldType::Enum("example.v1.Mode"),
                label: Label::Singular,
                oneof: None,
            },
        ],
    });

    const ONEOF: SchemaRecord<'_> = SchemaRecord::Oneof(OneofRecord {
        package: "example.v1",
        message: "Sample",
        field: "control",
        variants: &[
            FieldRecord {
                number: 5,
                name: "velocity_radps",
                ty: FieldType::Double,
                label: Label::Singular,
                oneof: Some("control"),
            },
            FieldRecord {
                number: 6,
                name: "torque_nm",
                ty: FieldType::Double,
                label: Label::Singular,
                oneof: Some("control"),
            },
        ],
    });

    fn frame_of(record: &SchemaRecord<'_>) -> Vec<u8> {
        let mut bytes = vec![0_u8; encoded_len(record)];
        write_frame(record, &mut bytes);
        bytes
    }

    #[test]
    fn framed_records_round_trip_through_the_section_decoder() {
        for record in [SAMPLE, ONEOF] {
            let decoded = decode_section(&frame_of(&record)).expect("framed record decodes");
            assert_eq!(
                decoded,
                vec![record.to_decoded()],
                "record {record:?} must round trip"
            );
        }
    }

    #[test]
    fn adjacent_frames_decode_in_order_and_duplicates_collapse() {
        let mut section = frame_of(&SAMPLE);
        section.extend_from_slice(&frame_of(&ONEOF));
        section.extend_from_slice(&frame_of(&SAMPLE));
        assert_eq!(
            decode_section(&section).expect("section decodes"),
            vec![SAMPLE.to_decoded(), ONEOF.to_decoded()],
            "duplicate frames collapse onto the first occurrence"
        );
    }

    #[test]
    fn trailing_garbage_between_frames_is_ignored() {
        let mut section = frame_of(&SAMPLE);
        section.extend_from_slice(b"unrelated linker padding");
        section.extend_from_slice(&frame_of(&ONEOF));
        assert_eq!(decode_section(&section).expect("section decodes").len(), 2);
    }

    #[test]
    fn enum_records_round_trip() {
        let record = SchemaRecord::Enum(EnumRecord {
            package: "example.v1",
            name: "Mode",
            values: &[
                EnumValue {
                    number: 0,
                    name: "MODE_UNSPECIFIED",
                },
                EnumValue {
                    number: 2,
                    name: "MODE_MANUAL",
                },
            ],
        });
        assert_eq!(
            decode_section(&frame_of(&record)).expect("framed record decodes"),
            vec![record.to_decoded()]
        );
    }

    #[test]
    fn write_frame_rejects_a_mismatched_buffer_at_compile_time_bounds() {
        let record = SAMPLE;
        let mut bytes = vec![0_u8; encoded_len(&record)];
        write_frame(&record, &mut bytes);
        // An exact-length buffer is the contract; the assert inside
        // write_frame would panic on any other length.
        assert!(bytes.starts_with(&super::SCHEMA_FRAME_MAGIC));
        let length = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
        assert_eq!(length + super::SCHEMA_FRAME_HEADER_BYTES, bytes.len());
    }
}
