//! The identity axes that reach the wire.
//!
//! - [`ExecutionId`] names one supervised run. It scopes participants, bus
//!   traffic, and authority, and it is the bus session root, so traffic from a
//!   previous execution cannot physically be observed as current.
//! - [`ProducerId`] names one publishing session. It is minted by `phoxal-bus`
//!   before opening the transport and pinned as the Zenoh session id.
//! - [`TimelineId`] names one world history. A simulation reset or a replay
//!   branch creates a new timeline within the same execution.
//!
//! `ExecutionId` and `ProducerId` are both Zenoh session identities and share
//! one text form: exactly 32 lowercase hexadecimal characters with a non-zero
//! leading nibble. That is the full 16-byte session value, so neither identity
//! can be silently shortened or normalized at a transport boundary. Minting
//! an execution or producer repairs only a zero most-significant nibble, so
//! every non-zero leading digit remains reachable.
//!
//! All three are opaque. They compare only for equality and carry no
//! generation order, no embedded host or path, and no secret.
//!
//! The supervisor-internal `ProcessKey` and project-lock identities are process
//! management, not bus identity; they stay in the supervisor and never reach the
//! wire.

use std::borrow::Borrow;
use std::fmt;
use std::num::NonZeroU64;

use serde::{Deserialize, Deserializer, Serialize};

/// The grammar shared by the topology identities that appear in a persisted
/// runtime document.
///
/// This intentionally lives in the process-contract crate rather than in the
/// source compiler. A participant id is read by a process that may not have
/// any authored sources installed, so validating it cannot require the
/// compiler or its identifier types.
/// Whether a value is one normalized topology token.
///
/// Process and source-model identifiers use this one predicate so their
/// accepted alphabets cannot drift across crate boundaries.
#[must_use]
pub fn is_topology_token(value: &str) -> bool {
    !value.is_empty()
        && value.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '_' | '-')
        })
}

macro_rules! topology_identifier {
    ($(#[$doc:meta])* $name:ident, $error:ident, $kind:literal) => {
        $(#[$doc])*
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);

        impl $name {
            /// Validate and construct the identifier.
            pub fn new(value: impl Into<String>) -> Result<Self, TopologyIdError> {
                let value = value.into();
                if is_topology_token(&value) {
                    Ok(Self(value))
                } else {
                    Err(TopologyIdError::$error(value))
                }
            }

            /// What this identifier names, used in diagnostics.
            pub const KIND: &'static str = $kind;

            /// The canonical wire token.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl Borrow<str> for $name {
            fn borrow(&self) -> &str {
                self.as_str()
            }
        }

        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                self.as_str() == other
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.as_str() == *other
            }
        }

        impl std::str::FromStr for $name {
            type Err = TopologyIdError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = TopologyIdError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
            }
        }
    };
}

topology_identifier!(
    /// The canonical stable identity of one compiled robot.
    RobotId,
    Robot,
    "robot id"
);

topology_identifier!(
    /// The identity of one component instance in the compiled robot.
    ComponentInstanceId,
    ComponentInstance,
    "component instance id"
);

/// A topology identifier that is not one normalized token.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TopologyIdError {
    #[error("robot id must be a non-empty normalized token, got {0:?}")]
    Robot(String),
    #[error("component instance id must be a non-empty normalized token, got {0:?}")]
    ComponentInstance(String),
}

/// The identity of one participant instance in a compiled runtime topology.
///
/// This is deliberately distinct from [`ParticipantArtifactId`]: an instance
/// is the thing the supervisor launches, while an artifact is the reusable
/// compiled role/executable selected by that instance. It is also distinct
/// from [`ProducerId`], which names one transport session incarnation.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ParticipantId(String);

impl ParticipantId {
    /// Validate and construct a participant id.
    pub fn new(value: impl Into<String>) -> Result<Self, ParticipantIdError> {
        let value = value.into();
        if is_topology_token(&value) {
            Ok(Self(value))
        } else {
            Err(ParticipantIdError(value))
        }
    }

