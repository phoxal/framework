//
// Copyright (c) 2024 ZettaScale Technology
//
// This program and the accompanying materials are made available under the
// terms of the Eclipse Public License 2.0 which is available at
// http://www.eclipse.org/legal/epl-2.0, or the Apache License, Version 2.0
// which is available at https://www.apache.org/licenses/LICENSE-2.0.
//
// SPDX-License-Identifier: EPL-2.0 OR Apache-2.0
//
// Contributors:
//   ZettaScale Zenoh Team, <zenoh@zettascale.tech>
//

//! Typed wrappers for compile-time payload safety.
//!
//! These wrappers build on `serde` payload types to provide typed pub/sub and
//! query/reply where the payload type is part of the contract. Wrong types are
//! compile errors, not runtime failures.

use std::{
    future::{IntoFuture, Ready},
    marker::PhantomData,
    time::Duration,
};

use crate::bus::pubsub::Stamped;
use serde::{Serialize, de::DeserializeOwned};
use zenoh::{
    Error, Result as ZResult, Wait,
    bytes::{Encoding, ZBytes},
    handlers::{Callback, DefaultHandler, FifoChannelHandler},
    key_expr::KeyExpr,
    matching::MatchingStatus,
    pubsub::{Publisher, PublisherBuilder, Subscriber, SubscriberBuilder},
    query::{Query, QueryConsolidation, QueryTarget, Queryable, QueryableBuilder, ReplyKeyExpr},
    sample::{Locality, Sample},
    session::Session,
};

type TypedDecodeError = rmp_serde::decode::Error;

fn typed_encoding(schema_name: &str) -> Encoding {
    Encoding::from(format!("zenoh-ext/typed:{schema_name}"))
}

fn serialize_payload<T: Serialize>(value: &T) -> ZResult<ZBytes> {
    rmp_serde::to_vec_named(value)
        .map(ZBytes::from)
        .map_err(Into::into)
}

fn deserialize_payload<T: DeserializeOwned>(payload: &ZBytes) -> Result<T, TypedDecodeError> {
    rmp_serde::from_slice(payload.to_bytes().as_ref())
}

/// Trait for types that provide a stable, user-defined schema name.
///
/// `std::any::type_name` is not guaranteed stable across Rust compiler versions,
/// so types used with [`TypedPublisher`] and [`TypedSubscriber`] must implement
/// this trait to provide a fixed identifier for wire encoding and error messages.
///
/// The schema name is embedded in the Zenoh [`Encoding`](zenoh::bytes::Encoding)
/// as `zenoh-ext/typed:{SCHEMA_NAME}`, making it cross-language compatible
/// (Python/C++ clients can match on the same string).
///
/// # Example
/// ```ignore
/// impl TypedSchema for TelemetryPayload {
///     const SCHEMA_NAME: &'static str = "telemetry-payload";
/// }
/// ```
pub trait TypedSchema {
    /// A stable, human-readable name for this type's schema.
    ///
    /// Must be unique per type within your application and stable across
    /// compiler versions and languages.
    const SCHEMA_NAME: &'static str;

    /// Monotonic schema version for this wire contract.
    ///
    /// Change this when the wire payload changes incompatibly.
    const SCHEMA_VERSION: u32;
}

pub trait BusyResponse {
    fn busy() -> Self;
}

impl TypedSchema for () {
    const SCHEMA_NAME: &'static str = "unit";
    const SCHEMA_VERSION: u32 = 1;
}

impl<T: TypedSchema> TypedSchema for Stamped<T> {
    const SCHEMA_NAME: &'static str = T::SCHEMA_NAME;
    const SCHEMA_VERSION: u32 = T::SCHEMA_VERSION;
}

/// Error type for typed payload operations that include encoding and version checking.
#[derive(Debug)]
pub enum TypedPayloadError {
    /// The payload was required but not present.
    MissingPayload,
    /// The payload is missing the required schema version attachment.
    MissingSchemaVersion { expected: u32, type_name: String },
    /// The payload carried an invalid schema version attachment.
    InvalidSchemaVersion { expected: u32, type_name: String },
    /// The payload's encoding doesn't match the expected `zenoh-ext/typed:{SCHEMA_NAME}`.
    EncodingMismatch { expected: String, received: String },
    /// The payload's schema version doesn't match the expected version.
    VersionMismatch {
        expected: u32,
        received: u32,
        type_name: String,
    },
    /// The payload could not be deserialized into the expected type.
    DeserializationFailed(TypedDecodeError),
}

impl std::fmt::Display for TypedPayloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingPayload => write!(f, "missing payload"),
            Self::MissingSchemaVersion {
                expected,
                type_name,
            } => write!(
                f,
                "missing schema version for {type_name}: expected {expected}"
            ),
            Self::InvalidSchemaVersion {
                expected,
                type_name,
            } => write!(
                f,
                "invalid schema version for {type_name}: expected {expected}"
            ),
            Self::EncodingMismatch { expected, received } => write!(
                f,
                "encoding mismatch: expected {expected}, received {received}"
            ),
            Self::VersionMismatch {
                expected,
                received,
                type_name,
            } => write!(
                f,
                "schema version mismatch for {type_name}: expected {expected}, received {received}"
            ),
            Self::DeserializationFailed(e) => write!(f, "deserialization failed: {e}"),
        }
    }
}

impl std::error::Error for TypedPayloadError {}

impl From<TypedDecodeError> for TypedPayloadError {
    fn from(e: TypedDecodeError) -> Self {
        Self::DeserializationFailed(e)
    }
}

/// Encode a schema version into a ZBytes attachment value.
fn encode_schema_version(version: u32) -> ZBytes {
    ZBytes::from(version.to_le_bytes().to_vec())
}

