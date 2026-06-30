//! Typed topics - the api-local builder output (D61).
//!
//! A [`Topic`] is a versionless topic key plus a phantom [`TopicKind`] that ties
//! the key to its body type(s). The api tree's `topic` builders return these; the
//! `SetupContext` handle builders consume them. The wire body never appears in the
//! key - the key is `drive/state`, not `drive/state/v1` (D62).

use std::borrow::Cow;
use std::marker::PhantomData;

/// A pub/sub topic carrying body `B`.
pub struct PubSub<B>(PhantomData<fn() -> B>);

/// A query topic carrying request `Req` and response `Resp`.
pub struct Query<Req, Resp>(PhantomData<fn() -> (Req, Resp)>);

mod sealed {
    pub trait Sealed {}
}

/// Marker for the kind of a [`Topic`] (pub/sub vs query). Sealed.
pub trait TopicKind: sealed::Sealed {}

impl<B> sealed::Sealed for PubSub<B> {}
impl<B> TopicKind for PubSub<B> {}
impl<Req, Resp> sealed::Sealed for Query<Req, Resp> {}
impl<Req, Resp> TopicKind for Query<Req, Resp> {}

/// A typed topic: a versionless key bound to its body type(s) via `Kind`.
pub struct Topic<Kind> {
    key: Cow<'static, str>,
    _kind: PhantomData<Kind>,
}

impl<Kind> Topic<Kind> {
    /// Construct a topic from a static key.
    ///
    /// `#[doc(hidden)] pub` raw constructor - an unsupported escape hatch, NOT
    /// part of the authored surface. It is `pub` only because it must be callable
    /// across the `phoxal` / `phoxal-bus` crate split: the `phoxal_api_tree!` macro
    /// (generated in the `phoxal` engine crate, where the dated API versions live)
    /// calls it over each contract's canonical key, and a `pub(crate)` constructor
    /// cannot cross that boundary. Author correctness does not come from hiding
    /// this: it comes from the typed handles and the api-tree builders
    /// (`api::topic::new()....()`), which keep the typed `Kind` and the bus key in
    /// lockstep (D61/D62). Stronger owner-side enforcement - a runner-minted owner
    /// capability that closes this raw-bus hole - is deferred to plan #00's L2.
    #[doc(hidden)]
    pub fn new_static(key: &'static str) -> Self {
        Topic {
            key: Cow::Borrowed(key),
            _kind: PhantomData,
        }
    }

    /// Construct a topic from an owned (dynamically built) key.
    ///
    /// `#[doc(hidden)] pub` raw constructor, the owned-key counterpart of
    /// [`new_static`](Self::new_static): an unsupported escape hatch that is `pub`
    /// only to cross the `phoxal` / `phoxal-bus` crate split. The generated api
    /// builder calls it for nodes with dynamic segments, filling the carried
    /// variables into the canonical key. Not part of the authored surface;
    /// correctness for authors comes from the typed handles + api-tree builders,
    /// and stronger owner-side enforcement is deferred to plan #00's L2.
    #[doc(hidden)]
    pub fn new_owned(key: String) -> Self {
        Topic {
            key: Cow::Owned(key),
            _kind: PhantomData,
        }
    }

    /// The versionless topic key (e.g. `drive/state`).
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
