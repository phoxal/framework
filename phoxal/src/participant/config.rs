//! Participant configuration schemas.
//!
//! `ParticipantConfig` is the compile-time schema contract emitted by
//! `#[derive(phoxal::Config)]`.  It lives separately from the lifecycle traits
//! because configuration composition and participant execution have different
//! owners.

/// Const-eval plumbing the participant attribute macros
/// (`crates/macros/src/authoring.rs`'s `expand_participant`) use to build the
/// binary's embedded linker-section metadata static:
/// `{"schema", "api", "schemas", "id", "kind", "config_schema"}`.
///
/// The config schema is composed recursively from nested `ParticipantConfig`
/// implementations (`Self::SCHEMA_JSON`, itself built the same way), so the
/// final JSON string is only known after `rustc` const-evaluates the whole tree
/// in the downstream participant crate. The participant attribute therefore
/// emits tokens, not a string: a call into [`meta::concatcp`] splices the
/// participant id and `<Config as ParticipantConfig>::SCHEMA_JSON` between
/// macro-time-known JSON literal fragments. [`meta::bytes_of`] then copies the
/// final string into the fixed byte array placed in the linker section.
#[doc(hidden)]
pub mod meta {
    /// `const_format::concatcp!`, made reachable as
    /// `$crate::__private::meta::concatcp!`.
    ///
    /// Role attributes expand inside a participant's own crate, which does not
    /// depend on `const_format`, so the expansion cannot name that crate
    /// directly. Routing the call through here makes it hygienic.
    #[doc(hidden)]
    pub use const_format::concatcp;

    /// Fixed-capacity const-eval string builder used for recursively composed
    /// config schemas. A fixed backing array is necessary because stable Rust
    /// cannot express an array length computed from a generic associated
    /// constant; only the used prefix is exposed by [`ConstSchema::as_str`].
    #[derive(Clone, Copy)]
    pub struct ConstSchema {
        bytes: [u8; 65_536],
        len: usize,
    }

    impl Default for ConstSchema {
        fn default() -> Self {
            Self::new()
        }
    }

    impl ConstSchema {
        pub const fn new() -> Self {
            Self {
                bytes: [0; 65_536],
                len: 0,
            }
        }

        pub const fn from_str(value: &str) -> Self {
            Self::new().push_str(value)
        }

        #[must_use]
        pub const fn push_str(mut self, value: &str) -> Self {
            let value = value.as_bytes();
            assert!(
                self.len + value.len() <= self.bytes.len(),
                "phoxal: const config schema exceeds 64 KiB"
            );
            let mut index = 0;
            while index < value.len() {
                self.bytes[self.len + index] = value[index];
                index += 1;
            }
            self.len += value.len();
            self
        }

        pub const fn as_str(&self) -> &str {
            let (used, _) = self.bytes.split_at(self.len);
            // Every byte originates in a Rust `&str`, so concatenation
            // preserves UTF-8 validity.
            unsafe { core::str::from_utf8_unchecked(used) }
        }
    }

    /// Copies a `rustc`-const-evaluated `&str` into a fixed `[u8; N]` array so
    /// it can be assigned to a `#[link_section]` static.
    pub const fn bytes_of<const N: usize>(s: &str) -> [u8; N] {
        let bytes = s.as_bytes();
        assert!(
            bytes.len() == N,
            "phoxal: metadata manifest length mismatch"
        );
        let mut out = [0u8; N];
        let mut i = 0;
        while i < N {
            out[i] = bytes[i];
            i += 1;
        }
        out
    }
}

/// Emitted by `#[derive(phoxal::Config)]`: the participant config's compile-time
/// JSON Schema (Draft 2020-12).
pub trait ParticipantConfig: serde::de::DeserializeOwned + Send + 'static {
    #[doc(hidden)]
    const __SCHEMA: meta::ConstSchema;
    /// A complete schema or subschema, const-composable by another derived
    /// config without runtime allocation.
    const SCHEMA_JSON: &'static str = Self::__SCHEMA.as_str();
}

