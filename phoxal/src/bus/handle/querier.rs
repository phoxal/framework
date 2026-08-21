//! The caller side of the request/response leg.

use std::marker::PhantomData;
use std::time::Duration;

use zenoh::bytes::Encoding;
use zenoh::key_expr::OwnedKeyExpr;
use zenoh::sample::Sample;

use crate::bus::abi::{Codec, MessagePack};
use crate::bus::contract::{Payload, QueryEndpoint};
use crate::bus::error::Result;
use crate::bus::handle::decode_payload;
use crate::bus::query::{QueryError, QueryFailure};
use crate::bus::session::BusHandle;
use crate::bus::topic::{AskQuery, Topic};

/// The Phoxal-pinned finite query timeout - not Zenoh's 10 s default.
pub const DEFAULT_QUERY_TIMEOUT: Duration = Duration::from_secs(5);

/// How long Zenoh's own query timeout outlasts the caller's deadline.
///
/// Zenoh keeps a query registered until it is answered or its own timeout
/// fires, and cancelling one needs the `unstable` feature this workspace
/// deliberately does not enable. So Zenoh's timeout is what finally
/// unregisters a query, and pinning it to the caller's deadline is what keeps
/// a 5 s contract from lingering for Zenoh's 10 s default.
///
/// It is the caller's deadline *plus* a grace rather than the deadline itself
/// because Zenoh answers its own timeout with an `Err(ReplyError("Timeout"))`
/// reply. A caller that saw that reply would report it as a malformed server
/// error instead of [`QueryError::Timeout`], so the framework's deadline has to
/// be strictly the first of the two to fire.
const ZENOH_QUERY_TIMEOUT_GRACE: Duration = Duration::from_millis(500);

/// Asks one request of the endpoint owner and expects exactly one response.
///
/// Requests carry no robot timestamp. Each call uses the finite
/// [`DEFAULT_QUERY_TIMEOUT`], and timeout, unavailable service, server failure,
/// protocol failure, and duplicate responders are returned as typed
/// [`QueryError`] values.
pub struct Querier<E: QueryEndpoint> {
    bus: BusHandle,
    key: String,
    topic: String,
    timeout: Duration,
    _endpoint: PhantomData<fn() -> E>,
}

// Manual - see `Outbox`'s `Clone` impl docs for why (identical reasoning:
// `query` takes `&self`, so a clone is just a second handle to the same query
// key).
impl<E: QueryEndpoint> Clone for Querier<E> {
    fn clone(&self) -> Self {
        Querier {
            bus: self.bus.clone(),
            key: self.key.clone(),
            topic: self.topic.clone(),
            timeout: self.timeout,
            _endpoint: PhantomData,
        }
    }
}

impl<E: QueryEndpoint> Querier<E> {
    /// Build a querier over a query topic.
    ///
    /// The author-facing path is `ctx.querier(...)` in `Participant::setup`.
    /// `pub` only because the runner and the host SDKs build one directly; see
    /// [`crate::bus::handle::stamp`]'s module docs.
    #[doc(hidden)]
    pub fn new(bus: BusHandle, topic: &Topic<AskQuery<E>>, timeout: Duration) -> Result<Self> {
        let key = bus.full_key(topic.publish_key()?);
        Ok(Querier {
            bus,
            key,
            topic: topic.key().to_owned(),
            timeout,
            _endpoint: PhantomData,
        })
    }