/// Decode a schema version from a ZBytes attachment value.
fn decode_schema_version(zbytes: &ZBytes) -> Option<u32> {
    let bytes = zbytes.to_bytes();
    if bytes.len() == 4 {
        Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    } else {
        None
    }
}

/// Check if a payload attachment carries a matching schema version.
fn check_schema_version(
    attachment: Option<&ZBytes>,
    expected_version: Option<u32>,
    schema_name: &str,
) -> Result<(), TypedPayloadError> {
    let expected = match expected_version {
        Some(v) => v,
        None => return Ok(()),
    };

    let Some(attachment) = attachment else {
        return Err(TypedPayloadError::MissingSchemaVersion {
            expected,
            type_name: schema_name.to_string(),
        });
    };

    let Some(received) = decode_schema_version(attachment) else {
        return Err(TypedPayloadError::InvalidSchemaVersion {
            expected,
            type_name: schema_name.to_string(),
        });
    };

    if received != expected {
        return Err(TypedPayloadError::VersionMismatch {
            expected,
            received,
            type_name: schema_name.to_string(),
        });
    }

    Ok(())
}

/// Check that a payload encoding matches the expected typed encoding.
fn check_encoding(received: Option<&Encoding>, schema_name: &str) -> Result<(), TypedPayloadError> {
    let expected = typed_encoding(schema_name);
    let Some(received) = received else {
        return Err(TypedPayloadError::EncodingMismatch {
            expected: expected.to_string(),
            received: "<none>".to_string(),
        });
    };

    if *received != expected {
        return Err(TypedPayloadError::EncodingMismatch {
            expected: expected.to_string(),
            received: received.to_string(),
        });
    }
    Ok(())
}

/// Error type for [`TypedSessionExt::typed_get`] operations.
///
/// Distinguishes between reply errors (the remote end sent an error reply)
/// and typed payload errors (contract mismatch or deserialization failure).
#[derive(Debug)]
pub enum TypedGetError {
    /// The remote queryable replied with an error payload.
    ReplyError(ZBytes),
    /// The reply payload failed typed contract validation or deserialization.
    Payload(TypedPayloadError),
}

impl std::fmt::Display for TypedGetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReplyError(_) => write!(f, "remote replied with error"),
            Self::Payload(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for TypedGetError {}

impl From<TypedPayloadError> for TypedGetError {
    fn from(e: TypedPayloadError) -> Self {
        Self::Payload(e)
    }
}

/// A publisher that only accepts payloads of type `T`.
///
/// Wraps a [`Publisher`] and serializes `T` via MessagePack
/// on each `put()`. Attempting to publish a wrong type is a compile error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishPolicy {
    Lazy,
    Eager,
}

pub struct TypedPublisher<'a, T: Serialize + TypedSchema> {
    inner: Publisher<'a>,
    schema_version: Option<u32>,
    publish_policy: PublishPolicy,
    _phantom: PhantomData<T>,
}

impl<T: Serialize + TypedSchema> TypedPublisher<'_, T> {
    /// Publish a typed payload.
    pub async fn put(&self, payload: &T) -> ZResult<()> {
        if let Ok(false) = self.should_publish().await {
            return Ok(());
        }

        let payload = serialize_payload(payload)?;
        let mut inner = self.inner.put(payload);
        if let Some(version) = self.schema_version {
            inner = inner.attachment(encode_schema_version(version));
        }
        inner.await
    }

    pub fn publish_policy(&self) -> PublishPolicy {
        self.publish_policy
    }

    pub async fn has_matching_subscribers(&self) -> ZResult<bool> {
        self.matching_status().await.map(|status| status.matching())
    }

    pub async fn should_publish(&self) -> ZResult<bool> {
        match self.publish_policy {
            PublishPolicy::Lazy => self.has_matching_subscribers().await,
            PublishPolicy::Eager => Ok(true),
        }
    }

    pub async fn matching_status(&self) -> ZResult<MatchingStatus> {
        self.inner.matching_status().await
    }

    /// Returns the [`KeyExpr`] this publisher publishes to.
    pub fn key_expr(&self) -> &KeyExpr<'_> {
        self.inner.key_expr()
    }

    /// Returns the [`Encoding`] set on this publisher.
    pub fn encoding(&self) -> &Encoding {
        self.inner.encoding()
    }
}

/// A subscriber that deserializes incoming payloads into type `T`.
///
/// Wraps a [`Subscriber`] and yields `Result<T, TypedPayloadError>` on each
/// received sample. Malformed payloads yield `Err`, never panic.
///
/// The `Handler` type parameter defaults to [`FifoChannelHandler<Sample>`].
/// Use [`TypedSubscriberBuilder::with`] to specify a custom handler.
pub struct TypedSubscriber<T: DeserializeOwned + TypedSchema, Handler = FifoChannelHandler<Sample>>
{
    inner: Subscriber<Handler>,
    schema_version: Option<u32>,
    _phantom: PhantomData<T>,
}

impl<T: DeserializeOwned + TypedSchema> TypedSubscriber<T, FifoChannelHandler<Sample>> {
    /// Wait for an incoming typed message.
    ///
    /// The outer `ZResult` fails only if the channel is closed.
    /// The inner `Result` indicates deserialization success/failure,
    /// or a version mismatch if schema versioning is configured.
    pub async fn recv_async(&self) -> ZResult<Result<T, TypedPayloadError>> {
        let sample = self.inner.recv_async().await?;
        Ok(self.deserialize_sample(&sample))
    }

