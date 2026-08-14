//! The caller side of the request/response leg.

use std::marker::PhantomData;
use std::time::Duration;

use zenoh::bytes::Encoding;
use zenoh::key_expr::OwnedKeyExpr;
use zenoh::sample::Sample;

use crate::abi::{Codec, MessagePack};
use crate::contract::{Payload, QueryEndpointDescriptor};
use crate::error::Result;
use crate::handle::decode_payload;
use crate::query::{QueryError, QueryFailure};
use crate::session::BusHandle;
use crate::topic::{AskQuery, Topic};

/// The Phoxal-pinned finite query timeout - not Zenoh's 10 s default.
pub const DEFAULT_QUERY_TIMEOUT: Duration = Duration::from_secs(5);

/// Meaning: asks one request of the endpoint owner and expects exactly one response.
///
/// Timestamp: requests express no robot time.
///
/// Buffering and admission: each call has a finite, Phoxal-pinned
/// [`timeout`](DEFAULT_QUERY_TIMEOUT), not Zenoh's 10 s default, and no
/// endpoint-local backlog is exposed.
///
/// Observable overload or loss: every incomplete or ambiguous outcome is a
/// typed `QueryError`:
///
/// - a success reply decodes to the plain `Resp` body;
/// - a handler error rides Zenoh's native `ReplyError` and surfaces as
///   [`QueryError::Server`] carrying the [`QueryFailure`];
/// - the deadline elapsing with no reply is [`QueryError::Timeout`], and the
///   reply stream closing with no reply is [`QueryError::Unavailable`];
/// - a second reply (a duplicate responder, also a launch-topology error) is
///   [`QueryError::TooManyResponders`].
pub struct Querier<Req, Resp> {
    bus: BusHandle,
    key: String,
    topic: &'static str,
    timeout: Duration,
    _p: PhantomData<fn() -> (Req, Resp)>,
}

// Manual, unbounded on `Req`/`Resp` - see `Outbox`'s `Clone` impl docs for
// why (identical reasoning: `query` takes `&self`, so a clone is just a
// second handle to the same query key).
impl<Req, Resp> Clone for Querier<Req, Resp> {
    fn clone(&self) -> Self {
        Querier {
            bus: self.bus.clone(),
            key: self.key.clone(),
            topic: self.topic,
            timeout: self.timeout,
            _p: PhantomData,
        }
    }
}

impl<Req: Payload, Resp: Payload> Querier<Req, Resp> {
    /// Build a querier over a query topic.
    ///
    /// The author-facing path is `ctx.querier(...)` in `Participant::setup`.
    /// `pub` only because the generated api tree and the runner live in other
    /// crates; see [`crate::handle::stamp`]'s module docs.
    #[doc(hidden)]
    pub fn new<E>(bus: BusHandle, topic: &Topic<AskQuery<E>>, timeout: Duration) -> Result<Self>
    where
        E: QueryEndpointDescriptor<Request = Req, Response = Resp>,
    {
        let key = bus.full_key(topic.publish_key()?);
        Ok(Querier {
            bus,
            key,
            topic: E::TOPIC,
            timeout,
            _p: PhantomData,
        })
    }

    /// Issue a query and await the single response (or a typed error).
    ///
    /// The request body is MessagePack-encoded with mirroring provenance; a
    /// request expresses no robot time, so `produced_at` is `None`. The wait is
    /// bounded by this querier's timeout.
    pub async fn query(&self, request: Req) -> std::result::Result<Resp, QueryError> {
        let payload =
            MessagePack::encode(&request).map_err(|e| QueryError::Protocol(e.to_string()))?;
        let metadata = self
            .bus
            .metadata(None)
            .map_err(|e| QueryError::Protocol(e.to_string()))?;
        let attachment = metadata
            .encode()
            .map_err(|e| QueryError::Protocol(format!("failed to encode bus metadata: {e}")))?;
        let key = OwnedKeyExpr::new(self.key.clone())
            .map_err(|e| QueryError::Protocol(format!("invalid query key '{}': {e}", self.key)))?;

        // Keep the admission lease named for the whole query, including the
        // reply receive loop. A temporary chained expression would release it
        // as soon as `get().await` returned, allowing close to race the reply
        // wait without tracking the in-flight operation.
        let session = self
            .bus
            .session()
            .map_err(|error| QueryError::Protocol(error.to_string()))?;
        let replies = session
            .get(key)
            .payload(payload)
            .encoding(Encoding::from(MessagePack::ID.encoding_string()))
            .attachment(attachment)
            // Target ALL matching responders (not just BestMatching) and do not
            // consolidate, so a duplicate responder on an exclusive topic surfaces
            // as a second reply (→ `TooManyResponders`) rather than being hidden.
            .target(zenoh::query::QueryTarget::All)
            .consolidation(zenoh::query::ConsolidationMode::None)
            .await
            .map_err(|e| QueryError::Protocol(e.to_string()))?;

        // An exclusive query topic has exactly one responder: collect replies
        // until the stream closes, returning the single reply. A second reply is
        // `TooManyResponders` (a duplicate responder - also a launch-topology
        // error). The Phoxal-pinned finite timeout bounds the wait: deadline with
        // no reply → `Timeout`; the stream closing with no reply → `Unavailable`.
        let deadline = tokio::time::Instant::now() + self.timeout;
        let mut outcome: Option<std::result::Result<Resp, QueryError>> = None;
        loop {
            match tokio::time::timeout_at(deadline, replies.recv_async()).await {
                Ok(Ok(reply)) => {
                    if outcome.is_some() {
                        return Err(QueryError::TooManyResponders);
                    }
                    outcome = Some(decode_reply_result::<Resp>(reply.into_result(), self.topic));
                }
                Ok(Err(_)) => break, // reply stream closed
                Err(_elapsed) => {
                    return outcome.unwrap_or_else(|| {
                        Err(QueryError::Timeout(QueryFailure::deadline_exceeded(
                            "query deadline exceeded",
                        )))
                    });
                }
            }
        }
        outcome.unwrap_or(Err(QueryError::Unavailable))
    }
}