    /// Issue a query and await the single response (or a typed error).
    ///
    /// The request body is MessagePack-encoded with mirroring provenance; a
    /// request expresses no robot time, so `produced_at` is `None`. The wait is
    /// bounded by this querier's timeout.
    pub async fn query(&self, request: E) -> std::result::Result<E::Response, QueryError> {
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

        // Taken before the query is registered, so the caller's deadline is
        // strictly earlier than the Zenoh timeout set from it below.
        let deadline = tokio::time::Instant::now() + self.timeout;

        // Deliver replies through the framework's own channel rather than
        // Zenoh's default FIFO handler. That handler logs `tracing::error!`
        // ("sending on a closed channel") whenever it delivers into a receiver
        // that is already gone - and without the `unstable` feature a query
        // cannot be cancelled, so a delivery with nobody left to receive it is
        // the *expected* end of every query that timed out, that found a
        // duplicate responder, or whose future was aborted. A reply nobody is
        // waiting for is a non-event, not an error.
        //
        // Unbounded because Zenoh calls this from its own runtime: a bounded
        // FIFO blocks that thread when full, and the alternative - dropping on
        // full - would discard a real reply. One query takes one reply per
        // responder plus its final, so the bound this gives up is not one a
        // correct exchange can reach.
        let (replies_tx, mut replies) = tokio::sync::mpsc::unbounded_channel();
        session
            .get(key)
            .payload(payload)
            .encoding(Encoding::from(MessagePack::ID.encoding_string()))
            .attachment(attachment)
            // Target ALL matching responders (not just BestMatching) and do not
            // consolidate, so a duplicate responder on an exclusive topic surfaces
            // as a second reply (→ `TooManyResponders`) rather than being hidden.
            .target(zenoh::query::QueryTarget::All)
            .consolidation(zenoh::query::ConsolidationMode::None)
            // Bound how long Zenoh keeps this query registered past the
            // caller's deadline - see `ZENOH_QUERY_TIMEOUT_GRACE`.
            .timeout(self.timeout + ZENOH_QUERY_TIMEOUT_GRACE)
            .callback(move |reply| {
                // The send failure is deliberately dropped: it means the
                // receiver below has already returned, which is exactly what a
                // late reply, a `ResponseFinal`, or Zenoh's own timeout reply
                // arrives into. Reporting it would turn every ordinary
                // timeout and abort into an ERROR on the operator's terminal.
                let _ = replies_tx.send(reply);
            })
            .await
            .map_err(|e| QueryError::Protocol(e.to_string()))?;

        // An exclusive query topic has exactly one responder: collect replies
        // until the stream closes, returning the single reply. A second reply is
        // `TooManyResponders` (a duplicate responder - also a launch-topology
        // error). The Phoxal-pinned finite timeout bounds the wait: deadline with
        // no reply → `Timeout`; the stream closing with no reply → `Unavailable`.
        let mut outcome: Option<std::result::Result<E::Response, QueryError>> = None;
        loop {
            match tokio::time::timeout_at(deadline, replies.recv()).await {
                Ok(Some(reply)) => {
                    if outcome.is_some() {
                        return Err(QueryError::TooManyResponders);
                    }
                    outcome = Some(decode_reply_result::<E::Response>(
                        reply.into_result(),
                        &self.topic,
                    ));
                }
                // Every sender is gone: Zenoh dropped the callback, so the
                // query is finished.
                Ok(None) => break,
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
    use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

    use crate::bus::query::QueryCode;
    use crate::bus::session::BusOwner;
    use crate::bus::test_support::{GET_TOPIC, GetRequest, GetResponse, bound, participant_config};

    /// Every Zenoh ERROR record this test binary has produced.
    ///
    /// The line these tests are about is emitted from Zenoh's own runtime
    /// thread, so a thread-local test subscriber cannot see it - only the
    /// process-wide default can. Kept to Zenoh's own targets so an unrelated
    /// error elsewhere in the binary cannot fail a query test.
    static ZENOH_ERRORS: Mutex<Vec<String>> = Mutex::new(Vec::new());

    fn zenoh_errors() -> MutexGuard<'static, Vec<String>> {
        ZENOH_ERRORS.lock().unwrap_or_else(PoisonError::into_inner)
    }

    struct CaptureZenohErrors;

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CaptureZenohErrors {
        /// Keeps the process-wide max level at ERROR, so installing this
        /// costs no other callsite anything.
        fn max_level_hint(&self) -> Option<tracing_subscriber::filter::LevelFilter> {
            Some(tracing_subscriber::filter::LevelFilter::ERROR)
        }

        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let metadata = event.metadata();
            if *metadata.level() == tracing::Level::ERROR && metadata.target().starts_with("zenoh")
            {
                zenoh_errors().push(format!("{} at {}", metadata.target(), metadata.name()));
            }
        }
    }

    /// Install the capture (once for the binary) and take the count to compare
    /// the end of a test against.
    fn watch_zenoh_errors() -> usize {
        static INSTALLED: OnceLock<()> = OnceLock::new();
        INSTALLED.get_or_init(|| {
            use tracing_subscriber::layer::SubscriberExt;
            let subscriber = tracing_subscriber::registry().with(CaptureZenohErrors);
            let _ = tracing::subscriber::set_global_default(subscriber);
        });
        zenoh_errors().len()
    }

    fn assert_silent(baseline: usize) {
        let recorded = zenoh_errors();
        assert_eq!(
            recorded.len(),
            baseline,
            "a delivery nobody is waiting for must be silent, but Zenoh logged {:?}",
            &recorded[baseline..]
        );
    }

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_query_round_trip_ok_then_error() {
        let (owner, bus) = BusOwner::open(participant_config("q")).await.unwrap();
        let server = bus.declare_server(GET_TOPIC).await.unwrap();
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

        let topic = bound::<GetRequest>(GET_TOPIC).client();
        let querier =
            Querier::<GetRequest>::new(bus.clone(), &topic, Duration::from_secs(5)).unwrap();

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
        let server = bus.declare_server(GET_TOPIC).await.unwrap();

        let server_task = tokio::spawn(async move {
            let _incoming = server.recv().await.unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
        });

        let topic = bound::<GetRequest>(GET_TOPIC).client();
        let querier =
            Querier::<GetRequest>::new(bus.clone(), &topic, Duration::from_millis(20)).unwrap();

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

    /// A reply that arrives after its caller gave up is a non-event.
    ///
    /// This is the seam that used to print `ERROR sending on a closed channel`
    /// on the operator's terminal after `phoxal status` had already returned:
    /// a query cannot be cancelled without Zenoh's `unstable` feature, so the
    /// responder's late reply, its final, and Zenoh's own timeout all land in
    /// a receiver the timed-out caller has dropped - which Zenoh's default
    /// FIFO handler reports as an error.
    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_reply_after_the_caller_gave_up_is_silent() {
        let baseline = watch_zenoh_errors();
        let (owner, bus) = BusOwner::open(participant_config("late-reply"))
            .await
            .unwrap();
        let server = bus.declare_server(GET_TOPIC).await.unwrap();

        // A responder that takes the query and answers only well after the
        // caller's deadline.
        let server_bus = bus.clone();
        let server_task = tokio::spawn(async move {
            let incoming = server.recv().await.unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
            let response = GetResponse::Found { bytes: vec![7] };
            let payload = rmp_serde::to_vec_named(&response).unwrap();
            let _ = incoming.reply(&server_bus, payload).await;
        });

        let topic = bound::<GetRequest>(GET_TOPIC).client();
        let deadline = Duration::from_millis(50);
        let querier = Querier::<GetRequest>::new(bus.clone(), &topic, deadline).unwrap();
        let error = querier
            .query(GetRequest {
                path: "late".to_string(),
            })
            .await
            .expect_err("the caller's own deadline decides the outcome");
        match error {
            QueryError::Timeout(failure) => assert_eq!(failure.code, QueryCode::DeadlineExceeded),
            other => panic!("expected QueryError::Timeout, got {other:?}"),
        }

        // Long enough for all three late deliveries: the responder's reply, its
        // final, and Zenoh's own timeout one grace after the caller's deadline.
        tokio::time::sleep(deadline + ZENOH_QUERY_TIMEOUT_GRACE + Duration::from_millis(300)).await;
        assert_silent(baseline);

        server_task.await.unwrap();
        owner.close().await;
    }

    /// The same holds when the query future is dropped rather than timed out:
    /// an aborted `phoxal status` feed is exactly this shape.
    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_reply_after_the_query_future_was_aborted_is_silent() {
        let baseline = watch_zenoh_errors();
        let (owner, bus) = BusOwner::open(participant_config("aborted-query"))
            .await
            .unwrap();
        let server = bus.declare_server(GET_TOPIC).await.unwrap();

        let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
        let (abort_tx, abort_rx) = tokio::sync::oneshot::channel();
        let server_bus = bus.clone();
        let server_task = tokio::spawn(async move {
            let incoming = server.recv().await.unwrap();
            seen_tx.send(()).unwrap();
            // Answer only once the caller is provably gone.
            abort_rx.await.unwrap();
            let response = GetResponse::Found { bytes: vec![7] };
            let payload = rmp_serde::to_vec_named(&response).unwrap();
            let _ = incoming.reply(&server_bus, payload).await;
        });

        let topic = bound::<GetRequest>(GET_TOPIC).client();
        let querier =
            Querier::<GetRequest>::new(bus.clone(), &topic, Duration::from_secs(5)).unwrap();
        let query_task = tokio::spawn(async move {
            querier
                .query(GetRequest {
                    path: "abandoned".to_string(),
                })
                .await
        });
        seen_rx.await.expect("the query reached the responder");

        query_task.abort();
        let _ = query_task.await;
        abort_tx.send(()).unwrap();

        server_task.await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_silent(baseline);

        owner.close().await;
    }

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn close_tracks_a_query_reply_wait_until_the_query_finishes() {
        let (owner, bus) = BusOwner::open(participant_config("query-close-race"))
            .await
            .unwrap();
        let server = bus.declare_server(GET_TOPIC).await.unwrap();
        let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
        let server_task = tokio::spawn(async move {
            let _incoming = server.recv().await.unwrap();
            seen_tx.send(()).unwrap();
            std::future::pending::<()>().await;
        });

        let topic = bound::<GetRequest>(GET_TOPIC).client();
        let querier =
            Querier::<GetRequest>::new(bus.clone(), &topic, Duration::from_secs(5)).unwrap();
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
            matches!(timeout, crate::bus::BusCloseTimeout::Operations(count) if *count > 0)
        }));
        let _ = query_task.await;
        server_task.abort();
    }
}
