use super::*;

pub(super) async fn declare<E: QueryEndpoint>(
    bus: &BusHandle,
    topic: &Topic<ServeQuery<E>>,
) -> Result<ServerQueryable> {
    Ok(bus.declare_server(topic.key()).await?)
}
pub(super) async fn decode<T: serde::de::DeserializeOwned>(incoming: &IncomingQuery) -> Result<Option<T>> {
    match MessagePack::decode(&incoming.request_bytes()?) {
        Ok(request) => Ok(Some(request)),
        Err(error) => {
            incoming
                .reply_err(&QueryFailure::invalid_argument(error.to_string()))
                .await?;
            Ok(None)
        }
    }
}

pub(super) async fn reply<T: serde::Serialize>(
    incoming: &IncomingQuery,
    bus: &BusHandle,
    response: &T,
) -> Result<()> {
    incoming
        .reply(bus, MessagePack::encode(response)?)
        .await
        .map_err(Into::into)
}
