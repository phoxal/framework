//! Every version identity that crosses a Phoxal process boundary.
//!
//! A cross-binary version is a closed set, so it is an enum whose variants are
//! the versions and whose serde renames are the canonical spellings. Two
//! binaries agree by enum equality; a binary from another train fails at
//! deserialize, where serde names the foreign token and the expected set. There
//! is no `&str` constant to compare against and no opaque string newtype to
//! carry an unrecognized value onwards as if it were data.
//!
//! These identities live on the process-boundary floor rather than in the crate
//! that implements each contract, because the record that declares them
//! ([`crate::metadata::ParticipantMetadata`]) has to name all of them at once
//! and this crate is below `phoxal-bus`, `phoxal-api`, and `phoxal-manifest`
//! in the graph. `phoxal-api` pins [`RobotApi`] to the revision its contract tree
//! actually speaks, and `workspace-policy` pins the three document identities
//! to the grammars `phoxal-manifest` accepts.

use serde::{Deserialize, Serialize};

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
    /// The robot API revision a binary's contract tree speaks.
    ///
    /// Namespaced, unlike the bare `v0.1` that appears as a bus key segment:
    /// the key segment is addressing inside an already-Phoxal keyspace, while
    /// this is an identity declared alongside four others.
    RobotApi {
        /// Spelled `V0_1`, not `V01`: the revision has two components and the
        /// separator is part of the identity.
        V0_1 = "phoxal/robot-api/v0.1",
        /// The current control-wire revision. The historic `V0_1` identity
        /// remains available to compatibility adapters and is immutable.
        V0_2 = "phoxal/robot-api/v0.2",
    }
}

version_identity! {
    /// The process launch compatibility identity.
    LaunchAbi {
        V0 = "phoxal/participant-launch/v0",
    }
}

version_identity! {
    /// The authored robot document grammar (`robot.yaml`).
    RobotSchema {
        V0 = "phoxal/robot/v0",
    }
}

version_identity! {
    /// The authored component document grammar (`component.yaml`).
    ComponentSchema {
        V0 = "phoxal/component/v0",
    }
}

version_identity! {
    /// The authored simulation document grammar (`simulation.yaml`).
    SimulationSchema {
        V0 = "phoxal/simulation/v0",
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
        assert_round_trip!(RobotApi::V0_1);
        assert_round_trip!(RobotApi::V0_2);
        assert_round_trip!(LaunchAbi::V0);
        assert_round_trip!(RobotSchema::V0);
        assert_round_trip!(ComponentSchema::V0);
        assert_round_trip!(SimulationSchema::V0);
    }

    #[test]
    fn the_canonical_spellings_are_the_tokens_a_peer_binary_expects() {
        assert_eq!(BusAbi::V0.as_str(), "phoxal/bus-abi/v0");
        assert_eq!(RobotApi::V0_1.as_str(), "phoxal/robot-api/v0.1");
        assert_eq!(RobotApi::V0_2.as_str(), "phoxal/robot-api/v0.2");
        assert_eq!(LaunchAbi::V0.as_str(), "phoxal/participant-launch/v0");
        assert_eq!(RobotSchema::V0.as_str(), "phoxal/robot/v0");
        assert_eq!(ComponentSchema::V0.as_str(), "phoxal/component/v0");
        assert_eq!(SimulationSchema::V0.as_str(), "phoxal/simulation/v0");
    }

    #[test]
    fn an_unknown_version_is_rejected_with_the_expected_set_named() {
        let error = serde_json::from_str::<BusAbi>("\"phoxal/bus-abi/v1\"")
            .expect_err("a version this train does not speak must not parse");
        let message = error.to_string();
        assert!(message.contains("phoxal/bus-abi/v1"), "{message}");
        assert!(message.contains("phoxal/bus-abi/v0"), "{message}");
    }

    /// Each document grammar is its own type, so a record can never compare
    /// one grammar's version against another's.
    #[test]
    fn identities_of_different_kinds_are_different_types() {
        assert_ne!(RobotSchema::V0.as_str(), ComponentSchema::V0.as_str());
    }
}
