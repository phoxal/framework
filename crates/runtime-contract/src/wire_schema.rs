//! The wire shapes Phoxal's process-boundary contracts actually put on the
//! wire, as data.
//!
//! This model exists so compatibility CI can prove a process/wire contract
//! against a published baseline instead of trusting a reviewer to notice. A
//! [`WireSchema`] describes the **serialized** representation one type
//! produces: the map keys, the tag, the element order. It is not the Rust
//! declaration it was written from, and not the predicates a decoder
//! additionally enforces.
//!
//! Three properties make that claim usable:
//!
//! - **It models the real serde representation.** Every shape here corresponds
//!   to something a `Serializer` is actually told to write: a struct is a map
//!   of serialized field names, an externally tagged enum is a one-key map, an
//!   internally tagged enum merges its tag into the body. Nothing is inferred
//!   from a type name.
//! - **An unsupported serde feature fails loudly.** `#[derive(DescribeWire)]`
//!   refuses any attribute whose serialized shape it cannot compute -
//!   `flatten`, adjacent tagging, an arbitrary `serialize_with` - with a
//!   compile error naming the remedy. The schema can never silently
//!   approximate, because a type it cannot model has no impl at all.
//! - **Its rendering is canonical.** [`WireSchema::canonical_json`] emits
//!   sorted-key, whitespace-free JSON, and struct fields and tagged-enum
//!   variants are held in serialized-name order. Two structurally identical
//!   declarations written in different orders render byte-identical, so string
//!   equality is a meaningful comparison. The structure is kept, never hashed
//!   away, so a later checker can name exactly which field or variant moved.
//!
//! Field and tagged-variant order is normalized because every Phoxal codec is
//! name-addressed: the bus encodes MessagePack with named fields
//! (`rmp_serde::to_vec_named`) and every persisted document is JSON. Reordering
//! a struct's fields is therefore not a wire change, and the canonical form
//! says so. Three orders *are* wire facts and survive exactly: tuple, sequence,
//! and array elements, and an untagged enum's variants, which a decoder tries
//! in declaration order and resolves to the first that accepts the value.
//!
//! # Decode-side narrowing is deliberately not shape
//!
//! `#[serde(try_from = "…")]`, `#[serde(from = "…")]`, and a field's
//! `deserialize_with` change *which* wire values a decoder accepts - a finite
//! float, a canonical yaw, a bounded string - not what an encoder writes. This
//! module models the shape, so those are recorded nowhere and are not a
//! compatibility axis here. `#[serde(into = "…")]` is different: it replaces
//! the serialized form outright, so the schema delegates to the named type.
//!
//! # Generator evolution
//!
//! Within one compatibility line, a change to this machinery must not alter the
//! rendered output for an unchanged contract. The canonical rendering is part
//! of the promise: if a refactor here moved bytes, every baseline comparison
//! would report a break that never happened. Change the rendering only together
//! with a deliberate re-baseline.

use std::collections::{BTreeMap, BTreeSet};
use std::num::{NonZeroU32, NonZeroU64};

/// One serialized wire shape.
///
/// The variants are exactly the serde data-model shapes this workspace's
/// process contracts reach for. A shape serde can express but Phoxal never
/// writes is deliberately absent: an author who needs one adds it here on
/// purpose rather than getting an approximation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WireSchema {
    Bool,
    U8,
    U16,
    U32,
    U64,
    I8,
    I16,
    I32,
    I64,
    F32,
    F64,
    String,
    /// A byte string written through `serialize_bytes`, which every Phoxal
    /// codec keeps distinct from a sequence of integers.
    Bytes,
    /// The unit value: `null` in JSON, nil in MessagePack.
    Unit,
    /// A schema-carrying document whose interior is genuinely open, such as an
    /// embedded JSON Schema. Nothing below this point is a compatibility axis.
    Freeform,
    Option(Box<WireSchema>),
    Seq(Box<WireSchema>),
    Map {
        key: Box<WireSchema>,
        value: Box<WireSchema>,
    },
    /// A fixed-arity heterogeneous sequence. Element order is significant.
    Tuple(Vec<WireSchema>),
    /// A fixed-length homogeneous sequence. Element order is significant.
    Array {
        element: Box<WireSchema>,
        length: usize,
    },
    /// A map of serialized field names, held in field-name order.
    Struct {
        fields: Vec<WireField>,
    },
    /// A newtype struct, which serde writes as its inner value with no
    /// wrapping at all.
    Newtype(Box<WireSchema>),
    /// A sum type. A tagged one holds its variants in variant-name order; an
    /// untagged one holds them in declaration order, because that order is what
    /// decides which variant an ambiguous value resolves to.
    Enum {
        representation: EnumRepresentation,
        variants: Vec<WireVariant>,
    },
    /// A type whose `Serialize` is hand-written, together with the wire form
    /// that implementation declares.
    ///
    /// The name says which implementation owns the form, so a checker can
    /// point at the hand-maintained boundary rather than at an anonymous
    /// string somewhere inside a document.
    Opaque {
        name: String,
        wire: Box<WireSchema>,
    },
}

/// One serialized field of a struct or struct variant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WireField {
    /// The serialized name, after `rename` and `rename_all` have been applied.
    pub name: String,
    pub schema: WireSchema,
    pub presence: FieldPresence,
}

impl WireField {
    /// Declare one field.
    #[must_use]
    pub fn new(name: impl Into<String>, schema: WireSchema, presence: FieldPresence) -> Self {
        Self {
            name: name.into(),
            schema,
            presence,
        }
    }

    /// Declare a field that is always written and always required.
    #[must_use]
    pub fn required(name: impl Into<String>, schema: WireSchema) -> Self {
        Self::new(name, schema, FieldPresence::Required)
    }
}

