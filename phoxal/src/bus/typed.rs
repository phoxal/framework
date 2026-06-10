use std::{borrow::Cow, marker::PhantomData};

use serde::{Serialize, de::DeserializeOwned};
use zenoh::{
    Wait,
    bytes::{Encoding, ZBytes},
    handlers::FifoChannelHandler,
    key_expr::OwnedKeyExpr,
    pubsub::Subscriber,
    query::{Query as ZenohQuery, QueryConsolidation, QueryTarget, Queryable, ReplyKeyExpr},
    sample::{Locality, Sample},
};

use crate::bus::metadata::BusMetadata;
use crate::bus::query::{Retry, retry_query};
use crate::bus::topic::{PubSub, Query as TopicQuery, Topic};
use crate::bus::zenoh::{deserialize_payload, serialize_payload, typed_encoding};
use crate::bus::{Bus, Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct Received<T> {
    pub at_ns: Option<u64>,
    pub value: T,
}

pub struct TypedTopicSubscriber<T> {
    inner: Subscriber<FifoChannelHandler<Sample>>,
    topic_key: String,
    schema: &'static str,
    schema_version: u32,
    _phantom: PhantomData<T>,
}

impl<T: DeserializeOwned> TypedTopicSubscriber<T> {
    pub async fn recv(&self) -> Result<Received<T>> {
        let sample = self.inner.recv_async().await?;
        decode_sample(&sample, &self.topic_key, self.schema, self.schema_version)
    }

    pub fn key_expr(&self) -> &zenoh::key_expr::KeyExpr<'static> {
        self.inner.key_expr()
    }
}

pub struct TypedTopicResponder<Req, Resp> {
    inner: Queryable<FifoChannelHandler<ZenohQuery>>,
    topic_key: String,
    schema: &'static str,
    schema_version: u32,
    _phantom: PhantomData<(Req, Resp)>,
}

impl<Req, Resp> TypedTopicResponder<Req, Resp> {
    pub async fn recv(&self) -> Result<TypedTopicQuery<Req, Resp>> {
        let query = self.inner.recv_async().await?;
        Ok(TypedTopicQuery {
            inner: query,
            topic_key: self.topic_key.clone(),
            schema: self.schema,
            schema_version: self.schema_version,
            _phantom: PhantomData,
        })
    }
}

pub struct TypedTopicQuery<Req, Resp> {
    inner: ZenohQuery,
    topic_key: String,
    schema: &'static str,
    schema_version: u32,
    _phantom: PhantomData<(Req, Resp)>,
}

impl<Req: DeserializeOwned, Resp: Serialize> TypedTopicQuery<Req, Resp> {
    pub fn request(&self) -> Result<Req> {
        let payload = self.inner.payload().ok_or_else(|| {
            Error::TypedDecode(format!(
                "topic '{}' schema '{}' query is missing a request payload",
                self.topic_key, self.schema
            ))
        })?;
        validate_contract(
            self.inner.encoding(),
            self.inner.attachment(),
            &self.topic_key,
            self.schema,
            self.schema_version,
        )?;
        deserialize_payload(payload).map_err(Error::from)
    }

    pub async fn reply(&self, response: &Resp) -> Result<()> {
        let payload = serialize_payload(response)?;
        self.inner
            .reply(self.inner.key_expr(), payload)
            .encoding(typed_encoding(self.schema))
            .attachment(
                BusMetadata {
                    schema_version: self.schema_version,
                    produced_at_ns: None,
                }
                .encode(),
            )
            .await?;
        Ok(())
    }

    pub async fn reply_err<Payload: Into<ZBytes>>(&self, payload: Payload) -> Result<()> {
        self.inner.reply_err(payload).await?;
        Ok(())
    }
}

impl Bus {
    pub async fn publish<T: Serialize>(
        &self,
        topic: &Topic<PubSub<T>>,
        at_ns: u64,
        data: &T,
    ) -> Result<()> {
        let key = concrete_publish_key(topic)?;
        let key = owned_key(self.topic(key.as_ref()))?;
        let payload = serialize_payload(data)?;
        self.session()
            .put(key, payload)
            .encoding(typed_encoding(topic.schema()))
            .attachment(
                BusMetadata {
                    schema_version: topic.version(),
                    produced_at_ns: Some(at_ns),
                }
                .encode(),
            )
            .await?;
        Ok(())
    }