    /// Blocking receive for an incoming typed message.
    pub fn recv(&self) -> ZResult<Result<T, TypedPayloadError>> {
        let sample = self.inner.recv()?;
        Ok(self.deserialize_sample(&sample))
    }
}

impl<T: DeserializeOwned + TypedSchema, Handler> TypedSubscriber<T, Handler> {
    fn deserialize_sample(&self, sample: &Sample) -> Result<T, TypedPayloadError> {
        check_encoding(Some(sample.encoding()), T::SCHEMA_NAME)?;
        check_schema_version(sample.attachment(), self.schema_version, T::SCHEMA_NAME)?;
        deserialize_payload::<T>(sample.payload()).map_err(TypedPayloadError::from)
    }
}

impl<T: DeserializeOwned + TypedSchema, Handler> TypedSubscriber<T, Handler> {
    /// Returns the [`KeyExpr`] this subscriber is subscribed to.
    pub fn key_expr(&self) -> &KeyExpr<'static> {
        self.inner.key_expr()
    }

    /// Returns a reference to the inner handler.
    pub fn handler(&self) -> &Handler {
        self.inner.handler()
    }
}

/// Builder for [`TypedPublisher`].
pub struct TypedPublisherBuilder<'a, 'b, T: Serialize + TypedSchema> {
    inner: PublisherBuilder<'a, 'b>,
    schema_version: Option<u32>,
    publish_policy: PublishPolicy,
    _phantom: PhantomData<T>,
}

impl<'a, 'b, T: Serialize + TypedSchema> TypedPublisherBuilder<'a, 'b, T> {
    /// Set the schema version for this publisher.
    ///
    /// When set, the version is attached to every publication as metadata.
    /// Subscribers with a matching expected version will accept the message;
    /// subscribers expecting a different version will reject it.
    pub fn schema_version(mut self, version: u32) -> Self {
        self.schema_version = Some(version);
        self
    }

    pub fn lazy(self) -> Self {
        self.publish_policy(PublishPolicy::Lazy)
    }

    pub fn eager(self) -> Self {
        self.publish_policy(PublishPolicy::Eager)
    }

    pub fn publish_policy(mut self, publish_policy: PublishPolicy) -> Self {
        self.publish_policy = publish_policy;
        self
    }

    pub fn allowed_destination(self, destination: Locality) -> Self {
        Self {
            inner: self.inner.allowed_destination(destination),
            schema_version: self.schema_version,
            publish_policy: self.publish_policy,
            _phantom: self._phantom,
        }
    }

    fn build(self) -> ZResult<TypedPublisher<'b, T>> {
        assert!(
            !T::SCHEMA_NAME.trim().is_empty(),
            "TypedSchema::SCHEMA_NAME must not be empty or whitespace-only"
        );
        let inner = self.inner.wait()?;
        Ok(TypedPublisher {
            inner,
            schema_version: self.schema_version,
            publish_policy: self.publish_policy,
            _phantom: PhantomData,
        })
    }
}

impl<'b, T: Serialize + TypedSchema> IntoFuture for TypedPublisherBuilder<'_, 'b, T> {
    type Output = ZResult<TypedPublisher<'b, T>>;
    type IntoFuture = Ready<Self::Output>;

    fn into_future(self) -> Self::IntoFuture {
        std::future::ready(self.build())
    }
}

/// Builder for [`TypedSubscriber`].
///
/// By default, uses [`FifoChannelHandler`] (the zenoh default handler).
/// Use [`.with(handler)`](TypedSubscriberBuilder::with) to specify a custom handler.
pub struct TypedSubscriberBuilder<
    'a,
    'b,
    T: DeserializeOwned + TypedSchema,
    Handler = DefaultHandler,
> {
    inner: SubscriberBuilder<'a, 'b, Handler>,
    schema_version: Option<u32>,
    _phantom: PhantomData<T>,
}

impl<'a, 'b, T: DeserializeOwned + TypedSchema, Handler>
    TypedSubscriberBuilder<'a, 'b, T, Handler>
{
    /// Set the expected schema version for this subscriber.
    ///
    /// When set, incoming messages with a mismatched version attachment
    /// will yield [`TypedPayloadError::VersionMismatch`] without attempting
    /// deserialization.
    pub fn schema_version(mut self, version: u32) -> Self {
        self.schema_version = Some(version);
        self
    }

    pub fn allowed_origin(self, origin: Locality) -> Self {
        Self {
            inner: self.inner.allowed_origin(origin),
            schema_version: self.schema_version,
            _phantom: self._phantom,
        }
    }
}

impl<'a, 'b, T: DeserializeOwned + TypedSchema> TypedSubscriberBuilder<'a, 'b, T, DefaultHandler> {
    /// Specify a custom handler for this subscriber.
    ///
    /// The handler must implement [`IntoHandler<Sample>`].
    pub fn with<NewHandler>(
        self,
        handler: NewHandler,
    ) -> TypedSubscriberBuilder<'a, 'b, T, NewHandler>
    where
        NewHandler: zenoh::handlers::IntoHandler<Sample>,
    {
        TypedSubscriberBuilder {
            inner: self.inner.with(handler),
            schema_version: self.schema_version,
            _phantom: PhantomData,
        }
    }

    pub fn callback<F>(self, callback: F) -> TypedSubscriberBuilder<'a, 'b, T, Callback<Sample>>
    where
        F: Fn(Sample) + Send + Sync + 'static,
    {
        TypedSubscriberBuilder {
            inner: self.inner.callback(callback),
            schema_version: self.schema_version,
            _phantom: PhantomData,
        }
    }

