//! The typed field codec shared by authored and generated definitions.
//!
//! [`PhoxalWire`] is the one surface a containing message's generated
//! Prost `Message` implementation delegates every field to: scalars,
//! strings, bytes, messages, enumerations, and oneofs all encode and
//! decode through it, so a field's Rust type alone decides its wire
//! behavior — no authoring hint distinguishes an enumeration field from a
//! message field. Prost remains the wire implementation: values use
//! Prost's public encoding primitives, and the byte output matches what
//! Prost's own derived codecs produce for the same Protobuf definition
//! (implicit presence skips defaults, repeated scalars and enumerations
//! pack, optional fields encode whenever present). Enumeration values
//! fail decoding when the wire carries a number the definition does not
//! declare.

use crate::generated::prost::DecodeError;
use crate::generated::prost::bytes::{Buf, BufMut};
use crate::generated::prost::encoding::{
    DecodeContext, WireType, encode_key, encode_varint, encoded_len_varint, key_len,
};

/// One Protobuf definition usable as a message field.
///
/// Implementations do not require `Default`: a payload enum has no valid
/// unselected value, so constructing one occurrence from wire input goes
/// through [`PhoxalWire::phoxal_merge_fresh`] instead of defaulting and
/// merging.
pub trait PhoxalWire: Sized {
    /// The wire type of one encoded occurrence of this field.
    const WIRE_TYPE: WireType;
    /// Whether containing schema records render this definition as an
    /// enumeration field rather than a message field.
    const IS_ENUM: bool;
    /// Whether an implicit-presence (non-`Option`) occurrence of this
    /// field is omitted when it equals its default value, matching
    /// Prost's derived encoders.
    const SKIP_DEFAULT: bool;

    /// Appends this value's bytes without the field key.
    fn phoxal_write<B: BufMut>(&self, buf: &mut B);
    /// The length of this value's bytes without the field key.
    fn phoxal_value_len(&self) -> usize;
    /// Merges one occurrence of `tag` into `self` in place.
    fn phoxal_merge<B: Buf>(
        &mut self,
        tag: u32,
        wire_type: WireType,
        buf: &mut B,
        ctx: DecodeContext,
    ) -> Result<(), DecodeError>;
    /// Constructs one value from a single occurrence of `tag`, failing
    /// when the occurrence does not select a valid value.
    fn phoxal_merge_fresh<B: Buf>(
        tag: u32,
        wire_type: WireType,
        buf: &mut B,
        ctx: DecodeContext,
    ) -> Result<Self, DecodeError>;

    /// Appends this field's key and value for `tag`.
    fn phoxal_encode<B: BufMut>(&self, tag: u32, buf: &mut B) {
        encode_key(tag, Self::WIRE_TYPE, buf);
        self.phoxal_write(buf);
    }

    /// The encoded length of this field including its key.
    fn phoxal_encoded_len(&self, tag: u32) -> usize {
        key_len(tag) + self.phoxal_value_len()
    }
}

/// Constructs one defaulted value merged from a single occurrence: the
/// [`PhoxalWire::phoxal_merge_fresh`] implementation every `Default`-valued
/// definition shares.
pub fn merge_fresh_default<T, B>(
    tag: u32,
    wire_type: WireType,
    buf: &mut B,
    ctx: DecodeContext,
) -> Result<T, DecodeError>
where
    T: PhoxalWire + Default,
    B: Buf,
{
    let mut value = T::default();
    value.phoxal_merge(tag, wire_type, buf, ctx)?;
    Ok(value)
}

macro_rules! varint_wire {
    ($ty:ty, $module:ident, to_u64($value:ident) $conversion:expr) => {
        impl PhoxalWire for $ty {
            const WIRE_TYPE: WireType = WireType::Varint;
            const IS_ENUM: bool = false;
            const SKIP_DEFAULT: bool = true;

            fn phoxal_write<B: BufMut>(&self, buf: &mut B) {
                let $value = *self;
                encode_varint($conversion, buf);
            }

            fn phoxal_value_len(&self) -> usize {
                let $value = *self;
                encoded_len_varint($conversion)
            }

            fn phoxal_merge<B: Buf>(
                &mut self,
                _tag: u32,
                wire_type: WireType,
                buf: &mut B,
                ctx: DecodeContext,
            ) -> Result<(), DecodeError> {
                crate::generated::prost::encoding::$module::merge(wire_type, self, buf, ctx)
            }

            fn phoxal_merge_fresh<B: Buf>(
                tag: u32,
                wire_type: WireType,
                buf: &mut B,
                ctx: DecodeContext,
            ) -> Result<Self, DecodeError> {
                merge_fresh_default(tag, wire_type, buf, ctx)
            }
        }
    };
}

varint_wire!(bool, bool, to_u64(value) u64::from(value));
varint_wire!(i32, int32, to_u64(value) value as i64 as u64);
varint_wire!(i64, int64, to_u64(value) value as u64);
varint_wire!(u32, uint32, to_u64(value) u64::from(value));
varint_wire!(u64, uint64, to_u64(value) value);

