use std::num::NonZeroUsize;
use std::sync::Arc;

use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;

use crate::bus::typed::TypedTopicResponder;
use crate::bus::zenoh::BusyResponse;

pub struct ReadCell<V> {
    inner: Arc<arc_swap::ArcSwap<V>>,
}

pub struct Reader<V> {
    inner: Arc<arc_swap::ArcSwap<V>>,
}

impl<V> Clone for Reader<V> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<V: Send + Sync + 'static> ReadCell<V> {
    pub fn new(initial: V) -> Self {
        Self {
            inner: Arc::new(arc_swap::ArcSwap::from_pointee(initial)),
        }
    }

    pub fn publish(&self, next: V) {
        self.publish_arc(Arc::new(next));
    }

    pub fn publish_arc(&self, next: Arc<V>) {
        self.inner.store(next);
    }

    pub fn reader(&self) -> Reader<V> {
        Reader {
            inner: self.inner.clone(),
        }
    }

    pub fn load(&self) -> Arc<V> {
        self.inner.load_full()
    }
}

impl<V: Send + Sync + 'static> Reader<V> {
    pub fn load(&self) -> Arc<V> {
        self.inner.load_full()
    }
}

pub struct QueryOptions {
    pub max_in_flight: NonZeroUsize,
}

impl QueryOptions {
    pub fn single() -> Self {
        Self {
            max_in_flight: NonZeroUsize::new(1).expect("1 is non-zero"),
        }
    }

    pub fn max_in_flight(max: NonZeroUsize) -> Self {
        Self { max_in_flight: max }
    }
}

pub(super) fn spawn_topic_query_responder<Req, Resp, V, F>(
    responder: TypedTopicResponder<Req, Resp>,
    reader: Reader<V>,
    options: QueryOptions,
    handler: F,
) -> JoinHandle<()>
where
    Req: DeserializeOwned + Send + Sync + 'static,
    Resp: Serialize + BusyResponse + Send + Sync + 'static,
    V: Send + Sync + 'static,
    F: Fn(&V, Req) -> Resp + Send + Sync + 'static,
{
    let executor = QueryExecutor::new(reader, options, handler);

    tokio::spawn(async move {
        loop {
            let query = match responder.recv().await {
                Ok(query) => query,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        request_type = std::any::type_name::<Req>(),
                        "failed to receive typed topic query"
                    );
                    return;
                }
            };
            let request = match query.request() {
                Ok(request) => request,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        request_type = std::any::type_name::<Req>(),
                        "failed to decode typed topic query payload"
                    );
                    continue;
                }
            };
            match executor.start::<Req, Resp>(request) {
                QueryStart::Busy(response) => {
                    if let Err(error) = query.reply(&response).await {
                        tracing::warn!(
                            %error,
                            response_type = std::any::type_name::<Resp>(),
                            "failed to reply with busy typed topic query response"
                        );
                    }
                }
                QueryStart::Accepted(call) => {
                    tokio::spawn(async move {
                        let response = call.run();
                        if let Err(error) = query.reply(&response).await {
                            tracing::warn!(
                                %error,
                                response_type = std::any::type_name::<Resp>(),
                                "failed to reply to typed topic query"
                            );
                        }
                    });
                }
            }
        }
    })
}

struct QueryExecutor<V, F> {
    reader: Reader<V>,
    permits: Arc<Semaphore>,
    handler: Arc<F>,
}

impl<V, F> QueryExecutor<V, F> {
    fn new(reader: Reader<V>, options: QueryOptions, handler: F) -> Self {
        Self {
            reader,
            permits: Arc::new(Semaphore::new(options.max_in_flight.get())),
            handler: Arc::new(handler),
        }
    }
}

impl<V, F> QueryExecutor<V, F>
where
    V: Send + Sync + 'static,
    F: Send + Sync + 'static,
{
    fn start<Req, Resp>(&self, request: Req) -> QueryStart<Req, Resp, V, F>
    where
        Resp: BusyResponse,
    {
        match self.permits.clone().try_acquire_owned() {
            Ok(permit) => QueryStart::Accepted(QueryCall {
                permit,
                view: self.reader.load(),
                handler: self.handler.clone(),
                request,
            }),
            Err(_) => QueryStart::Busy(Resp::busy()),
        }
    }
}

enum QueryStart<Req, Resp, V, F> {
    Busy(Resp),
    Accepted(QueryCall<Req, V, F>),
}

struct QueryCall<Req, V, F> {
    permit: OwnedSemaphorePermit,
    view: Arc<V>,
    handler: Arc<F>,
    request: Req,
}

impl<Req, V, F> QueryCall<Req, V, F>
where
    F: Send + Sync + 'static,
{
    fn run<Resp>(self) -> Resp
    where
        F: Fn(&V, Req) -> Resp,
    {
        let Self {
            permit,
            view,
            handler,
            request,
        } = self;
        let _permit = permit;
        handler(&view, request)
    }
}

#[cfg(test)]
mod tests {
    use super::{QueryExecutor, QueryStart, ReadCell};
    use crate::bus::zenoh::BusyResponse;
    use std::sync::{Arc, Condvar, Mutex};

    #[test]
    fn read_cell_loads_published_value() {
        let cell = ReadCell::new(1);

        cell.publish(2);

        assert_eq!(*cell.load(), 2);
        assert_eq!(*cell.reader().load(), 2);
    }

    #[test]
    fn read_cell_loaded_arc_pins_previous_snapshot() {
        let cell = ReadCell::new(String::from("old"));
        let old = cell.load();

        cell.publish(String::from("new"));

        assert_eq!(old.as_str(), "old");
        assert_eq!(cell.load().as_str(), "new");
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct QueryRequest {
        id: u8,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum QueryResponse {
        Value(u8),
        Busy,
    }

    impl BusyResponse for QueryResponse {
        fn busy() -> Self {
            Self::Busy
        }
    }

    #[test]
    fn query_executor_returns_busy_when_max_in_flight_is_saturated() {
        let view = ReadCell::new(());
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let release_handler = release.clone();

        let executor = QueryExecutor::new(
            view.reader(),
            super::QueryOptions::single(),
            move |_: &(), request: QueryRequest| {
                if request.id == 1 {
                    started_tx.send(()).expect("test should receive start");
                    let (lock, cvar) = &*release_handler;
                    let mut released = lock.lock().expect("release lock should not be poisoned");
                    while !*released {
                        released = cvar
                            .wait(released)
                            .expect("release lock should not be poisoned");
                    }
                }
                QueryResponse::Value(request.id)
            },
        );

        let first_call = match executor.start::<QueryRequest, QueryResponse>(QueryRequest { id: 1 })
        {
            QueryStart::Accepted(call) => call,
            QueryStart::Busy(_) => panic!("first query should acquire the single permit"),
        };
        let first = std::thread::spawn(move || first_call.run::<QueryResponse>());

        started_rx
            .recv()
            .expect("first query handler should signal start");

        let second = executor.start::<QueryRequest, QueryResponse>(QueryRequest { id: 2 });

        assert!(matches!(second, QueryStart::Busy(QueryResponse::Busy)));

        {
            let (lock, cvar) = &*release;
            let mut released = lock.lock().expect("release lock should not be poisoned");
            *released = true;
            cvar.notify_one();
        }

        let first = first
            .join()
            .expect("first query handler thread should complete");
        assert_eq!(first, QueryResponse::Value(1));
    }
}