impl ParticipantConfig for () {
    const __SCHEMA: meta::ConstSchema = meta::ConstSchema::from_str(r#"{"type":"null"}"#);
}

/// An optional config is a config: `config = Option<T>` works whenever
/// `T: ParticipantConfig`.
impl<T: ParticipantConfig> ParticipantConfig for Option<T> {
    const __SCHEMA: meta::ConstSchema = meta::ConstSchema::new()
        .push_str(r#"{"anyOf":["#)
        .push_str(T::SCHEMA_JSON)
        .push_str(r#",{"type":"null"}]}"#);
}

impl<T: ParticipantConfig> ParticipantConfig for Vec<T> {
    const __SCHEMA: meta::ConstSchema = meta::ConstSchema::new()
        .push_str(r#"{"type":"array","items":"#)
        .push_str(T::SCHEMA_JSON)
        .push_str("}");
}

impl<T: ParticipantConfig> ParticipantConfig for std::collections::BTreeMap<String, T> {
    const __SCHEMA: meta::ConstSchema = meta::ConstSchema::new()
        .push_str(r#"{"type":"object","additionalProperties":"#)
        .push_str(T::SCHEMA_JSON)
        .push_str("}");
}

impl<T: ParticipantConfig> ParticipantConfig for std::collections::HashMap<String, T> {
    const __SCHEMA: meta::ConstSchema = meta::ConstSchema::new()
        .push_str(r#"{"type":"object","additionalProperties":"#)
        .push_str(T::SCHEMA_JSON)
        .push_str("}");
}

macro_rules! primitive_config_schema {
    ($ty:ty => $schema:literal) => {
        impl ParticipantConfig for $ty {
            const __SCHEMA: meta::ConstSchema = meta::ConstSchema::from_str($schema);
        }
    };
}

primitive_config_schema!(bool => r#"{"type":"boolean"}"#);
primitive_config_schema!(String => r#"{"type":"string"}"#);
primitive_config_schema!(char => r#"{"type":"string","minLength":1,"maxLength":1}"#);
primitive_config_schema!(i8 => r#"{"type":"integer","format":"int8"}"#);
primitive_config_schema!(i16 => r#"{"type":"integer","format":"int16"}"#);
primitive_config_schema!(i32 => r#"{"type":"integer","format":"int32"}"#);
primitive_config_schema!(i64 => r#"{"type":"integer","format":"int64"}"#);
primitive_config_schema!(i128 => r#"{"type":"integer"}"#);
primitive_config_schema!(isize => r#"{"type":"integer"}"#);
primitive_config_schema!(u8 => r#"{"type":"integer","format":"uint8","minimum":0,"maximum":255}"#);
primitive_config_schema!(u16 => r#"{"type":"integer","format":"uint16","minimum":0,"maximum":65535}"#);
primitive_config_schema!(u32 => r#"{"type":"integer","format":"uint32","minimum":0}"#);
primitive_config_schema!(u64 => r#"{"type":"integer","format":"uint64","minimum":0}"#);
primitive_config_schema!(u128 => r#"{"type":"integer","minimum":0}"#);
primitive_config_schema!(usize => r#"{"type":"integer","minimum":0}"#);
primitive_config_schema!(f32 => r#"{"type":"number","format":"float"}"#);
primitive_config_schema!(f64 => r#"{"type":"number","format":"double"}"#);

#[cfg(test)]
mod tests {
    use super::{ParticipantConfig, meta::ConstSchema};

    #[test]
    fn const_schema_exposes_only_the_pushed_prefix() {
        let schema = ConstSchema::from_str(r#"{"type":"#)
            .push_str(r#""string""#)
            .push_str("}");
        assert_eq!(schema.as_str(), r#"{"type":"string"}"#);
        assert_eq!(ConstSchema::new().as_str(), "");
    }

    /// The blanket impls this module owns compose a nested schema without
    /// allocating, so a config type never has to spell its own container
    /// schemas.
    #[test]
    fn blanket_config_schemas_compose_from_the_inner_schema() {
        assert_eq!(<() as ParticipantConfig>::SCHEMA_JSON, r#"{"type":"null"}"#);
        assert_eq!(
            <Option<bool> as ParticipantConfig>::SCHEMA_JSON,
            r#"{"anyOf":[{"type":"boolean"},{"type":"null"}]}"#
        );
        assert_eq!(
            <Vec<String> as ParticipantConfig>::SCHEMA_JSON,
            r#"{"type":"array","items":{"type":"string"}}"#
        );
        assert_eq!(
            <std::collections::BTreeMap<String, u32> as ParticipantConfig>::SCHEMA_JSON,
            r#"{"type":"object","additionalProperties":{"type":"integer","format":"uint32","minimum":0}}"#
        );
    }
}
