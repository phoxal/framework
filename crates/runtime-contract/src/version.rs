//! The one compatibility identity that crosses a Phoxal process boundary.
//!
//! Two Phoxal binaries speak the same contracts exactly when they were built
//! from the same framework train, so the framework's SemVer version is that
//! statement in full: one [`FrameworkVersion`] per participant, compared for
//! exact equality. There is no second, per-boundary identity to negotiate, and
//! no way for a bus, launch, or document claim to disagree with the train that
//! produced it.
//!
//! The schema tags on persisted documents (`phoxal/runtime-bundle/v0`,
//! `phoxal/participant-metadata/v0`) are not identities of this kind. They are
//! parse-time format discriminators owned by the document that carries them: a
//! reader refuses a tag it does not implement before it looks at any field.
//!
//! The identity lives on the process-boundary floor rather than in the crate
//! that implements a contract, because the record that declares it
//! ([`crate::metadata::ParticipantMetadata`]) sits below `phoxal-bus`,
//! `phoxal-api`, and `phoxal-manifest` in the graph.

use serde::{Deserialize, Serialize};

/// The framework train one binary was built from, and therefore the whole of
/// what it claims about compatibility.
///
/// Its canonical wire spelling is the SemVer string itself, e.g. `0.56.2`:
/// three decimal segments, no prefix, no padding, no pre-release or build
/// metadata. Comparison is exact equality; the [`CompatibilityLine`] a version
/// belongs to is reported separately so a looser rule can never be applied by
/// accident.
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
    /// This reports the line; it does not decide compatibility. Two binaries
    /// are compatible when their versions are equal.
    #[must_use]
    pub const fn compatibility_line(self) -> CompatibilityLine {
        if self.major == 0 {
            CompatibilityLine::PreV1 { minor: self.minor }
        } else {
            CompatibilityLine::Stable { major: self.major }
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

/// The SemVer line a [`FrameworkVersion`] belongs to.
///
/// Pre-1.0 the breaking axis is the minor; from 1.0 on it is the major. The
/// line is a report about a version, never a comparison rule applied to two of
/// them.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CompatibilityLine {
    /// A `0.x` train, whose line is its minor.
    PreV1 { minor: u16 },
    /// A released train, whose line is its major.
    Stable { major: u16 },
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

/// An exact, open robot API identity carried at the process boundary.
///
/// Its canonical wire spelling is `phoxal/robot-api/v<major>.<minor>`. This
/// type is retired by the API-family migration and is no longer part of any
/// participant's compatibility declaration; it remains only while the
/// supervisor runtime report still names a revision.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RobotApiVersion {
    major: u16,
    minor: u16,
}

impl RobotApiVersion {
    /// Construct one exact robot API version.
    #[must_use]
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    /// The API's major component.
    #[must_use]
    pub const fn major(self) -> u16 {
        self.major
    }

    /// The API's minor component.
    #[must_use]
    pub const fn minor(self) -> u16 {
        self.minor
    }
}

impl std::fmt::Display for RobotApiVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "phoxal/robot-api/v{}.{}", self.major, self.minor)
    }
}

impl Serialize for RobotApiVersion {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for RobotApiVersion {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        const PREFIX: &str = "phoxal/robot-api/v";
        let Some(version) = value.strip_prefix(PREFIX) else {
            return Err(serde::de::Error::custom(format!(
                "invalid robot API '{value}'; expected {PREFIX}<major>.<minor>"
            )));
        };
        let Some((major, minor)) = version.split_once('.') else {
            return Err(serde::de::Error::custom(format!(
                "invalid robot API '{value}'; expected {PREFIX}<major>.<minor>"
            )));
        };
        if major.is_empty()
            || minor.is_empty()
            || minor.contains('.')
            || !major.bytes().all(|byte| byte.is_ascii_digit())
            || !minor.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(serde::de::Error::custom(format!(
                "invalid robot API '{value}'; expected {PREFIX}<major>.<minor>"
            )));
        }
        let major = major.parse().map_err(serde::de::Error::custom)?;
        let minor = minor.parse().map_err(serde::de::Error::custom)?;
        let parsed = Self::new(major, minor);
        if parsed.to_string() != value {
            return Err(serde::de::Error::custom(format!(
                "robot API '{value}' is not canonical; expected '{parsed}'"
            )));
        }
        Ok(parsed)
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

    /// Two versions on one line are still two versions: the framework compares
    /// them for exact equality.
    #[test]
    fn versions_on_the_same_line_are_not_equal() {
        let earlier = FrameworkVersion::new(0, 57, 0);
        let later = FrameworkVersion::new(0, 57, 1);
        assert_eq!(earlier.compatibility_line(), later.compatibility_line());
        assert_ne!(earlier, later);
    }

    #[test]
    fn robot_api_is_open_but_canonical() {
        let known = RobotApiVersion::new(0, 1);
        let future = RobotApiVersion::new(42, 7);
        assert_eq!(known.to_string(), "phoxal/robot-api/v0.1");
        assert_eq!(
            serde_json::from_str::<RobotApiVersion>("\"phoxal/robot-api/v42.7\"").unwrap(),
            future
        );
        assert!(serde_json::from_str::<RobotApiVersion>("\"phoxal/robot-api/v042.7\"").is_err());
        assert!(serde_json::from_str::<RobotApiVersion>("\"phoxal/robot-api/v42\"").is_err());
    }
}
