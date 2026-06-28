//! Body-typed handles over the `bus_abi` boundary (D35).
//!
//! - [`Publisher<B>`] — MessagePack-encodes the plain body and enqueues it on the
//!   non-blocking outbound queue (a publish never blocks the step loop).
//! - [`Subscriber<B>`] — a drop-oldest ring (depth 32) of decoded bodies.
//! - [`Latest<B>`] — keep-last-1: the most recent decoded body.
//!
//! All three fast-reject on the metadata `api_version` before decoding the body;
//! a mismatch is counted + logged as a health signal, never a silent accept.

use std::collections::VecDeque;
use std::marker::PhantomData;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use arc_swap::ArcSwapOption;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use zenoh::bytes::Encoding;
use zenoh::key_expr::OwnedKeyExpr;
use zenoh::sample::Sample;

use crate::api::{ApiVersion, ContractBody};
use crate::bus::LogicalTime;
use crate::bus::abi::{CodecId, encoding_string};
use crate::bus::codec::{Codec, MessagePack};
use crate::bus::error::{BusError, Result};
use crate::bus::metadata::{BusMetadata, Source};
use crate::bus::query::QueryError;
use crate::bus::session::Bus;
use crate::bus::topic::{PubSub, Query, Topic};

/// The Phoxal-pinned finite query timeout (D31) — not Zenoh's 10 s default.
pub const DEFAULT_QUERY_TIMEOUT: Duration = Duration::from_secs(5);

/// Publishes plain bodies of `B` on a versionless key; metadata carries the
/// version identity. A publish is a non-blocking enqueue (D35/D43e).
pub struct Publisher<B> {
    bus: Bus,
    key: String,
    _body: PhantomData<fn() -> B>,
}

impl<B: ContractBody> Publisher<B> {
    pub(crate) fn new(bus: Bus, topic: &Topic<PubSub<B>>) -> Result<Self> {
        let key = bus.full_key(topic.publish_key()?);
        Ok(Publisher {
            bus,
            key,
            _body: PhantomData,
        })
    }

    /// Publish `body` stamped at logical time `at`. Non-blocking: the body is
    /// enqueued on the runner-owned outbound queue and never blocks the caller.
    #[allow(clippy::unused_async)]
    pub async fn publish_at(&self, at: LogicalTime, body: B) -> Result<()> {
        self.try_publish(at, body)
    }

    /// The explicit non-blocking publish op (D43e). Returns immediately; a full
    /// outbound queue drops the sample + bumps the drop counter.
    pub fn try_publish(&self, at: LogicalTime, body: B) -> Result<()> {
        let payload = MessagePack::encode(&body)?;
        let api_version = <B::Api as ApiVersion>::ID;
        let metadata = BusMetadata {
            api_version: api_version.to_string(),
            family: B::FAMILY.to_string(),
            codec: MessagePack::ID.as_u8(),
            produced_at_ns: at.time_ns(),
            epoch: at.epoch(),
            source: Source {
                participant: self.bus.participant().to_string(),
                incarnation: self.bus.incarnation(),
                sequence: self.bus.next_sequence(),
            },
        };
        let encoding = encoding_string(B::FAMILY, api_version, MessagePack::ID);
        self.bus
            .enqueue(self.key.clone(), encoding, metadata.encode(), payload);
        Ok(())
    }
}

/// Issues queries on a query topic and returns `Result<Resp, QueryError>` (D31).
///
/// Carries a Phoxal-pinned finite timeout. A success reply is the plain `Resp`
/// body; a handler error rides Zenoh's `ReplyError` as a `QueryFailure`. More
/// than one responder on the topic is reported as `TooManyResponders`.
pub struct Querier<Req, Resp> {
    bus: Bus,
    key: String,
    timeout: Duration,
    _p: PhantomData<fn() -> (Req, Resp)>,
}

