//! The one compatibility identity that crosses a Phoxal process boundary.
//!
//! Two Phoxal binaries speak the same contracts exactly when they were built
//! from the same [`CompatibilityLine`], so the framework's SemVer version is
//! that statement in full: one [`FrameworkVersion`] per participant, compared
//! with [`FrameworkVersion::is_compatible_with`]. There is no second,
//! per-boundary identity to negotiate, and no way for a bus, launch, or
//! document claim to disagree with the train that produced it.
//!
//! The version a binary records stays exact. It is the provenance a diagnostic
//! names and the value the frozen `supervisor/connect` bootstrap reports; only
//! the comparison is the line. What makes the looser comparison truthful is the
//! compatibility CI: the release gates prove every wire and process surface
//! against the trains already published on the line, and refuse a candidate
//! version too small for what its contracts changed.
//!
//! The schema tags on persisted documents (`phoxal/manifest/v0`,
//! `phoxal/participant-metadata/v0`) are not identities of this kind. They are
//! parse-time format discriminators owned by the document that carries them: a
//! reader refuses a tag it does not implement before it looks at any field.
//!
//! The identity lives on the process-boundary floor rather than beside a
//! contract that uses it: the record that declares it - the participant
//! metadata document, which a host profile reads through
//! `phoxal::participant::metadata` - sits below the bus, the api tree, and the
//! authored-source layer.

use serde::{Deserialize, Serialize};

use crate::__compat::wire::{DescribeWire, WireSchema};

/// The framework train one binary was built from, and therefore the whole of
/// what it claims about compatibility.
///
/// Its canonical wire spelling is the SemVer string itself, e.g. `0.56.2`:
/// three decimal segments, no prefix, no padding, no pre-release or build
/// metadata. Equality is exact, so provenance and diagnostics always name the
/// precise train; compatibility is [`Self::is_compatible_with`], which asks
/// whether two versions share a [`CompatibilityLine`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FrameworkVersion {
    major: u16,
    minor: u16,
    patch: u16,
}

impl FrameworkVersion {
    /// The canonical spelling of the train this binary was built from.
    ///
    /// The crate version is the workspace train version: `[workspace.package]
    /// version` sets it and exact `=` pins keep every internal dependency on
    /// the same train.
    pub const CURRENT_SPELLING: &'static str = env!("CARGO_PKG_VERSION");

    /// The train this binary was built from.
    ///
    /// Parsed from [`Self::CURRENT_SPELLING`] during const evaluation, so a
    /// crate version this type cannot represent fails the build rather than
    /// reaching a process boundary.
    pub const CURRENT: Self = match Self::parse(Self::CURRENT_SPELLING.as_bytes()) {
        Some(version) => version,
        None => panic!("the crate version is not a canonical <major>.<minor>.<patch> version"),
    };

    /// Construct one exact framework version.
    #[must_use]
    pub const fn new(major: u16, minor: u16, patch: u16) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// The version's major component.
    #[must_use]
    pub const fn major(self) -> u16 {
        self.major
    }

    /// The version's minor component.
    #[must_use]
    pub const fn minor(self) -> u16 {
        self.minor
    }

    /// The version's patch component.
    #[must_use]
    pub const fn patch(self) -> u16 {
        self.patch
    }

    /// The SemVer line this version belongs to: pre-1.0 trains break on every
    /// minor, and a released major breaks only on the major.
    ///
    /// The line is what decides compatibility, through
    /// [`Self::is_compatible_with`]. The version stays exact so a record or a
    /// diagnostic can still name the train a binary was built from.
    #[must_use]
    pub const fn compatibility_line(self) -> CompatibilityLine {
        if self.major == 0 {
            CompatibilityLine::PreV1 { minor: self.minor }
        } else {
            CompatibilityLine::Stable { major: self.major }
        }
    }

    /// Whether a peer built from `other` speaks this version's contracts.
    ///
    /// Two trains interoperate exactly when they share a
    /// [`CompatibilityLine`], so this is the comparison every validator makes:
    /// a launch, a bundle admission, and a client attachment all ask this
    /// question and never for equality. The exact version remains available
    /// for provenance and diagnostics.
    ///
    /// The promise is truthful because the compatibility CI enforces it at
    /// release: the gates prove each candidate's wire and process surfaces
    /// against the trains already published on its line, and refuse a version
    /// too small for what its contracts changed. A patch train therefore
    /// cannot carry a surface change that a peer on the same line would fail
    /// to speak.
    #[must_use]
    pub const fn is_compatible_with(self, other: Self) -> bool {
        // `CompatibilityLine` is `Eq`, but a derived `PartialEq` is not a const
        // function, so the two lines are matched here instead.
        match (self.compatibility_line(), other.compatibility_line()) {
            (
                CompatibilityLine::PreV1 { minor },
                CompatibilityLine::PreV1 { minor: other_minor },
            ) => minor == other_minor,
            (
                CompatibilityLine::Stable { major },
                CompatibilityLine::Stable { major: other_major },
            ) => major == other_major,
            _ => false,
        }
    }