    pub async fn subscriber<T: DeserializeOwned>(
        &self,
        topic: &Topic<PubSub<T>>,
    ) -> Result<TypedTopicSubscriber<T>> {
        let key = self.topic(topic.key().as_ref());
        let inner = declare_subscriber(self, &key).await?;
        Ok(TypedTopicSubscriber {
            inner,
            topic_key: key,
            schema: topic.schema(),
            schema_version: topic.version(),
            _phantom: PhantomData,
        })
    }

    pub async fn request<Req: Serialize, Resp: DeserializeOwned>(
        &self,
        topic: &Topic<TopicQuery<Req, Resp>>,
        request: &Req,
        retry: &Retry,
    ) -> Result<Option<Resp>> {
        let name = topic.key().into_owned();
        retry_query(&name, retry, || async {
            self.request_once(topic, request).await
        })
        .await
    }

    pub async fn responder<Req: DeserializeOwned, Resp: Serialize>(
        &self,
        topic: &Topic<TopicQuery<Req, Resp>>,
    ) -> Result<TypedTopicResponder<Req, Resp>> {
        let key = self.topic(topic.key().as_ref());
        let inner = self
            .session()
            .declare_queryable(owned_key(key.clone())?)
            .await?;
        Ok(TypedTopicResponder {
            inner,
            topic_key: key,
            schema: topic.schema(),
            schema_version: topic.version(),
            _phantom: PhantomData,
        })
    }

    async fn request_once<Req: Serialize, Resp: DeserializeOwned>(
        &self,
        topic: &Topic<TopicQuery<Req, Resp>>,
        request: &Req,
    ) -> Result<Option<Resp>> {
        let key = self.topic(topic.key().as_ref());
        let receiver = self
            .session()
            .get(owned_key(key.clone())?)
            .payload(serialize_payload(request)?)
            .encoding(typed_encoding(topic.schema()))
            .attachment(
                BusMetadata {
                    schema_version: topic.version(),
                    produced_at_ns: None,
                }
                .encode(),
            )
            .target(QueryTarget::DEFAULT)
            .consolidation(QueryConsolidation::DEFAULT)
            .allowed_destination(Locality::default())
            .accept_replies(ReplyKeyExpr::MatchingQuery)
            .wait()?;

        while let Ok(reply) = receiver.recv_async().await {
            match reply.into_result() {
                Ok(sample) => {
                    return Ok(Some(decode_response(
                        &sample,
                        &key,
                        topic.schema(),
                        topic.version(),
                    )?));
                }
                Err(reply_error) => {
                    return Err(Error::TypedDecode(format!(
                        "topic '{key}' schema '{}' returned an error reply with {} bytes",
                        topic.schema(),
                        reply_error.payload().to_bytes().len()
                    )));
                }
            }
        }
        Ok(None)
    }
}

fn concrete_publish_key<T>(topic: &Topic<PubSub<T>>) -> Result<Cow<'static, str>> {
    topic.publish_key()
}

async fn declare_subscriber(
    bus: &Bus,
    key: &str,
) -> Result<Subscriber<FifoChannelHandler<Sample>>> {
    Ok(bus
        .session()
        .declare_subscriber(owned_key(key.to_string())?)
        .await?)
}

fn owned_key(key: String) -> Result<OwnedKeyExpr> {
    OwnedKeyExpr::new(key).map_err(|error| Error::InvalidTopic(error.to_string()))
}

fn decode_sample<T: DeserializeOwned>(
    sample: &Sample,
    topic_key: &str,
    schema: &'static str,
    schema_version: u32,
) -> Result<Received<T>> {
    let metadata = validate_contract(
        Some(sample.encoding()),
        sample.attachment(),
        topic_key,
        schema,
        schema_version,
    )?;
    let value = deserialize_payload(sample.payload()).map_err(Error::from)?;
    Ok(Received {
        at_ns: metadata.produced_at_ns,
        value,
    })
}

