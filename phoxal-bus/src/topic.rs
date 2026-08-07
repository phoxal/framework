//! Typed topics: the api-local builder output.
//!
//! A [`Topic`] is a version-qualified topic key plus a phantom [`TopicKind`]
//! that ties the key to its body type(s) **and to the side** the holder may
//! take. The api tree's `topic` builders return these; the `SetupContext` handle
//! builders consume them. The wire body never appears in the key, but the
//! version does - the key is `v0.1/drive/state`, not `drive/state`: folding the
//! version into the key is what makes different versioned names physically
//! distinct Zenoh keys.
//!
//! # Side branding
//!
//! The kind marker is the compile-time gate that makes taking the **wrong side**
//! of a topic a type error. The four markers split each wire shape by side:
//!
//! - [`Publish<B>`] - the participant *publishes* `B` (a client sending a command,
//!   or an owner publishing its state).
//! - [`Subscribe<B>`] - the participant *subscribes/observes* `B` (a client
//!   observing state, or an owner reading its command input).
//! - [`AskQuery<Req, Resp>`] - the **client** side of a query: it *calls* the owner.
//! - [`ServeQuery<Req, Resp>`] - the **owner** side of a query: it *serves* requests.
//!
//! The brand is a COMPILE-TIME marker only: the underlying key and the actual
//! `Publisher`/`Subscriber`/`Latest`/`Querier`/server ops are unchanged. The api
//! tree emits the builder tree twice - a public *client* builder and an
//! explicit *owner* builder over identical keys - so the side a participant gets
//! is decided by which builder it calls, and a wrong side fails to compile in the
//! `SetupContext` handle builder that consumes the `Topic`.

use std::borrow::Cow;
use std::marker::PhantomData;

/// A pub/sub topic the participant **publishes** `B` on (client command, or
/// owner state).
pub struct Publish<B>(PhantomData<fn() -> B>);

/// A pub/sub topic the participant **subscribes/observes** `B` on (client
/// observing state, or owner reading its command input).
pub struct Subscribe<B>(PhantomData<fn() -> B>);

/// The **client** side of a query topic carrying request `Req`/response `Resp`:
/// the holder *calls* the owner.
pub struct AskQuery<Req, Resp>(PhantomData<fn() -> (Req, Resp)>);

/// The **owner** side of a query topic carrying request `Req`/response `Resp`:
/// the holder *serves* requests.
pub struct ServeQuery<Req, Resp>(PhantomData<fn() -> (Req, Resp)>);

mod sealed {
    pub trait Sealed {}
}

/// Marker for the kind (wire shape + side) of a [`Topic`]. Sealed.
pub trait TopicKind: sealed::Sealed {}

impl<B> sealed::Sealed for Publish<B> {}
impl<B> TopicKind for Publish<B> {}
impl<B> sealed::Sealed for Subscribe<B> {}
impl<B> TopicKind for Subscribe<B> {}
impl<Req, Resp> sealed::Sealed for AskQuery<Req, Resp> {}
impl<Req, Resp> TopicKind for AskQuery<Req, Resp> {}
impl<Req, Resp> sealed::Sealed for ServeQuery<Req, Resp> {}
impl<Req, Resp> TopicKind for ServeQuery<Req, Resp> {}

/// A typed topic: a version-qualified key bound to its body type(s) via `Kind`.
pub struct Topic<Kind> {
    key: Cow<'static, str>,
    _kind: PhantomData<Kind>,
}

impl<Kind> Topic<Kind> {
    /// Construct a topic from a static key.
    ///
    /// # Why this is `pub`
    ///
    /// The `phoxal_api_tree!` macro expands in the `phoxal-api` crate, where the
    /// versioned APIs live, and its generated builders call this over each
    /// contract's canonical key. Generated code in a downstream crate needs a
    /// `pub` constructor, and Rust has no visibility between "this crate" and
    /// "the world", so `pub(crate)` cannot express the real boundary. The one
    /// caller this exists for is `phoxal-api`'s generated builder tree.
    ///
    /// It is generic over `Kind`, so a caller that reaches for it directly can
    /// forge either branded side of a topic. Author correctness does not come
    /// from this being hidden: it comes from the typed handles and the api-tree
    /// builders (`api::topic::client()` / `api::topic::owner()`), which keep the
    /// typed `Kind` and the bus key in lockstep.
    #[doc(hidden)]
    pub fn new_static(key: &'static str) -> Self {
        Topic {
            key: Cow::Borrowed(key),
            _kind: PhantomData,
        }
    }

    /// Construct a topic from an owned (dynamically built) key.
    ///
    /// The owned-key counterpart of [`new_static`](Self::new_static), called by
    /// the same generated builder for nodes with dynamic segments, filling the
    /// carried variables into the canonical key. It is `pub` for exactly the
    /// same crate-split reason, with exactly the same one intended caller.
    #[doc(hidden)]
    pub fn new_owned(key: String) -> Self {
        Topic {
            key: Cow::Owned(key),
            _kind: PhantomData,
        }
    }

    /// The version-qualified topic key (e.g. `v0.1/drive/state`).
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The key reusable as the publish key. Wildcard topics (`*`) are
    /// subscribe-only and rejected here before transport.
    pub fn publish_key(&self) -> Result<&str, WildcardPublish> {
        if self.key.split('/').any(|seg| seg == "*" || seg == "**") {
            Err(WildcardPublish {
                key: self.key.to_string(),
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
    fn a_concrete_key_is_publishable_and_a_wildcard_one_is_not() {
        let concrete = Topic::<Publish<()>>::new_static("v0.1/drive/state");
        assert_eq!(
            concrete
                .publish_key()
                .expect("a concrete key is publishable"),
            "v0.1/drive/state"
        );

        for wildcard in ["v0.1/component/*/state", "v0.1/component/**"] {
            let topic = Topic::<Subscribe<()>>::new_owned(wildcard.to_string());
            let rejected = topic
                .publish_key()
                .expect_err("a wildcard topic is subscribe-only");
            assert_eq!(rejected.key, wildcard);
        }
    }
}