impl PhoxalWire for f32 {
    const WIRE_TYPE: WireType = WireType::ThirtyTwoBit;
    const IS_ENUM: bool = false;
    const SKIP_DEFAULT: bool = true;

    fn phoxal_write<B: BufMut>(&self, buf: &mut B) {
        buf.put_u32_le(self.to_bits());
    }

    fn phoxal_value_len(&self) -> usize {
        4
    }

    fn phoxal_merge<B: Buf>(
        &mut self,
        _tag: u32,
        wire_type: WireType,
        buf: &mut B,
        ctx: DecodeContext,
    ) -> Result<(), DecodeError> {
        crate::generated::prost::encoding::float::merge(wire_type, self, buf, ctx)
    }

    fn phoxal_merge_fresh<B: Buf>(
        tag: u32,
        wire_type: WireType,
        buf: &mut B,
        ctx: DecodeContext,
    ) -> Result<Self, DecodeError> {
        merge_fresh_default(tag, wire_type, buf, ctx)
    }
}

impl PhoxalWire for f64 {
    const WIRE_TYPE: WireType = WireType::SixtyFourBit;
    const IS_ENUM: bool = false;
    const SKIP_DEFAULT: bool = true;

    fn phoxal_write<B: BufMut>(&self, buf: &mut B) {
        buf.put_u64_le(self.to_bits());
    }

    fn phoxal_value_len(&self) -> usize {
        8
    }

    fn phoxal_merge<B: Buf>(
        &mut self,
        _tag: u32,
        wire_type: WireType,
        buf: &mut B,
        ctx: DecodeContext,
    ) -> Result<(), DecodeError> {
        crate::generated::prost::encoding::double::merge(wire_type, self, buf, ctx)
    }

    fn phoxal_merge_fresh<B: Buf>(
        tag: u32,
        wire_type: WireType,
        buf: &mut B,
        ctx: DecodeContext,
    ) -> Result<Self, DecodeError> {
        merge_fresh_default(tag, wire_type, buf, ctx)
    }
}

macro_rules! delimited_wire {
    ($ty:ty, $module:ident) => {
        impl PhoxalWire for $ty {
            const WIRE_TYPE: WireType = WireType::LengthDelimited;
            const IS_ENUM: bool = false;
            const SKIP_DEFAULT: bool = true;

            fn phoxal_write<B: BufMut>(&self, buf: &mut B) {
                encode_varint(self.len() as u64, buf);
                buf.put_slice(self.as_ref());
            }

            fn phoxal_value_len(&self) -> usize {
                encoded_len_varint(self.len() as u64) + self.len()
            }

            fn phoxal_merge<B: Buf>(
                &mut self,
                _tag: u32,
                wire_type: WireType,
                buf: &mut B,
                ctx: DecodeContext,
            ) -> Result<(), DecodeError> {
                crate::generated::prost::encoding::$module::merge(wire_type, self, buf, ctx)
            }

            fn phoxal_merge_fresh<B: Buf>(
                tag: u32,
                wire_type: WireType,
                buf: &mut B,
                ctx: DecodeContext,
            ) -> Result<Self, DecodeError> {
                merge_fresh_default(tag, wire_type, buf, ctx)
            }
        }
    };
}

delimited_wire!(String, string);
delimited_wire!(Vec<u8>, bytes);

impl<T: PhoxalWire> PhoxalWire for Option<T> {
    const WIRE_TYPE: WireType = T::WIRE_TYPE;
    const IS_ENUM: bool = T::IS_ENUM;
    const SKIP_DEFAULT: bool = false;

    fn phoxal_write<B: BufMut>(&self, buf: &mut B) {
        if let Some(value) = self {
            value.phoxal_write(buf);
        }
    }

    fn phoxal_value_len(&self) -> usize {
        self.as_ref().map_or(0, |value| value.phoxal_value_len())
    }

    fn phoxal_merge<B: Buf>(
        &mut self,
        tag: u32,
        wire_type: WireType,
        buf: &mut B,
        ctx: DecodeContext,
    ) -> Result<(), DecodeError> {
        match self {
            Some(value) => value.phoxal_merge(tag, wire_type, buf, ctx),
            None => {
                *self = Some(T::phoxal_merge_fresh(tag, wire_type, buf, ctx)?);
                Ok(())
            }
        }
    }

    fn phoxal_merge_fresh<B: Buf>(
        tag: u32,
        wire_type: WireType,
        buf: &mut B,
        ctx: DecodeContext,
    ) -> Result<Self, DecodeError> {
        Ok(Some(T::phoxal_merge_fresh(tag, wire_type, buf, ctx)?))
    }

    /// An absent optional occurrence contributes no key either: the
    /// default keyed encode would leave a dangling key with no value.
    fn phoxal_encode<B: BufMut>(&self, tag: u32, buf: &mut B) {
        if let Some(value) = self {
            value.phoxal_encode(tag, buf);
        }
    }

