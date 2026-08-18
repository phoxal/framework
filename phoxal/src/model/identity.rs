//! Validated canonical identifiers and the token grammar they share.
//!
//! Every domain identifier in the canonical model is a newtype rather than a
//! bare `String`, so a component type can never be passed where a capability id
//! is expected. Each newtype is transparent on the wire: it serializes as the
//! same string the authored document carried.

use std::borrow::Borrow;
use std::fmt;

use crate::model::error::{IdentifierKind, ModelError};

pub use crate::identity::{ComponentInstanceId, RobotId, ServiceId};

/// The separator that joins a component instance id to a component-local
/// structural id when a component's structure is flattened into the robot's.
///
/// Robot links, robot joints and component instance ids may not contain it;
/// that is what makes a flattened structural identity unambiguous.
pub const MODULE_INSTANCE_SEPARATOR: &str = "__";

/// Whether `value` is a normalized canonical token.
///
/// A token is a non-empty sequence of ASCII lowercase letters (`a`-`z`), ASCII
/// digits (`0`-`9`), `_` and `-`. Nothing else is accepted: uppercase letters,
/// `.`, `/`, and any non-ASCII character are rejected.
///
/// Whitespace is rejected, never trimmed. `" abc"` and `"abc "` are not tokens.
/// A canonical identifier is compared byte for byte - it is a map key, a topic
/// component and a frame name - so trimming one on the way in would let two
/// distinct authored identifiers collide into a single runtime identity.
#[must_use]
pub fn is_valid_token(value: &str) -> bool {
    crate::identity::is_topology_token(value)
}

/// Declare a newtype whose values are exactly the strings [`is_valid_token`]
/// accepts. The wire form is the bare string, and every construction path -
/// including deserialization - goes through the one validating constructor.
macro_rules! token_identifier {
    ($(#[$doc:meta])* $name:ident, $kind:expr) => {
        $(#[$doc])*
        #[derive(
            serde::Serialize,
            serde::Deserialize,
            Clone,
            Debug,
            PartialEq,
            Eq,
            PartialOrd,
            Ord,
            Hash,
        )]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// What this identifier names, used in rejection messages.
            pub const KIND: IdentifierKind = $kind;

            /// Validate and construct the identifier.
            ///
            /// # Errors
            ///
            /// Returns [`ModelError::NotNormalized`] when `value` is not a
            /// token as defined by [`is_valid_token`].
            pub fn new(value: impl Into<String>) -> Result<Self, ModelError> {
                let value = value.into();
                if is_valid_token(&value) {
                    Ok(Self(value))
                } else {
                    Err(ModelError::NotNormalized {
                        kind: Self::KIND,
                        value,
                    })
                }
            }

            /// The normalized token.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        // Lets a `BTreeMap` keyed by this type be looked up with a plain `&str`:
        // a lookup does not need a validated key, only a construction does.
        impl Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                self.0 == other
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.0 == *other
            }
        }

        impl std::str::FromStr for $name {
            type Err = ModelError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = ModelError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl crate::__compat::wire::DescribeWire for $name {
            // Invariant: this states what `#[serde(into = "String")]` above
            // writes - the bare normalized token as one string. The token
            // grammar itself is a decode rule, not a shape.
            fn wire_schema() -> crate::__compat::wire::WireSchema {
                crate::__compat::wire::WireSchema::opaque(
                    stringify!($name),
                    crate::__compat::wire::WireSchema::String,
                )
            }
        }
    };
}

token_identifier!(
    /// The identity of a component *type* - one authored component definition.
    ComponentTypeId,
    IdentifierKind::ComponentType
);

token_identifier!(
    /// The identity of one capability within its component.
    CapabilityId,
    IdentifierKind::Capability
);