fn decode_response<T: DeserializeOwned>(
    sample: &Sample,
    topic_key: &str,
    schema: &'static str,
    schema_version: u32,
) -> Result<T> {
    validate_contract(
        Some(sample.encoding()),
        sample.attachment(),
        topic_key,
        schema,
        schema_version,
    )?;
    deserialize_payload(sample.payload()).map_err(Error::from)
}

fn validate_contract(
    encoding: Option<&Encoding>,
    attachment: Option<&ZBytes>,
    topic_key: &str,
    schema: &'static str,
    schema_version: u32,
) -> Result<BusMetadata> {
    validate_encoding(encoding, topic_key, schema)?;
    let metadata = decode_metadata(attachment, topic_key, schema)?;
    if metadata.schema_version != schema_version {
        return Err(Error::TypedDecode(format!(
            "topic '{topic_key}' schema '{schema}' version mismatch: expected {schema_version}, received {}",
            metadata.schema_version
        )));
    }
    Ok(metadata)
}

fn validate_encoding(
    encoding: Option<&Encoding>,
    topic_key: &str,
    schema: &'static str,
) -> Result<()> {
    let expected = typed_encoding(schema);
    let Some(received) = encoding else {
        return Err(Error::TypedDecode(format!(
            "topic '{topic_key}' schema '{schema}' encoding mismatch: expected {expected}, received <none>"
        )));
    };

    if *received != expected {
        return Err(Error::TypedDecode(format!(
            "topic '{topic_key}' schema '{schema}' encoding mismatch: expected {expected}, received {received}"
        )));
    }
    Ok(())
}

