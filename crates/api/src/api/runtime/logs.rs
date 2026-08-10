//! The one live log stream every Phoxal process publishes to.
//!
//! Every participant publishes its diagnostic events on the single
//! `runtime/logs` key. Which participant produced an event is the bus
//! envelope's source attribution, exactly as it is for telemetry: it is neither
//! a key segment nor a payload field, so a record cannot claim an origin its
//! transport disagrees with.

/// Wall-clock instant of one log event. Diagnostic only: log order comes from
/// [`Event::seq`] and the envelope, never from this value.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Timestamp {
    pub unix_seconds: i64,
    pub nanos: u32,
}

/// Severity of one log event.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
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
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum LogValue {
    Bool(bool),
    I64(i64),
    U64(u64),
    F64(#[serde(deserialize_with = "crate::api::runtime::logs::deserialize_finite_f64")] f64),
    String(String),
}

/// One diagnostic event as it goes on the wire.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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

phoxal_macros::phoxal_api_fragment! {
    path runtime / logs;

    topic self: Stream<Event>;
}
