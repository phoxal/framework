//! Serving the typed queries a participant registered during
//! `Participant::setup`.

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::bus::QueryFailure;
use crate::participant::api::{Participant, QueryRegistration};
use phoxal_bus::{Bus, IncomingQuery};

/// How many requests may wait between the receive tasks and the serialized
/// event loop before the transport is left to apply back-pressure.
const REQUEST_QUEUE_DEPTH: usize = 64;

/// The typed query surface a participant declared, and the tasks feeding it.
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
    receivers: Vec<JoinHandle<()>>,
}

impl<R: Participant> QuerySurface<R> {
    /// Declare one queryable per registration and spawn its receive task.
    ///
    /// Queryables are declared only after `Participant::setup` succeeds. The
    /// receive tasks do no participant work: they forward bounded, indexed
    /// requests to the same serialized event loop that owns state, step and
    /// reset. `Ok(None)` when the participant registered no queries.
    pub(crate) async fn declare(
        bus: &Bus,
        registrations: Vec<QueryRegistration<R>>,
    ) -> crate::Result<Option<Self>> {
        if registrations.is_empty() {
            return Ok(None);
        }
        let (sender, requests) = mpsc::channel(REQUEST_QUEUE_DEPTH);
        let mut receivers: Vec<JoinHandle<()>> = Vec::with_capacity(registrations.len());
        for (index, registration) in registrations.iter().enumerate() {
            let queryable = match bus.declare_server(registration.topic()).await {
                Ok(queryable) => queryable,
                Err(error) => {
                    // Nothing is serving yet. The queryables already declared go
                    // away with their tasks rather than accepting requests for a
                    // participant that never reached its run loop.
                    for receiver in receivers {
                        receiver.abort();
                    }
                    return Err(error.into());
                }
            };
            let sender = sender.clone();
            receivers.push(tokio::spawn(async move {
                while let Ok(incoming) = queryable.recv().await {
                    if sender.send((index, incoming)).await.is_err() {
                        break;
                    }
                }
            }));
        }
        // The only senders left are the ones the receive tasks own, so the
        // channel stays open for exactly as long as something can still feed it.
        Ok(Some(QuerySurface {
            registrations,
            requests,
            receivers,
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
        bus: &Bus,
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

    /// Stop serving: abort the receive tasks so nothing forwards a request into
    /// a participant that is already tearing down.
    pub(crate) fn close(self) {
        for receiver in self.receivers {
            receiver.abort();
        }
    }
}