/// Whether a field may be absent from the wire form, and on which side.
///
/// Both halves are compatibility facts: a reader that stops accepting an absent
/// field breaks old writers, and a writer that starts omitting one breaks old
/// readers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FieldPresence {
    /// Always written, and required to decode.
    Required,
    /// Always written; an absent field still decodes, because the declaration
    /// carries `#[serde(default)]` or is an `Option`.
    Defaulted,
    /// May be omitted when writing (`skip_serializing_if`), but is required to
    /// decode.
    Omissible,
    /// May be omitted when writing and is not required to decode.
    Optional,
}

impl FieldPresence {
    /// Build the presence from the two independent serde facts.
    #[must_use]
    pub const fn new(defaulted: bool, omissible: bool) -> Self {
        match (defaulted, omissible) {
            (false, false) => Self::Required,
            (true, false) => Self::Defaulted,
            (false, true) => Self::Omissible,
            (true, true) => Self::Optional,
        }
    }

    /// Whether the wire form admits the field being absent.
    #[must_use]
    pub const fn admits_absence(self) -> bool {
        matches!(self, Self::Defaulted | Self::Optional)
    }

    const fn token(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::Defaulted => "defaulted",
            Self::Omissible => "omissible",
            Self::Optional => "optional",
        }
    }
}

/// How a sum type carries the variant it is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnumRepresentation {
    /// serde's default: a unit variant is its own name, and any other variant
    /// is a one-key map from the variant name to its body.
    ExternallyTagged,
    /// `#[serde(tag = "…")]`: the variant name is a field of the body map.
    InternallyTagged { tag: String },
    /// `#[serde(untagged)]`: the body alone, with no discriminator.
    Untagged,
}

/// One variant of a sum type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WireVariant {
    /// The serialized name, after `rename` and `rename_all`.
    pub name: String,
    pub body: VariantBody,
}

impl WireVariant {
    /// Declare one variant.
    #[must_use]
    pub fn new(name: impl Into<String>, body: VariantBody) -> Self {
        Self {
            name: name.into(),
            body,
        }
    }
}

/// The payload one variant carries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VariantBody {
    Unit,
    /// A `#[serde(other)]` unit variant: the decode fallback for a variant name
    /// this build does not know. It is wire-relevant on its own, because
    /// removing it turns a forward-compatible decode into a hard failure.
    Other,
    Newtype(Box<WireSchema>),
    Tuple(Vec<WireSchema>),
    Struct(Vec<WireField>),
}

impl VariantBody {
    /// A newtype variant body.
    #[must_use]
    pub fn newtype(inner: WireSchema) -> Self {
        Self::Newtype(Box::new(inner))
    }

    /// A struct variant body, normalized into field-name order.
    #[must_use]
    pub fn structure(fields: impl IntoIterator<Item = WireField>) -> Self {
        Self::Struct(sorted_fields(fields))
    }
}

fn sorted_fields(fields: impl IntoIterator<Item = WireField>) -> Vec<WireField> {
    let mut fields = fields.into_iter().collect::<Vec<_>>();
    fields.sort_by(|left, right| left.name.cmp(&right.name));
    fields
}

impl WireSchema {
    /// An optional value.
    #[must_use]
    pub fn option(inner: WireSchema) -> Self {
        Self::Option(Box::new(inner))
    }

    /// A homogeneous sequence.
    #[must_use]
    pub fn seq(item: WireSchema) -> Self {
        Self::Seq(Box::new(item))
    }

    /// A map.
    #[must_use]
    pub fn map(key: WireSchema, value: WireSchema) -> Self {
        Self::Map {
            key: Box::new(key),
            value: Box::new(value),
        }
    }

    /// A fixed-length homogeneous sequence.
    #[must_use]
    pub fn array(element: WireSchema, length: usize) -> Self {
        Self::Array {
            element: Box::new(element),
            length,
        }
    }

    /// A newtype struct, which serde writes transparently.
    #[must_use]
    pub fn newtype(inner: WireSchema) -> Self {
        Self::Newtype(Box::new(inner))
    }

    /// A struct, normalized into field-name order.
    #[must_use]
    pub fn structure(fields: impl IntoIterator<Item = WireField>) -> Self {
        Self::Struct {
            fields: sorted_fields(fields),
        }
    }

    /// A sum type, normalized into variant-name order wherever that order is
    /// not itself a wire fact.
    ///
    /// A tagged enum is resolved by looking its tag up, so reordering its
    /// variants changes nothing and the canonical form sorts them. An untagged
    /// enum is resolved by trying its variants in **declaration order** and
    /// keeping the first that decodes, so for that one representation the order
    /// is part of the contract and is preserved exactly.
    #[must_use]
    pub fn enumeration(
        representation: EnumRepresentation,
        variants: impl IntoIterator<Item = WireVariant>,
    ) -> Self {
        let mut variants = variants.into_iter().collect::<Vec<_>>();
        if !matches!(representation, EnumRepresentation::Untagged) {
            variants.sort_by(|left, right| left.name.cmp(&right.name));
        }
        Self::Enum {
            representation,
            variants,
        }
    }

    /// The declared wire form of a hand-written `Serialize`.
    #[must_use]
    pub fn opaque(name: impl Into<String>, wire: WireSchema) -> Self {
        Self::Opaque {
            name: name.into(),
            wire: Box::new(wire),
        }
    }

    /// The shape with every transparent wrapper removed.
    ///
    /// `Opaque` names an implementation and `Newtype` is written with no
    /// wrapping at all, so neither changes what a decoder sees. Structural
    /// questions - "is this a map?" - are asked of this.
    #[must_use]
    pub fn resolved(&self) -> &WireSchema {
        match self {
            Self::Opaque { wire, .. } => wire.resolved(),
            Self::Newtype(inner) => inner.resolved(),
            other => other,
        }
    }

