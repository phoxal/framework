//! The two identity axes plus producer identity (#952 section B / G).
//!
//! - [`ExecutionId`] names one supervised run. It scopes participants, bus
//!   traffic, and authority, and it is part of the bus session root, so traffic
//!   from a previous execution cannot physically be observed as current.
//! - [`TimelineId`] names one world history. A simulation reset or a replay
//!   branch creates a new timeline within the same execution.
//! - [`ProducerId`] names one producing process. It replaces the
//!   participant-plus-incarnation pair: a fresh process is a fresh producer
//!   whose sequence starts at zero, so sequence-reset rules collapse into
//!   identity freshness.
//!
//! All three are opaque. They compare only for equality and carry no
//! generation order, no embedded host or path, and no secret.
//!
//! The supervisor-internal `ProcessKey` and project-lock identities are process
//! management, not bus identity; they stay in the supervisor and never reach the
//! wire.

use std::fmt;
use std::num::NonZeroU64;

use serde::{Deserialize, Deserializer, Serialize};

/// Bytes of opaque identity minted per execution and per producer.
const OPAQUE_BYTES: usize = 16;

/// One supervised run.
///
/// The supervisor mints it once per run and every bus participant carries it:
/// services, drivers, simulators, tools, ad hoc publishers, and later the
/// operator. It is part of the bus session root
/// (`<namespace>/robots/<robot-id>/x<execution-id>`), which turns "previous-run
/// traffic is not observed as current" from an operational assumption into a
/// structural property. It is transport scoping and never part of a contract
/// name.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExecutionId([u8; OPAQUE_BYTES]);

impl ExecutionId {
    /// The rendered length of an execution id, in key-safe characters.
    pub const LEN: usize = OPAQUE_BYTES * 2;

    /// Mint a fresh execution identity.
    pub fn mint() -> Self {
        ExecutionId(random_bytes())
    }

    /// Parse a rendered execution identity (as it appears in the launch
    /// contract and the key root).
    pub fn parse(value: &str) -> Result<Self, InvalidIdentity> {
        parse_hex(value).map(ExecutionId)
    }

    /// The identity as it appears in the bus key root and the launch contract.
    pub fn as_key_segment(&self) -> String {
        // A leading letter keeps the segment a legal Zenoh chunk regardless of
        // the random first nibble, and makes an execution-scoped key visually
        // obvious in a trace.
        format!("x{self}")
    }
}

impl fmt::Display for ExecutionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_hex(&self.0, formatter)
    }
}

impl fmt::Debug for ExecutionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ExecutionId({self})")
    }
}

impl Serialize for ExecutionId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ExecutionId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        ExecutionId::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// One producing process.
///
/// Every publisher carries one: a spawned participant receives a
/// supervisor-pre-minted id through the launch contract, and an ad hoc
/// publisher mints its own. Because it is fresh per process, repeated ad hoc
/// invocations never collide under strict per-producer sequence rejection, and
/// a restarted participant is structurally a different producer.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProducerId([u8; OPAQUE_BYTES]);

impl ProducerId {
    /// Mint a fresh producer identity.
    pub fn mint() -> Self {
        ProducerId(random_bytes())
    }

    /// Parse a rendered producer identity.
    pub fn parse(value: &str) -> Result<Self, InvalidIdentity> {
        parse_hex(value).map(ProducerId)
    }
}

impl fmt::Display for ProducerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_hex(&self.0, formatter)
    }
}

impl fmt::Debug for ProducerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ProducerId({self})")
    }
}

impl Serialize for ProducerId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for ProducerId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bytes = serde_bytes::ByteBuf::deserialize(deserializer)?;
        <[u8; OPAQUE_BYTES]>::try_from(bytes.as_ref())
            .map(ProducerId)
            .map_err(|_| {
                serde::de::Error::custom(format!(
                    "producer id must be {OPAQUE_BYTES} bytes, got {}",
                    bytes.len()
                ))
            })
    }
}

