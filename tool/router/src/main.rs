use phoxal::prelude::*;
use phoxal::raw::{Bus, LogicalTime, OwnerCap, Publisher};
use phoxal_api::y2026_1 as api;

const DEFAULT_LISTEN: &str = "tcp/localhost:7447";
const DEFAULT_RETRY_INITIAL_MS: u64 = 1_000;
const DEFAULT_RETRY_MAX_MS: u64 = 30_000;
const SERIAL_LISTEN_TRANSPORT_DIAGNOSTIC: &str = "serial_listen_transport_unavailable";

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
}