    /// The canonical wire token.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ParticipantId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl AsRef<str> for ParticipantId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::str::FromStr for ParticipantId {
    type Err = ParticipantIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for ParticipantId {
    type Error = ParticipantIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ParticipantId> for String {
    fn from(value: ParticipantId) -> Self {
        value.0
    }
}

impl Serialize for ParticipantId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ParticipantId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Why a [`ParticipantId`] is not a valid instance token.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("participant id must be a non-empty lowercase token, got '{0}'")]
pub struct ParticipantIdError(String);

/// The stable identity of a reusable compiled participant artifact.
///
/// The artifact id is the compile-time role identity embedded in the binary.
/// Multiple [`ParticipantId`] instance records may point at the same artifact
/// when one executable is mounted more than once in a runtime topology.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ParticipantArtifactId(String);

impl ParticipantArtifactId {
    /// Validate and construct an artifact id.
    pub fn new(value: impl Into<String>) -> Result<Self, ParticipantArtifactIdError> {
        let value = value.into();
        if is_topology_token(&value) {
            Ok(Self(value))
        } else {
            Err(ParticipantArtifactIdError(value))
        }
    }

    /// The canonical wire token.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ParticipantArtifactId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl AsRef<str> for ParticipantArtifactId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::str::FromStr for ParticipantArtifactId {
    type Err = ParticipantArtifactIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for ParticipantArtifactId {
    type Error = ParticipantArtifactIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ParticipantArtifactId> for String {
    fn from(value: ParticipantArtifactId) -> Self {
        value.0
    }
}

impl Serialize for ParticipantArtifactId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ParticipantArtifactId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Why a [`ParticipantArtifactId`] is empty or contains a non-canonical token.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("participant artifact id must be a non-empty lowercase token, got '{0}'")]
pub struct ParticipantArtifactIdError(String);

/// Bytes in a full-width session identity.
const ZID_BYTES: usize = 16;

/// Rendered width of a full-width session identity.
const ZID_HEX_LEN: usize = ZID_BYTES * 2;

/// The repair applied to a minted identity whose draw came up with a zero most
/// significant nibble, so its rendering never loses a leading nibble and
/// therefore never renders narrower than [`ZID_HEX_LEN`]. A draw that is already
/// nonzero up there is left alone, so every one of the fifteen nonzero leading
/// digits stays reachable.
const CANONICAL_TOP_NIBBLE: u128 = 1 << 124;

/// Mint one canonical full-width session value.
///
/// Both transport identities use the same representation. Keep the random
/// draw and the leading-nibble repair in one place so a future identity cannot
/// accidentally drift to a different canonicalization rule.
fn mint_canonical_value() -> u128 {
    let mut bytes = [0_u8; ZID_BYTES];
    #[expect(
        clippy::expect_used,
        reason = "a session identity is the root of bus provenance; a host without randomness cannot safely start one"
    )]
    getrandom::fill(&mut bytes).expect("the host must provide randomness");
    let mut value = u128::from_be_bytes(bytes);
    if value >> 124 == 0 {
        value |= CANONICAL_TOP_NIBBLE;
    }
    value
}

fn canonical_hex(value: u128) -> String {
    format!("{value:032x}")
}

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
        ExecutionId(mint_canonical_value())
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
        formatter.write_str(&canonical_hex(self.0))
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

/// One bus-session incarnation.
///
/// The unique bus owner mints this identity and pins it into the Zenoh client
/// configuration before opening the session. The id is then read back and
/// compared byte-for-byte; a transport that ignores or rewrites the requested
/// id cannot publish under a mismatched provenance. Reopening therefore always
/// creates a new producer, while every cloneable handle for one owner shares
/// exactly one producer and sequence allocator.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProducerId(u128);

impl ProducerId {
    /// The rendered width of a canonical producer id.
    pub const LEN: usize = ZID_HEX_LEN;