/// Declare a structural identifier newtype.
///
/// Structural names come from authored URDF, whose grammar is wider than
/// [`is_valid_token`] and is not ours to narrow, so these carry no validation.
/// They exist to keep a link id and a joint id from being interchangeable, and
/// to give namespacing a single owner.
macro_rules! structural_identifier {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(
            serde::Serialize,
            serde::Deserialize,
            Clone,
            Debug,
            PartialEq,
            Eq,
            PartialOrd,
            Ord,
            Hash,
        )]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Adopt an authored structural name.
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// The structural name.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// This component-local identity as it appears in the robot's
            /// flattened structure, under the instance that mounts it.
            #[must_use]
            pub fn namespaced(&self, component_id: &ComponentInstanceId) -> Self {
                Self(format!(
                    "{component_id}{MODULE_INSTANCE_SEPARATOR}{}",
                    self.0
                ))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                self.0 == other
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.0 == *other
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl crate::__compat::wire::DescribeWire for $name {
            // Invariant: this states what `#[serde(transparent)]` above writes -
            // the authored structural name as one string, with no wrapper.
            fn wire_schema() -> crate::__compat::wire::WireSchema {
                crate::__compat::wire::WireSchema::opaque(
                    stringify!($name),
                    crate::__compat::wire::WireSchema::String,
                )
            }
        }
    };
}

structural_identifier!(
    /// The identity of one structural link.
    LinkId
);

structural_identifier!(
    /// The identity of one structural joint.
    JointId
);

/// A capability id is the dynamic segment of every capability-scoped endpoint
/// key.
impl crate::bus::TopicSegment for CapabilityId {
    fn segment(&self) -> Result<crate::bus::KeySegment, crate::bus::KeySegmentError> {
        crate::bus::KeySegment::new(self.as_str())
    }
}

/// A joint id is the dynamic segment of the robot's per-joint endpoint keys.
impl crate::bus::TopicSegment for JointId {
    fn segment(&self) -> Result<crate::bus::KeySegment, crate::bus::KeySegmentError> {
        crate::bus::KeySegment::new(self.as_str())
    }
}

/// One capability of one component instance, written `component.capability`.
///
/// The wire form is that single dotted string, not a two-field object.
#[derive(
    serde::Serialize,
    serde::Deserialize,
    Debug,
    Clone,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    schemars::JsonSchema,
)]
// The wire form is one dotted string, so that is also the schema: describing
// the two private halves would describe a shape no document ever carries.
#[schemars(with = "String", inline)]
#[serde(try_from = "String", into = "String")]
pub struct CapabilityRef {
    pub component_id: ComponentInstanceId,
    pub capability_id: CapabilityId,
}

impl CapabilityRef {
    /// Pair two already-validated identities.
    #[must_use]
    pub const fn new(component_id: ComponentInstanceId, capability_id: CapabilityId) -> Self {
        Self {
            component_id,
            capability_id,
        }
    }
}

impl fmt::Display for CapabilityRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.component_id, self.capability_id)
    }
}

impl std::str::FromStr for CapabilityRef {
    type Err = ModelError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (component_id, capability_id) =
            value
                .split_once('.')
                .ok_or_else(|| ModelError::MalformedCapabilityReference {
                    value: value.to_string(),
                })?;
        Ok(Self::new(
            ComponentInstanceId::new(component_id)?,
            CapabilityId::new(capability_id)?,
        ))
    }
}

