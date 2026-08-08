//! The identity axes that reach the wire.
//!
//! - [`ExecutionId`] names one supervised run. It scopes participants, bus
//!   traffic, and authority, and it is the bus session root, so traffic from a
//!   previous execution cannot physically be observed as current.
//! - [`ProducerId`] names one publishing session. It is not minted: a session
//!   *is* its producer, so the identity is read back from the id the transport
//!   assigned the session.
//! - [`TimelineId`] names one world history. A simulation reset or a replay
//!   branch creates a new timeline within the same execution.
//!
//! `ExecutionId` and `ProducerId` are both Zenoh session identities and share
//! one text form: lowercase hexadecimal with no leading zeros, at most 16
//! bytes wide, which is exactly how Zenoh renders a session id. Minting an
//! execution forces the most significant nibble nonzero, so an execution
//! always renders at the full [`ExecutionId::LEN`] characters and the router
//! session it pins renders character for character the same.
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

/// Bytes in a full-width session identity.
const ZID_BYTES: usize = 16;

/// Rendered width of a full-width session identity.
const ZID_HEX_LEN: usize = ZID_BYTES * 2;

/// The repair applied to a minted execution whose draw came up with a zero most
/// significant nibble, so its rendering never loses a leading nibble and
/// therefore never renders narrower than [`ZID_HEX_LEN`]. A draw that is already
/// nonzero up there is left alone, so every one of the fifteen nonzero leading
/// digits stays reachable.
const CANONICAL_TOP_NIBBLE: u128 = 1 << 124;

/// One supervised run.
///
/// The supervisor mints it once per run and every bus participant carries it:
/// services, drivers, simulators, ad hoc publishers, and later the operator. It
/// is the bus session root (`phoxal/<execution-id>`), which turns "previous-run
/// traffic is not observed as current" from an operational assumption into a
/// structural property. It is transport scoping and never part of a contract
/// name.
///
/// It is also the identity the run's router session opens with, so the router
/// a trace names and the key root that trace carries are the same string.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExecutionId(u128);

impl ExecutionId {
    /// The rendered length of an execution id, in key-safe characters.
    pub const LEN: usize = ZID_HEX_LEN;

    /// Mint a fresh execution identity.
    ///
    /// The draw is repaired only when it would render narrower than
    /// [`ExecutionId::LEN`], which is to say only when its most significant
    /// nibble came up zero. Forcing the nibble unconditionally would pin the
    /// leading digit to the odd half of the alphabet; leaving a nonzero draw
    /// alone keeps the full nonzero leading-digit range that the transport's
    /// own session ids cover.
    pub fn mint() -> Self {
        let mut bytes = [0_u8; ZID_BYTES];
        #[expect(
            clippy::expect_used,
            reason = "an execution identity is the root every bus key and every authority \
                      decision is scoped by, so a host whose randomness source is unavailable \
                      cannot start an execution at all; there is no weaker identity to fall \
                      back to and no caller that could carry on without one"
        )]
        getrandom::fill(&mut bytes).expect("the host must provide randomness");
        let mut value = u128::from_be_bytes(bytes);
        if value >> 124 == 0 {
            value |= CANONICAL_TOP_NIBBLE;
        }
        ExecutionId(value)
    }

    /// Parse a rendered execution identity (as it appears in the launch
    /// contract, the key root, and the router session id).
    ///
    /// Only the canonical form is accepted: exactly [`ExecutionId::LEN`]
    /// lowercase hexadecimal characters, the first of which is not `0`.
    /// Anything else - uppercase, a leading zero, a shorter or longer run of
    /// digits - would render back differently than it was written, so it is
    /// rejected rather than normalized.
    pub fn parse(value: &str) -> Result<Self, IdentityError> {
        if value.len() != ZID_HEX_LEN || !value.bytes().all(is_lowercase_hex) {
            return Err(IdentityError(format!(
                "an execution id is exactly {ZID_HEX_LEN} lowercase hexadecimal \
                 characters, got '{value}'"
            )));
        }
        let value = u128::from_str_radix(value, 16)
            .map_err(|error| IdentityError(format!("'{value}' is not hexadecimal: {error}")))?;
        ExecutionId::try_from(value)
    }
}