    /// Parse the canonical spelling, or `None` when the bytes are anything
    /// else.
    ///
    /// One parser serves const evaluation, [`FromStr`](std::str::FromStr), and
    /// `Deserialize`, so what a peer reads off the wire, what a diagnostic
    /// prints, and what the build accepts cannot drift apart.
    const fn parse(bytes: &[u8]) -> Option<Self> {
        let (major, index) = match Self::parse_segment(bytes, 0) {
            Some(parsed) => parsed,
            None => return None,
        };
        if index >= bytes.len() || bytes[index] != b'.' {
            return None;
        }
        let (minor, index) = match Self::parse_segment(bytes, index + 1) {
            Some(parsed) => parsed,
            None => return None,
        };
        if index >= bytes.len() || bytes[index] != b'.' {
            return None;
        }
        let (patch, index) = match Self::parse_segment(bytes, index + 1) {
            Some(parsed) => parsed,
            None => return None,
        };
        if index != bytes.len() {
            return None;
        }
        Some(Self::new(major, minor, patch))
    }

    /// One decimal segment starting at `start`, with the index just past it.
    ///
    /// A segment is a non-empty run of ASCII digits that fits in `u16` and
    /// carries no leading zero, so `0` parses and `00` or `057` does not.
    const fn parse_segment(bytes: &[u8], start: usize) -> Option<(u16, usize)> {
        let mut index = start;
        let mut value: u16 = 0;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            let digit = (bytes[index] - b'0') as u16;
            value = match value.checked_mul(10) {
                Some(scaled) => scaled,
                None => return None,
            };
            value = match value.checked_add(digit) {
                Some(added) => added,
                None => return None,
            };
            index += 1;
        }
        if index == start {
            return None;
        }
        if bytes[start] == b'0' && index - start > 1 {
            return None;
        }
        Some((value, index))
    }
}

/// The SemVer line a [`FrameworkVersion`] belongs to, and therefore the unit
/// two Phoxal binaries have to agree on.
///
/// Pre-1.0 the breaking axis is the minor; from 1.0 on it is the major. Two
/// versions on one line interoperate, which is what
/// [`FrameworkVersion::is_compatible_with`] asks.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CompatibilityLine {
    /// A `0.x` train, whose line is its minor.
    PreV1 { minor: u16 },
    /// A released train, whose line is its major.
    Stable { major: u16 },
}

impl std::fmt::Display for CompatibilityLine {
    /// The line spelled the way an operator names a compatible release:
    /// `0.58.x` before 1.0, `1.x` after it. One spelling here keeps every
    /// diagnostic that offers a remediation from inventing its own.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PreV1 { minor } => write!(formatter, "0.{minor}.x"),
            Self::Stable { major } => write!(formatter, "{major}.x"),
        }
    }
}

impl std::fmt::Display for FrameworkVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl std::str::FromStr for FrameworkVersion {
    type Err = FrameworkVersionError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value.as_bytes()).ok_or_else(|| FrameworkVersionError {
            value: value.to_owned(),
        })
    }
}

/// A framework version that is not the canonical SemVer spelling of a version
/// this type can represent.
#[derive(Clone, Debug, thiserror::Error)]
#[error("invalid framework version '{value}'; expected <major>.<minor>.<patch>")]
pub struct FrameworkVersionError {
    value: String,
}

impl Serialize for FrameworkVersion {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for FrameworkVersion {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(value.as_bytes())
            .ok_or_else(|| serde::de::Error::custom(FrameworkVersionError { value }))
    }
}

