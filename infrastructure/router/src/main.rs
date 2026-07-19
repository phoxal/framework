use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;

const DEFAULT_PORT_START: u16 = 7_447;
const DEFAULT_PORT_COUNT: u16 = 16;

#[derive(Debug, Parser)]
#[command(
    name = "phoxal-infrastructure-router",
    about = "Run the Phoxal-owned Zenoh router"
)]
struct Args {
    /// Resolved Zenoh JSON5 configuration file.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Exact Zenoh listen endpoint. Repeat to replace authored listeners.
    #[arg(long = "listen", value_name = "ENDPOINT")]
    listen: Vec<String>,
}

struct OpenedRouter {
    session: zenoh::Session,
    listeners: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing()?;
    let opened = open_router(Args::parse()).await?;

    println!("{}", ready_line(&opened.listeners));
    std::io::stdout()
        .flush()
        .context("failed to flush router readiness event")?;
    tracing::info!(listeners = ?opened.listeners, "Phoxal infrastructure router ready");

    shutdown_signal().await?;
    opened
        .session
        .close()
        .await
        .map_err(|error| anyhow::anyhow!("failed to close Zenoh router: {error}"))?;
    Ok(())
}

fn init_tracing() -> Result<()> {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init()
        .map_err(|error| anyhow::anyhow!("failed to initialize router tracing: {error}"))?;
    Ok(())
}

async fn open_router(args: Args) -> Result<OpenedRouter> {
    open_router_in_range(args, DEFAULT_PORT_START, DEFAULT_PORT_COUNT).await
}

async fn open_router_in_range(
    args: Args,
    port_start: u16,
    port_count: u16,
) -> Result<OpenedRouter> {
    let authored = args.config.is_some();
    let mut base = load_config(args.config.as_deref())?;
    let authored_listeners = args
        .config
        .as_deref()
        .map(config_authors_listeners)
        .transpose()?
        .unwrap_or(false);
    force_router_mode(&mut base)?;
    // Authored configs retain their connect and scouting settings; the
    // loopback listener gate below still applies to every TCP listener.
    if !authored {
        apply_no_config_defaults(&mut base)?;
    }

    let listeners = if args.listen.is_empty() {
        if authored && !authored_listeners {
            Vec::new()
        } else {
            configured_listeners(&base)?
        }
    } else {
        args.listen
    };

    if !listeners.is_empty() {
        validate_listeners(&listeners)?;
        let session = open_exact(base, &listeners).await?;
        return Ok(OpenedRouter { session, listeners });
    }

    if port_count == 0 {
        bail!("default router listener range is empty");
    }

    let mut failures = Vec::new();
    for offset in 0..port_count {
        let Some(port) = port_start.checked_add(offset) else {
            break;
        };
        let listener = format!("tcp/127.0.0.1:{port}");
        match open_exact(base.clone(), std::slice::from_ref(&listener)).await {
            Ok(session) => {
                return Ok(OpenedRouter {
                    session,
                    listeners: vec![listener],
                });
            }
            Err(error) => failures.push(format!("{listener}: {error:#}")),
        }
    }

    bail!(
        "failed to bind a loopback router listener in the bounded range starting at {port_start}: {}",
        failures.join("; ")
    )
}

fn load_config(path: Option<&Path>) -> Result<zenoh::Config> {
    match path {
        Some(path) => zenoh::Config::from_file(path).map_err(|error| {
            anyhow::anyhow!(
                "failed to load Zenoh JSON5 config {}: {error}",
                path.display()
            )
        }),
        None => Ok(zenoh::Config::default()),
    }
}

/// Zenoh materializes mode-dependent listener defaults while parsing a config,
/// so inspect the validated source to distinguish an authored listener from an
/// inherited wildcard default. The source is parsed only for key presence;
/// Zenoh remains the authority for the configuration shape and values.
fn config_authors_listeners(path: &Path) -> Result<bool> {
    let source = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read Zenoh JSON5 config {}", path.display()))?;
    let value: serde_json::Value = json5::from_str(&source)
        .with_context(|| format!("failed to inspect Zenoh JSON5 config {}", path.display()))?;
    Ok(value
        .get("listen")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|listen| listen.contains_key("endpoints")))
}