impl TryFrom<u128> for ExecutionId {
    type Error = IdentityError;

    fn try_from(value: u128) -> Result<Self, IdentityError> {
        if value >> 124 == 0 {
            return Err(IdentityError(format!(
                "an execution id renders as {ZID_HEX_LEN} characters, so its most \
                 significant nibble is never zero"
            )));
        }
        Ok(ExecutionId(value))
    }
}

impl From<ExecutionId> for u128 {
    fn from(execution: ExecutionId) -> Self {
        execution.0
    }
}

impl fmt::Display for ExecutionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:x}", self.0)
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

/// One publishing session.
///
/// Nothing mints one. A session's producer identity *is* the identity the
/// transport gave that session, read back after the session opens, so a
/// reopened session is a different producer by construction and a process that
/// never opened a session has no producer to speak of.
///
/// Because it is fresh per session incarnation, repeated ad hoc invocations
/// never collide under strict per-producer sequence rejection, and a restarted
/// participant is structurally a different producer.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProducerId(u128);

impl ProducerId {
    /// Parse a rendered producer identity.
    ///
    /// This is the transport's own text form, so it accepts what the transport
    /// emits: 1 to [`ExecutionId::LEN`] lowercase hexadecimal characters with
    /// no leading zero. Unlike an execution, a producer is not minted here and
    /// is not pinned to the full width.
    pub fn parse(value: &str) -> Result<Self, IdentityError> {
        if value.is_empty()
            || value.len() > ZID_HEX_LEN
            || value.starts_with('0')
            || !value.bytes().all(is_lowercase_hex)
        {
            return Err(IdentityError(format!(
                "a producer id is 1 to {ZID_HEX_LEN} lowercase hexadecimal characters \
                 with no leading zero, got '{value}'"
            )));
        }
        let value = u128::from_str_radix(value, 16)
            .map_err(|error| IdentityError(format!("'{value}' is not hexadecimal: {error}")))?;
        ProducerId::try_from(value)
    }
}

impl TryFrom<u128> for ProducerId {
    type Error = IdentityError;

    fn try_from(value: u128) -> Result<Self, IdentityError> {
        if value == 0 {
            return Err(IdentityError("a producer id is never zero".to_string()));
        }
        Ok(ProducerId(value))
    }
}

impl From<ProducerId> for u128 {
    fn from(producer: ProducerId) -> Self {
        producer.0
    }
}

impl fmt::Display for ProducerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:x}", self.0)
    }
}

impl fmt::Debug for ProducerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ProducerId({self})")
    }
}

impl Serialize for ProducerId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Little-endian to match the transport's own byte order for the same
        // value, so a reader comparing raw bytes against a session id sees the
        // same ordering it would from the transport.
        serializer.serialize_bytes(&self.0.to_le_bytes())
    }
}

impl<'de> Deserialize<'de> for ProducerId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bytes = serde_bytes::ByteBuf::deserialize(deserializer)?;
        let bytes = <[u8; ZID_BYTES]>::try_from(bytes.as_ref()).map_err(|_| {
            serde::de::Error::custom(format!(
                "producer id must be {ZID_BYTES} bytes, got {}",
                bytes.len()
            ))
        })?;
        ProducerId::try_from(u128::from_le_bytes(bytes)).map_err(serde::de::Error::custom)
    }
}

/// One world history.
///
/// An opaque epoch. Timelines compare only for equality: a replacement
/// timeline is not "newer", it is simply different, and any instant from a
/// different timeline is incomparable. Zero is not a timeline - absence is
/// `Option::None`, never a sentinel value.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TimelineId(NonZeroU64);

impl TimelineId {
    /// Mint a fresh timeline identity.
    pub fn mint() -> Self {
        let mut bytes = [0_u8; 8];
        #[expect(
            clippy::expect_used,
            reason = "a timeline names one world history, so two histories separated by a \
                      predictable identity would be indistinguishable to every reader; a host \
                      whose randomness source is unavailable has no correct value to return"
        )]
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

/// A value this module refused to accept as one of its identities.
///
/// The message already names the rejected value and the shape that was
/// required, because the caller that produced it - a launch record, a wire
/// field, a transport session id - is never in a position to explain the
/// identity grammar itself.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct IdentityError(String);