    pub fn callback_mut<F>(self, callback: F) -> TypedSubscriberBuilder<'a, 'b, T, Callback<Sample>>
    where
        F: FnMut(Sample) + Send + Sync + 'static,
    {
        TypedSubscriberBuilder {
            inner: self.inner.callback_mut(callback),
            schema_version: self.schema_version,
            _phantom: PhantomData,
        }
    }
}

impl<T: DeserializeOwned + TypedSchema, Handler> TypedSubscriberBuilder<'_, '_, T, Handler>
where
    Handler: zenoh::handlers::IntoHandler<Sample> + Send,
    Handler::Handler: Send,
{
    fn build(self) -> ZResult<TypedSubscriber<T, Handler::Handler>> {
        assert!(
            !T::SCHEMA_NAME.trim().is_empty(),
            "TypedSchema::SCHEMA_NAME must not be empty or whitespace-only"
        );
        let inner = self.inner.wait()?;
        Ok(TypedSubscriber {
            inner,
            schema_version: self.schema_version,
            _phantom: PhantomData,
        })
    }
}

impl<T: DeserializeOwned + TypedSchema, Handler> IntoFuture
    for TypedSubscriberBuilder<'_, '_, T, Handler>
where
    Handler: zenoh::handlers::IntoHandler<Sample> + Send,
    Handler::Handler: Send,
{
    type Output = ZResult<TypedSubscriber<T, Handler::Handler>>;
    type IntoFuture = Ready<Self::Output>;

    fn into_future(self) -> Self::IntoFuture {
        std::future::ready(self.build())
    }
}

// -- Typed Query/Reply --

/// A typed query received by a [`TypedQueryable`].
///
/// Wraps a [`Query`] with typed deserialization of the request payload
/// and typed serialization of the reply.
pub struct TypedQuery<Req: DeserializeOwned + TypedSchema, Resp: Serialize + TypedSchema> {
    inner: Query,
    _phantom: PhantomData<(Req, Resp)>,
}

impl<Req: DeserializeOwned + TypedSchema, Resp: Serialize + TypedSchema> TypedQuery<Req, Resp> {
    /// Attempt to deserialize the query payload into the request type.
    ///
    /// Returns `Err` if the query has no payload or if deserialization fails.
    pub fn request(&self) -> Result<Req, TypedPayloadError> {
        let payload = self
            .inner
            .payload()
            .ok_or(TypedPayloadError::MissingPayload)?;
        check_encoding(self.inner.encoding(), Req::SCHEMA_NAME)?;
        check_schema_version(
            self.inner.attachment(),
            Some(Req::SCHEMA_VERSION),
            Req::SCHEMA_NAME,
        )?;
        deserialize_payload::<Req>(payload).map_err(TypedPayloadError::from)
    }

    /// Reply to this query with a typed response.
    pub async fn reply(&self, resp: &Resp) -> ZResult<()> {
        let zbytes = serialize_payload(resp)?;
        self.inner
            .reply(self.inner.key_expr(), zbytes)
            .encoding(typed_encoding(Resp::SCHEMA_NAME))
            .attachment(encode_schema_version(Resp::SCHEMA_VERSION))
            .await
    }

    /// Reply with an error payload.
    pub async fn reply_err<IntoZBytes: Into<ZBytes>>(&self, payload: IntoZBytes) -> ZResult<()> {
        self.inner.reply_err(payload).await
    }

    /// Access the underlying [`Query`] for metadata (key_expr, parameters, etc.).
    pub fn query(&self) -> &Query {
        &self.inner
    }
}

/// A queryable that yields typed queries.
///
/// Wraps a [`Queryable`] and yields [`TypedQuery<Req, Resp>`] on each
/// incoming query. Request payloads are deserialized into `Req`,
/// and `reply()` serializes `Resp`.
pub struct TypedQueryable<
    Req: DeserializeOwned + TypedSchema,
    Resp: Serialize + TypedSchema,
    Handler = FifoChannelHandler<Query>,
> {
    inner: Queryable<Handler>,
    _phantom: PhantomData<(Req, Resp)>,
}

impl<Req: DeserializeOwned + TypedSchema, Resp: Serialize + TypedSchema>
    TypedQueryable<Req, Resp, FifoChannelHandler<Query>>
{
    /// Wait for an incoming typed query.
    pub async fn recv_async(&self) -> ZResult<TypedQuery<Req, Resp>> {
        let query = self.inner.recv_async().await?;
        Ok(TypedQuery {
            inner: query,
            _phantom: PhantomData,
        })
    }

    /// Blocking receive for an incoming typed query.
    pub fn recv(&self) -> ZResult<TypedQuery<Req, Resp>> {
        let query = self.inner.recv()?;
        Ok(TypedQuery {
            inner: query,
            _phantom: PhantomData,
        })
    }
}

/// Builder for [`TypedQueryable`].
pub struct TypedQueryableBuilder<
    'a,
    'b,
    Req: DeserializeOwned + TypedSchema,
    Resp: Serialize + TypedSchema,
> {
    inner: QueryableBuilder<'a, 'b, DefaultHandler>,
    _phantom: PhantomData<(Req, Resp)>,
}