    /// Parse a rendered producer identity.
    ///
    /// Only the canonical full-width lowercase hexadecimal form is accepted,
    /// with a non-zero leading nibble.
    pub fn parse(value: &str) -> Result<Self, IdentityError> {
        if value.len() != ZID_HEX_LEN || !value.bytes().all(is_lowercase_hex) {
            return Err(IdentityError(format!(
                "a producer id is exactly {ZID_HEX_LEN} lowercase hexadecimal characters \
                 and is not zero, got '{value}'"
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
        if value >> 124 == 0 {
            return Err(IdentityError(
                "a producer id must have a non-zero leading nibble".to_string(),
            ));
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
        formatter.write_str(&canonical_hex(self.0))
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
/// required, because the caller that produced it - a process boundary value, a wire
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
    fn topology_ids_share_one_grammar_and_bare_string_wire_form() {
        let robot = RobotId::new("warehouse_rover").expect("canonical robot id");
        let component =
            ComponentInstanceId::new("front-lidar").expect("canonical component instance");
        assert_eq!(RobotId::KIND, "robot id");
        assert_eq!(ComponentInstanceId::KIND, "component instance id");
        assert_eq!(
            serde_json::to_string(&robot).unwrap(),
            "\"warehouse_rover\""
        );
        assert_eq!(
            serde_json::from_str::<ComponentInstanceId>("\"front-lidar\"").unwrap(),
            component
        );

        assert_eq!(
            RobotId::new("Warehouse Rover"),
            Err(TopologyIdError::Robot("Warehouse Rover".to_string()))
        );
        assert_eq!(
            ComponentInstanceId::new("front/lidar"),
            Err(TopologyIdError::ComponentInstance(
                "front/lidar".to_string()
            ))
        );
    }

    #[test]
    fn participant_ids_are_typed_canonical_tokens() {
        let id = ParticipantId::new("front_camera").expect("a canonical participant id");
        assert_eq!(id.as_str(), "front_camera");
        assert_eq!(id.to_string(), "front_camera");
        assert_eq!(
            serde_json::to_string(&id).expect("id serializes"),
            "\"front_camera\""
        );
        assert_eq!(
            serde_json::from_str::<ParticipantId>("\"front_camera\"").expect("id deserializes"),
            id
        );
    }

    #[test]
    fn participant_ids_reject_noncanonical_and_path_tokens() {
        for value in ["", "FrontCamera", "front camera", "../brain", "brain/extra"] {
            assert!(ParticipantId::new(value).is_err(), "{value:?}");
            assert!(
                serde_json::from_str::<ParticipantId>(&format!("\"{value}\"")).is_err(),
                "{value:?}"
            );
        }
    }

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
    fn a_producer_round_trips_in_the_canonical_transport_form() {
        let minted = ProducerId::try_from((1_u128 << 124) | 0x0123_4567_89ab_cdef).unwrap();
        assert_eq!(minted.to_string().len(), ProducerId::LEN);
        assert_eq!(ProducerId::parse(&minted.to_string()), Ok(minted));

        let wide = ProducerId::try_from(u128::MAX).unwrap();
        assert_eq!(wide.to_string(), "f".repeat(ZID_HEX_LEN));
        assert_eq!(ProducerId::parse(&wide.to_string()), Ok(wide));

        assert!(ProducerId::try_from(0).is_err());
        assert!(ProducerId::parse("").is_err());
        assert!(ProducerId::parse("01").is_err());
        assert!(ProducerId::parse("AB").is_err());
        assert!(ProducerId::parse(&"f".repeat(ZID_HEX_LEN + 1)).is_err());
        assert!(ProducerId::parse(&format!("0{}", "f".repeat(ZID_HEX_LEN - 1))).is_err());
    }

    #[test]
    fn producer_ids_round_trip_through_the_wire_encoding() {
        let producer = ProducerId::try_from((1_u128 << 124) | 0x0123_4567_89ab_cdef).unwrap();
        let encoded = rmp_serde::to_vec_named(&producer).unwrap();
        let decoded: ProducerId = rmp_serde::from_slice(&encoded).unwrap();
        assert_eq!(decoded, producer);
        assert_ne!(producer, ProducerId::try_from((1_u128 << 124) | 1).unwrap());
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