const fn is_lowercase_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || byte.is_ascii_lowercase() && byte <= b'f'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minted_execution_always_renders_at_the_canonical_width() {
        let first = ExecutionId::mint();
        let second = ExecutionId::mint();
        assert_ne!(first, second);

        let rendered = first.to_string();
        assert_eq!(rendered.len(), ExecutionId::LEN);
        assert!(!rendered.starts_with('0'));
        assert!(rendered.bytes().all(is_lowercase_hex));
        assert!(!rendered.contains('/') && !rendered.contains('*'));
        assert_eq!(ExecutionId::parse(&rendered), Ok(first));
    }

    #[test]
    fn minting_does_not_pin_the_leading_digit_to_half_the_alphabet() {
        // Forcing the top nibble unconditionally would leave only the odd
        // leading digits reachable. Over this many draws, seeing no even one is
        // astronomically less likely than any real flake.
        let saw_even_leading_digit = (0..64).any(|_| {
            let leading = ExecutionId::mint().to_string().as_bytes()[0];
            let digit = if leading.is_ascii_digit() {
                leading - b'0'
            } else {
                leading - b'a' + 10
            };
            digit % 2 == 0
        });
        assert!(
            saw_even_leading_digit,
            "a minted execution covers the whole nonzero leading-digit range"
        );
    }

    #[test]
    fn only_the_canonical_execution_form_parses() {
        let canonical = ExecutionId::mint().to_string();

        assert!(ExecutionId::parse("").is_err());
        assert!(ExecutionId::parse("deadbeef").is_err());
        assert!(
            ExecutionId::parse(&canonical.to_uppercase()).is_err(),
            "uppercase renders back differently, so it is not the same identity"
        );
        assert!(
            ExecutionId::parse(&format!("0{}", &canonical[1..])).is_err(),
            "a leading zero would render back one character shorter"
        );
        assert!(
            ExecutionId::parse(&format!("{canonical}0")).is_err(),
            "an over-long run of digits is not a session identity"
        );
        assert!(ExecutionId::parse(&"z".repeat(ExecutionId::LEN)).is_err());
        assert!(
            ExecutionId::parse(&format!("x{canonical}")).is_err(),
            "the key root is bare, so there is no prefix to strip"
        );
    }

    #[test]
    fn an_execution_round_trips_through_its_session_identity_value() {
        let execution = ExecutionId::mint();
        let value = u128::from(execution);
        assert_eq!(ExecutionId::try_from(value), Ok(execution));
        assert_eq!(format!("{value:x}"), execution.to_string());
        assert!(
            ExecutionId::try_from(u128::from(execution) >> 4).is_err(),
            "a value that renders narrower than the canonical width is not an execution"
        );
        assert!(ExecutionId::try_from(0).is_err());
    }

    #[test]
    fn a_producer_carries_the_transport_text_form_and_is_never_minted() {
        // A session identity may be narrower than the canonical execution
        // width, so a producer accepts what the transport actually renders.
        let narrow = ProducerId::try_from(1).unwrap();
        assert_eq!(narrow.to_string(), "1");
        assert_eq!(ProducerId::parse("1"), Ok(narrow));

        let wide = ProducerId::try_from(u128::MAX).unwrap();
        assert_eq!(wide.to_string(), "f".repeat(ZID_HEX_LEN));
        assert_eq!(ProducerId::parse(&wide.to_string()), Ok(wide));

        assert!(ProducerId::try_from(0).is_err());
        assert!(ProducerId::parse("").is_err());
        assert!(ProducerId::parse("01").is_err());
        assert!(ProducerId::parse("AB").is_err());
        assert!(ProducerId::parse(&"f".repeat(ZID_HEX_LEN + 1)).is_err());
    }

    #[test]
    fn producer_ids_round_trip_through_the_wire_encoding() {
        let producer = ProducerId::try_from(0x0123_4567_89ab_cdef).unwrap();
        let encoded = rmp_serde::to_vec_named(&producer).unwrap();
        let decoded: ProducerId = rmp_serde::from_slice(&encoded).unwrap();
        assert_eq!(decoded, producer);
        assert_ne!(producer, ProducerId::try_from(1).unwrap());
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
