use std::future::Future;
use std::time::Duration;

use derive_new::new;
use derive_setters::Setters;
use serde::{Serialize, de::DeserializeOwned};
use tokio::time::sleep;
use tracing::warn;
use zenoh::key_expr::OwnedKeyExpr;

use crate::bus::zenoh_typed::{
    TypedGetBuilder, TypedGetError, TypedQueryable, TypedQueryableBuilder, TypedSchema,
    TypedSessionExt,
};
use crate::bus::{Bus, Error, Result};

#[derive(Debug, Clone, new, Setters)]
#[setters(prefix = "with_")]
pub struct Retry {
    #[setters(skip)]
    pub max_attempts: u32,

    #[new(value = "Duration::from_millis(100)")]
    pub initial_backoff: Duration,

    #[new(value = "Duration::from_secs(2)")]
    pub max_backoff: Duration,

    #[new(value = "true")]
    pub retry_on_no_reply: bool,

    #[new(value = "true")]
    pub retry_on_transport_error: bool,
}

impl Retry {
    fn validate(&self) -> crate::bus::Result<()> {
        if self.max_attempts == 0 {
            return Err(crate::bus::Error::InvalidRetry(
                "max_attempts must be greater than zero".to_string(),
            ));
        }
        if self.initial_backoff.is_zero() {
            return Err(crate::bus::Error::InvalidRetry(
                "initial_backoff must be greater than zero".to_string(),
            ));
        }
        if self.max_backoff.is_zero() {
            return Err(crate::bus::Error::InvalidRetry(
                "max_backoff must be greater than zero".to_string(),
            ));
        }
        if self.initial_backoff > self.max_backoff {
            return Err(crate::bus::Error::InvalidRetry(
                "initial_backoff must be less than or equal to max_backoff".to_string(),
            ));
        }
        Ok(())
    }
}

pub async fn retry_query<T, F, Fut>(
    name: &str,
    retry: &Retry,
    mut attempt: F,
) -> crate::bus::Result<Option<T>>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = crate::bus::Result<Option<T>>>,
{
    retry.validate()?;

    let mut backoff = retry.initial_backoff;
    for attempt_index in 1..=retry.max_attempts {
        match attempt().await {
            Ok(Some(response)) => return Ok(Some(response)),
            Ok(None) => {
                if !retry.retry_on_no_reply || attempt_index == retry.max_attempts {
                    return Ok(None);
                }
                warn!(
                    query = name,
                    attempt = attempt_index,
                    max_attempts = retry.max_attempts,
                    backoff_ms = backoff.as_millis(),
                    "query returned no reply, retrying"
                );
            }
            Err(crate::bus::Error::Transport(error)) => {
                if !retry.retry_on_transport_error || attempt_index == retry.max_attempts {
                    return Err(crate::bus::Error::Transport(error));
                }
                warn!(
                    query = name,
                    attempt = attempt_index,
                    max_attempts = retry.max_attempts,
                    backoff_ms = backoff.as_millis(),
                    error = %error,
                    "query failed with transport error, retrying"
                );
            }
            Err(error) => return Err(error),
        }

        sleep(backoff).await;
        backoff = std::cmp::min(backoff.saturating_mul(2), retry.max_backoff);
    }

    Ok(None)
}

pub fn get_builder<'a, Req, Resp>(
    bus: &'a Bus,
    topic: &str,
    request: &'a Req,
) -> TypedGetBuilder<'a, 'static, Resp>
where
    Req: Serialize + TypedSchema,
    Resp: DeserializeOwned + TypedSchema,
{
    bus.session()
        .typed_get_builder::<Req, Resp, _>(bus.topic(topic), request)
}

pub async fn get<Req, Resp>(
    bus: &Bus,
    topic: &str,
    request: &Req,
) -> Result<Vec<std::result::Result<Resp, TypedGetError>>>
where
    Req: Serialize + TypedSchema,
    Resp: DeserializeOwned + TypedSchema,
{
    get_builder::<Req, Resp>(bus, topic, request)
        .await
        .map_err(Error::from)
}

pub async fn query<Req, Resp>(
    bus: &Bus,
    topic: &str,
    request: &Req,
    retry: &Retry,
) -> Result<Option<Resp>>
where
    Req: Serialize + TypedSchema,
    Resp: DeserializeOwned + TypedSchema,
{
    retry_query(topic, retry, || async {
        let mut replies = get::<Req, Resp>(bus, topic, request).await?.into_iter();
        match replies.next() {
            Some(Ok(response)) => Ok(Some(response)),
            Some(Err(error)) => Err(Error::from(error)),
            None => Ok(None),
        }
    })
    .await
}

pub fn queryable_builder<'a, Req, Resp>(
    bus: &'a Bus,
    topic: &str,
) -> Result<TypedQueryableBuilder<'a, 'static, Req, Resp>>
where
    Req: DeserializeOwned + TypedSchema,
    Resp: Serialize + TypedSchema,
{
    let topic = OwnedKeyExpr::new(bus.topic(topic))
        .map_err(|error| Error::InvalidTopic(error.to_string()))?;
    Ok(bus.session().declare_typed_queryable::<Req, Resp, _>(topic))
}

pub async fn queryable<Req, Resp>(bus: &Bus, topic: &str) -> Result<TypedQueryable<Req, Resp>>
where
    Req: DeserializeOwned + TypedSchema,
    Resp: Serialize + TypedSchema,
{
    queryable_builder::<Req, Resp>(bus, topic)?
        .await
        .map_err(Error::from)
}

#[cfg(test)]
mod tests {
    use super::{Retry, retry_query};
    use crate::bus::{Error, Result};
    use std::sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    };
    use std::time::Duration;

    #[tokio::test]
    async fn retry_query_retries_no_reply_until_success() -> Result<()> {
        let attempts = Arc::new(AtomicU32::new(0));
        let retry = Retry::new(3).with_initial_backoff(Duration::from_millis(1));
        let result = retry_query("robot/r1/asset/bundle", &retry, {
            let attempts = attempts.clone();
            move || {
                let attempts = attempts.clone();
                async move {
                    let current = attempts.fetch_add(1, Ordering::SeqCst);
                    if current < 2 {
                        Ok(None)
                    } else {
                        Ok(Some("model"))
                    }
                }
            }
        })
        .await?;

        assert_eq!(result, Some("model"));
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        Ok(())
    }

    #[tokio::test]
    async fn retry_query_retries_transport_errors_until_success() -> Result<()> {
        let attempts = Arc::new(AtomicU32::new(0));
        let retry = Retry::new(2).with_initial_backoff(Duration::from_millis(1));
        let result = retry_query("robot/r1/asset/bundle", &retry, {
            let attempts = attempts.clone();
            move || {
                let attempts = attempts.clone();
                async move {
                    let current = attempts.fetch_add(1, Ordering::SeqCst);
                    if current == 0 {
                        Err(Error::Transport(std::io::Error::other("transient").into()))
                    } else {
                        Ok(Some("model"))
                    }
                }
            }
        })
        .await?;

        assert_eq!(result, Some("model"));
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        Ok(())
    }

    #[tokio::test]
    async fn retry_query_rejects_invalid_retry() {
        let error = retry_query::<(), _, _>("robot/r1/asset/bundle", &Retry::new(0), || async {
            Ok(Some(()))
        })
        .await
        .expect_err("invalid retry should fail");
        assert!(matches!(error, Error::InvalidRetry(_)));
    }
}
