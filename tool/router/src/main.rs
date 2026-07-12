use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use phoxal::prelude::*;
use phoxal::raw::{Bus, BusMetadata, LogicalTime, OwnerCap, Publisher};
use phoxal_api::y2026_1 as api;
use phoxal_api::y2026_9 as api9;

const DEFAULT_LISTEN: &str = "tcp/localhost:7447";
const DEFAULT_RETRY_INITIAL_MS: u64 = 1_000;
const DEFAULT_RETRY_MAX_MS: u64 = 30_000;
const SERIAL_LISTEN_TRANSPORT_DIAGNOSTIC: &str = "serial_listen_transport_unavailable";

/// The mirror-subscription measurement window: per-topic counts are rolled up
/// and republished on this cadence.
const METRICS_WINDOW: Duration = Duration::from_secs(1);
/// Scopes the mirror subscription to phoxal's generation-qualified application
/// traffic (`y2026_*/...`, wherever it falls under a namespace/robot key
/// root) and away from Zenoh's own admin/liveliness keys - bounding what the
/// router pays to mirror-inspect.
const MIRROR_KEY_EXPR: &str = "**/y2026_*/**";

/// Launch-time router configuration carried in `PHOXAL_CONFIG`.
#[derive(Clone, Debug, Default, serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
struct RouterConfig {
    /// Additional listen endpoints merged by the launch plan from `bus.listen`.
    #[serde(default)]
    listen: Vec<String>,
    /// Optional upstream router connection for deployed robots.
    #[serde(default)]
    uplink: Option<UplinkConfig>,
}

/// Optional upstream connection configuration for the site router.
#[derive(Clone, Debug, serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
struct UplinkConfig {
    /// Upstream Zenoh endpoint the router should connect to.
    connect: String,
    /// Optional mTLS material by path, installed by deploy outside the release dir.
    #[serde(default)]
    auth: Option<MtlsAuth>,
    /// Capped retry backoff; defaults retry forever and never gates readiness.
    #[serde(default)]
    retry: RetryConfig,
}

/// mTLS material used when connecting the router uplink.
#[derive(Clone, Debug, serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
struct MtlsAuth {
    /// Root CA certificate path used to verify the upstream router.
    ca: String,
    /// Client certificate path identifying this robot/site to the upstream.
    cert: String,
    /// Client private-key path paired with `cert`.
    key: String,
}

/// Capped backoff configuration for the optional uplink.
#[derive(Clone, Debug, serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
struct RetryConfig {
    /// Initial retry delay in milliseconds.
    #[serde(default = "default_retry_initial_ms")]
    initial_ms: u64,
    /// Maximum retry delay in milliseconds.
    #[serde(default = "default_retry_max_ms")]
    max_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            initial_ms: DEFAULT_RETRY_INITIAL_MS,
            max_ms: DEFAULT_RETRY_MAX_MS,
        }
    }
}

fn default_retry_initial_ms() -> u64 {
    DEFAULT_RETRY_INITIAL_MS
}

fn default_retry_max_ms() -> u64 {
    DEFAULT_RETRY_MAX_MS
}

// Tools stay raw-bus only (decided 2026-07-09): no declared `Api` surface,
// just `ctx.raw_bus()` and the raw handle constructors.
#[phoxal::tool(id = "router", config = RouterConfig)]
struct ToolRouter {
    router: zenoh::Session,
}

#[phoxal::behavior]
impl ToolRouter {
    #[setup]
    async fn setup(
        ctx: &mut SetupContext<Self>,
        config: RouterConfig,
    ) -> Result<(Self, Self::Api)> {
        let zenoh_config = zenoh_router_config(&config)?;
        let router = zenoh::open(zenoh_config)
            .await
            .map_err(|error| anyhow::anyhow!("failed to open embedded Zenoh router: {error}"))?;

        let state_publisher = uplink_state_publisher(ctx.raw_bus());
        match &state_publisher {
            Ok(publisher) => {
                publish_uplink_state(publisher, initial_uplink_state(&config)).await?;
            }
            Err(error) => {
                tracing::warn!(target: "tool_router", error = %error, "uplink state publisher unavailable");
            }
        }

        spawn_metrics(ctx, &router).await?;

        // Dynamic egress downsampling belongs here later: this process owns the
        // uplink session and can re-establish it with refreshed rules.
        tracing::info!(
            target: "tool_router",
            listen = ?listen_endpoints(&config),
            uplink = config.uplink.as_ref().map(|uplink| uplink.connect.as_str()),
            "router ready"
        );

        Ok((Self { router }, ()))
    }