impl<'a, 'b, Req: DeserializeOwned + TypedSchema, Resp: Serialize + TypedSchema>
    TypedQueryableBuilder<'a, 'b, Req, Resp>
{
    pub fn with<Handler>(
        self,
        handler: Handler,
    ) -> TypedQueryableWithHandlerBuilder<'a, 'b, Req, Resp, Handler>
    where
        Handler: zenoh::handlers::IntoHandler<Query>,
    {
        TypedQueryableWithHandlerBuilder {
            inner: self.inner.with(handler),
            _phantom: PhantomData,
        }
    }

    pub fn callback<F>(
        self,
        callback: F,
    ) -> TypedQueryableWithHandlerBuilder<'a, 'b, Req, Resp, Callback<Query>>
    where
        F: Fn(Query) + Send + Sync + 'static,
    {
        TypedQueryableWithHandlerBuilder {
            inner: self.inner.callback(callback),
            _phantom: PhantomData,
        }
    }

    pub fn callback_mut<F>(
        self,
        callback: F,
    ) -> TypedQueryableWithHandlerBuilder<'a, 'b, Req, Resp, Callback<Query>>
    where
        F: FnMut(Query) + Send + Sync + 'static,
    {
        TypedQueryableWithHandlerBuilder {
            inner: self.inner.callback_mut(callback),
            _phantom: PhantomData,
        }
    }

    pub fn complete(self, complete: bool) -> Self {
        Self {
            inner: self.inner.complete(complete),
            _phantom: self._phantom,
        }
    }

    pub fn allowed_origin(self, origin: Locality) -> Self {
        Self {
            inner: self.inner.allowed_origin(origin),
            _phantom: self._phantom,
        }
    }

    fn build(self) -> ZResult<TypedQueryable<Req, Resp>> {
        let inner = self.inner.wait()?;
        Ok(TypedQueryable {
            inner,
            _phantom: PhantomData,
        })
    }
}

impl<Req: DeserializeOwned + TypedSchema, Resp: Serialize + TypedSchema> IntoFuture
    for TypedQueryableBuilder<'_, '_, Req, Resp>
{
    type Output = ZResult<TypedQueryable<Req, Resp>>;
    type IntoFuture = Ready<Self::Output>;

    fn into_future(self) -> Self::IntoFuture {
        std::future::ready(self.build())
    }
}

pub struct TypedQueryableWithHandlerBuilder<
    'a,
    'b,
    Req: DeserializeOwned + TypedSchema,
    Resp: Serialize + TypedSchema,
    Handler,
> {
    inner: QueryableBuilder<'a, 'b, Handler>,
    _phantom: PhantomData<(Req, Resp)>,
}

impl<'a, 'b, Req: DeserializeOwned + TypedSchema, Resp: Serialize + TypedSchema, Handler>
    TypedQueryableWithHandlerBuilder<'a, 'b, Req, Resp, Handler>
{
    pub fn complete(self, complete: bool) -> Self {
        Self {
            inner: self.inner.complete(complete),
            _phantom: self._phantom,
        }
    }

    pub fn allowed_origin(self, origin: Locality) -> Self {
        Self {
            inner: self.inner.allowed_origin(origin),
            _phantom: self._phantom,
        }
    }
}

impl<Req: DeserializeOwned + TypedSchema, Resp: Serialize + TypedSchema, Handler> IntoFuture
    for TypedQueryableWithHandlerBuilder<'_, '_, Req, Resp, Handler>
where
    Handler: zenoh::handlers::IntoHandler<Query> + Send,
    Handler::Handler: Send,
{
    type Output = ZResult<TypedQueryable<Req, Resp, Handler::Handler>>;
    type IntoFuture = Ready<Self::Output>;

    fn into_future(self) -> Self::IntoFuture {
        std::future::ready(self.inner.wait().map(|inner| TypedQueryable {
            inner,
            _phantom: PhantomData,
        }))
    }
}

/// A future that resolves to a vector of typed reply results.
///
/// Returned by [`TypedSessionExt::typed_get`]. Each element is either a
/// successfully deserialized response or a [`TypedGetError`] indicating
/// either a reply error from the remote or a deserialization failure.
pub struct TypedGetFuture<Resp: DeserializeOwned + TypedSchema> {
    result: ZResult<Vec<Result<Resp, TypedGetError>>>,
}

impl<Resp: DeserializeOwned + TypedSchema> std::future::IntoFuture for TypedGetFuture<Resp> {
    type Output = ZResult<Vec<Result<Resp, TypedGetError>>>;
    type IntoFuture = Ready<Self::Output>;

    fn into_future(self) -> Self::IntoFuture {
        std::future::ready(self.result)
    }
}

/// Extension trait for [`Session`] to declare typed publishers, subscribers, and queryables.
pub trait TypedSessionExt {
    /// Declare a [`TypedPublisher`] for the given key expression.
    fn declare_typed_publisher<'b, T: Serialize + TypedSchema, TryIntoKeyExpr>(
        &self,
        key_expr: TryIntoKeyExpr,
    ) -> TypedPublisherBuilder<'_, 'b, T>
    where
        TryIntoKeyExpr: TryInto<KeyExpr<'b>>,
        <TryIntoKeyExpr as TryInto<KeyExpr<'b>>>::Error: Into<Error>;

    /// Declare a [`TypedSubscriber`] for the given key expression.
    fn declare_typed_subscriber<'b, T: DeserializeOwned + TypedSchema, TryIntoKeyExpr>(
        &self,
        key_expr: TryIntoKeyExpr,
    ) -> TypedSubscriberBuilder<'_, 'b, T>
    where
        TryIntoKeyExpr: TryInto<KeyExpr<'b>>,
        <TryIntoKeyExpr as TryInto<KeyExpr<'b>>>::Error: Into<Error>;

    /// Declare a [`TypedQueryable`] for the given key expression.
    fn declare_typed_queryable<
        'b,
        Req: DeserializeOwned + TypedSchema,
        Resp: Serialize + TypedSchema,
        TryIntoKeyExpr,
    >(
        &self,
        key_expr: TryIntoKeyExpr,
    ) -> TypedQueryableBuilder<'_, 'b, Req, Resp>
    where
        TryIntoKeyExpr: TryInto<KeyExpr<'b>>,
        <TryIntoKeyExpr as TryInto<KeyExpr<'b>>>::Error: Into<Error>;

    fn typed_get_builder<
        'b,
        Req: Serialize + TypedSchema,
        Resp: DeserializeOwned + TypedSchema,
        TryIntoKeyExpr,
    >(
        &self,
        key_expr: TryIntoKeyExpr,
        request: &Req,
    ) -> TypedGetBuilder<'_, 'b, Resp>
    where
        TryIntoKeyExpr: TryInto<KeyExpr<'b>>,
        <TryIntoKeyExpr as TryInto<KeyExpr<'b>>>::Error: Into<Error>;

    /// Send a typed get (query) and collect typed replies.
    ///
    /// Serializes `Req` into the query payload, sends the query, and
    /// deserializes each reply into `Resp`.
    fn typed_get<
        'b,
        Req: Serialize + TypedSchema,
        Resp: DeserializeOwned + TypedSchema,
        TryIntoKeyExpr,
    >(
        &self,
        key_expr: TryIntoKeyExpr,
        request: &Req,
    ) -> TypedGetFuture<Resp>
    where
        TryIntoKeyExpr: TryInto<KeyExpr<'b>>,
        <TryIntoKeyExpr as TryInto<KeyExpr<'b>>>::Error: Into<Error>;
}

