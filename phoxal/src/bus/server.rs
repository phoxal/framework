//! Server-side query handling: a thin wrapper over a Zenoh queryable that the
//! runner uses to drive `#[server]`/`#[server_snapshot]` handlers (D16/D31).

use zenoh::handlers::FifoChannelHandler;
use zenoh::key_expr::OwnedKeyExpr;
use zenoh::query::{Query as ZenohQuery, Queryable};

use crate::bus::abi::{CodecId, encoding_string, parse_encoding_string};
use crate::bus::error::{BusError, Result};
use crate::bus::metadata::{BusMetadata, Source};
use crate::bus::query::QueryFailure;
use crate::bus::session::Bus;

/// A declared queryable bound to one server topic key.
pub struct ServerQueryable {
    inner: Queryable<FifoChannelHandler<ZenohQuery>>,
    topic_key: String,
}

impl ServerQueryable {
    /// Await the next incoming query.
    pub async fn recv(&self) -> Result<IncomingQuery> {
        let query = self
            .inner
            .recv_async()
            .await
            .map_err(|_| BusError::Closed)?;
        Ok(IncomingQuery {
            query,
            topic_key: self.topic_key.clone(),
        })
    }

    /// The versionless topic key this queryable serves.
    pub fn topic_key(&self) -> &str {
        &self.topic_key
    }
}

/// One incoming query, with the request payload and the reply legs.
pub struct IncomingQuery {
    query: ZenohQuery,
    topic_key: String,
}

impl IncomingQuery {
    /// The versionless topic key.
    pub fn topic_key(&self) -> &str {
        &self.topic_key
    }

    /// The raw request body bytes (MessagePack-encoded `Req`).
    pub fn request_bytes(&self) -> Result<Vec<u8>> {
        let payload = self.query.payload().ok_or_else(|| BusError::Metadata {
            topic: self.topic_key.clone(),
            detail: "query is missing a request payload".to_string(),
        })?;
        Ok(payload.to_bytes().to_vec())
    }

    /// The request's bus metadata (api_version, family, codec, …), decoded from
    /// the Zenoh attachment. The generated server dispatch validates `api_version`
    /// and `family` against the handler's request body before decode.
    pub fn request_metadata(&self) -> Result<BusMetadata> {
        let encoding = self.query.encoding().ok_or_else(|| BusError::Metadata {
            topic: self.topic_key.clone(),
            detail: "query is missing a request encoding".to_string(),
        })?;
        let encoding =
            parse_encoding_string(&encoding.to_string()).map_err(|e| BusError::Metadata {
                topic: self.topic_key.clone(),
                detail: format!("malformed request encoding string: {e}"),
            })?;
        match encoding.codec_id() {
            Some(CodecId::MessagePack) => {}
            None => {
                return Err(BusError::UnsupportedCodec(
                    encoding.codec,
                    self.topic_key.clone(),
                ));
            }
        }

        let attachment = self.query.attachment().ok_or_else(|| BusError::Metadata {
            topic: self.topic_key.clone(),
            detail: "query is missing a BusMetadata attachment".to_string(),
        })?;
        let metadata = BusMetadata::decode(attachment.to_bytes().as_ref()).map_err(|e| {
            BusError::Metadata {
                topic: self.topic_key.clone(),
                detail: format!("malformed request BusMetadata: {e}"),
            }
        })?;
        if metadata.api_version != encoding.api_version
            || metadata.family != encoding.family
            || metadata.codec != encoding.codec
        {
            return Err(BusError::Metadata {
                topic: self.topic_key.clone(),
                detail: format!(
                    "request encoding/BusMetadata mismatch: encoding family='{}' api='{}' codec={}, \
                     metadata family='{}' api='{}' codec={}",
                    encoding.family,
                    encoding.api_version,
                    encoding.codec,
                    metadata.family,
                    metadata.api_version,
                    metadata.codec
                ),
            });
        }
        Ok(metadata)
    }

    /// Send a success reply: the plain `Resp` body, with bus metadata mirroring
    /// the response contract (D62).
    pub async fn reply(
        &self,
        bus: &Bus,
        payload: Vec<u8>,
        family: &str,
        api_version: &str,
    ) -> Result<()> {
        let metadata = BusMetadata {
            api_version: api_version.to_string(),
            family: family.to_string(),
            codec: CodecId::MessagePack.as_u8(),
            produced_at_ns: 0,
            epoch: 0,
            source: Source {
                participant: bus.participant().to_string(),
                incarnation: bus.incarnation(),
                sequence: bus.next_sequence(),
            },
        };
        self.query
            .reply(self.query.key_expr(), payload)
            .encoding(encoding_string(family, api_version, CodecId::MessagePack))
            .attachment(metadata.encode())
            .await
            .map_err(|e| BusError::Transport(e.to_string()))
    }

    /// Send a structured error reply on Zenoh's native error leg (D31).
    pub async fn reply_err(&self, failure: &QueryFailure) -> Result<()> {
        self.query
            .reply_err(failure.encode())
            .await
            .map_err(|e| BusError::Transport(e.to_string()))
    }
}

impl Bus {
    /// Declare a server queryable on `topic_key` (under the bus root).
    pub async fn declare_server(&self, topic_key: &str) -> Result<ServerQueryable> {
        let full_key = self.full_key(topic_key);
        let key = OwnedKeyExpr::new(full_key.clone())
            .map_err(|e| BusError::Namespace(format!("invalid server key '{full_key}': {e}")))?;
        let inner = self
            .session()
            .declare_queryable(key)
            // A phoxal server fully answers its topic; `complete` lets a querier's
            // BestMatching target route to exactly one responder.
            .complete(true)
            .await
            .map_err(|e| BusError::Transport(e.to_string()))?;
        Ok(ServerQueryable {
            inner,
            topic_key: topic_key.to_string(),
        })
    }
}