fn decode_metadata(
    attachment: Option<&ZBytes>,
    topic_key: &str,
    schema: &'static str,
) -> Result<BusMetadata> {
    let attachment = attachment.ok_or_else(|| {
        Error::TypedDecode(format!(
            "topic '{topic_key}' schema '{schema}' is missing BusMetadata attachment"
        ))
    })?;
    BusMetadata::decode(attachment.to_bytes().as_ref()).map_err(|error| {
        Error::TypedDecode(format!(
            "topic '{topic_key}' schema '{schema}' has invalid BusMetadata attachment: {error}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use serde::{Deserialize, Serialize};
    use serial_test::serial;
    use tokio::time::timeout;
    use zenoh::{key_expr::KeyExpr, sample::SampleBuilder};

    use super::{concrete_publish_key, decode_sample};
    use crate::api::v1::{asset, component::capability::motor, topic};
    use crate::bus::metadata::BusMetadata;
    use crate::bus::query::Retry;
    use crate::bus::zenoh::{serialize_payload, typed_encoding};
    use crate::bus::{Bus, Error};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct TestPayload {
        value: u32,
    }

    #[test]
    fn typed_topic_keys_are_ready_for_bus_prefix_application() -> crate::bus::Result<()> {
        let concrete = topic::new()
            .v1()
            .component("base")
            .motor("left_wheel")
            .command();
        let wildcard = topic::new().v1().component_any().motor_any().command();

        assert_eq!(
            concrete_publish_key(&concrete)?.as_ref(),
            "v1/component/base/motor/left_wheel/command"
        );
        assert_eq!(wildcard.key().as_ref(), "v1/component/*/motor/*/command");
        assert_eq!(
            format!("robot-a/{}", concrete_publish_key(&concrete)?),
            "robot-a/v1/component/base/motor/left_wheel/command"
        );
        assert_eq!(
            format!("robot-a/{}", wildcard.key()),
            "robot-a/v1/component/*/motor/*/command"
        );
        Ok(())
    }

    #[test]
    fn typed_publish_rejects_wildcard_topic_before_transport() {
        let topic = topic::new().v1().component_any().motor_any().command();
        assert!(matches!(
            concrete_publish_key(&topic),
            Err(Error::InvalidTopic(_))
        ));
    }

    #[test]
    fn typed_decode_error_names_schema_mismatch_context() {
        let sample = sample_with_contract("other/schema", 1);
        let error = decode_sample::<TestPayload>(
            &sample,
            "v1/component/*/motor/*/command",
            "v1/component/motor/command",
            1,
        )
        .expect_err("schema mismatch should fail")
        .to_string();

        assert!(error.contains("v1/component/*/motor/*/command"));
        assert!(error.contains("v1/component/motor/command"));
        assert!(error.contains("other/schema"));
        assert!(error.contains("encoding mismatch"));
    }

    #[test]
    fn typed_decode_error_names_version_mismatch_context() {
        let sample = sample_with_contract("v1/component/motor/command", 2);
        let error = decode_sample::<TestPayload>(
            &sample,
            "v1/component/*/motor/*/command",
            "v1/component/motor/command",
            1,
        )
        .expect_err("version mismatch should fail")
        .to_string();

        assert!(error.contains("v1/component/*/motor/*/command"));
        assert!(error.contains("v1/component/motor/command"));
        assert!(error.contains("version mismatch"));
        assert!(error.contains("expected 1"));
        assert!(error.contains("received 2"));
    }

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn typed_live_pubsub_roundtrip_supports_wildcard_subscriber() -> crate::bus::Result<()> {
        let bus = open_bus("pubsub").await;
        let wildcard = topic::new().v1().component_any().motor_any().command();
        let wildcard_error = bus
            .publish(&wildcard, 1, &motor::Command::Velocity(0.0))
            .await
            .expect_err("publishing to a wildcard topic should fail");
        assert!(matches!(wildcard_error, Error::InvalidTopic(_)));

        let subscriber = bus.subscriber(&wildcard).await?;
        let concrete = topic::new()
            .v1()
            .component("base")
            .motor("left_wheel")
            .command();
        let now_ns = 42_000_000;
        let command = motor::Command::Velocity(0.75);

        bus.publish(&concrete, now_ns, &command).await?;

        let received = timeout(Duration::from_secs(5), subscriber.recv())
            .await
            .expect("typed subscriber should receive within timeout")?;
        assert_eq!(received.at_ns, Some(now_ns));
        match received.value {
            motor::Command::Velocity(value) => assert_eq!(value, 0.75),
            _ => panic!("expected velocity motor command"),
        }

        bus.close().await?;
        Ok(())
    }

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn typed_live_query_roundtrip_returns_response() -> crate::bus::Result<()> {
        let bus = open_bus("query").await;
        let topic = topic::new().v1().asset().get();
        let responder = bus.responder(&topic).await?;
        let request = asset::GetRequest {
            path: "fixture-map".to_string(),
        };
        let retry = Retry::new(1)
            .with_initial_backoff(Duration::from_millis(10))
            .with_max_backoff(Duration::from_millis(10));

        let (handled, response) = timeout(Duration::from_secs(5), async {
            let responder_task = async {
                let query = responder.recv().await?;
                let request = query.request()?;
                query
                    .reply(&asset::GetResponse::Ok {
                        bytes: vec![1, 2, 3, 5, 8],
                    })
                    .await?;
                crate::bus::Result::Ok(request)
            };
            tokio::join!(responder_task, bus.request(&topic, &request, &retry))
        })
        .await
        .expect("typed query should complete within timeout");

        assert_eq!(handled?, request);
        assert_eq!(
            response?,
            Some(asset::GetResponse::Ok {
                bytes: vec![1, 2, 3, 5, 8]
            })
        );

        bus.close().await?;
        Ok(())
    }

    fn sample_with_contract(schema: &'static str, version: u32) -> zenoh::sample::Sample {
        let payload =
            serialize_payload(&TestPayload { value: 42 }).expect("test payload should serialize");
        let key: KeyExpr<'static> = KeyExpr::try_from("v1/component/base/motor/left_wheel/command")
            .expect("test key should be valid");
        SampleBuilder::put(key, payload)
            .encoding(typed_encoding(schema))
            .attachment(
                BusMetadata {
                    schema_version: version,
                    produced_at_ns: Some(9),
                }
                .encode(),
            )
            .into()
    }

    async fn open_bus(suffix: &str) -> Bus {
        let session = zenoh::open(local_test_config())
            .await
            .expect("test zenoh session should open");
        Bus::new(session, unique_prefix(suffix))
    }

    fn local_test_config() -> zenoh::Config {
        let mut config = zenoh::Config::default();
        config.insert_json5("listen/endpoints", "[]").unwrap();
        config
            .insert_json5("scouting/multicast/enabled", "false")
            .unwrap();
        config
    }

    fn unique_prefix(suffix: &str) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("phoxal/tests/typed/{suffix}/{nanos}")
    }
}