pub struct TypedGetBuilder<'a, 'b, Resp: DeserializeOwned + TypedSchema> {
    session: &'a Session,
    key_expr: ZResult<KeyExpr<'b>>,
    payload: ZResult<ZBytes>,
    encoding: Encoding,
    schema_version: u32,
    target: QueryTarget,
    consolidation: QueryConsolidation,
    destination: Locality,
    timeout: Option<Duration>,
    accept_replies: ReplyKeyExpr,
    _phantom: PhantomData<Resp>,
}

impl<'a, 'b, Resp: DeserializeOwned + TypedSchema> TypedGetBuilder<'a, 'b, Resp> {
    pub fn target(self, target: QueryTarget) -> Self {
        Self { target, ..self }
    }

    pub fn consolidation<QC: Into<QueryConsolidation>>(self, consolidation: QC) -> Self {
        Self {
            consolidation: consolidation.into(),
            ..self
        }
    }

    pub fn allowed_destination(self, destination: Locality) -> Self {
        Self {
            destination,
            ..self
        }
    }

    pub fn timeout(self, timeout: Duration) -> Self {
        Self {
            timeout: Some(timeout),
            ..self
        }
    }

    pub fn accept_replies(self, accept: ReplyKeyExpr) -> Self {
        Self {
            accept_replies: accept,
            ..self
        }
    }
}

impl<Resp: DeserializeOwned + TypedSchema> IntoFuture for TypedGetBuilder<'_, '_, Resp> {
    type Output = ZResult<Vec<Result<Resp, TypedGetError>>>;
    type IntoFuture = Ready<Self::Output>;

    fn into_future(self) -> Self::IntoFuture {
        let result = (|| -> ZResult<Vec<Result<Resp, TypedGetError>>> {
            let key_expr = self.key_expr?;
            let payload = self.payload?;
            let receiver = self
                .session
                .get(key_expr)
                .payload(payload)
                .encoding(self.encoding)
                .attachment(encode_schema_version(self.schema_version))
                .target(self.target)
                .consolidation(self.consolidation)
                .allowed_destination(self.destination)
                .accept_replies(self.accept_replies);
            let receiver = if let Some(timeout) = self.timeout {
                receiver.timeout(timeout).wait()?
            } else {
                receiver.wait()?
            };
            let mut replies = Vec::new();
            while let Ok(reply) = receiver.recv() {
                match reply.into_result() {
                    Ok(sample) => {
                        let result = (|| -> Result<Resp, TypedPayloadError> {
                            check_encoding(Some(sample.encoding()), Resp::SCHEMA_NAME)?;
                            check_schema_version(
                                sample.attachment(),
                                Some(Resp::SCHEMA_VERSION),
                                Resp::SCHEMA_NAME,
                            )?;
                            deserialize_payload::<Resp>(sample.payload())
                                .map_err(TypedPayloadError::from)
                        })();
                        replies.push(result.map_err(TypedGetError::from));
                    }
                    Err(reply_err) => {
                        replies.push(Err(TypedGetError::ReplyError(reply_err.payload().clone())));
                    }
                }
            }
            Ok(replies)
        })();
        std::future::ready(result)
    }
}