fn force_router_mode(config: &mut zenoh::Config) -> Result<()> {
    insert(config, "mode", r#""router""#)
}

fn apply_no_config_defaults(config: &mut zenoh::Config) -> Result<()> {
    // Leave listener selection to the bounded allocator below instead of
    // inheriting Zenoh's router-mode wildcard default.
    insert(config, "listen/endpoints", "[]")?;
    insert(config, "connect/endpoints", "[]")?;
    insert(config, "scouting/multicast/enabled", "false")?;
    insert(config, "scouting/gossip/enabled", "false")?;
    insert(
        config,
        "scouting/multicast/autoconnect",
        r#"{ router: [], peer: [], client: [] }"#,
    )?;
    insert(
        config,
        "scouting/gossip/autoconnect",
        r#"{ router: [], peer: [], client: [] }"#,
    )?;
    Ok(())
}

fn configured_listeners(config: &zenoh::Config) -> Result<Vec<String>> {
    let json = config
        .get_json("listen/endpoints")
        .map_err(|error| anyhow::anyhow!("failed to read Zenoh listen endpoints: {error}"))?;
    let value: serde_json::Value =
        serde_json::from_str(&json).context("Zenoh listen endpoints are not valid JSON")?;
    let endpoints = match value {
        serde_json::Value::Array(endpoints) => endpoints,
        serde_json::Value::Object(mut modes) => modes
            .remove("router")
            .unwrap_or_else(|| serde_json::Value::Array(Vec::new()))
            .as_array()
            .cloned()
            .context("Zenoh router-mode listen endpoints are not an array")?,
        _ => bail!("Zenoh listen endpoints are not an array or mode map"),
    };
    endpoints
        .into_iter()
        .map(|endpoint| {
            endpoint
                .as_str()
                .map(str::to_string)
                .context("Zenoh listen endpoint is not a string")
        })
        .collect()
}

async fn open_exact(mut config: zenoh::Config, listeners: &[String]) -> Result<zenoh::Session> {
    validate_listeners(listeners)?;
    insert_json(&mut config, "listen/endpoints", listeners)?;
    insert(&mut config, "listen/exit_on_failure", "true")?;
    force_router_mode(&mut config)?;
    zenoh::open(config).await.map_err(|error| {
        anyhow::anyhow!(
            "failed to open Zenoh router on {}: {error}",
            listeners.join(", ")
        )
    })
}

fn validate_listeners(listeners: &[String]) -> Result<()> {
    for listener in listeners {
        let listener = listener.trim();
        if listener.is_empty() {
            bail!("router listen endpoint must not be empty");
        }
        if let Some((scheme, endpoint)) = listener.split_once('/')
            && scheme.eq_ignore_ascii_case("tcp")
            && !tcp_endpoint_is_loopback(endpoint)
        {
            bail!(
                "router TCP listener '{listener}' must bind loopback until listener authentication ships"
            );
        }
    }
    Ok(())
}

fn tcp_endpoint_is_loopback(endpoint: &str) -> bool {
    let endpoint = endpoint
        .split_once('#')
        .map_or(endpoint, |(value, _)| value);
    let host = endpoint
        .strip_prefix('[')
        .and_then(|tail| tail.split_once(']').map(|(host, _rest)| host))
        .or_else(|| endpoint.rsplit_once(':').map(|(host, _port)| host))
        .unwrap_or(endpoint);

    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn insert(config: &mut zenoh::Config, path: &str, json5: &str) -> Result<()> {
    config
        .insert_json5(path, json5)
        .map_err(|error| anyhow::anyhow!("failed to set Zenoh config path {path}: {error}"))
}

fn insert_json<T: serde::Serialize + ?Sized>(
    config: &mut zenoh::Config,
    path: &str,
    value: &T,
) -> Result<()> {
    insert(config, path, &serde_json::to_string(value)?)
}

fn ready_line(listeners: &[String]) -> String {
    serde_json::json!({
        "event": "ready",
        "listen": listeners,
    })
    .to_string()
}

#[cfg(unix)]
async fn shutdown_signal() -> Result<()> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("failed to install SIGTERM handler")?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => {
            result.context("failed to wait for Ctrl-C")?;
        }
        _ = terminate.recv() => {}
    }
    Ok(())
}