    #[shutdown]
    async fn shutdown(&mut self, _api: &mut Self::Api, _ctx: ShutdownContext) -> Result<()> {
        if let Err(error) = self.router.close().await {
            tracing::warn!(target: "tool_router", error = %error, "router close failed");
        }
        Ok(())
    }
}

/// Per-topic ingress counters, keyed by the full Zenoh key observed on the
/// mirror subscription. `cumulative` never resets (the contract's `count`
/// field); `window` resets every [`METRICS_WINDOW`] tick (feeds
/// `ingress_rate_hz`).
#[derive(Default)]
struct TopicCounter {
    cumulative: u64,
    window: u64,
    from_participant: String,
}

/// Shared ingest state: one background task increments it per received
/// sample, another drains + resets the window count on a timer.
type IngressCounters = Mutex<HashMap<String, TopicCounter>>;

/// A drained window's worth of one topic's ingress, ready to become a
/// `TopicMetric`.
struct TopicWindowSample {
    topic: String,
    from_participant: String,
    window_count: u64,
    cumulative_count: u64,
}

/// Declare the wildcard mirror subscription on the embedded router's own
/// session and spawn the two managed tasks that turn it into
/// `router::Metrics`: one ingests samples into the shared counters, the
/// other drains + publishes on [`METRICS_WINDOW`].
///
/// This mirror subscription adds bus traffic proportional to everything it
/// observes - the accepted tradeoff of measuring ingress this way (there is
/// no cheaper vantage point that sees traffic actually transiting the
/// router). It does not amplify itself: the publish cadence is timer-driven,
/// not triggered by inbound samples, so `Metrics` being itself mirrored and
/// counted (like any other topic) never feeds back into more publishes.
async fn spawn_metrics(ctx: &mut SetupContext<ToolRouter>, router: &zenoh::Session) -> Result<()> {
    let mirror_subscriber = router
        .declare_subscriber(MIRROR_KEY_EXPR)
        .await
        .map_err(|error| anyhow::anyhow!("failed to declare router mirror subscriber: {error}"))?;

    let counters: Arc<IngressCounters> = Arc::new(Mutex::new(HashMap::new()));

    let ingest_counters = Arc::clone(&counters);
    ctx.spawn_managed_with(
        "router-metrics-ingest",
        ManagedTaskPolicy::FaultOnExit,
        async move {
            while let Ok(sample) = mirror_subscriber.recv_async().await {
                let topic = sample.key_expr().to_string();
                let from_participant = participant_from_attachment(&sample);
                record_sample(&ingest_counters, topic, from_participant);
            }
        },
    );

    let cap = ctx.owner_capability();
    let metrics_publisher = Publisher::new(
        ctx.raw_bus(),
        &api9::topic::internal::new(cap).router().metrics(),
    )?;
    let publish_counters = Arc::clone(&counters);
    ctx.spawn_managed_with(
        "router-metrics-publish",
        ManagedTaskPolicy::FaultOnExit,
        async move {
            let mut ticker = tokio::time::interval(METRICS_WINDOW);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let samples = drain_window(&publish_counters);
                let metrics = build_metrics(&samples, METRICS_WINDOW);
                if let Err(error) = metrics_publisher.publish_at(now(), metrics).await {
                    tracing::warn!(target: "tool_router", error = %error, "router metrics publish failed");
                }
            }
        },
    );

    Ok(())
}

