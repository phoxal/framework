use serde::{Deserialize, Serialize, de::DeserializeOwned};
use zenoh::key_expr::OwnedKeyExpr;

use crate::bus::zenoh::{
    TypedPublisher, TypedPublisherBuilder, TypedSchema, TypedSessionExt, TypedSubscriber,
    TypedSubscriberBuilder,
};
use crate::bus::{Bus, Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamped<T> {
    pub timestamp_ns: u64,
    pub data: T,
}

impl<T> Stamped<T> {
    pub fn new(timestamp_ns: u64, data: T) -> Self {
        Self { timestamp_ns, data }
    }
}

pub fn publisher_builder<'a, T>(
    bus: &'a Bus,
    topic: &str,
) -> Result<TypedPublisherBuilder<'a, 'static, T>>
where
    T: Serialize + TypedSchema,
{
    let topic = OwnedKeyExpr::new(bus.topic(topic))
        .map_err(|error| Error::InvalidTopic(error.to_string()))?;
    Ok(bus
        .session()
        .declare_typed_publisher::<T, _>(topic)
        .schema_version(T::SCHEMA_VERSION))
}

pub async fn publisher<T>(bus: &Bus, topic: &str) -> Result<TypedPublisher<'static, T>>
where
    T: Serialize + TypedSchema,
{
    let topic = OwnedKeyExpr::new(bus.topic(topic))
        .map_err(|error| Error::InvalidTopic(error.to_string()))?;
    bus.cloned_session()
        .declare_typed_publisher::<T, _>(topic)
        .schema_version(T::SCHEMA_VERSION)
        .await
        .map_err(|error| Error::InvalidTopic(error.to_string()))
}

pub fn eager_publisher_builder<'a, T>(
    bus: &'a Bus,
    topic: &str,
) -> Result<TypedPublisherBuilder<'a, 'static, T>>
where
    T: Serialize + TypedSchema,
{
    publisher_builder::<T>(bus, topic).map(TypedPublisherBuilder::eager)
}

pub async fn eager_publisher<T>(bus: &Bus, topic: &str) -> Result<TypedPublisher<'static, T>>
where
    T: Serialize + TypedSchema,
{
    let topic = OwnedKeyExpr::new(bus.topic(topic))
        .map_err(|error| Error::InvalidTopic(error.to_string()))?;
    bus.cloned_session()
        .declare_typed_publisher::<T, _>(topic)
        .schema_version(T::SCHEMA_VERSION)
        .eager()
        .await
        .map_err(|error| Error::InvalidTopic(error.to_string()))
}

pub fn subscriber_builder<'a, T>(
    bus: &'a Bus,
    topic: &str,
) -> TypedSubscriberBuilder<'a, 'static, T>
where
    T: DeserializeOwned + TypedSchema,
{
    bus.session()
        .declare_typed_subscriber::<T, _>(bus.topic(topic))
        .schema_version(T::SCHEMA_VERSION)
}

pub async fn subscribe<T>(bus: &Bus, topic: &str) -> Result<TypedSubscriber<T>>
where
    T: DeserializeOwned + TypedSchema,
{
    subscriber_builder::<T>(bus, topic)
        .await
        .map_err(|error| Error::InvalidTopic(error.to_string()))
}