impl<Req, Resp> Querier<Req, Resp>
where
    Req: ContractBody,
    Resp: ContractBody,
{
    pub(crate) fn new(
        bus: Bus,
        topic: &Topic<Query<Req, Resp>>,
        timeout: Duration,
    ) -> Result<Self> {
        let key = bus.full_key(topic.publish_key()?);
        Ok(Querier {
            bus,
            key,
            timeout,
            _p: PhantomData,
        })
    }

    /// Issue a query and await the single response (or a typed error).
    pub async fn query(&self, request: Req) -> std::result::Result<Resp, QueryError> {
        let payload =
            MessagePack::encode(&request).map_err(|e| QueryError::Protocol(e.to_string()))?;
        let api_version = <Req::Api as ApiVersion>::ID;
        let metadata = BusMetadata {
            api_version: api_version.to_string(),
            family: Req::FAMILY.to_string(),
            codec: MessagePack::ID.as_u8(),
            produced_at_ns: 0,
            epoch: 0,
            source: Source {
                participant: self.bus.participant().to_string(),
                incarnation: self.bus.incarnation(),
                sequence: self.bus.next_sequence(),
            },
        };
        let key = OwnedKeyExpr::new(self.key.clone())
            .map_err(|e| QueryError::Protocol(format!("invalid query key '{}': {e}", self.key)))?;

        let replies = self
            .bus
            .session()
            .get(key)
            .payload(payload)
            .encoding(Encoding::from(encoding_string(
                Req::FAMILY,
                api_version,
                MessagePack::ID,
            )))
            .attachment(metadata.encode())
            .timeout(self.timeout)
            .await
            .map_err(|e| QueryError::Protocol(e.to_string()))?;

        // An exclusive query topic has a single responder: the first reply wins.
        // Duplicate responders are caught at build time by the `phoxal-cli check`
        // topology pass (D63), not by waiting at runtime.
        if let Ok(reply) = replies.recv_async().await {
            return match reply.into_result() {
                Ok(sample) => decode_reply::<Resp>(&sample),
                Err(reply_error) => {
                    let bytes = reply_error.payload().to_bytes();
                    match crate::bus::query::QueryFailure::decode(bytes.as_ref()) {
                        Ok(failure) => Err(QueryError::Server(failure)),
                        Err(e) => Err(QueryError::Protocol(format!("malformed error reply: {e}"))),
                    }
                }
            };
        }

        Err(QueryError::Unavailable)
    }
}

fn decode_reply<Resp: ContractBody>(sample: &Sample) -> std::result::Result<Resp, QueryError> {
    match decode_sample::<Resp>(sample, Resp::TOPIC, <Resp::Api as ApiVersion>::ID) {
        Ok((body, _)) => Ok(body),
        Err(e) => Err(QueryError::Decode(e.to_string())),
    }
}

/// A decoded inbound sample: the body plus its metadata.
#[derive(Clone, Debug)]
pub struct Received<B> {
    /// The decoded wire body.
    pub body: B,
    /// The sample's bus metadata.
    pub metadata: BusMetadata,
}

/// Keep-last-1 view of a topic: the most recently received decoded body.
pub struct Latest<B> {
    slot: Arc<ArcSwapOption<B>>,
    _guard: SubscriptionGuard,
}

impl<B: ContractBody> Latest<B> {
    pub(crate) async fn new(bus: &Bus, topic: &Topic<PubSub<B>>) -> Result<Self> {
        let slot: Arc<ArcSwapOption<B>> = Arc::new(ArcSwapOption::from(None));
        let store = Arc::clone(&slot);
        let guard = spawn_subscription::<B, _>(bus, topic.key(), move |body, _meta| {
            store.store(Some(Arc::new(body)));
        })
        .await?;
        Ok(Latest {
            slot,
            _guard: guard,
        })
    }

    /// The most recent decoded body, or `None` if nothing has arrived yet.
    pub fn latest(&self) -> Option<B> {
        self.slot.load_full().map(|arc| (*arc).clone())
    }
}

/// A drop-oldest ring subscription (depth 32 by default) of decoded bodies.
pub struct Subscriber<B> {
    ring: Arc<Ring<B>>,
    _guard: SubscriptionGuard,
}

impl<B: ContractBody> Subscriber<B> {
    pub(crate) async fn new(bus: &Bus, topic: &Topic<PubSub<B>>, depth: usize) -> Result<Self> {
        let ring = Arc::new(Ring::new(depth.max(1)));
        let push = Arc::clone(&ring);
        let drops = bus.clone();
        let guard = spawn_subscription::<B, _>(bus, topic.key(), move |body, metadata| {
            if push.push(Received { body, metadata }) {
                drops.health().inbound_drops.fetch_add(1, Ordering::Relaxed);
            }
        })
        .await?;
        Ok(Subscriber {
            ring,
            _guard: guard,
        })
    }

    /// Await the next decoded body (drop-oldest under congestion).
    pub async fn recv(&self) -> Result<Received<B>> {
        self.ring.recv().await
    }

    /// Take the next decoded body if one is buffered, without awaiting.
    pub fn try_recv(&self) -> Option<Received<B>> {
        self.ring.try_pop()
    }
}