    /// The canonical rendering: sorted keys, no whitespace, one shape per
    /// declaration.
    ///
    /// Byte equality of two renderings is exactly structural equality of the
    /// two schemas, which is what makes a stored baseline comparable with a
    /// plain string compare.
    #[must_use]
    pub fn canonical_json(&self) -> String {
        let mut out = String::new();
        self.render(&mut out);
        out
    }

    fn render(&self, out: &mut String) {
        match self {
            Self::Bool => out.push_str(r#"{"kind":"bool"}"#),
            Self::U8 => out.push_str(r#"{"kind":"u8"}"#),
            Self::U16 => out.push_str(r#"{"kind":"u16"}"#),
            Self::U32 => out.push_str(r#"{"kind":"u32"}"#),
            Self::U64 => out.push_str(r#"{"kind":"u64"}"#),
            Self::I8 => out.push_str(r#"{"kind":"i8"}"#),
            Self::I16 => out.push_str(r#"{"kind":"i16"}"#),
            Self::I32 => out.push_str(r#"{"kind":"i32"}"#),
            Self::I64 => out.push_str(r#"{"kind":"i64"}"#),
            Self::F32 => out.push_str(r#"{"kind":"f32"}"#),
            Self::F64 => out.push_str(r#"{"kind":"f64"}"#),
            Self::String => out.push_str(r#"{"kind":"string"}"#),
            Self::Bytes => out.push_str(r#"{"kind":"bytes"}"#),
            Self::Unit => out.push_str(r#"{"kind":"unit"}"#),
            Self::Freeform => out.push_str(r#"{"kind":"freeform"}"#),
            Self::Option(inner) => {
                out.push_str(r#"{"kind":"option","value":"#);
                inner.render(out);
                out.push('}');
            }
            Self::Seq(item) => {
                out.push_str(r#"{"item":"#);
                item.render(out);
                out.push_str(r#","kind":"seq"}"#);
            }
            Self::Map { key, value } => {
                out.push_str(r#"{"key":"#);
                key.render(out);
                out.push_str(r#","kind":"map","value":"#);
                value.render(out);
                out.push('}');
            }
            Self::Tuple(items) => {
                out.push_str(r#"{"items":"#);
                render_list(items, out, WireSchema::render);
                out.push_str(r#","kind":"tuple"}"#);
            }
            Self::Array { element, length } => {
                out.push_str(r#"{"element":"#);
                element.render(out);
                out.push_str(r#","kind":"array","length":"#);
                out.push_str(&length.to_string());
                out.push('}');
            }
            Self::Struct { fields } => {
                out.push_str(r#"{"fields":"#);
                render_list(fields, out, WireField::render);
                out.push_str(r#","kind":"struct"}"#);
            }
            Self::Newtype(inner) => {
                out.push_str(r#"{"inner":"#);
                inner.render(out);
                out.push_str(r#","kind":"newtype"}"#);
            }
            Self::Enum {
                representation,
                variants,
            } => {
                out.push_str(r#"{"kind":"enum","representation":"#);
                representation.render(out);
                out.push_str(r#","variants":"#);
                render_list(variants, out, WireVariant::render);
                out.push('}');
            }
            Self::Opaque { name, wire } => {
                out.push_str(r#"{"kind":"opaque","name":"#);
                render_string(name, out);
                out.push_str(r#","wire":"#);
                wire.render(out);
                out.push('}');
            }
        }
    }
}

impl WireField {
    fn render(&self, out: &mut String) {
        out.push_str(r#"{"name":"#);
        render_string(&self.name, out);
        out.push_str(r#","presence":""#);
        out.push_str(self.presence.token());
        out.push_str(r#"","schema":"#);
        self.schema.render(out);
        out.push('}');
    }
}

impl EnumRepresentation {
    fn render(&self, out: &mut String) {
        match self {
            Self::ExternallyTagged => out.push_str(r#"{"style":"external"}"#),
            Self::InternallyTagged { tag } => {
                out.push_str(r#"{"style":"internal","tag":"#);
                render_string(tag, out);
                out.push('}');
            }
            Self::Untagged => out.push_str(r#"{"style":"untagged"}"#),
        }
    }
}

impl WireVariant {
    fn render(&self, out: &mut String) {
        out.push_str(r#"{"body":"#);
        self.body.render(out);
        out.push_str(r#","name":"#);
        render_string(&self.name, out);
        out.push('}');
    }
}

impl VariantBody {
    fn render(&self, out: &mut String) {
        match self {
            Self::Unit => out.push_str(r#"{"kind":"unit"}"#),
            Self::Other => out.push_str(r#"{"kind":"other"}"#),
            Self::Newtype(inner) => {
                out.push_str(r#"{"inner":"#);
                inner.render(out);
                out.push_str(r#","kind":"newtype"}"#);
            }
            Self::Tuple(items) => {
                out.push_str(r#"{"items":"#);
                render_list(items, out, WireSchema::render);
                out.push_str(r#","kind":"tuple"}"#);
            }
            Self::Struct(fields) => {
                out.push_str(r#"{"fields":"#);
                render_list(fields, out, WireField::render);
                out.push_str(r#","kind":"struct"}"#);
            }
        }
    }
}

fn render_list<T>(items: &[T], out: &mut String, render: impl Fn(&T, &mut String)) {
    out.push('[');
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        render(item, out);
    }
    out.push(']');
}

/// One JSON string literal, escaped exactly as a JSON writer would.
fn render_string(value: &str, out: &mut String) {
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if control <= '\u{1f}' => {
                out.push_str(&format!("\\u{:04x}", control as u32));
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

/// A type whose serialized wire shape is declared.
///
/// Implement it with `#[derive(phoxal_macros::DescribeWire)]` wherever the
/// declaration alone decides the shape, and by hand beside a type's custom
/// `Serialize` where it does not. A hand-written implementation states a fact
/// the compiler cannot check, so it belongs next to the serializer it mirrors
/// and is pinned by a test that serializes a real value and asserts the
/// declared schema names the shape that came out.
pub trait DescribeWire {
    /// The shape a value of this type puts on the wire.
    fn wire_schema() -> WireSchema;
}

macro_rules! primitive_wire_schema {
    ($($ty:ty => $schema:expr),* $(,)?) => {
        $(
            impl DescribeWire for $ty {
                fn wire_schema() -> WireSchema {
                    $schema
                }
            }
        )*
    };
}

primitive_wire_schema! {
    bool => WireSchema::Bool,
    u8 => WireSchema::U8,
    u16 => WireSchema::U16,
    u32 => WireSchema::U32,
    u64 => WireSchema::U64,
    // serde writes both pointer-sized integers at their widest form on every
    // target, so the wire shape does not vary with the host.
    usize => WireSchema::U64,
    i8 => WireSchema::I8,
    i16 => WireSchema::I16,
    i32 => WireSchema::I32,
    i64 => WireSchema::I64,
    isize => WireSchema::I64,
    f32 => WireSchema::F32,
    f64 => WireSchema::F64,
    String => WireSchema::String,
    str => WireSchema::String,
    () => WireSchema::Unit,
    NonZeroU32 => WireSchema::U32,
    NonZeroU64 => WireSchema::U64,
}

impl<T: DescribeWire + ?Sized> DescribeWire for &T {
    fn wire_schema() -> WireSchema {
        T::wire_schema()
    }
}

impl<T: DescribeWire + ?Sized> DescribeWire for Box<T> {
    fn wire_schema() -> WireSchema {
        T::wire_schema()
    }
}

impl<T: DescribeWire> DescribeWire for Option<T> {
    fn wire_schema() -> WireSchema {
        WireSchema::option(T::wire_schema())
    }
}

impl<T: DescribeWire> DescribeWire for Vec<T> {
    fn wire_schema() -> WireSchema {
        WireSchema::seq(T::wire_schema())
    }
}

impl<T: DescribeWire> DescribeWire for [T] {
    fn wire_schema() -> WireSchema {
        WireSchema::seq(T::wire_schema())
    }
}

impl<T: DescribeWire, const N: usize> DescribeWire for [T; N] {
    fn wire_schema() -> WireSchema {
        WireSchema::array(T::wire_schema(), N)
    }
}

impl<T: DescribeWire> DescribeWire for BTreeSet<T> {
    fn wire_schema() -> WireSchema {
        WireSchema::seq(T::wire_schema())
    }
}

impl<K: DescribeWire, V: DescribeWire> DescribeWire for BTreeMap<K, V> {
    fn wire_schema() -> WireSchema {
        WireSchema::map(K::wire_schema(), V::wire_schema())
    }
}

macro_rules! tuple_wire_schema {
    ($($name:ident),+) => {
        impl<$($name: DescribeWire),+> DescribeWire for ($($name,)+) {
            fn wire_schema() -> WireSchema {
                WireSchema::Tuple(::std::vec![$($name::wire_schema()),+])
            }
        }
    };
}

tuple_wire_schema!(A);
tuple_wire_schema!(A, B);
tuple_wire_schema!(A, B, C);
tuple_wire_schema!(A, B, C, D);
tuple_wire_schema!(A, B, C, D, E);
tuple_wire_schema!(A, B, C, D, E, F);

impl DescribeWire for serde_json::Value {
    /// A schema-carrying config document is genuinely open: its interior is
    /// authored per participant and is not a framework compatibility axis.
    fn wire_schema() -> WireSchema {
        WireSchema::opaque("serde_json::Value", WireSchema::Freeform)
    }
}

/// A concrete value that does not have the shape its type declared.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{path}: expected {expected}, found {found}")]
pub struct WireMismatch {
    /// Where in the document the disagreement is, as a JSON-ish path.
    pub path: String,
    pub expected: String,
    pub found: String,
}

impl WireMismatch {
    fn new(path: &str, expected: impl Into<String>, found: impl Into<String>) -> Self {
        Self {
            path: if path.is_empty() {
                String::from("$")
            } else {
                path.to_string()
            },
            expected: expected.into(),
            found: found.into(),
        }
    }
}

impl WireSchema {
    /// Check one serialized value against this shape.
    ///
    /// This is what makes a hand-written [`DescribeWire`] trustworthy: a test
    /// serializes a real value and asserts the declared schema names the shape
    /// that actually came out, so a declaration cannot quietly drift from the
    /// serializer beside it.
    ///
    /// The value is inspected as `serde_json::Value`, which collapses two
    /// distinctions the serde data model keeps. Integer width is checked by
    /// range rather than by type, and a byte string arrives as an array of
    /// byte-valued numbers (what `serde_json` writes for `serialize_bytes`) or
    /// as a string, so both are accepted for [`WireSchema::Bytes`].
    pub fn conforms(&self, value: &serde_json::Value) -> Result<(), WireMismatch> {
        self.check(value, "")
    }

    fn check(&self, value: &serde_json::Value, path: &str) -> Result<(), WireMismatch> {
        use serde_json::Value;

        let mismatch = |expected: &str| Err(WireMismatch::new(path, expected, describe(value)));
        match self {
            Self::Opaque { wire, .. } => wire.check(value, path),
            Self::Newtype(inner) => inner.check(value, path),
            Self::Freeform => Ok(()),
            Self::Bool => {
                if value.is_boolean() {
                    Ok(())
                } else {
                    mismatch("bool")
                }
            }
            Self::U8 => check_unsigned(value, path, u64::from(u8::MAX), "u8"),
            Self::U16 => check_unsigned(value, path, u64::from(u16::MAX), "u16"),
            Self::U32 => check_unsigned(value, path, u64::from(u32::MAX), "u32"),
            Self::U64 => check_unsigned(value, path, u64::MAX, "u64"),
            Self::I8 => check_signed(value, path, i64::from(i8::MIN), i64::from(i8::MAX), "i8"),
            Self::I16 => check_signed(value, path, i64::from(i16::MIN), i64::from(i16::MAX), "i16"),
            Self::I32 => check_signed(value, path, i64::from(i32::MIN), i64::from(i32::MAX), "i32"),
            Self::I64 => check_signed(value, path, i64::MIN, i64::MAX, "i64"),
            Self::F32 | Self::F64 => {
                if value.is_number() {
                    Ok(())
                } else {
                    mismatch("a number")
                }
            }
            Self::String => {
                if value.is_string() {
                    Ok(())
                } else {
                    mismatch("a string")
                }
            }
            Self::Bytes => match value {
                Value::String(_) => Ok(()),
                Value::Array(items) => {
                    for (index, item) in items.iter().enumerate() {
                        check_unsigned(
                            item,
                            &format!("{path}[{index}]"),
                            u64::from(u8::MAX),
                            "a byte",
                        )?;
                    }
                    Ok(())
                }
                _ => mismatch("a byte string"),
            },
            Self::Unit => {
                if value.is_null() {
                    Ok(())
                } else {
                    mismatch("null")
                }
            }
            Self::Option(inner) => {
                if value.is_null() {
                    Ok(())
                } else {
                    inner.check(value, path)
                }
            }
            Self::Seq(item) => match value {
                Value::Array(items) => {
                    for (index, element) in items.iter().enumerate() {
                        item.check(element, &format!("{path}[{index}]"))?;
                    }
                    Ok(())
                }
                _ => mismatch("a sequence"),
            },
            Self::Array { element, length } => match value {
                Value::Array(items) if items.len() == *length => {
                    for (index, item) in items.iter().enumerate() {
                        element.check(item, &format!("{path}[{index}]"))?;
                    }
                    Ok(())
                }
                _ => mismatch(&format!("a sequence of exactly {length}")),
            },
            Self::Tuple(items) => match value {
                Value::Array(elements) if elements.len() == items.len() => {
                    for (index, (schema, element)) in items.iter().zip(elements).enumerate() {
                        schema.check(element, &format!("{path}[{index}]"))?;
                    }
                    Ok(())
                }
                _ => mismatch(&format!("a tuple of {}", items.len())),
            },
            Self::Map { key, value: item } => match value {
                Value::Object(entries) => {
                    for (name, entry) in entries {
                        key.check_key(name, path)?;
                        item.check(entry, &format!("{path}.{name}"))?;
                    }
                    Ok(())
                }
                _ => mismatch("a map"),
            },
            Self::Struct { fields } => match value {
                Value::Object(entries) => check_fields(fields, entries, path),
                _ => mismatch("a map"),
            },
            Self::Enum {
                representation,
                variants,
            } => representation.check(variants, value, path),
        }
    }

    /// A map key, which every Phoxal codec writes as text even when the key
    /// type is not a string.
    fn check_key(&self, key: &str, path: &str) -> Result<(), WireMismatch> {
        let path = format!("{path}.{key}(key)");
        match self.resolved() {
            Self::U8 | Self::U16 | Self::U32 | Self::U64 => match key.parse::<u64>() {
                Ok(parsed) => self.check(&serde_json::Value::from(parsed), &path),
                Err(_) => Err(WireMismatch::new(&path, "an unsigned key", "text")),
            },
            Self::I8 | Self::I16 | Self::I32 | Self::I64 => match key.parse::<i64>() {
                Ok(parsed) => self.check(&serde_json::Value::from(parsed), &path),
                Err(_) => Err(WireMismatch::new(&path, "a signed key", "text")),
            },
            _ => self.check(&serde_json::Value::String(key.to_string()), &path),
        }
    }
}

fn check_fields(
    fields: &[WireField],
    entries: &serde_json::Map<String, serde_json::Value>,
    path: &str,
) -> Result<(), WireMismatch> {
    for field in fields {
        match entries.get(&field.name) {
            Some(value) => field
                .schema
                .check(value, &format!("{path}.{}", field.name))?,
            None if field.presence.admits_absence() => {}
            None => {
                return Err(WireMismatch::new(
                    &format!("{path}.{}", field.name),
                    "the declared field",
                    "an absent field",
                ));
            }
        }
    }
    for name in entries.keys() {
        if !fields.iter().any(|field| field.name == *name) {
            return Err(WireMismatch::new(
                &format!("{path}.{name}"),
                "no such field in the declared schema",
                "a serialized field",
            ));
        }
    }
    Ok(())
}

impl EnumRepresentation {
    fn check(
        &self,
        variants: &[WireVariant],
        value: &serde_json::Value,
        path: &str,
    ) -> Result<(), WireMismatch> {
        use serde_json::Value;

        match self {
            Self::ExternallyTagged => match value {
                Value::String(name) => match find(variants, name) {
                    Some(variant)
                        if matches!(variant.body, VariantBody::Unit | VariantBody::Other) =>
                    {
                        Ok(())
                    }
                    Some(_) => Err(WireMismatch::new(path, "a unit variant name", "a string")),
                    None => Err(WireMismatch::new(path, "a declared variant", "a string")),
                },
                Value::Object(entries) if entries.len() == 1 => {
                    let Some((name, body)) = entries.iter().next() else {
                        return Err(WireMismatch::new(path, "a one-key map", "an empty map"));
                    };
                    let Some(variant) = find(variants, name) else {
                        return Err(WireMismatch::new(
                            &format!("{path}.{name}"),
                            "a declared variant",
                            "an unknown variant",
                        ));
                    };
                    variant.body.check(body, &format!("{path}.{name}"))
                }
                _ => Err(WireMismatch::new(
                    path,
                    "an externally tagged enum",
                    describe(value),
                )),
            },
            Self::InternallyTagged { tag } => {
                let Value::Object(entries) = value else {
                    return Err(WireMismatch::new(path, "a map", describe(value)));
                };
                let Some(Value::String(name)) = entries.get(tag) else {
                    return Err(WireMismatch::new(
                        &format!("{path}.{tag}"),
                        "the variant tag",
                        "an absent or non-text tag",
                    ));
                };
                let Some(variant) = find(variants, name) else {
                    return Err(WireMismatch::new(
                        &format!("{path}.{tag}"),
                        "a declared variant",
                        "an unknown variant",
                    ));
                };
                let mut body = entries.clone();
                body.remove(tag);
                match &variant.body {
                    VariantBody::Unit | VariantBody::Other => {
                        if body.is_empty() {
                            Ok(())
                        } else {
                            Err(WireMismatch::new(path, "no further fields", "extra fields"))
                        }
                    }
                    VariantBody::Struct(fields) => check_fields(fields, &body, path),
                    // serde merges a newtype variant's map into the tagged
                    // object, so the inner shape has to be a map itself.
                    VariantBody::Newtype(inner) => match inner.resolved() {
                        WireSchema::Struct { fields } => check_fields(fields, &body, path),
                        _ => Err(WireMismatch::new(
                            path,
                            "a newtype variant over a map",
                            "a non-map inner shape",
                        )),
                    },
                    VariantBody::Tuple(_) => Err(WireMismatch::new(
                        path,
                        "no tuple variant under an internal tag",
                        "a tuple variant",
                    )),
                }
            }
            Self::Untagged => {
                for variant in variants {
                    if variant.body.check(value, path).is_ok() {
                        return Ok(());
                    }
                }
                Err(WireMismatch::new(
                    path,
                    "any declared untagged variant",
                    describe(value),
                ))
            }
        }
    }
}

fn find<'a>(variants: &'a [WireVariant], name: &str) -> Option<&'a WireVariant> {
    variants.iter().find(|variant| variant.name == name)
}

impl VariantBody {
    fn check(&self, value: &serde_json::Value, path: &str) -> Result<(), WireMismatch> {
        match self {
            Self::Unit | Self::Other => WireSchema::Unit.check(value, path),
            Self::Newtype(inner) => inner.check(value, path),
            Self::Tuple(items) => WireSchema::Tuple(items.clone()).check(value, path),
            Self::Struct(fields) => match value {
                serde_json::Value::Object(entries) => check_fields(fields, entries, path),
                _ => Err(WireMismatch::new(path, "a map", describe(value))),
            },
        }
    }
}

fn check_unsigned(
    value: &serde_json::Value,
    path: &str,
    max: u64,
    expected: &str,
) -> Result<(), WireMismatch> {
    match value.as_u64() {
        Some(parsed) if parsed <= max => Ok(()),
        _ => Err(WireMismatch::new(path, expected, describe(value))),
    }
}

fn check_signed(
    value: &serde_json::Value,
    path: &str,
    min: i64,
    max: i64,
    expected: &str,
) -> Result<(), WireMismatch> {
    match value.as_i64() {
        Some(parsed) if (min..=max).contains(&parsed) => Ok(()),
        _ => Err(WireMismatch::new(path, expected, describe(value))),
    }
}

fn describe(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => String::from("null"),
        serde_json::Value::Bool(_) => String::from("a bool"),
        serde_json::Value::Number(number) => format!("the number {number}"),
        serde_json::Value::String(_) => String::from("a string"),
        serde_json::Value::Array(items) => format!("a sequence of {}", items.len()),
        serde_json::Value::Object(_) => String::from("a map"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(name: &str, schema: WireSchema) -> WireField {
        WireField::required(name, schema)
    }

    /// The canonical rendering is a function of the shape alone, so two
    /// declarations that differ only in the order their fields and variants
    /// were written render the same bytes. This is what lets a checker compare
    /// a stored baseline with a plain string equality.
    #[test]
    fn authoring_order_does_not_change_the_canonical_bytes() {
        let one = WireSchema::structure([
            field("alpha", WireSchema::U32),
            field("beta", WireSchema::String),
            field("gamma", WireSchema::Bool),
        ]);
        let other = WireSchema::structure([
            field("gamma", WireSchema::Bool),
            field("alpha", WireSchema::U32),
            field("beta", WireSchema::String),
        ]);
        assert_eq!(one.canonical_json(), other.canonical_json());
        assert_eq!(one, other);

        let first = WireSchema::enumeration(
            EnumRepresentation::ExternallyTagged,
            [
                WireVariant::new("a", VariantBody::Unit),
                WireVariant::new("b", VariantBody::newtype(WireSchema::U8)),
            ],
        );
        let second = WireSchema::enumeration(
            EnumRepresentation::ExternallyTagged,
            [
                WireVariant::new("b", VariantBody::newtype(WireSchema::U8)),
                WireVariant::new("a", VariantBody::Unit),
            ],
        );
        assert_eq!(first.canonical_json(), second.canonical_json());
    }

    /// Element order in a tuple or an array *is* the wire shape, so it is one
    /// place normalization must not reach.
    #[test]
    fn positional_element_order_is_preserved() {
        let one = WireSchema::Tuple(vec![WireSchema::U8, WireSchema::String]);
        let other = WireSchema::Tuple(vec![WireSchema::String, WireSchema::U8]);
        assert_ne!(one.canonical_json(), other.canonical_json());
    }

    /// An untagged enum decodes by keeping the first variant that accepts the
    /// value, so its declaration order decides what an ambiguous value becomes
    /// and must survive normalization. A tagged one is looked up by name and
    /// does not.
    #[test]
    fn untagged_variant_order_is_the_one_variant_order_that_survives() {
        let declared = [
            WireVariant::new("Zulu", VariantBody::newtype(WireSchema::I64)),
            WireVariant::new("Alpha", VariantBody::newtype(WireSchema::F64)),
        ];
        let untagged = WireSchema::enumeration(EnumRepresentation::Untagged, declared.clone());
        let WireSchema::Enum { variants, .. } = &untagged else {
            panic!("an enum");
        };
        assert_eq!(variants[0].name, "Zulu");

        let tagged = WireSchema::enumeration(EnumRepresentation::ExternallyTagged, declared);
        let WireSchema::Enum { variants, .. } = &tagged else {
            panic!("an enum");
        };
        assert_eq!(variants[0].name, "Alpha");
    }

    /// Every rendering is one whitespace-free JSON object whose keys ascend, so
    /// a reviewer reading a diff sees a stable document and not a formatting
    /// artifact.
    #[test]
    fn every_variant_renders_as_canonical_json() {
        let every = [
            WireSchema::Bool,
            WireSchema::U8,
            WireSchema::U16,
            WireSchema::U32,
            WireSchema::U64,
            WireSchema::I8,
            WireSchema::I16,
            WireSchema::I32,
            WireSchema::I64,
            WireSchema::F32,
            WireSchema::F64,
            WireSchema::String,
            WireSchema::Bytes,
            WireSchema::Unit,
            WireSchema::Freeform,
            WireSchema::option(WireSchema::U8),
            WireSchema::seq(WireSchema::U8),
            WireSchema::map(WireSchema::String, WireSchema::U8),
            WireSchema::Tuple(vec![WireSchema::U8, WireSchema::Bool]),
            WireSchema::array(WireSchema::U8, 4),
            WireSchema::structure([field("only", WireSchema::U8)]),
            WireSchema::newtype(WireSchema::String),
            WireSchema::enumeration(
                EnumRepresentation::InternallyTagged {
                    tag: String::from("schema"),
                },
                [WireVariant::new(
                    "v0",
                    VariantBody::structure([field("value", WireSchema::U8)]),
                )],
            ),
            WireSchema::enumeration(
                EnumRepresentation::Untagged,
                [
                    WireVariant::new("Text", VariantBody::newtype(WireSchema::String)),
                    WireVariant::new("Count", VariantBody::Other),
                    WireVariant::new("Pair", VariantBody::Tuple(vec![WireSchema::U8])),
                ],
            ),
            WireSchema::opaque("Digest", WireSchema::String),
        ];
        for schema in every {
            let rendered = schema.canonical_json();
            assert!(
                !rendered.contains(' ') && rendered.starts_with('{') && rendered.ends_with('}'),
                "{rendered}"
            );
            let parsed = serde_json::from_str::<serde_json::Value>(&rendered)
                .expect("the canonical rendering is JSON");
            assert!(parsed.is_object(), "{rendered}");
            // A second render of the same shape is the same bytes.
            assert_eq!(schema.canonical_json(), rendered);
        }
    }

    #[test]
    fn nested_composition_renders_deterministically() {
        let schema = WireSchema::structure([
            field(
                "rows",
                WireSchema::seq(WireSchema::structure([
                    field(
                        "id",
                        WireSchema::opaque("ParticipantId", WireSchema::String),
                    ),
                    WireField::new(
                        "detail",
                        WireSchema::option(WireSchema::String),
                        FieldPresence::Optional,
                    ),
                ])),
            ),
            field(
                "index",
                WireSchema::map(WireSchema::String, WireSchema::seq(WireSchema::U32)),
            ),
        ]);
        assert_eq!(
            schema.canonical_json(),
            concat!(
                r#"{"fields":["#,
                r#"{"name":"index","presence":"required","schema":"#,
                r#"{"key":{"kind":"string"},"kind":"map","value":{"item":{"kind":"u32"},"kind":"seq"}}},"#,
                r#"{"name":"rows","presence":"required","schema":{"item":{"fields":["#,
                r#"{"name":"detail","presence":"optional","schema":{"kind":"option","value":{"kind":"string"}}},"#,
                r#"{"name":"id","presence":"required","schema":{"kind":"opaque","name":"ParticipantId","wire":{"kind":"string"}}}"#,
                r#"],"kind":"struct"},"kind":"seq"}}"#,
                r#"],"kind":"struct"}"#,
            )
        );
    }

    #[test]
    fn a_name_with_json_metacharacters_is_escaped() {
        let schema = WireSchema::opaque("a\"b\\c\nd", WireSchema::Unit);
        let rendered = schema.canonical_json();
        assert!(rendered.contains(r#""a\"b\\c\nd""#), "{rendered}");
        serde_json::from_str::<serde_json::Value>(&rendered).expect("still valid JSON");
    }

    #[test]
    fn the_standard_impls_describe_the_shapes_serde_writes() {
        assert_eq!(
            <Option<u8>>::wire_schema(),
            WireSchema::option(WireSchema::U8)
        );
        assert_eq!(
            <Vec<String>>::wire_schema(),
            WireSchema::seq(WireSchema::String)
        );
        assert_eq!(
            <BTreeMap<String, u64>>::wire_schema(),
            WireSchema::map(WireSchema::String, WireSchema::U64)
        );
        assert_eq!(
            <[u8; 3]>::wire_schema(),
            WireSchema::array(WireSchema::U8, 3)
        );
        assert_eq!(
            <(u8, bool)>::wire_schema(),
            WireSchema::Tuple(vec![WireSchema::U8, WireSchema::Bool])
        );
        assert_eq!(<&str>::wire_schema(), WireSchema::String);
        assert_eq!(<()>::wire_schema(), WireSchema::Unit);
        assert_eq!(<NonZeroU64>::wire_schema(), WireSchema::U64);
        assert_eq!(
            <serde_json::Value>::wire_schema(),
            WireSchema::opaque("serde_json::Value", WireSchema::Freeform)
        );
        // Pointer-sized integers are written at their widest form regardless of
        // the host, so the declared shape cannot vary with the target.
        assert_eq!(<usize>::wire_schema(), WireSchema::U64);
    }

    #[test]
    fn conformance_accepts_the_shape_and_names_the_first_disagreement() {
        let schema = WireSchema::structure([
            field("count", WireSchema::U8),
            WireField::new(
                "label",
                WireSchema::option(WireSchema::String),
                FieldPresence::Optional,
            ),
        ]);
        assert_eq!(
            schema.conforms(&serde_json::json!({"count": 7, "label": "ok"})),
            Ok(())
        );
        // An omissible field may be absent.
        assert_eq!(schema.conforms(&serde_json::json!({"count": 7})), Ok(()));

        let missing = schema
            .conforms(&serde_json::json!({"label": null}))
            .expect_err("a required field cannot be absent");
        assert_eq!(missing.path, ".count");

        let extra = schema
            .conforms(&serde_json::json!({"count": 1, "label": null, "surprise": 2}))
            .expect_err("an undeclared field is a disagreement");
        assert_eq!(extra.path, ".surprise");

        let too_wide = schema
            .conforms(&serde_json::json!({"count": 300}))
            .expect_err("an out-of-range integer is not a u8");
        assert_eq!(too_wide.expected, "u8");
    }

    #[test]
    fn conformance_reads_each_enum_representation_the_way_serde_writes_it() {
        let external = WireSchema::enumeration(
            EnumRepresentation::ExternallyTagged,
            [
                WireVariant::new("stop", VariantBody::Unit),
                WireVariant::new(
                    "go",
                    VariantBody::structure([field("speed", WireSchema::F32)]),
                ),
            ],
        );
        assert_eq!(external.conforms(&serde_json::json!("stop")), Ok(()));
        assert_eq!(
            external.conforms(&serde_json::json!({"go": {"speed": 1.5}})),
            Ok(())
        );
        assert!(external.conforms(&serde_json::json!("fly")).is_err());

        let internal = WireSchema::enumeration(
            EnumRepresentation::InternallyTagged {
                tag: String::from("schema"),
            },
            [WireVariant::new(
                "phoxal/example/v0",
                VariantBody::structure([field("value", WireSchema::U8)]),
            )],
        );
        assert_eq!(
            internal.conforms(&serde_json::json!({"schema": "phoxal/example/v0", "value": 3})),
            Ok(())
        );
        assert!(
            internal
                .conforms(&serde_json::json!({"schema": "phoxal/example/v1", "value": 3}))
                .is_err()
        );

        // An internally tagged newtype variant merges the inner map into the
        // tagged object, exactly as serde writes it.
        let merged = WireSchema::enumeration(
            EnumRepresentation::InternallyTagged {
                tag: String::from("schema"),
            },
            [WireVariant::new(
                "phoxal/example/v0",
                VariantBody::newtype(WireSchema::opaque(
                    "Inner",
                    WireSchema::structure([field("value", WireSchema::U8)]),
                )),
            )],
        );
        assert_eq!(
            merged.conforms(&serde_json::json!({"schema": "phoxal/example/v0", "value": 3})),
            Ok(())
        );

        let untagged = WireSchema::enumeration(
            EnumRepresentation::Untagged,
            [
                WireVariant::new("Bool", VariantBody::newtype(WireSchema::Bool)),
                WireVariant::new("Text", VariantBody::newtype(WireSchema::String)),
            ],
        );
        assert_eq!(untagged.conforms(&serde_json::json!(true)), Ok(()));
        assert_eq!(untagged.conforms(&serde_json::json!("hello")), Ok(()));
        assert!(untagged.conforms(&serde_json::json!(1)).is_err());
    }

    #[test]
    fn conformance_reads_a_byte_string_in_both_renderings() {
        assert_eq!(
            WireSchema::Bytes.conforms(&serde_json::json!([1, 2, 255])),
            Ok(())
        );
        assert_eq!(WireSchema::Bytes.conforms(&serde_json::json!("ab")), Ok(()));
        assert!(
            WireSchema::Bytes
                .conforms(&serde_json::json!([256]))
                .is_err()
        );
    }

    #[test]
    fn conformance_checks_a_maps_keys_as_well_as_its_values() {
        let text_keys = WireSchema::map(
            WireSchema::opaque("CapabilityId", WireSchema::String),
            WireSchema::I8,
        );
        assert_eq!(
            text_keys.conforms(&serde_json::json!({"wheel": -1})),
            Ok(())
        );
        assert!(
            text_keys
                .conforms(&serde_json::json!({"wheel": 900}))
                .is_err()
        );

        // A numeric key is still written as text, so it is parsed back before
        // the declared shape decides.
        let numeric_keys = WireSchema::map(WireSchema::U32, WireSchema::Bool);
        assert_eq!(
            numeric_keys.conforms(&serde_json::json!({"7": true})),
            Ok(())
        );
        assert!(
            numeric_keys
                .conforms(&serde_json::json!({"seven": true}))
                .is_err()
        );
    }

    /// A newtype wrapper and a named opaque form are both transparent to a
    /// decoder, so resolving through them is what structural questions ask.
    #[test]
    fn transparent_wrappers_resolve_to_the_shape_underneath() {
        let schema = WireSchema::opaque(
            "Wrapper",
            WireSchema::newtype(WireSchema::structure([field("value", WireSchema::U8)])),
        );
        assert!(matches!(schema.resolved(), WireSchema::Struct { .. }));
        assert_eq!(schema.conforms(&serde_json::json!({"value": 1})), Ok(()));
    }
}
