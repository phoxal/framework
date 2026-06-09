use zenoh::{
    Result as ZResult,
    handlers::FifoChannelHandler,
    key_expr::OwnedKeyExpr,
    liveliness::LivelinessToken,
    pubsub::Subscriber,
    sample::{Sample, SampleKind},
};

use crate::bus::Bus;

/// Event for a liveliness key appearing or disappearing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LivelinessEvent {
    Alive(String),
    Dropped(String),
}

/// Declare a liveliness token for a key.
///
/// The announcement remains alive until the returned token is dropped or
/// undeclared.
pub async fn declare_liveliness_token(bus: &Bus, key: &str) -> ZResult<LivelinessToken> {
    let key_expr = OwnedKeyExpr::new(bus.topic(key))?;
    bus.session().liveliness().declare_token(key_expr).await
}

/// Subscribe to liveliness changes under a key expression.
///
/// `key_prefix` is a Zenoh key expression, so wildcard prefixes such as
/// `component/front_camera/rgb/profile/**` are accepted.
pub async fn liveliness_subscriber(bus: &Bus, key_prefix: &str) -> ZResult<LivelinessSubscriber> {
    let key_expr = OwnedKeyExpr::new(bus.topic(key_prefix))?;
    let inner = bus
        .session()
        .liveliness()
        .declare_subscriber(key_expr)
        .history(true)
        .await?;
    Ok(LivelinessSubscriber { inner })
}

pub struct LivelinessSubscriber {
    inner: Subscriber<FifoChannelHandler<Sample>>,
}

impl LivelinessSubscriber {
    pub async fn recv(&self) -> ZResult<LivelinessEvent> {
        self.recv_async().await
    }

    pub async fn recv_async(&self) -> ZResult<LivelinessEvent> {
        let sample = self.inner.recv_async().await?;
        Ok(event_from_sample(&sample))
    }

    pub fn key_expr(&self) -> &zenoh::key_expr::KeyExpr<'static> {
        self.inner.key_expr()
    }
}

fn event_from_sample(sample: &Sample) -> LivelinessEvent {
    let key = sample.key_expr().as_str().to_string();
    match sample.kind() {
        SampleKind::Put => LivelinessEvent::Alive(key),
        SampleKind::Delete => LivelinessEvent::Dropped(key),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use serial_test::serial;
    use tokio::time::timeout;

    use super::{LivelinessEvent, declare_liveliness_token, liveliness_subscriber};
    use crate::bus::Bus;

    fn unique_key(suffix: &str) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("phoxal/tests/liveliness/{suffix}/{nanos}")
    }

    fn local_test_config() -> zenoh::Config {
        let mut config = zenoh::Config::default();
        config.insert_json5("listen/endpoints", "[]").unwrap();
        config
            .insert_json5("scouting/multicast/enabled", "false")
            .unwrap();
        config
    }

    async fn open_bus() -> Bus {
        let session = zenoh::open(local_test_config())
            .await
            .expect("test zenoh session should open");
        Bus::new(session, String::new())
    }

    #[serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn liveliness_subscriber_receives_alive_and_dropped_events() {
        let bus = open_bus().await;
        let prefix = unique_key("profile");
        let token_key = format!("{prefix}/rgb_640x480_30");
        let subscriber = liveliness_subscriber(&bus, &format!("{prefix}/**"))
            .await
            .expect("liveliness subscriber should declare");

        let token = declare_liveliness_token(&bus, &token_key)
            .await
            .expect("liveliness token should declare");

        let alive = timeout(Duration::from_secs(5), subscriber.recv())
            .await
            .expect("subscriber should receive alive event")
            .expect("liveliness subscriber should stay open");
        assert_eq!(alive, LivelinessEvent::Alive(token_key.clone()));

        drop(token);

        let dropped = timeout(Duration::from_secs(5), subscriber.recv())
            .await
            .expect("subscriber should receive dropped event")
            .expect("liveliness subscriber should stay open");
        assert_eq!(dropped, LivelinessEvent::Dropped(token_key));

        drop(subscriber);
        bus.close().await.expect("bus should close");
    }
}
