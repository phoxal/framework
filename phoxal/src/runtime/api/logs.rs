//! The one live log stream every Phoxal process publishes to.
//!
//! Every participant publishes its diagnostic events on the single
//! `runtime/logs` key. Which participant produced an event is the bus
//! envelope's source attribution, exactly as it is for telemetry: it is neither
//! a key segment nor a payload field, so a record cannot claim an origin its
//! transport disagrees with.

crate::endpoints! {
    self: Stream<Event, Out>;
}

/// Wall-clock instant of one log event. Diagnostic only: log order comes from
/// [`Event::seq`] and the envelope, never from this value.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub struct Timestamp {
    pub unix_seconds: i64,
    pub nanos: u32,
}

/// Severity of one log event.
#[derive(
    phoxal_macros::DescribeWire,
    Clone,
    Copy,
    Debug,
    Eq,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

/// One structured field value. Floating-point fields are finite on decode, so a
/// hostile or broken producer cannot inject a value no consumer can render.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(untagged)]
pub enum LogValue {
    Bool(bool),
    I64(i64),
    U64(u64),
    F64(#[serde(deserialize_with = "crate::runtime::api::logs::deserialize_finite_f64")] f64),
    String(String),
}

/// One diagnostic event as it goes on the wire.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub struct Event {
    /// The producer's own monotonic counter, so a consumer sees loss.
    pub seq: u64,
    pub time: Timestamp,
    pub level: Level,
    pub target: String,
    pub message: String,
    pub fields: ::std::collections::BTreeMap<String, LogValue>,
    /// Events the producer dropped before this one under back pressure.
    pub dropped: u32,
    /// Source bytes removed while bounding this event's text.
    #[serde(default)]
    pub truncated: u32,
}

fn deserialize_finite_f64<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <f64 as serde::Deserialize>::deserialize(deserializer)?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(serde::de::Error::custom(
            "expected a finite floating-point value",
        ))
    }
}


#[cfg(test)]
mod tests {
    use crate::__compat::wire::{DescribeWire, EnumRepresentation, WireSchema};

    use super::{Event, Level, LogValue, Timestamp};

    /// An untagged field value is decoded by keeping the first variant that
    /// accepts it, so the declared shape carries the variants in declaration
    /// order rather than normalizing it away.
    #[test]
    fn the_field_value_is_declared_as_an_untagged_sum_in_declaration_order() {
        let schema = LogValue::wire_schema();
        let WireSchema::Enum {
            representation,
            variants,
        } = &schema
        else {
            panic!("a log value is a sum type: {schema:?}");
        };
        assert_eq!(*representation, EnumRepresentation::Untagged);
        assert_eq!(
            variants
                .iter()
                .map(|variant| variant.name.as_str())
                .collect::<Vec<_>>(),
            ["Bool", "I64", "U64", "F64", "String"]
        );
        for value in [
            LogValue::Bool(true),
            LogValue::I64(-4),
            LogValue::U64(4),
            LogValue::F64(1.5),
            LogValue::String(String::from("text")),
        ] {
            let json = serde_json::to_value(&value).expect("a field value serializes");
            assert_eq!(schema.conforms(&json), Ok(()), "{json}");
        }
    }

    /// The event carries a map of untagged values and one defaulted counter,
    /// which is the shape a consumer from another build has to keep accepting.
    #[test]
    fn the_declared_event_shape_is_the_shape_serde_writes() {
        let event = Event {
            seq: 7,
            time: Timestamp {
                unix_seconds: 17,
                nanos: 250,
            },
            level: Level::Warn,
            target: String::from("phoxal::drive"),
            message: String::from("target stale"),
            fields: [(String::from("age_ms"), LogValue::U64(120))]
                .into_iter()
                .collect(),
            dropped: 0,
            truncated: 0,
        };
        let json = serde_json::to_value(&event).expect("an event serializes");
        assert_eq!(Event::wire_schema().conforms(&json), Ok(()));
        assert_eq!(json["level"], "warn");

        // `#[serde(default)]` is a wire fact: an older producer that never
        // wrote the counter still decodes.
        let mut without_counter = json.clone();
        let object = without_counter.as_object_mut().expect("an event is a map");
        object.remove("truncated");
        assert_eq!(Event::wire_schema().conforms(&without_counter), Ok(()));
        assert!(serde_json::from_value::<Event>(without_counter).is_ok());
    }
}
