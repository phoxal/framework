//! Typed topics: what a walk of the api tree hands to a handle builder.
//!
//! A [`Topic`] is one owned family-rooted key plus a phantom [`TopicKind`] that
//! ties the key to its endpoint **and to the side** the holder may take. The
//! api tree produces them - and only the api tree, through
//! [`BoundEndpoint::client`](crate::bus::BoundEndpoint::client) and
//! [`BoundEndpoint::owner`](crate::bus::BoundEndpoint::owner) - and the
//! `SetupContext` handle builders consume them. The wire body never appears in
//! the key, but the contract family does: the key is `robot/drive/state`, not
//! `drive/state`. The family is a semantic namespace, so robot contracts,
//! runtime plumbing, and supervisor process traffic occupy physically distinct
//! Zenoh subtrees. Nothing in the key encodes a version: compatibility is the
//! framework train version both peers were built from.
//!
//! # Side branding
//!
//! The kind marker is the compile-time gate that makes taking the **wrong side**
//! of a topic a type error. The four markers split each wire shape by side:
//!
//! - [`Publish<E>`] - the participant *publishes* `E` (a client sending a
//!   command, or an owner publishing its state).
//! - [`Subscribe<E>`] - the participant *subscribes/observes* `E` (a client
//!   observing state, or an owner reading its command input).
//! - [`AskQuery<E>`] - the **client** side of a query: it *calls* the owner.
//! - [`ServeQuery<E>`] - the **owner** side of a query: it *serves* requests.
//!
//! The brand is a COMPILE-TIME marker only: the underlying key and the actual
//! publisher/receiver/querier/server ops are unchanged. There is one path tree,
//! walked identically for both sides, and the side is chosen at the endpoint,
//! so an owner key and a client key are byte-identical and differ only in the
//! brand the type carries.

use std::marker::PhantomData;

/// One concrete dynamic segment in a generated topic key.
///
/// A segment is deliberately stricter than a general Zenoh key expression:
/// generated API builders are for concrete participant-owned topics, not
/// selectors. Wildcards therefore have to use a separate, explicit
/// subscription path rather than escaping into a publisher handle.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct KeySegment(String);

impl KeySegment {
    /// Validate and retain one concrete key segment.
    ///
    /// # Errors
    ///
    /// Returns [`KeySegmentError`] when the value is empty, carries a `/` or a
    /// wildcard, holds a control character, or is not a legal Zenoh key.
    pub fn new(value: impl Into<String>) -> Result<Self, KeySegmentError> {
        let value = value.into();
        if value.is_empty()
            || value.contains('/')
            || value.contains('*')
            || value.chars().any(|character| character.is_control())
            || zenoh::key_expr::OwnedKeyExpr::new(value.clone()).is_err()
        {
            return Err(KeySegmentError(value));
        }
        Ok(Self(value))
    }

    /// The validated segment text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for KeySegment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl TryFrom<String> for KeySegment {
    type Error = KeySegmentError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<&str> for KeySegment {
    type Error = KeySegmentError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// A dynamic topic value that is not one concrete key segment.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error(
    "topic key segment must be non-empty, concrete, and contain no '/', '*', or control characters; got {0:?}"
)]
pub struct KeySegmentError(String);

/// A pub/sub topic the participant **publishes** `E` on (client command, or
/// owner state).
pub struct Publish<E>(PhantomData<fn() -> E>);

/// A pub/sub topic the participant **subscribes/observes** `E` on (client
/// observing state, or owner reading its command input).
pub struct Subscribe<E>(PhantomData<fn() -> E>);

/// The **client** side of a query topic: the holder *calls* the owner.
pub struct AskQuery<E>(PhantomData<fn() -> E>);

/// The **owner** side of a query topic: the holder *serves* requests.
pub struct ServeQuery<E>(PhantomData<fn() -> E>);

mod sealed {
    pub trait Sealed {}
}

/// Marker for the kind (wire shape + side) of a [`Topic`]. Sealed.
pub trait TopicKind: sealed::Sealed {}

impl<E> sealed::Sealed for Publish<E> {}
impl<E> TopicKind for Publish<E> {}
impl<E> sealed::Sealed for Subscribe<E> {}
impl<E> TopicKind for Subscribe<E> {}
impl<E> sealed::Sealed for AskQuery<E> {}
impl<E> TopicKind for AskQuery<E> {}
impl<E> sealed::Sealed for ServeQuery<E> {}
impl<E> TopicKind for ServeQuery<E> {}

/// A typed topic: one owned family-rooted key bound to its endpoint and side
/// via `Kind`.
///
/// The key is owned, not a `Cow`: a path walk renders one concrete key at
/// handle construction, and one setup-time allocation is preferable to a
/// permanent dual representation with a forging constructor on each half.
pub struct Topic<Kind> {
    key: String,
    _kind: PhantomData<Kind>,
}

impl<Kind> Topic<Kind> {
    /// Bind a rendered concrete key to its brand.
    ///
    /// Crate-private, and the only constructor: the api tree reaches it through
    /// [`BoundEndpoint`](crate::bus::BoundEndpoint), which is what keeps the
    /// typed `Kind` and the bus key in lockstep, and the bus's own unit tests
    /// reach it the same way.
    pub(crate) fn new(key: String) -> Self {
        Topic {
            key,
            _kind: PhantomData,
        }
    }

    /// The family-rooted topic key (e.g. `robot/drive/state`).
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The key reusable as the publish key. Wildcard topics (`*`) are
    /// subscribe-only and rejected here before transport.
    ///
    /// # Errors
    ///
    /// Returns [`WildcardPublish`] when the key holds a wildcard segment.
    pub fn publish_key(&self) -> Result<&str, WildcardPublish> {
        if self.key.split('/').any(|seg| seg == "*" || seg == "**") {
            Err(WildcardPublish {
                key: self.key.clone(),
            })
        } else {
            Ok(&self.key)
        }
    }
}

impl<Kind> Clone for Topic<Kind> {
    fn clone(&self) -> Self {
        Topic {
            key: self.key.clone(),
            _kind: PhantomData,
        }
    }
}

/// Attempted to publish on a wildcard (subscribe-only) topic.
#[derive(Debug, thiserror::Error)]
#[error("cannot publish on wildcard topic '{key}' (wildcards are subscribe-only)")]
pub struct WildcardPublish {
    /// The offending key.
    pub key: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_segments_reject_non_concrete_values() {
        for invalid in ["", "a/b", "*", "**", "a\n"] {
            assert!(
                KeySegment::new(invalid).is_err(),
                "{invalid:?} must not cross a dynamic builder boundary"
            );
        }

        let segment = KeySegment::new("front_left").expect("concrete segment");
        assert_eq!(segment.as_str(), "front_left");
        assert_eq!(segment.to_string(), "front_left");
    }

    #[test]
    fn a_concrete_key_is_publishable_and_a_wildcard_one_is_not() {
        let concrete = Topic::<Publish<()>>::new("robot/drive/state".to_owned());
        assert_eq!(
            concrete
                .publish_key()
                .expect("a concrete key is publishable"),
            "robot/drive/state"
        );

        for wildcard in ["robot/component/*/state", "robot/component/**"] {
            let topic = Topic::<Subscribe<()>>::new(wildcard.to_owned());
            let rejected = topic
                .publish_key()
                .expect_err("a wildcard topic is subscribe-only");
            assert_eq!(rejected.key, wildcard);
        }
    }
}