/// Best-effort producer id for one sample, decoded from the phoxal
/// `BusMetadata` attachment if present and well-formed. `None` (not an
/// error) when the attachment is absent, malformed, or empty - not every
/// mirrored key carries phoxal's envelope, and the measured rate/count stay
/// honest either way.
fn participant_from_attachment(sample: &zenoh::sample::Sample) -> Option<String> {
    let attachment = sample.attachment()?;
    let metadata = BusMetadata::decode(attachment.to_bytes().as_ref()).ok()?;
    if metadata.source.participant.is_empty() {
        None
    } else {
        Some(metadata.source.participant)
    }
}

fn record_sample(counters: &IngressCounters, topic: String, from_participant: Option<String>) {
    let mut guard = counters.lock().expect("ingress counters mutex poisoned");
    let entry = guard.entry(topic).or_default();
    entry.cumulative += 1;
    entry.window += 1;
    if let Some(participant) = from_participant {
        entry.from_participant = participant;
    }
}

/// Snapshot every topic's window count (resetting it to 0) and cumulative
/// count (left untouched).
fn drain_window(counters: &IngressCounters) -> Vec<TopicWindowSample> {
    let mut guard = counters.lock().expect("ingress counters mutex poisoned");
    guard
        .iter_mut()
        .map(|(topic, counter)| {
            let sample = TopicWindowSample {
                topic: topic.clone(),
                from_participant: counter.from_participant.clone(),
                window_count: counter.window,
                cumulative_count: counter.cumulative,
            };
            counter.window = 0;
            sample
        })
        .collect()
}

/// Turn one window's drained samples into the wire `Metrics` state.
/// `ingress_rate_hz` is each topic's window count over the window length in
/// seconds; `throughput_msg_s` sums those rates; `count` is the cumulative
/// (all-time) total, unaffected by the window reset.
fn build_metrics(samples: &[TopicWindowSample], window: Duration) -> api9::router::Metrics {
    let window_secs = window.as_secs_f32();
    let mut topics = Vec::with_capacity(samples.len());
    let mut throughput_msg_s = 0.0f32;
    for sample in samples {
        let ingress_rate_hz = if window_secs > 0.0 {
            sample.window_count as f32 / window_secs
        } else {
            0.0
        };
        throughput_msg_s += ingress_rate_hz;
        topics.push(api9::router::TopicMetric {
            topic: sample.topic.clone(),
            from_participant: sample.from_participant.clone(),
            ingress_rate_hz,
            count: sample.cumulative_count,
        });
    }
    api9::router::Metrics {
        topics,
        throughput_msg_s,
        window_ns: u64::try_from(window.as_nanos()).unwrap_or(u64::MAX),
    }
}