fn decode_reply_result<Resp: Payload>(
    result: std::result::Result<Sample, zenoh::query::ReplyError>,
    topic: &str,
) -> std::result::Result<Resp, QueryError> {
    match result {
        Ok(sample) => decode_payload::<Resp>(&sample, topic)
            .map(|(body, _)| body)
            .map_err(|e| QueryError::Decode(e.to_string())),
        Err(reply_error) => {
            let bytes = reply_error.payload().to_bytes();
            match QueryFailure::decode(bytes.as_ref()) {
                Ok(failure) => Err(QueryError::Server(failure)),
                Err(e) => Err(QueryError::Protocol(format!("malformed error reply: {e}"))),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    use crate::query::QueryCode;
    use crate::session::BusOwner;
    use crate::test_support::{GetEndpoint, GetRequest, GetResponse, participant_config};

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_query_round_trip_ok_then_error() {
        let (owner, bus) = BusOwner::open(participant_config("q")).await.unwrap();
        let server = bus.declare_server("yTEST/asset/get").await.unwrap();
        let server_bus = bus.clone();

        let server_task = tokio::spawn(async move {
            // First query -> a Found response. Scoped so the query is dropped
            // right after replying, letting the complete queryable's reply
            // stream close.
            {
                let incoming = server.recv().await.unwrap();
                let response = GetResponse::Found {
                    bytes: vec![9, 9, 9],
                };
                let payload = rmp_serde::to_vec_named(&response).unwrap();
                incoming.reply(&server_bus, payload).await.unwrap();
            }

            // Second query -> a structured error on the native error leg.
            {
                let incoming = server.recv().await.unwrap();
                incoming
                    .reply_err(&QueryFailure::not_found("no such asset"))
                    .await
                    .unwrap();
            }
        });

        let topic = Topic::<AskQuery<GetEndpoint>>::new_static("yTEST/asset/get");
        let querier =
            Querier::<GetRequest, GetResponse>::new(bus.clone(), &topic, Duration::from_secs(5))
                .unwrap();

        let ok = querier
            .query(GetRequest {
                path: "a".to_string(),
            })
            .await
            .expect("first query should succeed");
        assert!(matches!(ok, GetResponse::Found { .. }));

        let error = querier
            .query(GetRequest {
                path: "b".to_string(),
            })
            .await
            .expect_err("second query should be a server error");
        match error {
            QueryError::Server(failure) => assert_eq!(failure.code, QueryCode::NotFound),
            other => panic!("expected QueryError::Server, got {other:?}"),
        }

        server_task.await.unwrap();
        owner.close().await;
    }

    /// The caller-side deadline is the querier's own, not Zenoh's: a handler
    /// that never answers must not hold the caller for Zenoh's 10 s default.
    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_query_timeout_maps_to_deadline_exceeded() {
        let (owner, bus) = BusOwner::open(participant_config("timeout")).await.unwrap();
        let server = bus.declare_server("yTEST/asset/get").await.unwrap();

        let server_task = tokio::spawn(async move {
            let _incoming = server.recv().await.unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
        });

        let topic = Topic::<AskQuery<GetEndpoint>>::new_static("yTEST/asset/get");
        let querier =
            Querier::<GetRequest, GetResponse>::new(bus.clone(), &topic, Duration::from_millis(20))
                .unwrap();

        let error = querier
            .query(GetRequest {
                path: "slow".to_string(),
            })
            .await
            .expect_err("query should time out");
        match error {
            QueryError::Timeout(failure) => assert_eq!(failure.code, QueryCode::DeadlineExceeded),
            other => panic!("expected QueryError::Timeout, got {other:?}"),
        }

        server_task.await.unwrap();
        owner.close().await;
    }

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn close_tracks_a_query_reply_wait_until_the_query_finishes() {
        let (owner, bus) = BusOwner::open(participant_config("query-close-race"))
            .await
            .unwrap();
        let server = bus.declare_server("yTEST/asset/get").await.unwrap();
        let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
        let server_task = tokio::spawn(async move {
            let _incoming = server.recv().await.unwrap();
            seen_tx.send(()).unwrap();
            std::future::pending::<()>().await;
        });

        let topic = Topic::<AskQuery<GetEndpoint>>::new_static("yTEST/asset/get");
        let querier =
            Querier::<GetRequest, GetResponse>::new(bus.clone(), &topic, Duration::from_secs(5))
                .unwrap();
        let query_task = tokio::spawn(async move {
            querier
                .query(GetRequest {
                    path: "held-open".to_string(),
                })
                .await
        });
        seen_rx.await.expect("the query reached the responder");

        let report = owner.close().await;
        assert!(report.timed_out.iter().any(|timeout| {
            matches!(timeout, crate::BusCloseTimeout::Operations(count) if *count > 0)
        }));
        let _ = query_task.await;
        server_task.abort();
    }
}