/// One world history.
///
/// This is #931's opaque epoch, renamed. Timelines compare only for equality:
/// a replacement timeline is not "newer", it is simply different, and any
/// instant from a different timeline is incomparable. Zero is not a timeline -
/// absence is `Option::None`, never a sentinel value.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TimelineId(NonZeroU64);

impl TimelineId {
    /// Mint a fresh timeline identity.
    pub fn mint() -> Self {
        let mut bytes = [0_u8; 8];
        getrandom::fill(&mut bytes).expect("the host must provide randomness");
        // A zero draw is astronomically unlikely and trivially repaired; the
        // point is that the type has no zero value at all.
        TimelineId(NonZeroU64::new(u64::from_le_bytes(bytes)).unwrap_or(NonZeroU64::MIN))
    }

    /// Rebuild a timeline identity from its wire representation.
    pub const fn from_raw(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(TimelineId(value)),
            None => None,
        }
    }

    /// The wire representation.
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl fmt::Display for TimelineId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "t{:016x}", self.0.get())
    }
}

impl fmt::Debug for TimelineId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "TimelineId({self})")
    }
}

/// A rendered identity that is not a valid one.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InvalidIdentity(String);

fn random_bytes() -> [u8; OPAQUE_BYTES] {
    let mut bytes = [0_u8; OPAQUE_BYTES];
    getrandom::fill(&mut bytes).expect("the host must provide randomness");
    bytes
}

fn write_hex(bytes: &[u8; OPAQUE_BYTES], formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    for byte in bytes {
        write!(formatter, "{byte:02x}")?;
    }
    Ok(())
}

fn parse_hex(value: &str) -> Result<[u8; OPAQUE_BYTES], InvalidIdentity> {
    let value = value.strip_prefix('x').unwrap_or(value);
    if value.len() != OPAQUE_BYTES * 2 {
        return Err(InvalidIdentity(format!(
            "expected {} hex characters, got {}",
            OPAQUE_BYTES * 2,
            value.len()
        )));
    }
    let mut bytes = [0_u8; OPAQUE_BYTES];
    for (index, byte) in bytes.iter_mut().enumerate() {
        let pair = &value[index * 2..index * 2 + 2];
        *byte = u8::from_str_radix(pair, 16)
            .map_err(|_| InvalidIdentity(format!("'{pair}' is not a hex byte")))?;
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_ids_are_opaque_unique_and_key_safe() {
        let first = ExecutionId::mint();
        let second = ExecutionId::mint();
        assert_ne!(first, second);

        let segment = first.as_key_segment();
        assert_eq!(segment.len(), ExecutionId::LEN + 1);
        assert!(segment.starts_with('x'));
        assert!(!segment.contains('/') && !segment.contains('*'));
        assert_eq!(ExecutionId::parse(&segment), Ok(first));
        assert_eq!(ExecutionId::parse(&first.to_string()), Ok(first));
    }

    #[test]
    fn a_malformed_execution_id_is_rejected_rather_than_truncated() {
        assert!(ExecutionId::parse("").is_err());
        assert!(ExecutionId::parse("xdeadbeef").is_err());
        assert!(ExecutionId::parse(&"z".repeat(ExecutionId::LEN)).is_err());
    }

    #[test]
    fn producer_ids_round_trip_through_the_wire_encoding() {
        let producer = ProducerId::mint();
        let encoded = rmp_serde::to_vec_named(&producer).unwrap();
        let decoded: ProducerId = rmp_serde::from_slice(&encoded).unwrap();
        assert_eq!(decoded, producer);
        assert_ne!(producer, ProducerId::mint());
    }

    #[test]
    fn timelines_have_no_zero_value_and_no_generation_order() {
        assert_eq!(TimelineId::from_raw(0), None);
        let timeline = TimelineId::mint();
        assert_eq!(TimelineId::from_raw(timeline.get()), Some(timeline));
        // Equality is the only meaning: a replacement timeline is different,
        // not newer. None of the three identities implements ordering, so no
        // caller can read one as a generation counter.
        assert_ne!(timeline, TimelineId::mint());
    }
}