impl TypedSessionExt for Session {
    fn declare_typed_publisher<'b, T: Serialize + TypedSchema, TryIntoKeyExpr>(
        &self,
        key_expr: TryIntoKeyExpr,
    ) -> TypedPublisherBuilder<'_, 'b, T>
    where
        TryIntoKeyExpr: TryInto<KeyExpr<'b>>,
        <TryIntoKeyExpr as TryInto<KeyExpr<'b>>>::Error: Into<Error>,
    {
        TypedPublisherBuilder {
            inner: self
                .declare_publisher(key_expr)
                .encoding(typed_encoding(T::SCHEMA_NAME)),
            schema_version: None,
            publish_policy: PublishPolicy::Lazy,
            _phantom: PhantomData,
        }
    }

    fn declare_typed_subscriber<'b, T: DeserializeOwned + TypedSchema, TryIntoKeyExpr>(
        &self,
        key_expr: TryIntoKeyExpr,
    ) -> TypedSubscriberBuilder<'_, 'b, T>
    where
        TryIntoKeyExpr: TryInto<KeyExpr<'b>>,
        <TryIntoKeyExpr as TryInto<KeyExpr<'b>>>::Error: Into<Error>,
    {
        TypedSubscriberBuilder {
            inner: self.declare_subscriber(key_expr),
            schema_version: None,
            _phantom: PhantomData,
        }
    }

    fn declare_typed_queryable<
        'b,
        Req: DeserializeOwned + TypedSchema,
        Resp: Serialize + TypedSchema,
        TryIntoKeyExpr,
    >(
        &self,
        key_expr: TryIntoKeyExpr,
    ) -> TypedQueryableBuilder<'_, 'b, Req, Resp>
    where
        TryIntoKeyExpr: TryInto<KeyExpr<'b>>,
        <TryIntoKeyExpr as TryInto<KeyExpr<'b>>>::Error: Into<Error>,
    {
        TypedQueryableBuilder {
            inner: self.declare_queryable(key_expr),
            _phantom: PhantomData,
        }
    }

    fn typed_get_builder<
        'b,
        Req: Serialize + TypedSchema,
        Resp: DeserializeOwned + TypedSchema,
        TryIntoKeyExpr,
    >(
        &self,
        key_expr: TryIntoKeyExpr,
        request: &Req,
    ) -> TypedGetBuilder<'_, 'b, Resp>
    where
        TryIntoKeyExpr: TryInto<KeyExpr<'b>>,
        <TryIntoKeyExpr as TryInto<KeyExpr<'b>>>::Error: Into<Error>,
    {
        TypedGetBuilder {
            session: self,
            key_expr: key_expr.try_into().map_err(Into::into),
            payload: serialize_payload(request),
            encoding: typed_encoding(Req::SCHEMA_NAME),
            schema_version: Req::SCHEMA_VERSION,
            target: QueryTarget::DEFAULT,
            consolidation: QueryConsolidation::DEFAULT,
            destination: Locality::default(),
            timeout: None,
            accept_replies: ReplyKeyExpr::MatchingQuery,
            _phantom: PhantomData,
        }
    }

    fn typed_get<
        'b,
        Req: Serialize + TypedSchema,
        Resp: DeserializeOwned + TypedSchema,
        TryIntoKeyExpr,
    >(
        &self,
        key_expr: TryIntoKeyExpr,
        request: &Req,
    ) -> TypedGetFuture<Resp>
    where
        TryIntoKeyExpr: TryInto<KeyExpr<'b>>,
        <TryIntoKeyExpr as TryInto<KeyExpr<'b>>>::Error: Into<Error>,
    {
        let result = (|| -> ZResult<Vec<Result<Resp, TypedGetError>>> {
            let builder = self.typed_get_builder::<Req, Resp, TryIntoKeyExpr>(key_expr, request);
            let key_expr = builder.key_expr?;
            let payload = builder.payload?;
            let receiver = builder
                .session
                .get(key_expr)
                .payload(payload)
                .encoding(builder.encoding)
                .attachment(encode_schema_version(builder.schema_version))
                .target(builder.target)
                .consolidation(builder.consolidation)
                .allowed_destination(builder.destination)
                .accept_replies(builder.accept_replies);
            let receiver = if let Some(timeout) = builder.timeout {
                receiver.timeout(timeout).wait()?
            } else {
                receiver.wait()?
            };
            let mut replies = Vec::new();
            while let Ok(reply) = receiver.recv() {
                match reply.into_result() {
                    Ok(sample) => {
                        let result = (|| -> Result<Resp, TypedPayloadError> {
                            check_encoding(Some(sample.encoding()), Resp::SCHEMA_NAME)?;
                            check_schema_version(
                                sample.attachment(),
                                Some(Resp::SCHEMA_VERSION),
                                Resp::SCHEMA_NAME,
                            )?;
                            deserialize_payload::<Resp>(sample.payload())
                                .map_err(TypedPayloadError::from)
                        })();
                        replies.push(result.map_err(TypedGetError::from));
                    }
                    Err(reply_err) => {
                        replies.push(Err(TypedGetError::ReplyError(reply_err.payload().clone())));
                    }
                }
            }
            Ok(replies)
        })();
        TypedGetFuture { result }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use serial_test::serial;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use zenoh::key_expr::KeyExpr;
    use zenoh::sample::SampleBuilder;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct TestPayload {
        value: u32,
    }

    impl TypedSchema for TestPayload {
        const SCHEMA_NAME: &'static str = "typed-publisher-test-payload";
        const SCHEMA_VERSION: u32 = 1;
    }

    struct PanicPayload;

    impl Serialize for PanicPayload {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            panic!("payload should not be serialized")
        }
    }

    impl TypedSchema for PanicPayload {
        const SCHEMA_NAME: &'static str = "typed-publisher-panic-payload";
        const SCHEMA_VERSION: u32 = 1;
    }

    fn unique_topic(suffix: &str) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("phoxal/tests/{suffix}/{nanos}")
    }

    async fn open_session() -> zenoh::Session {
        zenoh::open(zenoh::Config::default())
            .await
            .expect("test zenoh session should open")
    }

    fn make_sample_with_encoding(encoding: &str) -> Sample {
        let payload = serialize_payload(&42u32).unwrap();
        let key: KeyExpr<'static> = KeyExpr::try_from("test/key").unwrap();
        SampleBuilder::put(key, payload)
            .encoding(Encoding::from(encoding))
            .into()
    }

    fn make_sample_default_encoding() -> Sample {
        let payload = serialize_payload(&42u32).unwrap();
        let key: KeyExpr<'static> = KeyExpr::try_from("test/key").unwrap();
        SampleBuilder::put(key, payload).into()
    }

    #[test]
    fn check_encoding_accepts_correct_typed_encoding() {
        let sample = make_sample_with_encoding("zenoh-ext/typed:test-payload");
        assert!(check_encoding(Some(sample.encoding()), "test-payload").is_ok());
    }

    #[test]
    fn check_encoding_rejects_wrong_typed_encoding() {
        let sample = make_sample_with_encoding("zenoh-ext/typed:other-type");
        let result = check_encoding(Some(sample.encoding()), "test-payload");
        assert!(result.is_err());
        match result.unwrap_err() {
            TypedPayloadError::EncodingMismatch { expected, received } => {
                assert!(
                    expected.contains("test-payload"),
                    "expected should contain schema name, got: {expected}"
                );
                assert!(
                    received.contains("other-type"),
                    "received should contain actual schema name, got: {received}"
                );
            }
            other => panic!("expected EncodingMismatch, got: {other:?}"),
        }
    }

    #[test]
    fn check_encoding_rejects_untyped_encoding() {
        let sample = make_sample_with_encoding("application/json");
        let result = check_encoding(Some(sample.encoding()), "test-payload");
        assert!(result.is_err());
        match result.unwrap_err() {
            TypedPayloadError::EncodingMismatch { expected, received } => {
                assert!(
                    expected.contains("test-payload"),
                    "expected should reference schema name, got: {expected}"
                );
                assert!(
                    received.contains("application/json"),
                    "received should show actual encoding, got: {received}"
                );
            }
            other => panic!("expected EncodingMismatch, got: {other:?}"),
        }
    }

    #[test]
    fn check_encoding_rejects_default_encoding() {
        let sample = make_sample_default_encoding();
        let result = check_encoding(Some(sample.encoding()), "test-payload");
        assert!(
            result.is_err(),
            "default encoding should not pass typed encoding check"
        );
        assert!(
            matches!(
                result.unwrap_err(),
                TypedPayloadError::EncodingMismatch { .. }
            ),
            "should be EncodingMismatch variant"
        );
    }

    #[test]
    fn encoding_mismatch_display_distinguishes_from_deserialization_error() {
        let encoding_err = TypedPayloadError::EncodingMismatch {
            expected: "zenoh-ext/typed:foo".to_string(),
            received: "application/json".to_string(),
        };
        let deser_err = TypedPayloadError::DeserializationFailed(
            deserialize_payload::<u32>(&ZBytes::from(Vec::<u8>::new())).unwrap_err(),
        );

        let encoding_msg = encoding_err.to_string();
        let deser_msg = deser_err.to_string();

        assert!(
            encoding_msg.contains("encoding mismatch"),
            "encoding error should say 'encoding mismatch', got: {encoding_msg}"
        );
        assert!(
            deser_msg.contains("deserialization"),
            "deser error should mention 'deserialization', got: {deser_msg}"
        );
        assert_ne!(
            encoding_msg, deser_msg,
            "error messages should be distinguishable"
        );
    }

    #[test]
    fn strict_schema_version_requires_attachment() {
        let sample = make_sample_with_encoding("zenoh-ext/typed:test-payload");
        let result = check_schema_version(sample.attachment(), Some(1), "test-payload");
        assert!(matches!(
            result,
            Err(TypedPayloadError::MissingSchemaVersion { expected: 1, .. })
        ));
    }

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn typed_publishers_default_to_lazy_policy() {
        let session = open_session().await;
        let topic = unique_topic("lazy-default");
        let publisher = session
            .declare_typed_publisher::<TestPayload, _>(topic)
            .await
            .expect("publisher should build");

        assert_eq!(publisher.publish_policy(), PublishPolicy::Lazy);

        drop(publisher);
        session.close().await.expect("session should close");
    }

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn lazy_publisher_skips_serialization_when_unmatched() {
        let session = open_session().await;
        let topic = unique_topic("lazy-unmatched");
        let publisher = session
            .declare_typed_publisher::<PanicPayload, _>(topic)
            .await
            .expect("publisher should build");

        publisher
            .put(&PanicPayload)
            .await
            .expect("unmatched lazy publisher should short-circuit");

        drop(publisher);
        session.close().await.expect("session should close");
    }

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn eager_publisher_reports_publishable_without_subscribers() {
        let session = open_session().await;
        let topic = unique_topic("eager-unmatched");
        let publisher = session
            .declare_typed_publisher::<TestPayload, _>(topic)
            .eager()
            .await
            .expect("publisher should build");

        assert_eq!(publisher.publish_policy(), PublishPolicy::Eager);
        assert!(
            publisher
                .should_publish()
                .await
                .expect("matching status should resolve")
        );

        drop(publisher);
        session.close().await.expect("session should close");
    }

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn lazy_publisher_publishes_once_subscriber_matches() {
        let publisher_session = open_session().await;
        let subscriber_session = open_session().await;
        let topic = unique_topic("lazy-matched");
        let publisher = publisher_session
            .declare_typed_publisher::<TestPayload, _>(&topic)
            .await
            .expect("publisher should build");
        let subscriber = subscriber_session
            .declare_typed_subscriber::<TestPayload, _>(&topic)
            .await
            .expect("subscriber should build");

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if publisher
                    .has_matching_subscribers()
                    .await
                    .expect("matching status should resolve")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("publisher should observe subscriber match");

        publisher
            .put(&TestPayload { value: 7 })
            .await
            .expect("matched lazy publisher should publish");

        let received = tokio::time::timeout(Duration::from_secs(5), subscriber.recv_async())
            .await
            .expect("subscriber should receive payload")
            .expect("subscriber channel should stay open")
            .expect("payload should decode");
        assert_eq!(received, TestPayload { value: 7 });

        drop(subscriber);
        drop(publisher);
        publisher_session
            .close()
            .await
            .expect("publisher session should close");
        subscriber_session
            .close()
            .await
            .expect("subscriber session should close");
    }
}
