use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use serde::{Serialize, de::DeserializeOwned};
use tokio::task::JoinHandle;

use crate::bus::Bus;
use crate::bus::topic::{PubSub, Query as TopicQuery, Topic};
use crate::bus::typed::{Received, TypedTopicSubscriber};
use crate::bus::zenoh::BusyResponse;
use crate::runtime::input::{
    InputPolicy, RecordingBuffer, SourceBuffer, SourceHandle, TypedRecordingBuffer,
    push_source_input,
};
use crate::runtime::query::{QueryOptions, Reader, spawn_topic_query_responder};

pub struct Io<Input> {
    bus: Option<Bus>,
    handles: Vec<JoinHandle<()>>,
    sources: Vec<SourceHandle<Input>>,
    recorded_puts: HashMap<String, Arc<dyn RecordingBuffer>>,
}

impl<Input> Io<Input> {
    pub fn recording() -> Self {
        Self {
            bus: None,
            handles: Vec::new(),
            sources: Vec::new(),
            recorded_puts: HashMap::new(),
        }
    }

    pub fn recorded_puts<T: Clone + 'static>(&self, topic: &str) -> Vec<T> {
        self.recorded_puts
            .get(topic)
            .and_then(|buffer| buffer.dyn_value().downcast_ref::<TypedRecordingBuffer<T>>())
            .and_then(|buffer| buffer.values.lock().ok().map(|values| values.clone()))
            .unwrap_or_default()
    }
}

impl<Input: Send + 'static> Io<Input> {
    pub(super) fn live(bus: Bus, _runtime_id: &'static str) -> Self {
        Self {
            bus: Some(bus),
            handles: Vec::new(),
            sources: Vec::new(),
            recorded_puts: HashMap::new(),
        }
    }

    pub(super) fn into_parts(self) -> (Vec<JoinHandle<()>>, Vec<SourceHandle<Input>>) {
        (self.handles, self.sources)
    }

    pub fn bus(&self) -> Result<Bus> {
        self.bus
            .clone()
            .ok_or_else(|| anyhow!("live bus is unavailable in recording IO"))
    }

    pub async fn subscribe_topic<T, F>(&mut self, topic: Topic<PubSub<T>>, map: F) -> Result<()>
    where
        T: DeserializeOwned + Send + Sync + 'static,
        F: Fn(Received<T>) -> Input + Send + 'static,
    {
        self.subscribe_topic_with(topic, InputPolicy::All, map)
            .await
    }

    pub async fn subscribe_topic_with<T, F>(
        &mut self,
        topic: Topic<PubSub<T>>,
        policy: InputPolicy,
        map: F,
    ) -> Result<()>
    where
        T: DeserializeOwned + Send + Sync + 'static,
        F: Fn(Received<T>) -> Input + Send + 'static,
    {
        if let Some(bus) = &self.bus {
            let source = Arc::new(Mutex::new(SourceBuffer::new(policy)));
            self.sources.push(source.clone());
            self.handles.push(spawn_topic_subscription_forwarder(
                bus.subscriber(&topic).await?,
                source,
                map,
            ));
        }
        Ok(())
    }

    pub async fn serve_query_topic<Req, Resp, V, F>(
        &mut self,
        topic: Topic<TopicQuery<Req, Resp>>,
        reader: Reader<V>,
        options: QueryOptions,
        handler: F,
    ) -> Result<()>
    where
        Req: DeserializeOwned + Send + Sync + 'static,
        Resp: Serialize + BusyResponse + Send + Sync + 'static,
        V: Send + Sync + 'static,
        F: Fn(&V, Req) -> Resp + Send + Sync + 'static,
    {
        if let Some(bus) = &self.bus {
            self.handles.push(spawn_topic_query_responder(
                bus.responder(&topic).await?,
                reader,
                options,
                handler,
            ));
        }
        Ok(())
    }

    pub async fn publisher_topic<T>(&mut self, topic: Topic<PubSub<T>>) -> Result<TopicPublisher<T>>
    where
        T: Serialize + Clone + Send + 'static,
    {
        let key = topic.publish_key()?.into_owned();

        if let Some(bus) = &self.bus {
            return Ok(TopicPublisher {
                inner: TopicPublisherInner::Live {
                    bus: bus.clone(),
                    topic,
                },
            });
        }

        let values = Arc::new(Mutex::new(Vec::new()));
        self.recorded_puts.insert(
            key,
            Arc::new(TypedRecordingBuffer {
                values: values.clone(),
            }),
        );
        Ok(TopicPublisher {
            inner: TopicPublisherInner::Recording(values),
        })
    }
}

pub struct TopicPublisher<T>
where
    T: Serialize + Clone,
{
    inner: TopicPublisherInner<T>,
}

enum TopicPublisherInner<T>
where
    T: Serialize + Clone,
{
    Live { bus: Bus, topic: Topic<PubSub<T>> },
    Recording(Arc<Mutex<Vec<T>>>),
}

impl<T> TopicPublisher<T>
where
    T: Serialize + Clone,
{
    pub async fn put(&self, at_ns: u64, data: &T) -> Result<()> {
        match &self.inner {
            TopicPublisherInner::Live { bus, topic } => {
                bus.publish(topic, at_ns, data).await.map_err(Into::into)
            }
            TopicPublisherInner::Recording(values) => {
                values
                    .lock()
                    .map_err(|error| anyhow!("recorded publisher lock poisoned: {error}"))?
                    .push(data.clone());
                Ok(())
            }
        }
    }
}

fn spawn_topic_subscription_forwarder<T, U, F>(
    subscriber: TypedTopicSubscriber<T>,
    source: SourceHandle<U>,
    map: F,
) -> JoinHandle<()>
where
    T: DeserializeOwned + Send + Sync + 'static,
    U: Send + 'static,
    F: Fn(Received<T>) -> U + Send + 'static,
{
    tokio::spawn(async move {
        loop {
            match subscriber.recv().await {
                Ok(received) => {
                    push_source_input(&source, map(received));
                }
                Err(crate::bus::Error::TypedDecode(error)) => {
                    tracing::warn!(
                        %error,
                        payload_type = std::any::type_name::<T>(),
                        "failed to decode typed topic payload"
                    );
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        payload_type = std::any::type_name::<T>(),
                        "failed to receive typed topic payload"
                    );
                    return;
                }
            }
        }
    })
}
