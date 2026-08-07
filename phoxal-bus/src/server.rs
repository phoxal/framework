//! Server-side query handling: a thin wrapper over a Zenoh queryable that the
//! runner uses to drive typed participant query handlers.
//!
//! This is the responder side of the request/response leg whose caller is
//! [`Querier`](crate::handle::querier::Querier). [`Bus::declare_server`] declares a
//! `complete` queryable on one topic key; [`ServerQueryable::recv`] yields each
//! [`IncomingQuery`], which exposes the raw request bytes + its [`BusMetadata`]
//! and the two reply legs:
//!
//! - [`IncomingQuery::reply`] sends the plain `Resp` body, with a fresh
//!   provenance-only metadata attachment, on the success leg;
//! - [`IncomingQuery::reply_err`] sends a [`QueryFailure`] on Zenoh's native
//!   error leg, which the caller decodes back into the failure.
//!
//! These public types carry untyped payload bytes; the generated server dispatch
//! does the typed encode/decode around them. Contract identity is not validated
//! here: a query only ever reaches the handler for its own version-qualified
//! topic key, so the codec is the only thing left to check.

use zenoh::handlers::FifoChannelHandler;
use zenoh::key_expr::OwnedKeyExpr;
use zenoh::query::{Query as ZenohQuery, Queryable};

use crate::abi::{CodecId, EncodingMetadata};
use crate::error::{BusError, MetadataProblem, Result};
use crate::metadata::BusMetadata;
use crate::query::QueryFailure;
use crate::session::Bus;

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

    /// The version-qualified topic key this queryable serves.
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
    /// The version-qualified topic key.
    pub fn topic_key(&self) -> &str {
        &self.topic_key
    }

    /// The raw request body bytes (MessagePack-encoded `Req`).
    pub fn request_bytes(&self) -> Result<Vec<u8>> {
        let payload = self
            .query
            .payload()
            .ok_or_else(|| self.malformed(MetadataProblem::MissingPayload))?;
        Ok(payload.to_bytes().to_vec())
    }

    fn malformed(&self, problem: MetadataProblem) -> BusError {
        BusError::metadata(&self.topic_key, problem)
    }

    /// The request's bus metadata (codec + provenance), decoded from the Zenoh
    /// attachment. Contract identity is not carried here: this queryable only
    /// ever receives requests on its own version-qualified topic key.
    pub fn request_metadata(&self) -> Result<BusMetadata> {
        let encoding = self
            .query
            .encoding()
            .ok_or_else(|| self.malformed(MetadataProblem::MissingEncoding))?;
        let encoding: EncodingMetadata = encoding
            .to_string()
            .parse()
            .map_err(|e: crate::abi::EncodingError| self.malformed(e.into()))?;
        if encoding.codec_id() != Some(CodecId::MessagePack) {
            return Err(BusError::UnsupportedCodec {
                codec: encoding.codec,
                topic: self.topic_key.clone(),
            });
        }

        let attachment = self
            .query
            .attachment()
            .ok_or_else(|| self.malformed(MetadataProblem::MissingAttachment))?;
        let metadata = BusMetadata::decode(attachment.to_bytes().as_ref())
            .map_err(|e| self.malformed(e.into()))?;
        if metadata.codec != encoding.codec {
            return Err(self.malformed(MetadataProblem::CodecMismatch {
                encoding: encoding.codec,
                attachment: metadata.codec,
            }));
        }
        Ok(metadata)
    }

    /// Send a success reply: the plain `Resp` body, with a fresh
    /// provenance-only metadata attachment.
    pub async fn reply(&self, bus: &Bus, payload: Vec<u8>) -> Result<()> {
        // A reply expresses no robot time: it answers a question, it does not
        // observe the world.
        let metadata = bus.metadata(None)?;
        let attachment = metadata
            .encode()
            .map_err(|e| self.malformed(MetadataProblem::Encode(e)))?;
        self.query
            .reply(self.query.key_expr(), payload)
            .encoding(CodecId::MessagePack.encoding_string())
            .attachment(attachment)
            .await
            .map_err(|e| BusError::Transport(e.to_string()))
    }

    /// Send a structured error reply on Zenoh's native error leg.
    pub async fn reply_err(&self, failure: &QueryFailure) -> Result<()> {
        let payload = failure
            .encode()
            .map_err(|e| BusError::Transport(format!("failed to encode a query failure: {e}")))?;
        self.query
            .reply_err(payload)
            .await
            .map_err(|e| BusError::Transport(e.to_string()))
    }
}

impl Bus {
    /// Declare a server queryable on `topic_key` (under the bus root).
    pub async fn declare_server(&self, topic_key: &str) -> Result<ServerQueryable> {
        let full_key = self.full_key(topic_key);
        let key = OwnedKeyExpr::new(full_key.clone())
            .map_err(|e| BusError::not_a_key_expression(&full_key, e))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use serial_test::serial;
    use zenoh::bytes::Encoding;

    use crate::session::BusConfig;
    use crate::test_support::{GetRequest, metadata};

    /// A request whose encoding string and attachment disagree about the codec
    /// does not agree with itself about how to read its own body, so the server
    /// rejects it before decoding anything.
    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn incoming_query_rejects_encoding_attachment_codec_mismatch() {
        let bus = Bus::open(BusConfig::in_process("q-mismatch"))
            .await
            .unwrap();
        let server = bus.declare_server("yTEST/asset/get").await.unwrap();

        let request = GetRequest {
            path: "asset.bin".to_string(),
        };
        let payload = rmp_serde::to_vec_named(&request).unwrap();
        let mut meta = metadata();
        meta.codec = CodecId::MessagePack.as_u8();

        let key = OwnedKeyExpr::new(bus.full_key("yTEST/asset/get")).unwrap();
        let _replies = bus
            .session()
            .get(key)
            .payload(payload)
            // The encoding string claims a codec the attachment disagrees with.
            .encoding(Encoding::from("phoxal/v0;codec=99".to_string()))
            .attachment(meta.encode().expect("test metadata encodes"))
            .target(zenoh::query::QueryTarget::All)
            .consolidation(zenoh::query::ConsolidationMode::None)
            .await
            .unwrap();

        let incoming = tokio::time::timeout(Duration::from_secs(5), server.recv())
            .await
            .expect("the query must reach the server")
            .unwrap();
        let error = incoming.request_metadata().unwrap_err();
        match error {
            BusError::UnsupportedCodec { codec: 99, .. } => {}
            other => panic!("expected unsupported codec 99, got {other:?}"),
        }

        bus.close().await.unwrap();
    }
}