impl DescribeWire for FrameworkVersion {
    // Invariant: this states what the `Serialize` above writes - one string
    // holding the canonical SemVer spelling, never the three-field struct the
    // type is made of.
    fn wire_schema() -> WireSchema {
        WireSchema::opaque("FrameworkVersion", WireSchema::String)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wire_spelling_is_the_semver_string_and_round_trips() {
        let version = FrameworkVersion::new(0, 57, 2);
        assert_eq!(version.to_string(), "0.57.2");
        assert_eq!(
            serde_json::to_string(&version).expect("a framework version serializes"),
            "\"0.57.2\""
        );
        assert_eq!(
            serde_json::from_str::<FrameworkVersion>("\"0.57.2\"").expect("the spelling parses"),
            version
        );
        assert_eq!(
            "0.57.2"
                .parse::<FrameworkVersion>()
                .expect("the spelling parses"),
            version
        );
        assert_eq!(
            (version.major(), version.minor(), version.patch()),
            (0, 57, 2)
        );
    }

    /// The wire accepts one spelling. A prefix, a missing segment, pre-release
    /// metadata, padding, or a structural object are all a different document
    /// than the one this contract defines.
    #[test]
    fn every_non_canonical_spelling_is_rejected() {
        for value in [
            "\"v0.57.2\"",
            "\"0.57\"",
            "\"0.57.2.1\"",
            "\"0.57.2-rc.1\"",
            "\"0.57.2+build.5\"",
            "\"0.057.2\"",
            "\"00.57.2\"",
            "\"0.57.2 \"",
            "\" 0.57.2\"",
            "\"\"",
            r#"{"major":0,"minor":57,"patch":2}"#,
        ] {
            assert!(
                serde_json::from_str::<FrameworkVersion>(value).is_err(),
                "{value} must not parse as a framework version"
            );
        }
        assert!("0.57.2-rc.1".parse::<FrameworkVersion>().is_err());
        assert!("65536.0.0".parse::<FrameworkVersion>().is_err());
    }

    /// The declared wire shape and what the serializer writes are checked
    /// against each other rather than asserted, so the hand-written
    /// declaration cannot drift from the impl beside it.
    #[test]
    fn the_declared_wire_shape_is_the_shape_the_serializer_writes() {
        let value = serde_json::to_value(FrameworkVersion::new(0, 57, 2))
            .expect("a framework version serializes");
        assert_eq!(FrameworkVersion::wire_schema().conforms(&value), Ok(()));
        assert_eq!(
            FrameworkVersion::wire_schema().canonical_json(),
            r#"{"kind":"opaque","name":"FrameworkVersion","wire":{"kind":"string"}}"#
        );
    }

    /// The const-evaluated constant and the crate version are the same fact,
    /// checked here through the runtime parser so a const-eval mistake cannot
    /// hide behind itself.
    #[test]
    fn current_is_the_crate_version() {
        assert_eq!(
            FrameworkVersion::CURRENT.to_string(),
            env!("CARGO_PKG_VERSION")
        );
        assert_eq!(
            env!("CARGO_PKG_VERSION")
                .parse::<FrameworkVersion>()
                .expect("the crate version is a canonical framework version"),
            FrameworkVersion::CURRENT
        );
        assert_eq!(
            FrameworkVersion::CURRENT_SPELLING,
            FrameworkVersion::CURRENT.to_string()
        );
    }

    #[test]
    fn the_compatibility_line_is_the_minor_before_v1_and_the_major_after() {
        assert_eq!(
            FrameworkVersion::new(0, 57, 2).compatibility_line(),
            CompatibilityLine::PreV1 { minor: 57 }
        );
        assert_eq!(
            FrameworkVersion::new(0, 58, 0).compatibility_line(),
            CompatibilityLine::PreV1 { minor: 58 }
        );
        assert_eq!(
            FrameworkVersion::new(1, 4, 9).compatibility_line(),
            CompatibilityLine::Stable { major: 1 }
        );
        assert_eq!(
            FrameworkVersion::new(2, 0, 0).compatibility_line(),
            CompatibilityLine::Stable { major: 2 }
        );
    }

    /// A line is spelled once, as the release an operator would ask for.
    #[test]
    fn a_line_is_spelled_as_the_release_it_names() {
        assert_eq!(
            FrameworkVersion::new(0, 58, 2)
                .compatibility_line()
                .to_string(),
            "0.58.x"
        );
        assert_eq!(
            FrameworkVersion::new(1, 4, 9)
                .compatibility_line()
                .to_string(),
            "1.x"
        );
    }

    /// Two versions on one line are still two versions: equality distinguishes
    /// them so a record and a diagnostic can name the exact train. Only
    /// compatibility treats them as one.
    #[test]
    fn versions_on_the_same_line_are_not_equal_but_are_compatible() {
        let earlier = FrameworkVersion::new(0, 57, 0);
        let later = FrameworkVersion::new(0, 57, 1);
        assert_eq!(earlier.compatibility_line(), later.compatibility_line());
        assert_ne!(earlier, later);
        assert!(earlier.is_compatible_with(later));
        assert!(later.is_compatible_with(earlier));
    }

    /// Compatibility is line equality: pre-1.0 the minor is the break, from
    /// 1.0 on the major is, and the two eras never interoperate.
    #[test]
    fn compatibility_is_the_line_and_the_line_is_the_break() {
        let pre_v1 = FrameworkVersion::new(0, 58, 0);
        assert!(pre_v1.is_compatible_with(FrameworkVersion::new(0, 58, 7)));
        assert!(!pre_v1.is_compatible_with(FrameworkVersion::new(0, 59, 0)));
        assert!(!pre_v1.is_compatible_with(FrameworkVersion::new(0, 57, 9)));

        let stable = FrameworkVersion::new(1, 4, 2);
        assert!(stable.is_compatible_with(FrameworkVersion::new(1, 9, 0)));
        assert!(!stable.is_compatible_with(FrameworkVersion::new(2, 0, 0)));
        assert!(!stable.is_compatible_with(pre_v1));
        assert!(!pre_v1.is_compatible_with(stable));

        assert!(FrameworkVersion::CURRENT.is_compatible_with(FrameworkVersion::CURRENT));
    }
}
