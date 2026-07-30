//! Canonical immutable runtime robot model and strict `robot.json` encoding.

pub mod component;
pub mod robot;
pub mod simulation;
pub mod structure;

pub use robot::{ROBOT_SCHEMA, Robot, RobotIdentity};

/// Compiler linkage that is intentionally absent from the runtime model API.
#[doc(hidden)]
pub mod __private {
    /// Construct a validated structure from the manifest compiler's normalized
    /// JSON value. Runtime consumers decode a complete [`crate::Robot`] instead.
    pub fn structure_from_compiler_value(
        document: serde_json::Value,
    ) -> Result<crate::structure::Structure, crate::ModelError> {
        crate::structure::Structure::from_compiler_value(document)
    }
}

/// Invalid canonical robot semantics.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("{0}")]
    Invalid(String),
}

/// Canonical encoding failure.
#[derive(Debug, thiserror::Error)]
#[error("failed to encode canonical robot: {0}")]
pub struct EncodeError(#[from] serde_json::Error);

/// Canonical decoding failure.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("invalid canonical robot JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported robot schema '{0}'")]
    UnsupportedSchema(String),
    #[error("invalid canonical robot: {0}")]
    Model(#[from] ModelError),
}

mod strict_json {
    use std::collections::BTreeMap;

    use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
    use serde_json::{Number, Value};

    use crate::DecodeError;

    pub(crate) fn parse(bytes: &[u8]) -> Result<Value, DecodeError> {
        let mut deserializer = serde_json::Deserializer::from_slice(bytes);
        let value = StrictValue::deserialize(&mut deserializer)?.0;
        deserializer.end()?;
        Ok(value)
    }

    struct StrictValue(Value);

    impl<'de> Deserialize<'de> for StrictValue {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            deserializer.deserialize_any(StrictVisitor)
        }
    }

    struct StrictVisitor;

    impl<'de> Visitor<'de> for StrictVisitor {
        type Value = StrictValue;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a JSON value without duplicate object fields")
        }

        fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
            Ok(StrictValue(Value::Bool(value)))
        }

        fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
            Ok(StrictValue(Value::Number(Number::from(value))))
        }

        fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
            Ok(StrictValue(Value::Number(Number::from(value))))
        }

        fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Self::Value, E> {
            Number::from_f64(value)
                .map(Value::Number)
                .map(StrictValue)
                .ok_or_else(|| E::custom("non-finite JSON number"))
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
            Ok(StrictValue(Value::String(value.to_string())))
        }

        fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
            Ok(StrictValue(Value::String(value)))
        }

        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(StrictValue(Value::Null))
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(StrictValue(Value::Null))
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
            let mut values = Vec::new();
            while let Some(value) = sequence.next_element::<StrictValue>()? {
                values.push(value.0);
            }
            Ok(StrictValue(Value::Array(values)))
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut values = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, StrictValue>()? {
                if values.insert(key.clone(), value.0).is_some() {
                    return Err(serde::de::Error::custom(format!(
                        "duplicate JSON object field '{key}'"
                    )));
                }
            }
            Ok(StrictValue(Value::Object(values.into_iter().collect())))
        }
    }
}
