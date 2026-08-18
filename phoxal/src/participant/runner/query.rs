//! Serving the typed queries a participant registered during
//! `Participant::setup`.

use crate::bus::QueryFailure;
use crate::bus::{BusHandle, IncomingQuery};
use crate::participant::api::Participant;
use crate::participant::context::QueryContext;
use crate::participant::managed::{ManagedTaskPolicy, ManagedTasks};
use crate::participant::query::{QueryRegistration, ServerOutcome};
use std::time::Duration;
use tokio::sync::mpsc;

/// How many requests may wait between the receive tasks and the serialized
/// event loop before the transport is left to apply back-pressure.
const REQUEST_QUEUE_DEPTH: usize = 64;
/// How many encoded replies may wait for the runner-owned transport worker.
/// Saturation faults the participant rather than making the serialized owner
/// await external transport back-pressure.
// Keep enough room for every request admitted by the ingress queue plus the
// small number of receive tasks that may already have one item in hand.  The
// queue remains finite; a pathological producer still fails closed instead of
// making the serialized owner await transport back-pressure.
const REPLY_QUEUE_DEPTH: usize = 256;

/// The typed query surface a participant declared. Its receive loops and the
/// reply transport worker are runner-owned `Critical` tasks in the same
/// registry as participant tasks; this value retains the registrations,
/// serialized request queue, and bounded reply queue.
///
/// The registrations, the channel, and the receive tasks are all-or-nothing: the
/// channel exists only because there are registrations to dispatch to, a
/// registration with no channel could never be reached, and a receive task with
/// no registration would forward an index that answers nothing. Holding them as
/// one value is what makes "half a query surface" unrepresentable - a
/// participant that registered no queries simply has no surface at all.
pub(crate) struct QuerySurface<R: Participant> {
    registrations: Vec<QueryRegistration<R>>,
    requests: mpsc::Receiver<(usize, IncomingQuery)>,
    replies: mpsc::Sender<PendingReply>,
}

/// A reply is deliberately handed to a runner-owned task after the typed
/// handler has returned.  `IncomingQuery::reply*` performs transport IO and
/// must never be awaited while the serialized participant state is borrowed.
struct PendingReply {
    incoming: IncomingQuery,
    outcome: ServerOutcome,
}

impl<R: Participant> QuerySurface<R> {
    /// Declare one queryable per registration and spawn its receive task.
    ///
    /// Queryables are declared only after `Participant::setup` succeeds. The
    /// receive tasks do no participant work: they forward bounded, indexed
    /// requests to the same serialized event loop that owns state, step and
    /// reset. `Ok(None)` when the participant registered no queries.
    pub(crate) async fn declare(
        bus: &BusHandle,
        registrations: Vec<QueryRegistration<R>>,
        managed_tasks: &mut ManagedTasks,
        reply_delay: Option<Duration>,
    ) -> crate::Result<Option<Self>> {
        if registrations.is_empty() {
            return Ok(None);
        }
        let (sender, requests) = mpsc::channel(REQUEST_QUEUE_DEPTH);
        let (reply_sender, mut reply_receiver) = mpsc::channel(REPLY_QUEUE_DEPTH);
        let reply_bus = bus.clone();
        managed_tasks.spawn("query-reply", ManagedTaskPolicy::Critical, async move {
            while let Some(PendingReply { incoming, outcome }) = reply_receiver.recv().await {
                if let Some(delay) = reply_delay {
                    tokio::time::sleep(delay).await;
                }
                let result = match outcome {
                    Ok(reply) => incoming.reply(&reply_bus, reply.payload).await,
                    Err(failure) => incoming.reply_err(&failure).await,
                };
                if let Err(error) = result {
                    // A caller disappearing while its reply is in flight is
                    // not a participant fault. The reply worker remains
                    // alive to serve the next bounded request.
                    tracing::debug!(
                        target: "phoxal.runtime",
                        error = %error,
                        "query reply transport failed"
                    );
                }
            }
            Ok::<(), anyhow::Error>(())
        });
        for (index, registration) in registrations.iter().enumerate() {
            let queryable = match bus.declare_server(registration.topic()).await {
                Ok(queryable) => queryable,
                Err(error) => {
                    // Nothing is serving yet. The already-registered query
                    // loops belong to `managed_tasks`; setup rollback will
                    // cancel and join them before returning this error.
                    return Err(error.into());
                }
            };
            let sender = sender.clone();
            let topic = registration.topic().to_string();
            managed_tasks.spawn(
                format!("query-ingest-{index}"),
                ManagedTaskPolicy::Critical,
                async move {
                    loop {
                        let incoming = queryable.recv().await.map_err(|error| {
                            anyhow::anyhow!("query ingest for {topic} terminated: {error}")
                        })?;
                        if sender.send((index, incoming)).await.is_err() {
                            // The serialized runner has dropped its receiver as
                            // part of teardown. That is the one expected clean
                            // exit; transport/session failure above remains a
                            // Critical task fault.
                            return Ok::<(), anyhow::Error>(());
                        }
                    }
                },
            );
        }
        // The only senders left are the ones the receive tasks own, so the
        // channel stays open for exactly as long as something can still feed it.
        Ok(Some(QuerySurface {
            registrations,
            requests,
            replies: reply_sender,
        }))
    }

    /// The next request a receive task forwarded.
    ///
    /// Pends forever once the channel closes, so a closed channel disables the
    /// runner's query branch instead of spinning it.
    pub(crate) async fn next_request(&mut self) -> (usize, IncomingQuery) {
        match self.requests.recv().await {
            Some(request) => request,
            None => std::future::pending().await,
        }
    }

    /// Serve one typed query on the serialized participant state.
    pub(crate) fn serve(
        &self,
        request: (usize, IncomingQuery),
        participant: &R,
        api: &R::Api,
        state: &mut R::State,
    ) -> crate::Result<()> {
        let (index, incoming) = request;
        let Some(registration) = self.registrations.get(index) else {
            return self.enqueue(
                incoming,
                Err(QueryFailure::internal("invalid query registration")),
            );
        };

        let metadata = match incoming.request_metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                return self.enqueue(
                    incoming,
                    Err(QueryFailure::invalid_argument(error.to_string())),
                );
            }
        };
        if metadata.codec_id().is_none() {
            return self.enqueue(
                incoming,
                Err(QueryFailure::invalid_argument(format!(
                    "unsupported request codec id {}",
                    metadata.codec
                ))),
            );
        }
        let query_context = QueryContext::new(metadata.source.producer());
        let body = match incoming.request_bytes() {
            Ok(bytes) => bytes,
            Err(error) => {
                return self.enqueue(
                    incoming,
                    Err(QueryFailure::invalid_argument(error.to_string())),
                );
            }
        };
        let outcome = registration.dispatch(participant, api, query_context, state, body);
        self.enqueue(incoming, outcome)
    }

    fn enqueue(&self, incoming: IncomingQuery, outcome: ServerOutcome) -> crate::Result<()> {
        self.replies.try_send(PendingReply { incoming, outcome }).map_err(|error| {
            anyhow::anyhow!(
                "query reply queue is saturated or closed; refusing to stall the serialized runner: {error}"
            )
        })
    }

    /// Stop admitting requests. The receive tasks are cancelled and joined by
    /// the runner's managed-task teardown after this receiver is dropped.
    pub(crate) fn close(self) {}
}