impl TryFrom<String> for CapabilityRef {
    type Error = ModelError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<CapabilityRef> for String {
    fn from(value: CapabilityRef) -> Self {
        value.to_string()
    }
}

impl crate::__compat::wire::DescribeWire for CapabilityRef {
    // Invariant: this states what `#[serde(into = "String")]` above writes -
    // the one dotted `component.capability` string, never the two-field struct
    // the type is made of.
    fn wire_schema() -> crate::__compat::wire::WireSchema {
        crate::__compat::wire::WireSchema::opaque(
            "CapabilityRef",
            crate::__compat::wire::WireSchema::String,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_is_lowercase_ascii_digits_underscore_or_dash() {
        for valid in ["a", "front_left_drive", "vl53l1x", "imu-0", "0"] {
            assert!(is_valid_token(valid), "{valid}");
        }
        for invalid in ["", "Abc", "a.b", "a/b", "a b", "café"] {
            assert!(!is_valid_token(invalid), "{invalid}");
        }
    }

    #[test]
    fn surrounding_whitespace_is_rejected_never_trimmed() {
        // Pinned deliberately: trimming would collapse distinct authored ids
        // onto one runtime identity.
        assert!(!is_valid_token(" abc"));
        assert!(!is_valid_token("abc "));
        assert!(!is_valid_token(" abc "));
        assert!(!is_valid_token(" "));
        assert!(!is_valid_token(""));
        assert!(is_valid_token("abc"));
    }

    #[test]
    fn token_identifiers_reject_untrimmed_values() {
        assert!(ComponentInstanceId::new(" abc").is_err());
        assert!(CapabilityId::new("abc ").is_err());
        assert!(ComponentTypeId::new("").is_err());
        assert_eq!(RobotId::new("rover").unwrap().as_str(), "rover");
    }

    #[test]
    fn token_identifiers_round_trip_as_bare_strings() {
        let id = ComponentInstanceId::new("front_left_drive").unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"front_left_drive\"");
        assert_eq!(
            serde_json::from_str::<ComponentInstanceId>(&json).unwrap(),
            id
        );

        for json in ["\" abc\"", "\"Abc\"", "\"\""] {
            assert!(
                serde_json::from_str::<CapabilityId>(json).is_err(),
                "{json}"
            );
        }
    }

    #[test]
    fn token_identifiers_are_usable_as_map_keys_on_the_wire() {
        let mut map = std::collections::BTreeMap::new();
        map.insert(CapabilityId::new("rgb").unwrap(), 1_u8);
        let json = serde_json::to_string(&map).unwrap();
        assert_eq!(json, "{\"rgb\":1}");
        assert_eq!(
            serde_json::from_str::<std::collections::BTreeMap<CapabilityId, u8>>(&json).unwrap(),
            map
        );
    }

    #[test]
    fn structural_identifiers_round_trip_as_bare_strings() {
        let link = LinkId::new("base_link");
        assert_eq!(serde_json::to_string(&link).unwrap(), "\"base_link\"");
        assert_eq!(
            serde_json::from_str::<LinkId>("\"base_link\"").unwrap(),
            link
        );

        let joint = JointId::new("wheel_joint");
        assert_eq!(serde_json::to_string(&joint).unwrap(), "\"wheel_joint\"");
        assert_eq!(
            serde_json::from_str::<JointId>("\"wheel_joint\"").unwrap(),
            joint
        );
    }

    #[test]
    fn namespacing_joins_the_instance_id_with_the_reserved_separator() {
        let instance = ComponentInstanceId::new("left_drive").unwrap();
        assert_eq!(
            LinkId::new("wheel").namespaced(&instance).as_str(),
            "left_drive__wheel"
        );
        assert_eq!(
            JointId::new("axle").namespaced(&instance).as_str(),
            "left_drive__axle"
        );
    }

    #[test]
    fn a_capability_reference_is_one_dotted_string_on_the_wire() {
        let reference: CapabilityRef = "front_camera.rgb".parse().unwrap();
        assert_eq!(reference.component_id, "front_camera");
        assert_eq!(reference.capability_id, "rgb");

        let json = serde_json::to_string(&reference).unwrap();
        assert_eq!(json, "\"front_camera.rgb\"");
        assert_eq!(
            serde_json::from_str::<CapabilityRef>(&json).unwrap(),
            reference
        );
    }

    #[test]
    fn a_capability_reference_needs_both_halves_normalized() {
        assert!("front_camera".parse::<CapabilityRef>().is_err());
        assert!("Front.rgb".parse::<CapabilityRef>().is_err());
        assert!("front. rgb".parse::<CapabilityRef>().is_err());
    }

    #[test]
    fn references_sort_by_component_then_capability() {
        let mut references = [
            CapabilityRef::new(
                ComponentInstanceId::new("b").unwrap(),
                CapabilityId::new("a").unwrap(),
            ),
            CapabilityRef::new(
                ComponentInstanceId::new("a").unwrap(),
                CapabilityId::new("b").unwrap(),
            ),
            CapabilityRef::new(
                ComponentInstanceId::new("a").unwrap(),
                CapabilityId::new("a").unwrap(),
            ),
        ];
        references.sort();
        assert_eq!(
            references
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["a.a", "a.b", "b.a"]
        );
    }
}