    fn phoxal_encoded_len(&self, tag: u32) -> usize {
        self.as_ref()
            .map_or(0, |value| value.phoxal_encoded_len(tag))
    }
}

/// Encodes one implicit-presence occurrence: a default-valued scalar,
/// string, bytes, or enumeration field is omitted, matching Prost's
/// derived encoders; explicit-presence and message fields always encode.
pub fn singular_encode<T, B>(value: &T, tag: u32, buf: &mut B)
where
    T: PhoxalWire + Default + PartialEq,
    B: BufMut,
{
    if !(T::SKIP_DEFAULT && *value == T::default()) {
        value.phoxal_encode(tag, buf);
    }
}

/// The encoded length of one implicit-presence occurrence including its
/// key, mirroring [`singular_encode`].
pub fn singular_len<T>(value: &T, tag: u32) -> usize
where
    T: PhoxalWire + Default + PartialEq,
{
    if T::SKIP_DEFAULT && *value == T::default() {
        0
    } else {
        value.phoxal_encoded_len(tag)
    }
}

/// Encodes one occurrence of a repeated field: packable elements use a
/// packed run and length-delimited elements repeat one keyed occurrence
/// per element, matching Prost's derived encoders.
pub fn encode_repeated<T: PhoxalWire, B: BufMut>(values: &[T], tag: u32, buf: &mut B) {
    if values.is_empty() {
        return;
    }
    if T::WIRE_TYPE == WireType::LengthDelimited {
        for value in values {
            value.phoxal_encode(tag, buf);
        }
        return;
    }
    encode_key(tag, WireType::LengthDelimited, buf);
    let len: usize = values.iter().map(PhoxalWire::phoxal_value_len).sum();
    encode_varint(len as u64, buf);
    for value in values {
        value.phoxal_write(buf);
    }
}

/// The encoded length of one repeated field's occurrences.
pub fn repeated_len<T: PhoxalWire>(values: &[T], tag: u32) -> usize {
    if values.is_empty() {
        return 0;
    }
    if T::WIRE_TYPE == WireType::LengthDelimited {
        return values
            .iter()
            .map(|value| value.phoxal_encoded_len(tag))
            .sum();
    }
    let len: usize = values.iter().map(PhoxalWire::phoxal_value_len).sum();
    key_len(tag) + encoded_len_varint(len as u64) + len
}

/// Merges one occurrence of a repeated field.
///
/// A length-delimited occurrence of a varint or fixed element is a packed
/// run; every other occurrence carries one element. Element encoding,
/// decoding, and unknown-value rejection all follow the element's
/// [`PhoxalWire`] implementation. Packed runs decode through Prost's
/// bounded [`merge_loop`](crate::generated::prost::encoding::merge_loop),
/// so an element crossing the run's declared length fails with the same
/// `DelimitedLengthExceeded` error Prost's own derived decoders produce.
pub fn merge_repeated<T: PhoxalWire, B: Buf>(
    values: &mut Vec<T>,
    wire_type: WireType,
    buf: &mut B,
    ctx: DecodeContext,
) -> Result<(), DecodeError> {
    match (wire_type, T::WIRE_TYPE) {
        (WireType::LengthDelimited, WireType::LengthDelimited) => {
            // One length-delimited element: a message, string, or bytes.
            values.push(T::phoxal_merge_fresh(0, wire_type, buf, ctx)?);
            Ok(())
        }
        (WireType::LengthDelimited, element) => {
            // A packed run of varint or fixed elements.
            crate::generated::prost::encoding::merge_loop(values, buf, ctx, |values, buf, ctx| {
                values.push(T::phoxal_merge_fresh(0, element, buf, ctx)?);
                Ok(())
            })
        }
        (wire_type, element) if wire_type == element => {
            values.push(T::phoxal_merge_fresh(0, wire_type, buf, ctx)?);
            Ok(())
        }
        (wire_type, _) => Err(decode_error(format!("unexpected wire type {wire_type:?}"))),
    }
}

/// Prost offers no public `DecodeError` constructor.
#[allow(deprecated, reason = "DecodeError::new is the only constructor")]
fn decode_error(message: impl Into<std::borrow::Cow<'static, str>>) -> DecodeError {
    DecodeError::new(message)
}

/// The decode failure for an enumeration number the definition does not
/// declare: typed enumeration surfaces reject unknown values instead of
/// silently storing them.
pub fn unknown_enum_value_error(value: crate::generated::prost::UnknownEnumValue) -> DecodeError {
    decode_error(format!("unknown enumeration value {value}"))
}

/// The decode failure for a tag no oneof variant declares.
pub fn unknown_oneof_tag(tag: u32) -> DecodeError {
    decode_error(format!("no oneof variant declares tag {tag}"))
}

/// The decode failure for a payload-enum envelope that selected no
/// variant: absence is not a valid value of the public enum.
#[allow(
    dead_code,
    reason = "referenced by macro-generated payload-enum decoding"
)]
pub fn missing_variant_error(type_name: &str) -> DecodeError {
    decode_error(format!("{type_name} selected no variant"))
}