struct Ring<B> {
    buf: Mutex<VecDeque<Received<B>>>,
    notify: Notify,
    cap: usize,
}

impl<B> Ring<B> {
    fn new(cap: usize) -> Self {
        Ring {
            buf: Mutex::new(VecDeque::with_capacity(cap)),
            notify: Notify::new(),
            cap,
        }
    }

    /// Push, dropping the oldest if full. Returns `true` if a drop occurred.
    fn push(&self, item: Received<B>) -> bool {
        let mut dropped = false;
        {
            let mut buf = self.buf.lock().expect("ring mutex poisoned");
            if buf.len() == self.cap {
                buf.pop_front();
                dropped = true;
            }
            buf.push_back(item);
        }
        self.notify.notify_one();
        dropped
    }

    fn try_pop(&self) -> Option<Received<B>> {
        self.buf.lock().expect("ring mutex poisoned").pop_front()
    }

    async fn recv(&self) -> Result<Received<B>> {
        loop {
            // Register the waiter *before* checking, so a push between the check
            // and the await is not missed (tokio::sync::Notify semantics).
            let notified = self.notify.notified();
            // Hold the std mutex only to pop; never across the await below.
            if let Some(item) = self.buf.lock().expect("ring mutex poisoned").pop_front() {
                return Ok(item);
            }
            notified.await;
        }
    }
}

/// Keeps a subscription's background task alive; aborts it on drop.
struct SubscriptionGuard {
    task: JoinHandle<()>,
}

impl Drop for SubscriptionGuard {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Declare a Zenoh subscriber on `topic_key` (under the bus root) and spawn a
/// task that decodes each sample and feeds it to `on_sample`. Decode failures and
/// `api_version` mismatches are counted + logged, never silently accepted.
async fn spawn_subscription<B, F>(
    bus: &Bus,
    topic_key: &str,
    mut on_sample: F,
) -> Result<SubscriptionGuard>
where
    B: ContractBody,
    F: FnMut(B, BusMetadata) + Send + 'static,
{
    let full_key = bus.full_key(topic_key);
    let key_expr = OwnedKeyExpr::new(full_key.clone())
        .map_err(|e| BusError::Namespace(format!("invalid subscribe key '{full_key}': {e}")))?;
    let subscriber = bus
        .session()
        .declare_subscriber(key_expr)
        .await
        .map_err(|e| BusError::Transport(e.to_string()))?;

    let expected_api = <B::Api as ApiVersion>::ID;
    let topic_owned = topic_key.to_string();
    let health_bus = bus.clone();

    let task = tokio::spawn(async move {
        while let Ok(sample) = subscriber.recv_async().await {
            match decode_sample::<B>(&sample, &topic_owned, expected_api) {
                Ok((body, metadata)) => on_sample(body, metadata),
                Err(err) => {
                    match &err {
                        BusError::ApiVersionMismatch { .. } => {
                            health_bus
                                .health()
                                .api_mismatches
                                .fetch_add(1, Ordering::Relaxed);
                        }
                        _ => {
                            health_bus
                                .health()
                                .decode_errors
                                .fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    tracing::warn!(target: "phoxal.bus", topic = %topic_owned, error = %err, "dropped inbound sample");
                }
            }
        }
    });

    Ok(SubscriptionGuard { task })
}

/// Decode one Zenoh sample into a body of `B`, fast-rejecting on the metadata
/// `api_version` and codec before touching the payload.
pub(crate) fn decode_sample<B: ContractBody>(
    sample: &Sample,
    topic: &str,
    expected_api: &str,
) -> Result<(B, BusMetadata)> {
    let attachment = sample.attachment().ok_or_else(|| BusError::Metadata {
        topic: topic.to_string(),
        detail: "missing BusMetadata attachment".to_string(),
    })?;
    let metadata =
        BusMetadata::decode(attachment.to_bytes().as_ref()).map_err(|e| BusError::Metadata {
            topic: topic.to_string(),
            detail: format!("malformed BusMetadata: {e}"),
        })?;

    if metadata.api_version != expected_api {
        return Err(BusError::ApiVersionMismatch {
            topic: topic.to_string(),
            expected: expected_api.to_string(),
            received: metadata.api_version,
        });
    }

    match metadata.codec_id() {
        Some(CodecId::MessagePack) => {}
        None => {
            return Err(BusError::UnsupportedCodec(
                metadata.codec,
                topic.to_string(),
            ));
        }
    }

    let body = MessagePack::decode::<B>(sample.payload().to_bytes().as_ref())?;
    Ok((body, metadata))
}