#[cfg(not(unix))]
async fn shutdown_signal() -> Result<()> {
    tokio::signal::ctrl_c()
        .await
        .context("failed to wait for Ctrl-C")
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use super::*;

    fn args(listen: Vec<String>) -> Args {
        Args {
            config: None,
            listen,
        }
    }

    fn contiguous_free_range(count: u16) -> (u16, Vec<TcpListener>) {
        for _ in 0..100 {
            let first = TcpListener::bind("127.0.0.1:0").expect("bind candidate");
            let start = first.local_addr().expect("candidate address").port();
            let Some(end) = start.checked_add(count.saturating_sub(1)) else {
                continue;
            };
            if end == u16::MAX {
                continue;
            }
            let mut listeners = vec![first];
            let mut complete = true;
            for port in start + 1..=end {
                match TcpListener::bind(("127.0.0.1", port)) {
                    Ok(listener) => listeners.push(listener),
                    Err(_) => {
                        complete = false;
                        break;
                    }
                }
            }
            if complete {
                return (start, listeners);
            }
        }
        panic!("failed to reserve a contiguous loopback port range")
    }

    #[test]
    fn forced_router_mode_overrides_authored_mode() {
        let mut config = zenoh::Config::from_json5(r#"{ mode: "peer" }"#).expect("config");
        force_router_mode(&mut config).expect("force mode");
        assert_eq!(config.get_json("mode").expect("mode"), r#""router""#);
    }

    #[test]
    fn no_config_defaults_are_loopback_only() {
        let mut config = zenoh::Config::default();
        apply_no_config_defaults(&mut config).expect("safe defaults");
        assert_eq!(config.get_json("listen/endpoints").expect("listen"), "[]");
        assert_eq!(config.get_json("connect/endpoints").expect("connect"), "[]");
        assert_eq!(
            config
                .get_json("scouting/multicast/enabled")
                .expect("multicast"),
            "false"
        );
        assert_eq!(
            config.get_json("scouting/gossip/enabled").expect("gossip"),
            "false"
        );
    }

    #[test]
    fn listener_gate_keeps_tcp_on_loopback_until_authentication_ships() {
        for allowed in [
            "tcp/localhost:7447",
            "tcp/127.0.0.1:7447",
            "tcp/127.2.3.4:7447",
            "tcp/[::1]:7447",
            "tls/0.0.0.0:7447",
        ] {
            validate_listeners(&[allowed.to_string()]).expect(allowed);
        }
        for rejected in [
            "tcp/0.0.0.0:7447",
            "tcp/192.0.2.1:7447",
            "tcp/[::]:7447",
            "tcp/127.example.com:7447",
            "TCP/0.0.0.0:7447",
        ] {
            let error = validate_listeners(&[rejected.to_string()]).expect_err(rejected);
            assert!(
                error
                    .to_string()
                    .contains("until listener authentication ships")
            );
        }
    }

    #[test]
    fn authored_config_listeners_use_the_same_loopback_gate() {
        let file = tempfile::Builder::new()
            .suffix(".json5")
            .tempfile()
            .expect("temporary config");
        std::fs::write(
            file.path(),
            r#"{ listen: { endpoints: ["tcp/0.0.0.0:7447"] } }"#,
        )
        .expect("write config");

        assert!(config_authors_listeners(file.path()).expect("inspect config"));
        let config = load_config(Some(file.path())).expect("load config");
        let listeners = configured_listeners(&config).expect("configured listeners");
        let error = validate_listeners(&listeners).expect_err("non-loopback TCP must fail");
        assert!(
            error
                .to_string()
                .contains("until listener authentication ships")
        );
    }

    #[test]
    fn authored_mode_dependent_listeners_select_router_mode() {
        let config = zenoh::Config::from_json5(
            r#"{
                listen: {
                    endpoints: {
                        router: ["tcp/127.0.0.1:7449"],
                        peer: ["tcp/127.0.0.1:7450"],
                        client: []
                    }
                }
            }"#,
        )
        .expect("config");

        assert_eq!(
            configured_listeners(&config).expect("router listeners"),
            vec!["tcp/127.0.0.1:7449".to_string()]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn authored_config_without_listeners_uses_bounded_loopback_allocation() {
        let (start, listeners) = contiguous_free_range(1);
        drop(listeners);
        let file = tempfile::Builder::new()
            .suffix(".json5")
            .tempfile()
            .expect("temporary config");
        std::fs::write(
            file.path(),
            r#"{
                mode: "peer",
                connect: { endpoints: [] },
                scouting: {
                    multicast: { enabled: false },
                    gossip: { enabled: false }
                }
            }"#,
        )
        .expect("write config");

        let opened = open_router_in_range(
            Args {
                config: Some(file.path().to_path_buf()),
                listen: Vec::new(),
            },
            start,
            1,
        )
        .await
        .expect("authored config should use fallback allocation");
        assert_eq!(opened.listeners, vec![format!("tcp/127.0.0.1:{start}")]);
        opened.session.close().await.expect("close router");
    }

    #[test]
    fn readiness_event_is_machine_readable() {
        let line = ready_line(&["tcp/127.0.0.1:7448".to_string()]);
        let event: serde_json::Value = serde_json::from_str(&line).expect("ready JSON");
        assert_eq!(event["event"], "ready");
        assert_eq!(event["listen"][0], "tcp/127.0.0.1:7448");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn explicit_occupied_listener_fails_instead_of_moving() {
        let occupied = TcpListener::bind("127.0.0.1:0").expect("occupy port");
        let endpoint = format!("tcp/{}", occupied.local_addr().expect("occupied address"));
        let error = match open_router_in_range(args(vec![endpoint.clone()]), 1, 1).await {
            Ok(_) => panic!("explicit listener must fail"),
            Err(error) => error,
        };
        let message = format!("{error:#}");
        assert!(message.contains("failed to open Zenoh router"), "{message}");
        assert!(message.contains(&endpoint), "{message}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn automatic_allocation_skips_an_unrelated_listener() {
        let (start, mut occupied) = contiguous_free_range(2);
        let first = occupied.remove(0);
        drop(occupied);

        let opened = open_router_in_range(args(Vec::new()), start, 2)
            .await
            .expect("fallback router");
        assert_eq!(
            opened.listeners,
            vec![format!("tcp/127.0.0.1:{}", start + 1)]
        );
        opened.session.close().await.expect("close router");
        drop(first);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bounded_allocation_reports_exhaustion() {
        let (start, occupied) = contiguous_free_range(2);
        let error = match open_router_in_range(args(Vec::new()), start, 2).await {
            Ok(_) => panic!("range must be exhausted"),
            Err(error) => error,
        };
        let message = format!("{error:#}");
        assert!(message.contains("bounded range"), "{message}");
        assert!(message.contains(&start.to_string()), "{message}");
        drop(occupied);
    }
}
