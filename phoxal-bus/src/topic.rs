//! Typed topics - the api-local builder output (D61), side-branded for L1 (plan #00).
//!
//! A [`Topic`] is a versionless topic key plus a phantom [`TopicKind`] that ties
//! the key to its body type(s) **and to the side** the holder may take. The api
//! tree's `topic` builders return these; the `SetupContext` handle builders
//! consume them. The wire body never appears in the key - the key is
//! `drive/state`, not `drive/state/v1` (D62).
//!
//! # Side branding (L1)
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
//! `internal` *owner* builder over identical keys - so the side a participant gets
//! is decided by which builder it calls, and a wrong side fails to compile in the
//! `SetupContext` handle builder that consumes the `Topic`.

use std::borrow::Cow;
use std::marker::PhantomData;

/// A pub/sub topic the participant **publishes** `B` on (client command, or owner
/// state). The publish side of the former side-agnostic `PubSub<B>`.
pub struct Publish<B>(PhantomData<fn() -> B>);

/// A pub/sub topic the participant **subscribes/observes** `B` on (client
/// observing state, or owner reading its command input). The subscribe side of
/// the former side-agnostic `PubSub<B>`.
pub struct Subscribe<B>(PhantomData<fn() -> B>);

/// The **client** side of a query topic carrying request `Req`/response `Resp`:
/// the holder *calls* the owner. The caller side of the former side-agnostic
/// `Query<Req, Resp>`.
pub struct AskQuery<Req, Resp>(PhantomData<fn() -> (Req, Resp)>);

/// The **owner** side of a query topic carrying request `Req`/response `Resp`:
/// the holder *serves* requests. The server side of the former side-agnostic
/// `Query<Req, Resp>`.
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
    /// across the `phoxal-api` / `phoxal-bus` crate split: the `phoxal_api_tree!`
    /// macro (invoked in the `phoxal-api` crate, where the dated API versions live)
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
    /// only to cross the `phoxal-api` / `phoxal-bus` crate split. The generated api
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