fn zenoh_router_config(config: &RouterConfig) -> Result<zenoh::Config> {
    let listen = listen_endpoints(config);
    reject_unsupported_serial_listen(&listen)?;

    let mut zenoh_config = zenoh::Config::default();
    insert(&mut zenoh_config, "mode", r#""router""#)?;
    insert_json(&mut zenoh_config, "listen/endpoints", &listen)?;
    insert(&mut zenoh_config, "scouting/multicast/enabled", "false")?;
    insert(&mut zenoh_config, "scouting/gossip/enabled", "false")?;
    insert(
        &mut zenoh_config,
        "scouting/multicast/autoconnect",
        r#"{ router: [], peer: [], client: [] }"#,
    )?;
    insert(
        &mut zenoh_config,
        "scouting/gossip/autoconnect",
        r#"{ router: [], peer: [], client: [] }"#,
    )?;
    if let Some(uplink) = &config.uplink {
        insert_json(
            &mut zenoh_config,
            "connect/endpoints",
            &vec![uplink.connect.clone()],
        )?;
        insert(
            &mut zenoh_config,
            "connect/retry",
            &format!(
                "{{ period_init_ms: {}, period_max_ms: {}, period_increase_factor: 2.0, exit_on_failure: false }}",
                uplink.retry.initial_ms, uplink.retry.max_ms
            ),
        )?;
        insert(&mut zenoh_config, "connect/exit_on_failure", "false")?;
        if let Some(auth) = &uplink.auth {
            insert(&mut zenoh_config, "transport/link/tls/enable_mtls", "true")?;
            insert_json(
                &mut zenoh_config,
                "transport/link/tls/root_ca_certificate",
                &auth.ca,
            )?;
            insert_json(
                &mut zenoh_config,
                "transport/link/tls/connect_certificate",
                &auth.cert,
            )?;
            insert_json(
                &mut zenoh_config,
                "transport/link/tls/connect_private_key",
                &auth.key,
            )?;
        }
    } else {
        insert(&mut zenoh_config, "connect/endpoints", "[]")?;
    }
    Ok(zenoh_config)
}

fn reject_unsupported_serial_listen(endpoints: &[String]) -> Result<()> {
    for endpoint in endpoints {
        if endpoint.trim().starts_with("serial/") {
            anyhow::bail!(
                "{SERIAL_LISTEN_TRANSPORT_DIAGNOSTIC}: serial listen endpoints require the serial transport feature, not enabled in this build (endpoint: {endpoint})"
            );
        }
    }
    Ok(())
}

fn insert(config: &mut zenoh::Config, path: &str, json5: &str) -> Result<()> {
    config
        .insert_json5(path, json5)
        .map_err(|error| anyhow::anyhow!("failed to set Zenoh config path {path}: {error}"))
}

fn insert_json<T: serde::Serialize>(
    config: &mut zenoh::Config,
    path: &str,
    value: &T,
) -> Result<()> {
    let json = serde_json::to_string(value)?;
    insert(config, path, &json)
}

fn listen_endpoints(config: &RouterConfig) -> Vec<String> {
    let mut endpoints = vec![DEFAULT_LISTEN.to_string()];
    for endpoint in &config.listen {
        if !endpoints.contains(endpoint) {
            endpoints.push(endpoint.clone());
        }
    }
    endpoints
}

fn uplink_state_publisher(bus: Bus) -> Result<Publisher<api::bus::uplink::State>> {
    let topic = api::topic::internal::new(OwnerCap::__mint())
        .bus()
        .uplink()
        .state();
    let publisher = Publisher::new(bus, &topic)?;
    Ok(publisher)
}

fn initial_uplink_state(config: &RouterConfig) -> api::bus::uplink::State {
    match &config.uplink {
        Some(uplink) => api::bus::uplink::State {
            phase: api::bus::uplink::UplinkPhase::Connecting,
            connect: Some(uplink.connect.clone()),
            retry_attempt: 0,
            detail: None,
        },
        None => api::bus::uplink::State {
            phase: api::bus::uplink::UplinkPhase::Disabled,
            connect: None,
            retry_attempt: 0,
            detail: None,
        },
    }
}

async fn publish_uplink_state(
    publisher: &Publisher<api::bus::uplink::State>,
    state: api::bus::uplink::State,
) -> Result<()> {
    publisher.publish_at(now(), state).await?;
    Ok(())
}

fn now() -> LogicalTime {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    LogicalTime::new(0, u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX))
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<ToolRouter>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_listen_is_always_present() {
        assert_eq!(
            listen_endpoints(&RouterConfig::default()),
            vec![DEFAULT_LISTEN.to_string()]
        );
    }

    #[test]
    fn listen_endpoints_deduplicate_default() {
        let config = RouterConfig {
            listen: vec![DEFAULT_LISTEN.to_string(), "tcp/127.0.0.1:7448".to_string()],
            uplink: None,
        };
        assert_eq!(
            listen_endpoints(&config),
            vec![DEFAULT_LISTEN.to_string(), "tcp/127.0.0.1:7448".to_string()]
        );
    }

    #[test]
    fn serial_listen_fails_with_named_diagnostic() {
        let config = RouterConfig {
            listen: vec!["serial//dev/ttyACM0#baudrate=115200".to_string()],
            uplink: None,
        };

        let error = zenoh_router_config(&config).expect_err("serial listen should be rejected");
        let error = error.to_string();
        assert!(error.contains(SERIAL_LISTEN_TRANSPORT_DIAGNOSTIC));
        assert!(error.contains("serial listen endpoints require the serial transport feature"));
        assert!(error.contains("serial//dev/ttyACM0#baudrate=115200"));
    }

    #[test]
    fn initial_uplink_state_reports_disabled_when_absent() {
        let state = initial_uplink_state(&RouterConfig::default());
        assert_eq!(state.phase, api::bus::uplink::UplinkPhase::Disabled);
        assert_eq!(state.connect, None);
    }

    #[test]
    fn zenoh_config_accepts_retry_and_tls_paths() {
        let config = RouterConfig {
            listen: Vec::new(),
            uplink: Some(UplinkConfig {
                connect: "tls/root.example.io:7447".to_string(),
                auth: Some(MtlsAuth {
                    ca: "identity/ca.pem".to_string(),
                    cert: "identity/robot.pem".to_string(),
                    key: "identity/robot.key".to_string(),
                }),
                retry: RetryConfig {
                    initial_ms: 2_000,
                    max_ms: 10_000,
                },
            }),
        };

        zenoh_router_config(&config).expect("router config should be accepted by zenoh");
    }

    #[test]
    fn build_metrics_computes_rate_from_window_count() {
        let samples = vec![TopicWindowSample {
            topic: "dev/robots/robot/y2026_9/joypad/devices".to_string(),
            from_participant: "joypad".to_string(),
            window_count: 10,
            cumulative_count: 1_000,
        }];
        let metrics = build_metrics(&samples, Duration::from_secs(1));
        assert_eq!(metrics.topics.len(), 1);
        assert_eq!(metrics.topics[0].ingress_rate_hz, 10.0);
        assert_eq!(metrics.topics[0].count, 1_000);
        assert_eq!(metrics.topics[0].from_participant, "joypad");
        assert_eq!(metrics.throughput_msg_s, 10.0);
        assert_eq!(metrics.window_ns, 1_000_000_000);
    }

    #[test]
    fn build_metrics_halves_the_rate_for_a_two_second_window() {
        let samples = vec![TopicWindowSample {
            topic: "dev/robots/robot/y2026_9/router/metrics".to_string(),
            from_participant: String::new(),
            window_count: 10,
            cumulative_count: 10,
        }];
        let metrics = build_metrics(&samples, Duration::from_secs(2));
        assert_eq!(metrics.topics[0].ingress_rate_hz, 5.0);
        assert_eq!(metrics.window_ns, 2_000_000_000);
    }

    #[test]
    fn build_metrics_sums_throughput_across_topics() {
        let samples = vec![
            TopicWindowSample {
                topic: "a".to_string(),
                from_participant: String::new(),
                window_count: 4,
                cumulative_count: 4,
            },
            TopicWindowSample {
                topic: "b".to_string(),
                from_participant: String::new(),
                window_count: 6,
                cumulative_count: 6,
            },
        ];
        let metrics = build_metrics(&samples, Duration::from_secs(1));
        assert_eq!(metrics.throughput_msg_s, 10.0);
    }

    #[test]
    fn record_sample_accumulates_cumulative_and_window_counts() {
        let counters: IngressCounters = Mutex::new(HashMap::new());
        record_sample(&counters, "topic".to_string(), Some("alice".to_string()));
        record_sample(&counters, "topic".to_string(), None);
        let guard = counters.lock().unwrap();
        let entry = guard.get("topic").expect("topic counted");
        assert_eq!(entry.cumulative, 2);
        assert_eq!(entry.window, 2);
        // A later sample with no attachment does not clobber a previously
        // observed participant.
        assert_eq!(entry.from_participant, "alice");
    }

    #[test]
    fn drain_window_resets_window_but_keeps_cumulative() {
        let counters: IngressCounters = Mutex::new(HashMap::new());
        record_sample(&counters, "topic".to_string(), None);
        record_sample(&counters, "topic".to_string(), None);
        let first = drain_window(&counters);
        assert_eq!(first[0].window_count, 2);
        assert_eq!(first[0].cumulative_count, 2);

        record_sample(&counters, "topic".to_string(), None);
        let second = drain_window(&counters);
        assert_eq!(second[0].window_count, 1);
        assert_eq!(second[0].cumulative_count, 3);
    }
}
