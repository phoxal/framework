//! Every version identity that crosses a Phoxal process boundary.
//!
//! Most cross-binary versions are a closed set. Robot API identity is the
//! deliberate exception: a process must be able to report an otherwise valid
//! newer robot API to an external client even when that client does not
//! implement that API's contract tree. It is therefore an open, validated
//! value rather than a train-maintained enum.
//!
//! These identities live on the process-boundary floor rather than in the crate
//! that implements each contract, because the record that declares them
//! ([`crate::metadata::ParticipantMetadata`]) has to name all of them at once
//! and this crate is below `phoxal-bus`, `phoxal-api`, and `phoxal-manifest`
//! in the graph. `phoxal-api` pins its generated `RobotApi` to the revision its contract tree
//! actually speaks, and the runtime bundle pins [`RuntimeSchema`] to the
//! compiled document grammar it persists. Authored source grammars are not
//! participant-binary compatibility claims.

use serde::{Deserialize, Serialize};

/// An exact, open robot API identity carried at the process boundary.
///
/// Its canonical wire spelling is `phoxal/robot-api/v<major>.<minor>`. This
/// type deliberately accepts versions unknown to this framework build; the
/// generated API catalogue decides whether a caller implements a particular
/// revision. Keeping that catalogue above this process-contract floor avoids a
/// dependency from this crate back to `phoxal-api`.
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

/// Declares one cross-binary version identity.
///
/// The canonical spelling is written exactly once per variant and is used for
/// both the serde rename and `as_str`, so the wire token and the diagnostic
/// token cannot drift apart.
macro_rules! version_identity {
    (
        $(#[$enum_meta:meta])*
        $name:ident {
            $(
                $(#[$variant_meta:meta])*
                $variant:ident = $token:literal
            ),+ $(,)?
        }
    ) => {
        $(#[$enum_meta])*
        #[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
        pub enum $name {
            $(
                $(#[$variant_meta])*
                #[serde(rename = $token)]
                $variant,
            )+
        }

        impl $name {
            /// The canonical spelling of this version, for diagnostics. It is
            /// the same literal the serde rename uses.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $token,)+
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

version_identity! {
    /// The bus wire ABI: the version-qualified key grammar, the sample
    /// metadata, and the encoding string together.
    ///
    /// Distinct from the `phoxal/v0` prefix inside a Zenoh encoding string:
    /// that token is per-sample wire overhead and stays short, while this one
    /// is a document identity that has to be unambiguous next to the launch
    /// and manifest identities it sits beside.
    BusAbi {
        V0 = "phoxal/bus-abi/v0",
    }
}

version_identity! {
    /// The process launch compatibility identity.
    LaunchAbi {
        V0 = "phoxal/participant-launch/v0",
    }
}

version_identity! {
    /// The compiled runtime document grammar consumed by participants and
    /// supervisors. This is deliberately distinct from authored robot,
    /// component, and simulation source schemas.
    RuntimeSchema {
        V0 = "phoxal/runtime-bundle/v0",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `as_str` and the serde rename are generated from one literal, so this
    /// asserts the property that literal is meant to have: what a peer reads
    /// off the wire is exactly what a diagnostic prints.
    macro_rules! assert_round_trip {
        ($value:expr) => {{
            let value = $value;
            let json = serde_json::to_string(&value).expect("a unit variant serializes");
            assert_eq!(json, format!("\"{}\"", value.as_str()));
            assert_eq!(
                serde_json::from_str::<_>(&json).ok(),
                Some(value),
                "the canonical spelling must deserialize back to the same variant"
            );
        }};
    }

    #[test]
    fn every_identity_serializes_to_its_canonical_spelling_and_back() {
        assert_round_trip!(BusAbi::V0);
        assert_round_trip!(LaunchAbi::V0);
        assert_round_trip!(RuntimeSchema::V0);
    }

    #[test]
    fn the_canonical_spellings_are_the_tokens_a_peer_binary_expects() {
        assert_eq!(BusAbi::V0.as_str(), "phoxal/bus-abi/v0");
        assert_eq!(LaunchAbi::V0.as_str(), "phoxal/participant-launch/v0");
        assert_eq!(RuntimeSchema::V0.as_str(), "phoxal/runtime-bundle/v0");
    }

    #[test]
    fn an_unknown_version_is_rejected_with_the_expected_set_named() {
        let error = serde_json::from_str::<BusAbi>("\"phoxal/bus-abi/v1\"")
            .expect_err("a version this train does not speak must not parse");
        let message = error.to_string();
        assert!(message.contains("phoxal/bus-abi/v1"), "{message}");
        assert!(message.contains("phoxal/bus-abi/v0"), "{message}");
    }

    /// Each process-boundary grammar is its own type, so a record can never
    /// compare one contract version against another's.
    #[test]
    fn identities_of_different_kinds_are_different_types() {
        assert_ne!(BusAbi::V0.as_str(), RuntimeSchema::V0.as_str());
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
