//! Synchronous typed query handler registration and dispatch.
//!
//! Query decoding, handler evaluation, and response encoding are deliberately
//! kept together with the serialized participant state owner.  The resulting
//! [`ServerOutcome`] is handed to runner-owned reply transport only after the
//! handler has returned, so transport IO cannot suspend the lifecycle owner.

use std::marker::PhantomData;

use crate::bus::{Codec, MessagePack, Payload, QueryFailure};
use crate::participant::api::Participant;
use crate::participant::context::QueryContext;

/// A successful server reply: the encoded plain `Resp` body.
///
/// The reply carries no contract identity of its own, because the request
/// already arrived on `Resp`'s family-rooted topic key - a receiver that
/// got the reply knows what it asked for.
#[derive(Debug)]
pub(crate) struct ServerReply {
    /// MessagePack-encoded `Resp` body.
    pub payload: Vec<u8>,
}

/// What a synchronous query dispatcher returns: a [`ServerReply`] or a
/// structured [`QueryFailure`]. Transport reply IO is queued by the runner
/// after this value is produced.
pub(crate) type ServerOutcome = std::result::Result<ServerReply, QueryFailure>;

/// One setup-time query binding, type-erased only after its request/response
/// types and handler have been checked at the `ctx.query(...)` call.
///
/// `topic` is a plain `String` rather than a typed
/// [`Topic`](crate::bus::Topic): erasure is the point of this type, and a
/// `Topic<ServeQuery<E>>` still names the endpoint descriptor, so it cannot
/// survive into the erased registration list the runner iterates. The key
/// string is all that is left to match an incoming query against, and it was
/// produced by the typed builder at the checked `ctx.query(...)` call site.
pub(crate) struct QueryRegistration<R: Participant> {
    topic: String,
    handler: Box<dyn ErasedQueryHandler<R>>,
}

impl<R: Participant> QueryRegistration<R> {
    pub(crate) fn new<Req, Resp, H>(topic: String, handler: H) -> Self
    where
        Req: Payload,
        Resp: Payload,
        H: for<'a> Fn(
                &'a R,
                &'a R::Api,
                QueryContext,
                Req,
                &'a mut R::State,
            ) -> crate::bus::QueryResult<Resp>
            + Send
            + Sync
            + 'static,
    {
        Self {
            topic,
            handler: Box::new(TypedQueryHandler::<H, Req, Resp> {
                handler,
                _types: PhantomData,
            }),
        }
    }

    pub(crate) fn topic(&self) -> &str {
        &self.topic
    }

    pub(crate) fn dispatch(
        &self,
        participant: &R,
        api: &R::Api,
        query_context: QueryContext,
        state: &mut R::State,
        request: Vec<u8>,
    ) -> ServerOutcome {
        self.handler
            .dispatch(participant, api, query_context, state, request)
    }
}

trait ErasedQueryHandler<R: Participant>: Send + Sync {
    fn dispatch(
        &self,
        participant: &R,
        api: &R::Api,
        query_context: QueryContext,
        state: &mut R::State,
        request: Vec<u8>,
    ) -> ServerOutcome;
}

struct TypedQueryHandler<H, Req, Resp> {
    handler: H,
    _types: PhantomData<fn(Req) -> Resp>,
}

impl<R, H, Req, Resp> ErasedQueryHandler<R> for TypedQueryHandler<H, Req, Resp>
where
    R: Participant,
    Req: Payload,
    Resp: Payload,
    H: for<'a> Fn(
            &'a R,
            &'a R::Api,
            QueryContext,
            Req,
            &'a mut R::State,
        ) -> crate::bus::QueryResult<Resp>
        + Send
        + Sync
        + 'static,
{
    fn dispatch(
        &self,
        participant: &R,
        api: &R::Api,
        query_context: QueryContext,
        state: &mut R::State,
        request: Vec<u8>,
    ) -> ServerOutcome {
        let request = MessagePack::decode::<Req>(&request).map_err(|error| {
            QueryFailure::invalid_argument(format!("decode query request: {error}"))
        })?;
        let response = (self.handler)(participant, api, query_context, request, state)?;
        let payload = MessagePack::encode(&response)
            .map_err(|error| QueryFailure::internal(format!("encode query response: {error}")))?;
        Ok(ServerReply { payload })
    }
}

#[cfg(test)]
mod tests {
    use super::QueryRegistration;
    use crate::bundle::BundlePath;
    use crate::bus::{Codec, MessagePack, QueryCode, QueryFailure};
    use crate::prelude::*;
    use crate::supervisor::api as supervisor;

    struct Api;

    #[derive(Default)]
    struct QueryState {
        calls: Vec<String>,
        requesters: Vec<crate::bus::ProducerId>,
    }

    #[phoxal::service(id = "query-test", state = QueryState, api = Api)]
    struct QueryParticipant;

    impl Participant for QueryParticipant {
        async fn setup(
            &self,
            _ctx: &mut SetupContext<Self>,
            _config: Self::Config,
        ) -> Result<(Self::State, Self::Api)> {
            Ok((QueryState::default(), Api))
        }
    }

    impl QueryParticipant {
        fn get(
            &self,
            _api: &Api,
            query: QueryContext,
            request: supervisor::bundle::GetRequest,
            state: &mut QueryState,
        ) -> QueryResult<supervisor::bundle::GetResponse> {
            state.calls.push(request.path.as_str().to_owned());
            state.requesters.push(query.producer());
            if request.path.as_str() == "ok" {
                Ok(supervisor::bundle::GetResponse::Chunk {
                    bytes: vec![1, 2, 3],
                    eof: true,
                })
            } else {
                Err(QueryFailure::not_found("no such asset"))
            }
        }
    }

    #[test]
    fn typed_query_dispatch_decodes_mutates_and_encodes() {
        let registration =
            QueryRegistration::new("supervisor/bundle/get".to_string(), QueryParticipant::get);
        let participant = QueryParticipant;
        let api = Api;
        let mut state = QueryState::default();
        let first_producer =
            crate::bus::ProducerId::try_from((1_u128 << 124) | 1).expect("canonical test producer");
        let second_producer =
            crate::bus::ProducerId::try_from((1_u128 << 124) | 2).expect("canonical test producer");

        let first = MessagePack::encode(&supervisor::bundle::GetRequest {
            path: BundlePath::new("ok").unwrap(),
            offset: 0,
        })
        .unwrap();
        let reply = registration
            .dispatch(
                &participant,
                &api,
                QueryContext::new(first_producer),
                &mut state,
                first,
            )
            .unwrap();
        let response: supervisor::bundle::GetResponse =
            MessagePack::decode(&reply.payload).unwrap();
        assert!(matches!(
            response,
            supervisor::bundle::GetResponse::Chunk { .. }
        ));

        let second = MessagePack::encode(&supervisor::bundle::GetRequest {
            path: BundlePath::new("missing").unwrap(),
            offset: 0,
        })
        .unwrap();
        let failure = registration
            .dispatch(
                &participant,
                &api,
                QueryContext::new(second_producer),
                &mut state,
                second,
            )
            .unwrap_err();
        assert_eq!(failure.code, QueryCode::NotFound);
        assert_eq!(state.calls, ["ok", "missing"]);
        assert_eq!(state.requesters, [first_producer, second_producer]);
    }
}
