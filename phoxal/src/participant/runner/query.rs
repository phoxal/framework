//! Serving the typed queries a participant registered during
//! `Participant::setup`.

use crate::bus::QueryFailure;
use crate::participant::api::{Participant, QueryRegistration};
use crate::participant::managed::{ManagedTaskPolicy, ManagedTasks};
use phoxal_bus::{BusHandle, IncomingQuery};
use tokio::sync::mpsc;

/// How many requests may wait between the receive tasks and the serialized
/// event loop before the transport is left to apply back-pressure.
const REQUEST_QUEUE_DEPTH: usize = 64;

/// The typed query surface a participant declared. Its receive loops are
/// runner-owned `Critical` tasks in the same registry as participant tasks;
/// this value only retains the registrations and serialized request queue.
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
    ) -> crate::Result<Option<Self>> {
        if registrations.is_empty() {
            return Ok(None);
        }
        let (sender, requests) = mpsc::channel(REQUEST_QUEUE_DEPTH);
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
    pub(crate) async fn serve(
        &self,
        request: (usize, IncomingQuery),
        participant: &R,
        api: &R::Api,
        state: &mut R::State,
        bus: &BusHandle,
    ) {
        let (index, incoming) = request;
        let Some(registration) = self.registrations.get(index) else {
            let _ = incoming
                .reply_err(&QueryFailure::internal("invalid query registration"))
                .await;
            return;
        };

        let metadata = match incoming.request_metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                let _ = incoming
                    .reply_err(&QueryFailure::invalid_argument(error.to_string()))
                    .await;
                return;
            }
        };
        if metadata.codec_id().is_none() {
            let _ = incoming
                .reply_err(&QueryFailure::invalid_argument(format!(
                    "unsupported request codec id {}",
                    metadata.codec
                )))
                .await;
            return;
        }
        let body = match incoming.request_bytes() {
            Ok(bytes) => bytes,
            Err(error) => {
                let _ = incoming
                    .reply_err(&QueryFailure::invalid_argument(error.to_string()))
                    .await;
                return;
            }
        };
        match registration.dispatch(participant, api, state, body).await {
            Ok(reply) => {
                let _ = incoming.reply(bus, reply.payload).await;
            }
            Err(failure) => {
                let _ = incoming.reply_err(&failure).await;
            }
        }
    }

    /// Stop admitting requests. The receive tasks are cancelled and joined by
    /// the runner's managed-task teardown after this receiver is dropped.
    pub(crate) fn close(self) {}
}
